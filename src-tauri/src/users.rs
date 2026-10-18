//! User management: Postgres roles and MySQL/MariaDB accounts, with their attributes, role memberships and
//! privileges. Reads use bound parameters. An edit arrives as the account as loaded and as edited, and the statements
//! are worked out from the difference, so the preview and the run produce the same SQL; only the password differs,
//! masked in the preview. Postgres gets the password as a SCRAM-SHA-256 verifier, never in clear.

use crate::db::{Conn, Dialect, Engine};
use crate::error::{Error, Result};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use tauri::State;

/// The most accounts, objects (databases, schemas, tables) and grants of one account read.
const MAX_ROWS: usize = 5000;
/// What the preview shows instead of the password.
const MASKED: &str = "'********'";

/// A privilege, as the server names it (`SHOW_VIEW` for `SHOW VIEW`). Ones not listed here (MySQL 8's dynamic
/// privileges such as `BACKUP_ADMIN`) are left out when reading, so an edit never touches them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Privilege {
    Select,
    Insert,
    Update,
    Delete,
    Truncate,
    References,
    Trigger,
    Maintain,
    Create,
    Connect,
    Temporary,
    Usage,
    Execute,
    Drop,
    Reload,
    Shutdown,
    Process,
    File,
    Index,
    Alter,
    ShowDatabases,
    Super,
    CreateTemporaryTables,
    LockTables,
    ReplicationSlave,
    ReplicationClient,
    CreateView,
    ShowView,
    CreateRoutine,
    AlterRoutine,
    CreateUser,
    Event,
    CreateTablespace,
    CreateRole,
    DropRole,
}

impl Privilege {
    const ALL: [Privilege; 35] = [
        Self::Select,
        Self::Insert,
        Self::Update,
        Self::Delete,
        Self::Truncate,
        Self::References,
        Self::Trigger,
        Self::Maintain,
        Self::Create,
        Self::Connect,
        Self::Temporary,
        Self::Usage,
        Self::Execute,
        Self::Drop,
        Self::Reload,
        Self::Shutdown,
        Self::Process,
        Self::File,
        Self::Index,
        Self::Alter,
        Self::ShowDatabases,
        Self::Super,
        Self::CreateTemporaryTables,
        Self::LockTables,
        Self::ReplicationSlave,
        Self::ReplicationClient,
        Self::CreateView,
        Self::ShowView,
        Self::CreateRoutine,
        Self::AlterRoutine,
        Self::CreateUser,
        Self::Event,
        Self::CreateTablespace,
        Self::CreateRole,
        Self::DropRole,
    ];

    fn sql(self) -> &'static str {
        match self {
            Self::Select => "SELECT",
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
            Self::Truncate => "TRUNCATE",
            Self::References => "REFERENCES",
            Self::Trigger => "TRIGGER",
            Self::Maintain => "MAINTAIN",
            Self::Create => "CREATE",
            Self::Connect => "CONNECT",
            Self::Temporary => "TEMPORARY",
            Self::Usage => "USAGE",
            Self::Execute => "EXECUTE",
            Self::Drop => "DROP",
            Self::Reload => "RELOAD",
            Self::Shutdown => "SHUTDOWN",
            Self::Process => "PROCESS",
            Self::File => "FILE",
            Self::Index => "INDEX",
            Self::Alter => "ALTER",
            Self::ShowDatabases => "SHOW DATABASES",
            Self::Super => "SUPER",
            Self::CreateTemporaryTables => "CREATE TEMPORARY TABLES",
            Self::LockTables => "LOCK TABLES",
            Self::ReplicationSlave => "REPLICATION SLAVE",
            Self::ReplicationClient => "REPLICATION CLIENT",
            Self::CreateView => "CREATE VIEW",
            Self::ShowView => "SHOW VIEW",
            Self::CreateRoutine => "CREATE ROUTINE",
            Self::AlterRoutine => "ALTER ROUTINE",
            Self::CreateUser => "CREATE USER",
            Self::Event => "EVENT",
            Self::CreateTablespace => "CREATE TABLESPACE",
            Self::CreateRole => "CREATE ROLE",
            Self::DropRole => "DROP ROLE",
        }
    }

    fn parse(name: &str) -> Option<Privilege> {
        Self::ALL.into_iter().find(|p| p.sql().eq_ignore_ascii_case(name))
    }
}

/// What a privilege is on. MySQL has global, database and table privileges; Postgres database, schema and table ones
/// (`schema` is the MySQL database for a table there).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Object {
    Global,
    Database { name: String },
    Schema { name: String },
    Table { schema: String, name: String },
}

impl Object {
    /// The `ON` part of GRANT and REVOKE.
    fn sql(&self, d: Dialect) -> Result<String> {
        Ok(match (d, self) {
            (Dialect::My, Object::Global) => "*.*".into(),
            (Dialect::My, Object::Database { name }) => format!("{}.*", d.ident(name)),
            (Dialect::My, Object::Table { schema, name }) => format!("{}.{}", d.ident(schema), d.ident(name)),
            (Dialect::Pg, Object::Database { name }) => format!("DATABASE {}", d.ident(name)),
            (Dialect::Pg, Object::Schema { name }) => format!("SCHEMA {}", d.ident(name)),
            (Dialect::Pg, Object::Table { schema, name }) => format!("TABLE {}.{}", d.ident(schema), d.ident(name)),
            _ => return Err(Error::Invalid("this engine has no privileges on that kind of object")),
        })
    }

    /// From a catalog row: kind, schema, name.
    fn from_row(kind: &str, schema: String, name: String) -> Option<Object> {
        Some(match kind {
            "global" => Object::Global,
            "database" => Object::Database { name },
            "schema" => Object::Schema { name },
            "table" => Object::Table { schema, name },
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub on: Object,
    pub privilege: Privilege,
    /// May pass the privilege on. MySQL keeps this per object rather than per privilege.
    pub passable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(clippy::struct_excessive_bools, reason = "the role attributes, each its own on/off keyword")]
pub struct PgRole {
    pub name: String,
    pub login: bool,
    pub superuser: bool,
    pub create_db: bool,
    pub create_role: bool,
    pub replication: bool,
    pub bypass_rls: bool,
    pub inherit: bool,
    /// -1 for no limit.
    pub connection_limit: i32,
    /// When the password stops working, as the server writes a timestamp; None for never.
    pub valid_until: Option<String>,
    /// The roles this one is a member of.
    pub member_of: Vec<String>,
    /// Filled in by [`user_privileges`]; the role list leaves it empty.
    #[serde(default)]
    pub privileges: Vec<Grant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MyUser {
    pub user: String,
    pub host: String,
    /// The authentication plugin; empty for the server's default (new accounts only).
    pub plugin: String,
    /// The resource limits; 0 for none.
    pub max_queries: u32,
    pub max_updates: u32,
    pub max_connections: u32,
    pub max_user_connections: u32,
    /// `ACCOUNT LOCK`: the account can't sign in. MySQL only; MariaDB keeps it elsewhere, so it's left alone there.
    #[serde(default)]
    pub locked: bool,
    /// The password has expired, so a new one is asked for at the next sign-in. Setting a password clears it. MySQL only.
    #[serde(default)]
    pub password_expired: bool,
    /// Days until the password expires: `None` follows the server's policy, `Some(0)` never. MySQL only.
    #[serde(default)]
    pub password_lifetime: Option<u32>,
    /// Filled in by [`user_privileges`]; the user list leaves it empty.
    #[serde(default)]
    pub privileges: Vec<Grant>,
}

/// One edit: `before` as loaded (None to create), `after` as edited (None to drop).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum Edit {
    Pg { before: Option<PgRole>, after: Option<PgRole> },
    My { before: Option<MyUser>, after: Option<MyUser> },
}

/// What the user manager window lists.
#[derive(Debug, Default, Serialize)]
pub struct Accounts {
    /// Postgres, including the predefined `pg_` roles, which can be granted but not edited.
    pub roles: Vec<PgRole>,
    /// MySQL/MariaDB.
    pub users: Vec<MyUser>,
    /// What privileges can be granted on.
    pub objects: Vec<Object>,
    /// MySQL/MariaDB: the active authentication plugins.
    pub plugins: Vec<String>,
    pub mariadb: bool,
}

fn text(row: &mut [Option<String>], i: usize) -> String {
    row.get_mut(i).and_then(Option::take).unwrap_or_default()
}

fn number<T: std::str::FromStr + Default>(row: &mut [Option<String>], i: usize) -> T {
    text(row, i).parse().unwrap_or_default()
}

/// The catalog rows `kind, schema, name, privilege, grantable` as grants, leaving out privileges this module doesn't
/// know.
fn grants(rows: Vec<Vec<Option<String>>>) -> Vec<Grant> {
    rows.into_iter()
        .filter_map(|mut r| {
            let on = Object::from_row(&text(&mut r, 0), text(&mut r, 1), text(&mut r, 2))?;
            let privilege = Privilege::parse(&text(&mut r, 3))?;
            if privilege == Privilege::Usage && on == Object::Global {
                return None; // MySQL's way of saying the account has no global privileges
            }
            let passable = matches!(text(&mut r, 4).as_str(), "true" | "YES");
            Some(Grant { on, privilege, passable })
        })
        .collect()
}

const PG_ROLES: &str = "select rolname::text, rolcanlogin::text, rolsuper::text, rolcreatedb::text, rolcreaterole::text, \
    rolreplication::text, rolbypassrls::text, rolinherit::text, rolconnlimit::text, rolvaliduntil::text \
    from pg_roles order by rolname limit 5000";
const PG_MEMBERS: &str = "select m.rolname::text, r.rolname::text from pg_auth_members a \
    join pg_roles r on r.oid = a.roleid join pg_roles m on m.oid = a.member";
/// Not the system schemas: their privileges are the server's business.
const PG_USER_SCHEMA: &str = "nspname !~ '^pg_' and nspname <> 'information_schema'";

fn pg_objects_sql() -> String {
    format!(
        "select 'database', '', datname::text from pg_database where not datistemplate \
         union all select 'schema', '', nspname::text from pg_namespace where {PG_USER_SCHEMA} \
         union all select 'table', nspname::text, relname::text from pg_class c join pg_namespace n on n.oid = c.relnamespace \
         where c.relkind in ('r', 'p', 'v', 'm', 'f') and {PG_USER_SCHEMA} order by 1, 2, 3 limit {MAX_ROWS}"
    )
}

/// The role's explicit grants: on every database, and on the schemas and tables of the current one.
fn pg_privileges_sql() -> String {
    let grantee = "a.grantee = (select oid from pg_roles where rolname = $1)";
    let cols = "a.privilege_type::text, a.is_grantable::text";
    format!(
        "select 'database', '', datname::text, {cols} from pg_database, aclexplode(datacl) a where {grantee} \
         union all select 'schema', '', nspname::text, {cols} from pg_namespace, aclexplode(nspacl) a \
         where {grantee} and {PG_USER_SCHEMA} \
         union all select 'table', nspname::text, relname::text, {cols} from pg_class c \
         join pg_namespace n on n.oid = c.relnamespace, aclexplode(c.relacl) a \
         where {grantee} and c.relkind in ('r', 'p', 'v', 'm', 'f') and {PG_USER_SCHEMA} limit {MAX_ROWS}"
    )
}

const MY_SYSTEM: &str = "('mysql', 'sys', 'performance_schema', 'information_schema')";

fn my_objects_sql() -> String {
    format!(
        "select 'database', '', schema_name from information_schema.schemata where schema_name not in {MY_SYSTEM} \
         union all select 'table', table_schema, table_name from information_schema.tables \
         where table_schema not in {MY_SYSTEM} order by 1, 2, 3 limit {MAX_ROWS}"
    )
}

/// The account's grants at each level; `information_schema` names an account `'user'@'host'`.
fn my_privileges_sql() -> String {
    let grantee = "grantee = concat('''', ?, '''@''', ?, '''')";
    format!(
        "select 'global', '', '', privilege_type, is_grantable from information_schema.user_privileges where {grantee} \
         union all select 'database', '', table_schema, privilege_type, is_grantable \
         from information_schema.schema_privileges where {grantee} \
         union all select 'table', table_schema, table_name, privilege_type, is_grantable \
         from information_schema.table_privileges where {grantee} limit {MAX_ROWS}"
    )
}

async fn load(conn: &mut Conn, engine: Engine) -> Result<Accounts> {
    match engine {
        Engine::Postgres => {
            let mut member_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for mut r in conn.fetch_with(PG_MEMBERS, &[]).await? {
                member_of.entry(text(&mut r, 0)).or_default().push(text(&mut r, 1));
            }
            let roles = conn.fetch_with(PG_ROLES, &[]).await?.into_iter().map(|mut r| {
                let name = text(&mut r, 0);
                let flag = |r: &mut Vec<Option<String>>, i| text(r, i) == "true";
                PgRole {
                    login: flag(&mut r, 1),
                    superuser: flag(&mut r, 2),
                    create_db: flag(&mut r, 3),
                    create_role: flag(&mut r, 4),
                    replication: flag(&mut r, 5),
                    bypass_rls: flag(&mut r, 6),
                    inherit: flag(&mut r, 7),
                    connection_limit: number(&mut r, 8),
                    valid_until: Some(text(&mut r, 9)).filter(|v| !v.is_empty() && v != "infinity"),
                    member_of: member_of.remove(&name).unwrap_or_default(),
                    privileges: vec![],
                    name,
                }
            });
            let objects = conn.fetch_with(&pg_objects_sql(), &[]).await?;
            Ok(Accounts { roles: roles.collect(), objects: objects_from(objects), ..Accounts::default() })
        }
        Engine::Mysql | Engine::Mariadb => {
            let mariadb = conn.server_version().await?.starts_with("MariaDB");
            // MariaDB lists its roles among the users; they aren't accounts to sign in with.
            let roles = if mariadb { " where is_role <> 'Y'" } else { "" };
            let query =
                |columns: &str| format!("select {columns} from mysql.user{roles} order by user, host limit {MAX_ROWS}");
            let base = "user, host, plugin, max_questions, max_updates, max_connections, max_user_connections";
            let rows = if mariadb {
                conn.fetch_with(&query(base), &[]).await?
            } else {
                let with_lifecycle = query(&format!("{base}, account_locked, password_expired, password_lifetime"));
                match conn.fetch_with(&with_lifecycle, &[]).await {
                    Ok(rows) => rows,
                    // A server from before MySQL 5.7.6 has no such columns; it shows them as unset.
                    Err(_) => conn.fetch_with(&query(base), &[]).await?,
                }
            };
            let users = rows.into_iter().map(|mut r| MyUser {
                user: text(&mut r, 0),
                host: text(&mut r, 1),
                plugin: text(&mut r, 2),
                max_queries: number(&mut r, 3),
                max_updates: number(&mut r, 4),
                max_connections: number(&mut r, 5),
                max_user_connections: number(&mut r, 6),
                locked: text(&mut r, 7) == "Y",
                password_expired: text(&mut r, 8) == "Y",
                password_lifetime: Some(text(&mut r, 9)).filter(|v| !v.is_empty()).and_then(|v| v.parse().ok()),
                privileges: vec![],
            });
            let users = users.collect();
            let mut objects = vec![Object::Global];
            objects.extend(objects_from(conn.fetch_with(&my_objects_sql(), &[]).await?));
            let plugins = "select plugin_name from information_schema.plugins \
                where plugin_type = 'AUTHENTICATION' and plugin_status = 'ACTIVE' order by 1";
            let plugins = conn.fetch_with(plugins, &[]).await?.into_iter().map(|mut r| text(&mut r, 0)).collect();
            Ok(Accounts { users, objects, plugins, mariadb, ..Accounts::default() })
        }
        Engine::Sqlite | Engine::Duckdb | Engine::Mssql | Engine::Clickhouse | Engine::Libsql | Engine::D1 => {
            Err(Error::Invalid("this engine has no user manager here"))
        }
        Engine::Redis => Err(Error::Invalid("Redis users are managed with ACL commands in the console")),
        Engine::Mongodb => Err(Error::Invalid("MongoDB users are managed in the shell: db.createUser(…)")),
        Engine::Dynamodb => Err(Error::Invalid("DynamoDB access is managed in AWS IAM")),
        Engine::Elasticsearch => {
            Err(Error::Invalid("Elasticsearch users are managed in the console: GET /_security/user"))
        }
        Engine::Cassandra => Err(Error::Invalid("Cassandra roles are managed in the console: LIST ROLES, CREATE ROLE")),
        Engine::Oracle => Err(Error::Invalid("Oracle users are managed in a SQL tab: CREATE USER, GRANT")),
        Engine::Bigquery => Err(Error::Invalid("BigQuery access is managed in Google Cloud IAM")),
    }
}

fn objects_from(rows: Vec<Vec<Option<String>>>) -> Vec<Object> {
    rows.into_iter().filter_map(|mut r| Object::from_row(&text(&mut r, 0), text(&mut r, 1), text(&mut r, 2))).collect()
}

// ---------- Statements ----------

/// The privileges per object, each with its grant option.
fn by_object(grants: &[Grant]) -> BTreeMap<&Object, BTreeMap<Privilege, bool>> {
    let mut m: BTreeMap<&Object, BTreeMap<Privilege, bool>> = BTreeMap::new();
    for g in grants {
        m.entry(&g.on).or_default().insert(g.privilege, g.passable);
    }
    m
}

fn list(privileges: &[Privilege]) -> String {
    privileges.iter().map(|p| p.sql()).collect::<Vec<_>>().join(", ")
}

/// GRANT and REVOKE statements taking `before`'s privileges to `after`'s for `to` (a quoted role or account).
fn privilege_sql(d: Dialect, to: &str, before: &[Grant], after: &[Grant]) -> Result<Vec<String>> {
    let (before, after) = (by_object(before), by_object(after));
    let objects: BTreeSet<&Object> = before.keys().chain(after.keys()).copied().collect();
    let empty = BTreeMap::new();
    let mut out = vec![];
    for object in objects {
        let (b, a) = (before.get(object).unwrap_or(&empty), after.get(object).unwrap_or(&empty));
        let on = object.sql(d)?;
        let pick = |m: &BTreeMap<Privilege, bool>, keep: &dyn Fn(Privilege, bool) -> bool| -> Vec<Privilege> {
            m.iter().filter(|(p, o)| keep(**p, **o)).map(|(p, _)| *p).collect()
        };
        let removed = pick(b, &|p, _| !a.contains_key(&p));
        if d == Dialect::My {
            // MySQL's grant option belongs to the object, not to one privilege.
            let (had, has) = (b.values().any(|o| *o), a.values().any(|o| *o));
            let added = pick(a, &|p, _| !b.contains_key(&p));
            if !removed.is_empty() {
                out.push(format!("REVOKE {} ON {on} FROM {to}", list(&removed)));
            }
            if had && !has {
                out.push(format!("REVOKE GRANT OPTION ON {on} FROM {to}"));
            }
            if has && !had {
                let what = if added.is_empty() { "USAGE".into() } else { list(&added) };
                out.push(format!("GRANT {what} ON {on} TO {to} WITH GRANT OPTION"));
            } else if !added.is_empty() {
                out.push(format!("GRANT {} ON {on} TO {to}", list(&added)));
            }
            continue;
        }
        let plain = pick(a, &|p, o| !o && !b.contains_key(&p));
        let with_option = pick(a, &|p, o| o && b.get(&p) != Some(&true));
        let lose_option = pick(a, &|p, o| !o && b.get(&p) == Some(&true));
        if !removed.is_empty() {
            out.push(format!("REVOKE {} ON {on} FROM {to}", list(&removed)));
        }
        if !lose_option.is_empty() {
            out.push(format!("REVOKE GRANT OPTION FOR {} ON {on} FROM {to}", list(&lose_option)));
        }
        if !plain.is_empty() {
            out.push(format!("GRANT {} ON {on} TO {to}", list(&plain)));
        }
        if !with_option.is_empty() {
            out.push(format!("GRANT {} ON {on} TO {to} WITH GRANT OPTION", list(&with_option)));
        }
    }
    Ok(out)
}

/// A role's on/off attributes with their keywords.
fn pg_flags(r: &PgRole) -> [(&'static str, bool); 7] {
    [
        ("LOGIN", r.login),
        ("SUPERUSER", r.superuser),
        ("CREATEDB", r.create_db),
        ("CREATEROLE", r.create_role),
        ("REPLICATION", r.replication),
        ("BYPASSRLS", r.bypass_rls),
        ("INHERIT", r.inherit),
    ]
}

fn valid_until(r: &PgRole) -> Option<&str> {
    r.valid_until.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

/// Postgres statements for an edit. `password` is the SQL literal to set, or None to leave the password alone.
fn pg_sql(before: Option<&PgRole>, after: Option<&PgRole>, password: Option<&str>) -> Result<Vec<String>> {
    let d = Dialect::Pg;
    let Some(a) = after else {
        let b = before.ok_or(Error::Invalid("nothing to change"))?;
        // Privileges would stop DROP ROLE, so they go first; the role's memberships go with it.
        let role = d.ident(&b.name);
        let mut out = privilege_sql(d, &role, &b.privileges, &[])?;
        out.push(format!("DROP ROLE {role}"));
        return Ok(out);
    };
    if a.name.trim().is_empty() {
        return Err(Error::Invalid("the role needs a name"));
    }
    let role = d.ident(&a.name);
    let mut out = vec![];
    let mut options = vec![];
    let (old_privileges, old_members) = match before {
        None => {
            for (word, on) in pg_flags(a) {
                options.push(if on { word.to_string() } else { format!("NO{word}") });
            }
            if a.connection_limit != -1 {
                options.push(format!("CONNECTION LIMIT {}", a.connection_limit));
            }
            if let Some(v) = valid_until(a) {
                options.push(format!("VALID UNTIL {}", d.literal(v)));
            }
            (&[][..], &[][..])
        }
        Some(b) => {
            if b.name != a.name {
                out.push(format!("ALTER ROLE {} RENAME TO {role}", d.ident(&b.name)));
            }
            for ((word, on), (_, was)) in pg_flags(a).into_iter().zip(pg_flags(b)) {
                if on != was {
                    options.push(if on { word.to_string() } else { format!("NO{word}") });
                }
            }
            if a.connection_limit != b.connection_limit {
                options.push(format!("CONNECTION LIMIT {}", a.connection_limit));
            }
            if valid_until(a) != valid_until(b) {
                options.push(format!("VALID UNTIL {}", d.literal(valid_until(a).unwrap_or("infinity"))));
            }
            (&b.privileges[..], &b.member_of[..])
        }
    };
    if let Some(pw) = password {
        options.push(format!("PASSWORD {pw}"));
    }
    match before {
        None => out.push(format!("CREATE ROLE {role} WITH {}", options.join(" "))),
        Some(_) if !options.is_empty() => out.push(format!("ALTER ROLE {role} WITH {}", options.join(" "))),
        Some(_) => {}
    }
    for parent in a.member_of.iter().filter(|m| !old_members.contains(m)) {
        out.push(format!("GRANT {} TO {role}", d.ident(parent)));
    }
    for parent in old_members.iter().filter(|m| !a.member_of.contains(m)) {
        out.push(format!("REVOKE {} FROM {role}", d.ident(parent)));
    }
    out.extend(privilege_sql(d, &role, old_privileges, &a.privileges)?);
    Ok(out)
}

fn my_account(u: &MyUser) -> String {
    format!("{}@{}", Dialect::My.literal(&u.user), Dialect::My.literal(&u.host))
}

fn my_limits(u: &MyUser) -> [(&'static str, u32); 4] {
    [
        ("MAX_QUERIES_PER_HOUR", u.max_queries),
        ("MAX_UPDATES_PER_HOUR", u.max_updates),
        ("MAX_CONNECTIONS_PER_HOUR", u.max_connections),
        ("MAX_USER_CONNECTIONS", u.max_user_connections),
    ]
}

/// MySQL/MariaDB statements for an edit. `password` is the SQL literal to set, or None to leave the password alone.
fn my_sql(
    mariadb: bool,
    before: Option<&MyUser>,
    after: Option<&MyUser>,
    password: Option<&str>,
) -> Result<Vec<String>> {
    let d = Dialect::My;
    let Some(a) = after else {
        let b = before.ok_or(Error::Invalid("nothing to change"))?;
        return Ok(vec![format!("DROP USER {}", my_account(b))]);
    };
    if a.user.trim().is_empty() {
        return Err(Error::Invalid("the user needs a name"));
    }
    if a.host.trim().is_empty() {
        return Err(Error::Invalid("the user needs a host; % allows any"));
    }
    let account = my_account(a);
    let plugin_changed = before.is_some_and(|b| b.plugin != a.plugin);
    if plugin_changed && password.is_none() {
        // MySQL would set an empty password.
        return Err(Error::Invalid("changing the authentication plugin needs a new password"));
    }
    let plugin = (!a.plugin.is_empty() && (before.is_none() || plugin_changed)).then(|| d.literal(&a.plugin));
    let auth = match (plugin, password) {
        (Some(p), Some(pw)) if mariadb => format!(" IDENTIFIED VIA {p} USING PASSWORD({pw})"),
        (Some(p), Some(pw)) => format!(" IDENTIFIED WITH {p} BY {pw}"),
        (Some(p), None) if mariadb => format!(" IDENTIFIED VIA {p}"),
        (Some(p), None) => format!(" IDENTIFIED WITH {p}"),
        (None, Some(pw)) => format!(" IDENTIFIED BY {pw}"),
        (None, None) => String::new(),
    };
    let limits: Vec<String> = match before {
        None => my_limits(a).into_iter().filter(|(_, n)| *n != 0).map(|(w, n)| format!("{w} {n}")).collect(),
        Some(b) => my_limits(a)
            .into_iter()
            .zip(my_limits(b))
            .filter(|((_, n), (_, was))| n != was)
            .map(|((w, n), _)| format!("{w} {n}"))
            .collect(),
    };
    let limits = if limits.is_empty() { String::new() } else { format!(" WITH {}", limits.join(" ")) };
    // MySQL's password lifetime and account lock, after the limits as its grammar has them; MariaDB's are left alone.
    let (lifetime, lock) = match (mariadb, before) {
        (true, _) => (None, None),
        (false, b) => (
            b.is_none_or(|b| b.password_lifetime != a.password_lifetime)
                .then_some(a.password_lifetime)
                .filter(|l| b.is_some() || l.is_some()),
            b.map_or(a.locked, |b| b.locked != a.locked).then_some(a.locked),
        ),
    };
    let policy = match lifetime {
        None => String::new(),
        Some(None) => " PASSWORD EXPIRE DEFAULT".into(),
        Some(Some(0)) => " PASSWORD EXPIRE NEVER".into(),
        Some(Some(days)) => format!(" PASSWORD EXPIRE INTERVAL {days} DAY"),
    };
    let lock = match lock {
        None => "",
        Some(true) => " ACCOUNT LOCK",
        Some(false) => " ACCOUNT UNLOCK",
    };
    let expire_now = !mariadb && a.password_expired && before.is_none_or(|b| !b.password_expired);
    let mut out = vec![];
    match before {
        None => out.push(format!("CREATE USER {account}{auth}{limits}{policy}{lock}")),
        Some(b) => {
            if b.user != a.user || b.host != a.host {
                out.push(format!("RENAME USER {} TO {account}", my_account(b)));
            }
            if !auth.is_empty() || !limits.is_empty() || !policy.is_empty() || !lock.is_empty() {
                out.push(format!("ALTER USER {account}{auth}{limits}{policy}{lock}"));
            }
        }
    }
    if expire_now {
        // After any new password, which would otherwise undo it.
        out.push(format!("ALTER USER {account} PASSWORD EXPIRE"));
    }
    out.extend(privilege_sql(d, &account, before.map_or(&[][..], |b| &b.privileges), &a.privileges)?);
    Ok(out)
}

/// The statements for `edit` on a connection of `engine`, with `password` (a SQL literal, already hashed for
/// Postgres) when one is set.
fn edit_sql(engine: Engine, mariadb: bool, edit: &Edit, password: Option<&str>) -> Result<Vec<String>> {
    match (engine, edit) {
        (Engine::Postgres, Edit::Pg { before, after }) => pg_sql(before.as_ref(), after.as_ref(), password),
        (Engine::Mysql | Engine::Mariadb, Edit::My { before, after }) => {
            my_sql(mariadb, before.as_ref(), after.as_ref(), password)
        }
        _ => Err(Error::Invalid("that edit is for another engine")),
    }
}

/// The SQL literal of a new password: a SCRAM-SHA-256 verifier for Postgres, so the server (and its statement log)
/// never sees it in clear; the password itself for MySQL.
fn password_literal(engine: Engine, password: &SecretString) -> String {
    let pw = password.expose_secret();
    match engine {
        Engine::Postgres => Dialect::Pg.literal(&postgres_protocol::password::scram_sha_256(pw.as_bytes())),
        _ => Dialect::My.literal(pw),
    }
}

/// A password was typed; a blank field leaves it as it is.
fn given(password: Option<&SecretString>) -> Option<&SecretString> {
    password.filter(|p| !p.expose_secret().is_empty())
}

async fn is_mariadb(live: &mut crate::Live) -> Result<bool> {
    if live.saved.kind == Engine::Postgres {
        return Ok(false);
    }
    let r = live.conn.server_version().await;
    Ok(live.check(r)?.starts_with("MariaDB"))
}

// ---------- Commands ----------

/// The accounts on the server, what privileges can be granted on, and (MySQL) the authentication plugins.
#[tauri::command]
pub async fn users(open: State<'_, crate::Open>, name: String) -> Result<Accounts> {
    let mut live = open.get(&name).await?;
    let kind = live.saved.kind;
    let r = load(&mut live.conn, kind).await;
    live.check(r)
}

/// One account's grants: a Postgres role's `user`, or a MySQL account's `user` and `host`.
#[tauri::command]
pub async fn user_privileges(
    open: State<'_, crate::Open>,
    name: String,
    user: String,
    host: Option<String>,
) -> Result<Vec<Grant>> {
    let mut live = open.get(&name).await?;
    let r = match live.saved.kind {
        Engine::Postgres => live.conn.fetch_with(&pg_privileges_sql(), &[Some(&user)]).await,
        Engine::Mysql | Engine::Mariadb => {
            let host = host.as_deref().ok_or(Error::Invalid("a MySQL account needs its host"))?;
            let params = [Some(user.as_str()), Some(host)];
            live.conn.fetch_with(&my_privileges_sql(), &[params, params, params].concat()).await
        }
        Engine::Sqlite | Engine::Duckdb | Engine::Mssql | Engine::Clickhouse | Engine::Libsql | Engine::D1 => {
            Err(Error::Invalid("this engine has no user manager here"))
        }
        Engine::Redis => Err(Error::Invalid("Redis users are managed with ACL commands in the console")),
        Engine::Mongodb => Err(Error::Invalid("MongoDB users are managed in the shell: db.createUser(…)")),
        Engine::Dynamodb => Err(Error::Invalid("DynamoDB access is managed in AWS IAM")),
        Engine::Elasticsearch => {
            Err(Error::Invalid("Elasticsearch users are managed in the console: GET /_security/user"))
        }
        Engine::Cassandra => Err(Error::Invalid("Cassandra roles are managed in the console: LIST ROLES, CREATE ROLE")),
        Engine::Oracle => Err(Error::Invalid("Oracle users are managed in a SQL tab: CREATE USER, GRANT")),
        Engine::Bigquery => Err(Error::Invalid("BigQuery access is managed in Google Cloud IAM")),
    };
    Ok(grants(live.check(r)?))
}

/// The statements an edit would run, for the preview; a new password shows masked.
#[tauri::command]
pub async fn user_sql(
    open: State<'_, crate::Open>,
    name: String,
    edit: Edit,
    password: Option<SecretString>,
) -> Result<Vec<String>> {
    let mut live = open.get(&name).await?;
    let mariadb = is_mariadb(&mut live).await?;
    edit_sql(live.saved.kind, mariadb, &edit, given(password.as_ref()).map(|_| MASKED))
}

/// Runs an edit the user saw in the preview. Safe mode applies as for [`crate::run_ddl`]. An error that lists the
/// statements already run lists them with the password masked.
#[tauri::command]
pub async fn apply_user_edit(
    open: State<'_, crate::Open>,
    name: String,
    edit: Edit,
    password: Option<SecretString>,
    confirmed: bool,
    unlock: Option<crate::safety::Unlock>,
) -> Result<()> {
    crate::guard(&open, &name, |_| false, confirmed, unlock.as_ref()).await?;
    let mut live = open.get(&name).await?;
    let (kind, mariadb) = (live.saved.kind, is_mariadb(&mut live).await?);
    let literal = given(password.as_ref()).map(|pw| password_literal(kind, pw));
    let statements = edit_sql(kind, mariadb, &edit, literal.as_deref())?;
    let r = live.conn.run_ddl(&statements).await;
    live.check(r).map_err(|e| match e {
        Error::DdlPartlyApplied { ran, source } => {
            Error::DdlPartlyApplied { ran: crate::console::redact(&ran).into_owned(), source }
        }
        e => e,
    })
}

#[cfg(test)]
mod tests {
    use super::{Edit, Grant, MyUser, Object, PgRole, Privilege as P, edit_sql, grants, my_sql, pg_sql, privilege_sql};
    use crate::db::{Conn, Dialect, Engine, Options};
    use crate::tls::Tls;

    fn role(name: &str) -> PgRole {
        PgRole {
            name: name.into(),
            login: true,
            superuser: false,
            create_db: false,
            create_role: false,
            replication: false,
            bypass_rls: false,
            inherit: true,
            connection_limit: -1,
            valid_until: None,
            member_of: vec![],
            privileges: vec![],
        }
    }

    fn user(name: &str, host: &str) -> MyUser {
        MyUser {
            user: name.into(),
            host: host.into(),
            plugin: String::new(),
            max_queries: 0,
            max_updates: 0,
            max_connections: 0,
            max_user_connections: 0,
            locked: false,
            password_expired: false,
            password_lifetime: None,
            privileges: vec![],
        }
    }

    fn table(schema: &str, name: &str) -> Object {
        Object::Table { schema: schema.into(), name: name.into() }
    }

    fn grant(on: &Object, privilege: P, passable: bool) -> Grant {
        Grant { on: on.clone(), privilege, passable }
    }

    #[test]
    fn pg_create_should_spell_out_every_attribute_then_grant_memberships_and_privileges() {
        let mut r = role("app\"user");
        r.connection_limit = 5;
        r.valid_until = Some("2030-01-01".into());
        r.member_of = vec!["readers".into()];
        r.privileges =
            vec![grant(&table("public", "t"), P::Select, false), grant(&table("public", "t"), P::Insert, true)];
        assert_eq!(
            pg_sql(None, Some(&r), Some("'x'")).unwrap(),
            [
                "CREATE ROLE \"app\"\"user\" WITH LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS \
                 INHERIT CONNECTION LIMIT 5 VALID UNTIL E'2030-01-01' PASSWORD 'x'",
                "GRANT \"readers\" TO \"app\"\"user\"",
                "GRANT SELECT ON TABLE \"public\".\"t\" TO \"app\"\"user\"",
                "GRANT INSERT ON TABLE \"public\".\"t\" TO \"app\"\"user\" WITH GRANT OPTION",
            ]
        );
    }

    #[test]
    fn pg_edit_should_rename_first_and_alter_only_what_changed() {
        let mut before = role("old");
        before.member_of = vec!["a".into(), "b".into()];
        before.valid_until = Some("2030-01-01 00:00:00+00".into());
        let mut after = before.clone();
        after.name = "new".into();
        after.create_db = true;
        after.inherit = false;
        after.valid_until = None;
        after.member_of = vec!["b".into(), "c".into()];
        assert_eq!(
            pg_sql(Some(&before), Some(&after), None).unwrap(),
            [
                "ALTER ROLE \"old\" RENAME TO \"new\"",
                "ALTER ROLE \"new\" WITH CREATEDB NOINHERIT VALID UNTIL E'infinity'",
                "GRANT \"c\" TO \"new\"",
                "REVOKE \"a\" FROM \"new\"",
            ]
        );
        assert!(pg_sql(Some(&before), Some(&before), None).unwrap().is_empty(), "nothing changed");
    }

    #[test]
    fn pg_privileges_should_grant_revoke_and_move_the_grant_option_per_privilege() {
        let (db, t) = (Object::Database { name: "d".into() }, table("s", "t"));
        let before = [grant(&db, P::Connect, false), grant(&t, P::Select, true), grant(&t, P::Update, false)];
        let after = [grant(&t, P::Select, false), grant(&t, P::Update, true), grant(&t, P::Delete, false)];
        assert_eq!(
            privilege_sql(Dialect::Pg, "\"r\"", &before, &after).unwrap(),
            [
                "REVOKE CONNECT ON DATABASE \"d\" FROM \"r\"",
                "REVOKE GRANT OPTION FOR SELECT ON TABLE \"s\".\"t\" FROM \"r\"",
                "GRANT DELETE ON TABLE \"s\".\"t\" TO \"r\"",
                "GRANT UPDATE ON TABLE \"s\".\"t\" TO \"r\" WITH GRANT OPTION",
            ]
        );
    }

    #[test]
    fn pg_drop_should_revoke_the_roles_privileges_first() {
        let mut r = role("gone");
        r.privileges = vec![grant(&Object::Schema { name: "s".into() }, P::Usage, false)];
        assert_eq!(
            pg_sql(Some(&r), None, None).unwrap(),
            ["REVOKE USAGE ON SCHEMA \"s\" FROM \"gone\"", "DROP ROLE \"gone\""]
        );
    }

    #[test]
    fn pg_should_refuse_a_nameless_role_and_global_privileges() {
        assert!(pg_sql(None, Some(&role(" ")), None).is_err());
        let mut r = role("r");
        r.privileges = vec![grant(&Object::Global, P::Select, false)];
        assert!(pg_sql(None, Some(&r), None).is_err());
    }

    #[test]
    fn my_create_should_quote_user_and_host_as_literals_with_plugin_and_limits() {
        let mut u = user("o'neil", "10.0.%");
        u.plugin = "caching_sha2_password".into();
        u.max_queries = 100;
        u.max_user_connections = 3;
        u.privileges =
            vec![grant(&Object::Global, P::Process, false), grant(&table("shop", "orders"), P::Select, true)];
        assert_eq!(
            my_sql(false, None, Some(&u), Some("'pw'")).unwrap(),
            [
                "CREATE USER 'o''neil'@'10.0.%' IDENTIFIED WITH 'caching_sha2_password' BY 'pw' \
                 WITH MAX_QUERIES_PER_HOUR 100 MAX_USER_CONNECTIONS 3",
                "GRANT PROCESS ON *.* TO 'o''neil'@'10.0.%'",
                "GRANT SELECT ON `shop`.`orders` TO 'o''neil'@'10.0.%' WITH GRANT OPTION",
            ]
        );
    }

    #[test]
    fn my_create_should_lock_and_set_the_password_lifetime_after_the_limits() {
        let mut u = user("u", "%");
        u.max_queries = 5;
        u.locked = true;
        u.password_lifetime = Some(90);
        u.password_expired = true;
        assert_eq!(
            my_sql(false, None, Some(&u), Some("'pw'")).unwrap(),
            [
                "CREATE USER 'u'@'%' IDENTIFIED BY 'pw' WITH MAX_QUERIES_PER_HOUR 5 \
                 PASSWORD EXPIRE INTERVAL 90 DAY ACCOUNT LOCK",
                "ALTER USER 'u'@'%' PASSWORD EXPIRE",
            ]
        );
    }

    #[test]
    fn my_edit_should_change_only_the_lifecycle_settings_that_changed() {
        let before = user("u", "%");
        let mut after = before.clone();
        after.password_lifetime = Some(0);
        assert_eq!(
            my_sql(false, Some(&before), Some(&after), None).unwrap(),
            ["ALTER USER 'u'@'%' PASSWORD EXPIRE NEVER"]
        );
        let locked = MyUser { locked: true, password_lifetime: Some(30), ..user("u", "%") };
        let unlocked = MyUser { locked: false, password_lifetime: None, ..locked.clone() };
        assert_eq!(
            my_sql(false, Some(&locked), Some(&unlocked), None).unwrap(),
            ["ALTER USER 'u'@'%' PASSWORD EXPIRE DEFAULT ACCOUNT UNLOCK"]
        );
        assert_eq!(my_sql(false, Some(&locked), Some(&locked), None).unwrap(), Vec::<String>::new(), "nothing changed");
    }

    #[test]
    fn my_edit_should_not_expire_a_password_that_already_is_or_touch_mariadb_lifecycles() {
        let expired = MyUser { password_expired: true, ..user("u", "%") };
        assert_eq!(my_sql(false, Some(&expired), Some(&expired), None).unwrap(), Vec::<String>::new());
        let wants_all = MyUser { locked: true, password_expired: true, password_lifetime: Some(7), ..user("u", "%") };
        assert_eq!(my_sql(true, None, Some(&wants_all), None).unwrap(), ["CREATE USER 'u'@'%'"]);
    }

    #[test]
    fn my_create_on_mariadb_should_use_via_and_the_password_function() {
        let mut u = user("u", "%");
        u.plugin = "ed25519".into();
        assert_eq!(
            my_sql(true, None, Some(&u), Some("'pw'")).unwrap(),
            ["CREATE USER 'u'@'%' IDENTIFIED VIA 'ed25519' USING PASSWORD('pw')"]
        );
    }

    #[test]
    fn my_edit_should_rename_then_alter_the_password_and_changed_limits() {
        let mut before = user("u", "localhost");
        before.plugin = "mysql_native_password".into();
        before.max_queries = 10;
        let mut after = before.clone();
        after.host = "%".into();
        after.max_queries = 0;
        after.max_connections = 7;
        assert_eq!(
            my_sql(false, Some(&before), Some(&after), Some("'pw'")).unwrap(),
            [
                "RENAME USER 'u'@'localhost' TO 'u'@'%'",
                "ALTER USER 'u'@'%' IDENTIFIED BY 'pw' WITH MAX_QUERIES_PER_HOUR 0 MAX_CONNECTIONS_PER_HOUR 7",
            ]
        );
        assert_eq!(my_sql(false, Some(&before), None, None).unwrap(), ["DROP USER 'u'@'localhost'"]);
    }

    #[test]
    fn my_edit_should_refuse_a_plugin_change_without_a_password() {
        let before = user("u", "%");
        let mut after = before.clone();
        after.plugin = "sha256_password".into();
        assert!(my_sql(false, Some(&before), Some(&after), None).is_err());
        assert_eq!(
            my_sql(false, Some(&before), Some(&after), Some("'pw'")).unwrap(),
            ["ALTER USER 'u'@'%' IDENTIFIED WITH 'sha256_password' BY 'pw'"]
        );
    }

    #[test]
    fn my_privileges_should_keep_the_grant_option_per_object() {
        let db = Object::Database { name: "shop".into() };
        let before = [grant(&db, P::Select, true), grant(&db, P::Insert, true)];
        let after = [grant(&db, P::Select, false), grant(&db, P::Update, false)];
        assert_eq!(
            privilege_sql(Dialect::My, "'u'@'%'", &before, &after).unwrap(),
            [
                "REVOKE INSERT ON `shop`.* FROM 'u'@'%'",
                "REVOKE GRANT OPTION ON `shop`.* FROM 'u'@'%'",
                "GRANT UPDATE ON `shop`.* TO 'u'@'%'",
            ]
        );
        let only_option = [grant(&db, P::Select, true), grant(&db, P::Insert, true)];
        assert_eq!(
            privilege_sql(Dialect::My, "'u'@'%'", &after[..1], &only_option[..1]).unwrap(),
            ["GRANT USAGE ON `shop`.* TO 'u'@'%' WITH GRANT OPTION"]
        );
    }

    #[test]
    fn edit_sql_should_refuse_an_edit_for_another_engine() {
        let edit = Edit::Pg { before: None, after: Some(role("r")) };
        assert!(edit_sql(Engine::Mysql, false, &edit, None).is_err());
    }

    #[test]
    fn grants_should_read_catalog_rows_and_skip_unknown_privileges() {
        let row = |cells: [&str; 5]| cells.iter().map(|c| Some((*c).to_string())).collect::<Vec<_>>();
        let read = grants(vec![
            row(["table", "s", "t", "SELECT", "true"]),
            row(["global", "", "", "SHOW DATABASES", "YES"]),
            row(["global", "", "", "BACKUP_ADMIN", "NO"]),
            row(["global", "", "", "USAGE", "NO"]),
        ]);
        assert_eq!(read, [grant(&table("s", "t"), P::Select, true), grant(&Object::Global, P::ShowDatabases, true)]);
    }

    #[test]
    fn edit_should_arrive_from_the_ui_as_json() {
        let json = r#"{"my":{"before":null,"after":{"user":"u","host":"%","plugin":"","max_queries":0,"max_updates":0,
            "max_connections":0,"max_user_connections":0,
            "privileges":[{"on":{"kind":"table","schema":"d","name":"t"},"privilege":"SHOW_VIEW","passable":false}]}}}"#;
        let edit: Edit = serde_json::from_str(json).unwrap();
        let Edit::My { after: Some(u), .. } = edit else { panic!("{edit:?}") };
        assert_eq!(u.privileges[0].privilege, P::ShowView);
        assert!(serde_json::from_str::<Edit>(&json.replace("\"plugin\"", "\"extra\":1,\"plugin\"")).is_err());
    }

    async fn run(conn: &mut Conn, engine: Engine, edit: &Edit, password: Option<&str>) {
        let pw = password.map(|p| super::password_literal(engine, &p.into()));
        let statements = edit_sql(engine, false, edit, pw.as_deref()).unwrap();
        conn.run_ddl(&statements).await.unwrap();
    }

    /// The role as the user manager reads it, privileges included; None once it's gone.
    async fn read_role(c: &mut Conn, name: &str) -> Option<PgRole> {
        let mut r = super::load(c, Engine::Postgres).await.unwrap().roles.into_iter().find(|x| x.name == name)?;
        r.privileges = grants(c.fetch_with(&super::pg_privileges_sql(), &[Some(name)]).await.unwrap());
        Some(r)
    }

    async fn read_user(c: &mut Conn, name: &str) -> Option<MyUser> {
        let mut u = super::load(c, Engine::Mysql).await.unwrap().users.into_iter().find(|x| x.user == name)?;
        let p = [Some(u.user.as_str()), Some(u.host.as_str())];
        let rows = c.fetch_with(&super::my_privileges_sql(), &[p, p, p].concat()).await.unwrap();
        u.privileges = grants(rows);
        u.privileges.sort_by(|a, b| a.on.cmp(&b.on));
        Some(u)
    }

    async fn open(engine: Engine, url: &str) -> Conn {
        Conn::open(engine, url, None, &Tls::default(), &Options::default(), None).await.unwrap()
    }

    #[test]
    #[ignore = "needs a local Postgres"]
    fn postgres_role_should_be_created_read_back_edited_and_dropped() {
        tauri::async_runtime::block_on(async {
            let url =
                std::env::var("DBM_PG_URL").unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5432/postgres".into());
            let mut c = open(Engine::Postgres, &url).await;
            let name = format!("tm_users_test_{}", std::process::id());
            c.query(&format!("create table \"{name}\" (id int)")).await.unwrap();
            let mut r = role(&name);
            r.create_db = true;
            r.connection_limit = 3;
            r.member_of = vec!["pg_read_all_stats".into()];
            r.privileges = vec![grant(&table("public", &name), P::Select, true)];
            run(&mut c, Engine::Postgres, &Edit::Pg { before: None, after: Some(r.clone()) }, Some("s3cret")).await;
            let read = read_role(&mut c, &name).await.unwrap();
            assert_eq!(read, r);
            assert!(super::load(&mut c, Engine::Postgres).await.unwrap().objects.contains(&table("public", &name)));
            let stored = c.fetch_with("select rolpassword from pg_authid where rolname = $1", &[Some(&name)]).await;
            let stored = stored.unwrap()[0][0].clone().unwrap();
            assert!(stored.starts_with("SCRAM-SHA-256$"), "sent hashed: {stored}");

            let mut edited = read.clone();
            edited.create_db = false;
            edited.member_of.clear();
            edited.privileges = vec![grant(&table("public", &name), P::Insert, false)];
            run(&mut c, Engine::Postgres, &Edit::Pg { before: Some(read), after: Some(edited.clone()) }, None).await;
            let again = read_role(&mut c, &name).await.unwrap();
            assert_eq!(again, edited);

            run(&mut c, Engine::Postgres, &Edit::Pg { before: Some(again), after: None }, None).await;
            assert!(read_role(&mut c, &name).await.is_none());
            c.query(&format!("drop table \"{name}\"")).await.unwrap();
        });
    }

    #[test]
    #[ignore = "needs a local MySQL"]
    fn mysql_user_should_be_created_read_back_edited_and_dropped() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Mysql, "mysql://root@127.0.0.1:3306").await;
            let name = format!("tm_users_test_{}", std::process::id());
            let mut u = user(&name, "%");
            u.plugin = "mysql_native_password".into();
            u.max_queries = 50;
            u.privileges = vec![
                grant(&Object::Global, P::Process, false),
                grant(&Object::Database { name: "mysql".into() }, P::Select, true),
                grant(&table("mysql", "db"), P::Select, false),
            ];
            run(&mut c, Engine::Mysql, &Edit::My { before: None, after: Some(u.clone()) }, Some("s3cret pw'")).await;
            let read = read_user(&mut c, &name).await.unwrap();
            assert_eq!(read, u);
            let login = format!("mysql://{name}:s3cret%20pw%27@127.0.0.1:3306");
            let mut signed_in = open(Engine::Mysql, &login).await;
            assert_eq!(signed_in.query("select current_user()").await.unwrap().rows[0][0], Some(format!("{name}@%")));
            drop(signed_in);

            let mut edited = read.clone();
            edited.host = "10.%".into();
            edited.max_queries = 0;
            edited.privileges = vec![grant(&Object::Database { name: "mysql".into() }, P::Select, false)];
            run(&mut c, Engine::Mysql, &Edit::My { before: Some(read), after: Some(edited.clone()) }, None).await;
            let again = read_user(&mut c, &name).await.unwrap();
            assert_eq!(again, edited);

            let locked = MyUser { locked: true, password_lifetime: Some(30), ..again };
            run(&mut c, Engine::Mysql, &Edit::My { before: Some(edited), after: Some(locked.clone()) }, None).await;
            let read = read_user(&mut c, &name).await.unwrap();
            assert_eq!(read, locked, "locked, with a 30 day lifetime");

            let expired = MyUser { locked: false, password_lifetime: None, password_expired: true, ..read };
            run(&mut c, Engine::Mysql, &Edit::My { before: Some(locked), after: Some(expired.clone()) }, None).await;
            let read = read_user(&mut c, &name).await.unwrap();
            assert_eq!(read, expired, "unlocked, the server's policy again, password expired");
            run(&mut c, Engine::Mysql, &Edit::My { before: Some(read), after: None }, None).await;
            assert!(read_user(&mut c, &name).await.is_none());
        });
    }
}

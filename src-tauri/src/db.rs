//! One connection type over the supported SQL engines: open (through the engine's [`Driver`]), run SQL (results as
//! text cells), browse the catalog.

use crate::driver::clickhouse::ClickHouse;
use crate::driver::d1::D1;
use crate::driver::duckdb::DuckDb;
use crate::driver::libsql::Libsql;
use crate::driver::mssql::SqlServer;
use crate::driver::mysql::Mysql;
use crate::driver::postgres::{Postgres, cockroach_version};
use crate::driver::sqlite::Sqlite;
use crate::driver::{Driver, Results};
use crate::error::{Error, Result};
use crate::tls::Tls;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub use crate::driver::{RowSink, Stopper};

/// How long opening a connection may take before giving up, so a dead host can't hang the app.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Postgres,
    Mysql,
    Mariadb,
    Sqlite,
    Duckdb,
    Mssql,
    Clickhouse,
    /// libSQL (`sqld`, Turso) over HTTP; SQLite underneath.
    Libsql,
    /// Cloudflare D1 through Cloudflare's HTTP API; SQLite underneath.
    D1,
    Redis,
    /// Not SQL: opened through `mongo_connect` (mongo/mod.rs), as Redis is.
    Mongodb,
    /// Not SQL: opened through `dynamo_connect` (dynamo/mod.rs).
    Dynamodb,
    /// Not SQL underneath: opened through `elastic_connect` (elastic/mod.rs).
    Elasticsearch,
}

/// Per-connection options beyond host, user and TLS. Engine-specific ones are ignored by other engines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
#[expect(clippy::struct_excessive_bools, reason = "independent on/off settings, one per checkbox in the form")]
pub struct Options {
    /// SQL run right after connecting, and again whenever the connection is reopened.
    pub startup_sql: String,
    /// Shell script run before connecting (and before any SSH tunnel), e.g. to bring up a VPN. It must finish
    /// within the connect timeout; a non-zero exit stops the connect.
    pub startup_script: String,
    /// Postgres 17+: start TLS straight away instead of asking the server first (`sslnegotiation=direct`).
    pub direct_tls: bool,
    /// Postgres: never use SCRAM channel binding, for poolers and proxies that break it.
    pub no_channel_binding: bool,
    /// Postgres: also list tables in `pg_catalog` and `information_schema`.
    pub system_schemas: bool,
    /// MySQL and Postgres: connect through this Unix socket instead of host and port (for Postgres, its folder or the
    /// `.s.PGSQL.<port>` file).
    pub socket: String,
    /// MySQL: the TLS server name to send and check, for proxies that route by SNI.
    pub sni: String,
    /// MySQL: allow the cleartext password plugin (PAM/LDAP accounts). Sends the password as is: use with TLS.
    pub cleartext_auth: bool,
}

impl Options {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Runs the connection's startup script with `/bin/sh -c` (`cmd /C` on Windows). The script is the user's own shell
/// code, so a shell is exactly what it needs; nothing from elsewhere is interpolated into it. Imported connections
/// only bring one along when the user saw it and chose to keep it (`launcher::Scripts`).
pub async fn run_startup_script(script: &str) -> Result<()> {
    let (shell, flag) = if cfg!(windows) { ("cmd", "/C") } else { ("/bin/sh", "-c") };
    let run = tokio::process::Command::new(shell).arg(flag).arg(script).kill_on_drop(true).output();
    let out = tokio::time::timeout(CONNECT_TIMEOUT, run)
        .await
        .map_err(|_| Error::StartupScript(format!("still running after {} seconds", CONNECT_TIMEOUT.as_secs())))??;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let last: String = stderr.trim().lines().last().unwrap_or_default().chars().take(300).collect();
    Err(Error::StartupScript(match out.status.code() {
        Some(code) if last.is_empty() => format!("exited with status {code}"),
        Some(code) => format!("exited with status {code}: {last}"),
        None => "was stopped by a signal".into(),
    }))
}

#[derive(Debug, Serialize)]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub primary: bool,
}

#[derive(Debug, Serialize, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub affected: u64,
    /// The row limit cut the result short.
    pub truncated: bool,
}

/// How an engine writes SQL, for code that builds statements without needing a live connection (DDL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Pg,
    My,
    Lite,
    Duck,
    Mssql,
    Click,
}

impl Dialect {
    /// A quoted identifier. Identifiers can't be bound as parameters, so this and [`Dialect::literal`] are the
    /// only places SQL is escaped by hand.
    pub fn ident(self, name: &str) -> String {
        match self {
            Dialect::My => format!("`{}`", name.replace('`', "``")),
            Dialect::Mssql => format!("[{}]", name.replace(']', "]]")),
            // ClickHouse reads names like strings: a backslash escapes the next character.
            Dialect::Click => format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\"")),
            _ => format!("\"{}\"", name.replace('"', "\"\"")),
        }
    }

    /// A table name; Postgres, `DuckDB` and `SQL Server` tables are `schema.table` (`DuckDB`'s `main` and `SQL Server`'s
    /// `dbo` schemas are left off).
    pub fn table_ident(self, table: &str) -> String {
        match (self, table.split_once('.')) {
            (Dialect::Pg | Dialect::Duck | Dialect::Mssql, Some((schema, name))) => {
                format!("{}.{}", self.ident(schema), self.ident(name))
            }
            _ => self.ident(table),
        }
    }

    /// A quoted string literal, for the few places a value can't be bound: DDL such as `COMMENT ON` takes no
    /// parameters. Postgres gets an `E''` string, whose meaning doesn't depend on `standard_conforming_strings`.
    /// MySQL gets escaped backslashes too: under `NO_BACKSLASH_ESCAPES` a backslash then reads doubled, but the
    /// quote still can't be closed early.
    pub fn literal(self, value: &str) -> String {
        let quoted = value.replace('\'', "''");
        match self {
            Dialect::Pg => format!("E'{}'", quoted.replace('\\', "\\\\")),
            Dialect::My => format!("'{}'", quoted.replace('\\', "\\\\")),
            Dialect::Lite | Dialect::Duck => format!("'{quoted}'"),
            // N'…' keeps characters outside the database's code page; a backslash is only a backslash.
            Dialect::Mssql => format!("N'{quoted}'"),
            // Backslash escapes, and the quote escaped that way rather than doubled.
            Dialect::Click => format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'")),
        }
    }

    /// The placeholder for the `n`th (1-based) bound parameter.
    pub fn param(self, n: usize) -> String {
        match self {
            Dialect::My => "?".into(),
            Dialect::Mssql => format!("@P{n}"),
            Dialect::Click => format!("{{p{n}:String}}"),
            _ => format!("${n}"),
        }
    }
}

/// A quoted identifier for code that only knows whether it's MySQL; [`Dialect::ident`] does the quoting.
pub(crate) fn quote_ident(mysql: bool, name: &str) -> String {
    if mysql { Dialect::My.ident(name) } else { Dialect::Pg.ident(name) }
}

/// Both crypto providers rustls ships with are in the build (the SQL Server driver wants one, MySQL's TLS the other),
/// so rustls can't pick a process-wide default itself. This installs `ring`, once; already installed is fine.
pub(crate) fn install_tls_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// An open connection: the engine's [`Driver`], and on top of it the catalog queries and statement building that
/// differ by [`Dialect`].
pub struct Conn {
    driver: Box<dyn Driver>,
}

impl Conn {
    /// Opens a connection and runs its startup SQL. `url` is a connection URL without its password, or a file
    /// path for SQLite; `password` is the login password, or the `SQLCipher` key for SQLite. With `tunnel_port`,
    /// the driver dials that port on 127.0.0.1 instead of the URL's host, but still checks TLS against it.
    pub async fn open(
        engine: Engine,
        url: &str,
        password: Option<&SecretString>,
        tls: &Tls,
        options: &Options,
        tunnel_port: Option<u16>,
    ) -> Result<Conn> {
        install_tls_provider();
        let open = async {
            Ok::<Box<dyn Driver>, Error>(match engine {
                Engine::Postgres => Box::new(Postgres::open(url, password, tls, options, tunnel_port).await?),
                Engine::Mysql | Engine::Mariadb => {
                    Box::new(Mysql::open(url, password, tls, options, tunnel_port).await?)
                }
                Engine::Sqlite => Box::new(Sqlite::open(url, password)?),
                Engine::Duckdb => Box::new(DuckDb::open(url)?),
                Engine::Clickhouse => Box::new(ClickHouse::open(url, password, tls, options, tunnel_port).await?),
                Engine::Libsql => Box::new(Libsql::open(url, password, tls, options, tunnel_port).await?),
                Engine::D1 => Box::new(D1::open(url, password, tls).await?),
                // Boxed: tiberius's connect future is big.
                Engine::Mssql => Box::new(Box::pin(SqlServer::open(url, password, tls, options, tunnel_port)).await?),
                Engine::Redis => return Err(Error::Invalid("Redis isn't SQL; it opens through redis_connect")),
                Engine::Mongodb => return Err(Error::Invalid("MongoDB isn't SQL; it opens through mongo_connect")),
                Engine::Dynamodb => return Err(Error::Invalid("DynamoDB isn't SQL; it opens through dynamo_connect")),
                Engine::Elasticsearch => {
                    return Err(Error::Invalid("Elasticsearch isn't SQL; it opens through elastic_connect"));
                }
            })
        };
        let driver = tokio::time::timeout(CONNECT_TIMEOUT, open)
            .await
            .map_err(|_| Error::ConnectTimeout(CONNECT_TIMEOUT.as_secs()))??;
        let mut conn = Conn { driver };
        if !options.startup_sql.trim().is_empty() {
            conn.batch(&options.startup_sql).await.map_err(|e| Error::StartupSql(Box::new(e)))?;
        }
        Ok(conn)
    }

    /// The SQLite connection, taken out of its `Conn`; None for other engines.
    pub fn into_sqlite(self) -> Option<rusqlite::Connection> {
        self.driver.into_any().downcast::<Sqlite>().ok().map(|s| s.0)
    }

    /// Runs one or more statements, discarding any results.
    async fn batch(&mut self, sql: &str) -> Result<()> {
        crate::console::record(sql);
        self.driver.batch(sql).await
    }

    pub fn stopper(&self) -> Stopper {
        self.driver.stopper()
    }

    /// Whether `err` came from SQLite being interrupted by its [`Stopper`].
    pub fn interrupted_by(err: &Error) -> bool {
        matches!(err, Error::Sqlite(rusqlite::Error::SqliteFailure(f, _)) if f.code == rusqlite::ErrorCode::OperationInterrupted)
    }

    /// A round trip that does nothing, to keep idle connections from being closed by the server or the network.
    pub async fn ping(&mut self) -> Result<()> {
        self.driver.ping().await
    }

    /// The TLS cipher the session uses; None when it's unencrypted (or SQLite).
    pub async fn tls_cipher(&mut self) -> Result<Option<String>> {
        let cipher = match self.dialect() {
            Dialect::Pg => self.scalar("select cipher from pg_stat_ssl where pid = pg_backend_pid()").await?,
            Dialect::My => {
                let r = self.query("show status like 'Ssl_cipher'").await?;
                r.rows.into_iter().next().and_then(|mut row| row.pop().flatten())
            }
            Dialect::Mssql => self
                .scalar("select case when encrypt_option = 'TRUE' then 'TLS' end from sys.dm_exec_connections where session_id = @@spid")
                .await?,
            Dialect::Lite | Dialect::Duck | Dialect::Click => None,
        };
        Ok(cipher.filter(|c| !c.is_empty()))
    }

    /// Whether `err` (from this connection) means the connection itself is gone, not just that a statement failed.
    pub fn lost_by(&self, err: &Error) -> bool {
        self.driver.lost_by(err)
    }

    /// Statements that make the database itself refuse writes, then undo that: a read-only transaction (rolled
    /// back), or SQLite's `query_only`. They catch what the read-only parse can't see, such as a function that writes.
    pub fn read_only_bounds(&self) -> (&'static str, &'static str) {
        self.driver.read_only_bounds()
    }

    /// Whether `err` is the database refusing a write inside [`Conn::read_only_bounds`]: Postgres's
    /// `read_only_sql_transaction` (25006), MySQL's `ER_CANT_EXECUTE_IN_READ_ONLY_TRANSACTION` (1792), or
    /// SQLite's `READONLY`.
    pub fn refused_as_read_only(err: &Error) -> bool {
        match err {
            Error::Postgres(e) => e.code() == Some(&tokio_postgres::error::SqlState::READ_ONLY_SQL_TRANSACTION),
            Error::MySql(mysql_async::Error::Server(e)) => e.code == 1792,
            Error::Sqlite(rusqlite::Error::SqliteFailure(e, _)) => e.code == rusqlite::ErrorCode::ReadOnly,
            _ => false,
        }
    }

    /// Runs SQL typed by the user and returns the last result set as text cells (None = NULL).
    pub async fn query(&mut self, sql: &str) -> Result<QueryResult> {
        self.query_limited(sql, None).await
    }

    /// Like [`Conn::query`], but keeps at most `max_rows` rows of the result; `truncated` says more were there.
    pub async fn query_limited(&mut self, sql: &str, max_rows: Option<usize>) -> Result<QueryResult> {
        Ok(self.query_results(sql, max_rows).await?.pop().unwrap_or_default())
    }

    /// Runs one or more statements and returns every statement's result in order (at least one). At most
    /// `max_rows` rows are kept across them all, and at most [`crate::driver::MAX_RESULTS`] results; `truncated` marks a result
    /// that lost rows. Rows past the limit are read and dropped, so memory stays bounded and the connection stays
    /// in step.
    pub async fn query_results(&mut self, sql: &str, max_rows: Option<usize>) -> Result<Vec<QueryResult>> {
        crate::console::record(sql);
        let mut out = Results::new(max_rows);
        self.driver.run(sql, &mut out).await?;
        Ok(out.finish())
    }

    /// Every table's columns in the current database, for autocomplete: (table, column) pairs in column order.
    /// Postgres tables are `schema.table`.
    pub async fn schema(&mut self) -> Result<Vec<(String, String)>> {
        let sql = match self.dialect() {
            Dialect::Pg => {
                "select table_schema || '.' || table_name, column_name from information_schema.columns \
                 where table_schema not in ('pg_catalog', 'information_schema', 'crdb_internal', 'pg_extension') \
                 order by table_schema, table_name, ordinal_position"
            }
            Dialect::My => {
                "select table_name, column_name from information_schema.columns where table_schema = database() \
                 order by table_name, ordinal_position"
            }
            Dialect::Lite => {
                "select m.name, p.name from sqlite_master m join pragma_table_info(m.name) p \
                 where m.type in ('table', 'view') and m.name not like '\\_cf\\_%' escape '\\' \
                 order by m.name, p.cid"
            }
            Dialect::Duck => {
                "select case when table_schema = 'main' then table_name else table_schema || '.' || table_name end, \
                 column_name from information_schema.columns where table_catalog = current_database() \
                 and table_schema not in ('information_schema', 'pg_catalog') \
                 order by table_schema, table_name, ordinal_position"
            }
            Dialect::Mssql => {
                "select case when table_schema = 'dbo' then table_name else table_schema + '.' + table_name end, \
                 column_name from information_schema.columns order by table_schema, table_name, ordinal_position"
            }
            Dialect::Click => {
                "select table, name from system.columns where database = currentDatabase() order by table, position"
            }
        };
        let r = self.query(sql).await?;
        Ok(r.rows
            .into_iter()
            .filter_map(|mut row| {
                let column = row.pop().flatten()?;
                Some((row.pop().flatten()?, column))
            })
            .collect())
    }

    /// The most bound parameters one statement should carry.
    pub(crate) fn max_params(&self) -> usize {
        self.driver.max_params()
    }

    /// The placeholder for the `n`th (1-based) bound parameter.
    pub(crate) fn param(&self, n: usize) -> String {
        self.driver.param(n)
    }

    /// Runs one statement with bound parameters and returns the rows it affected.
    pub(crate) async fn execute_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<u64> {
        crate::console::record(sql);
        self.driver.execute_with(sql, params).await
    }

    /// Runs a catalog query with bound parameters. Every selected column must be text (cast `::text` on Postgres).
    pub(crate) async fn fetch_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Vec<Vec<Option<String>>>> {
        crate::console::record(sql);
        self.driver.fetch_with(sql, params).await
    }

    /// Runs one query and hands its rows to `sink` as they arrive, so a large result never sits in memory. An
    /// error from the sink stops the query. With `params` on Postgres, every selected column must be text (as for
    /// [`Conn::fetch_with`]); without, any query works.
    pub(crate) async fn stream(&mut self, sql: &str, params: &[Option<&str>], sink: &mut dyn RowSink) -> Result<()> {
        crate::console::record(sql);
        self.driver.stream(sql, params, sink).await
    }

    async fn scalar(&mut self, sql: &str) -> Result<Option<String>> {
        Ok(self.query(sql).await?.rows.into_iter().next().and_then(|mut row| row.swap_remove(0)))
    }

    /// Databases on the server; empty for SQLite and `DuckDB` (one file, one database).
    pub async fn databases(&mut self) -> Result<Vec<String>> {
        let sql = match self.dialect() {
            Dialect::Pg => "select datname from pg_database where not datistemplate and datallowconn order by 1",
            Dialect::My => "select schema_name from information_schema.schemata order by 1",
            Dialect::Mssql => "select name from sys.databases where state = 0 and database_id > 4 order by name",
            Dialect::Click => {
                "select name from system.databases \
                 where name not in ('INFORMATION_SCHEMA', 'information_schema', 'system') order by name"
            }
            Dialect::Lite | Dialect::Duck => return Ok(vec![]),
        };
        Ok(self.query(sql).await?.rows.into_iter().filter_map(|mut row| row.swap_remove(0)).collect())
    }

    /// The selected database; None when a MySQL connection has none yet.
    pub async fn current_database(&mut self) -> Result<Option<String>> {
        match self.dialect() {
            Dialect::Pg => self.scalar("select current_database()").await,
            Dialect::My => self.scalar("select database()").await,
            Dialect::Mssql => self.scalar("select db_name()").await,
            Dialect::Click => self.scalar("select currentDatabase()").await,
            Dialect::Lite | Dialect::Duck => Ok(None),
        }
    }

    /// The server's name and version, e.g. "PostgreSQL 17.2", "`CockroachDB` 25.1.0" or "MariaDB 11.4.2".
    pub async fn server_version(&mut self) -> Result<String> {
        if self.dialect() == Dialect::Pg {
            // CockroachDB's server_version is the Postgres version it imitates; only version() names it.
            if let Some(v) = cockroach_version(&self.scalar("select version()").await?.unwrap_or_default()) {
                return Ok(v);
            }
        }
        let (label, sql) = match self.dialect() {
            Dialect::Pg => ("PostgreSQL", "show server_version"),
            Dialect::My => ("MySQL", "select version()"),
            Dialect::Lite => ("SQLite", "select sqlite_version()"),
            Dialect::Duck => ("DuckDB", "select version()"),
            Dialect::Mssql => ("SQL Server", "select cast(serverproperty('ProductVersion') as nvarchar(50))"),
            Dialect::Click => ("ClickHouse", "select version()"),
        };
        let v = self.scalar(sql).await?.unwrap_or_default();
        // MariaDB reports e.g. "11.4.2-MariaDB"; name it after what the server says it is.
        Ok(if v.contains("MariaDB") {
            format!("MariaDB {}", v.split('-').next().unwrap_or(&v))
        } else {
            format!("{label} {v}")
        })
    }

    /// MySQL switches in place; Postgres cannot change database within a session, so the caller reconnects.
    pub async fn use_database(&mut self, db: &str) -> Result<()> {
        match self.dialect() {
            Dialect::Click => self.driver.use_database(db).await,
            Dialect::My | Dialect::Mssql => {
                let sql = format!("use {}", self.ident(db));
                self.query(&sql).await.map(|_| ())
            }
            _ => Err(Error::Invalid("this engine switches database by reconnecting")),
        }
    }

    pub(crate) fn dialect(&self) -> Dialect {
        self.driver.dialect()
    }

    /// A quoted identifier; see [`Dialect::ident`].
    pub(crate) fn ident(&self, name: &str) -> String {
        self.dialect().ident(name)
    }

    pub(crate) fn table_ident(&self, table: &str) -> String {
        self.dialect().table_ident(table)
    }

    /// Columns of `table` (`schema.table` for Postgres) in definition order.
    pub async fn columns(&mut self, table: &str) -> Result<Vec<Column>> {
        let (sql, params) = match self.dialect() {
            Dialect::Pg => {
                let (schema, name) = table.split_once('.').unwrap_or(("public", table));
                (
                    // information_schema leaves out materialized views, so pg_attribute adds theirs.
                    "select name, data_type, nullable, dflt, pk from (\
                     select c.column_name::text as name, (c.udt_name || coalesce('(' || c.character_maximum_length || ')', ''))::text as data_type, \
                     c.is_nullable::text as nullable, c.column_default::text as dflt, case when exists (select 1 from information_schema.table_constraints t \
                     join information_schema.key_column_usage k on k.constraint_name = t.constraint_name \
                     and k.table_schema = t.table_schema and k.table_name = t.table_name \
                     where t.constraint_type = 'PRIMARY KEY' and t.table_schema = c.table_schema \
                     and t.table_name = c.table_name and k.column_name = c.column_name) then 'PRI' else '' end as pk, \
                     c.ordinal_position::int as pos \
                     from information_schema.columns c where c.table_schema = $1 and c.table_name = $2 \
                     union all \
                     select a.attname::text, (t.typname || case when a.atttypmod > 4 and t.typname in ('varchar', 'bpchar') \
                     then '(' || (a.atttypmod - 4) || ')' else '' end)::text, case when a.attnotnull then 'NO' else 'YES' end, \
                     null::text, '', a.attnum::int \
                     from pg_attribute a join pg_class c on c.oid = a.attrelid join pg_namespace n on n.oid = c.relnamespace \
                     join pg_type t on t.oid = a.atttypid \
                     where c.relkind = 'm' and n.nspname = $1 and c.relname = $2 and a.attnum > 0 and not a.attisdropped) x order by pos",
                    vec![Some(schema), Some(name)],
                )
            }
            Dialect::My => (
                "select column_name, column_type, is_nullable, column_default, column_key from information_schema.columns \
                 where table_schema = database() and table_name = ? order by ordinal_position",
                vec![Some(table)],
            ),
            Dialect::Lite => (
                "select name, type, case when \"notnull\" then 'NO' else 'YES' end, dflt_value, \
                 case when pk > 0 then 'PRI' else '' end from pragma_table_info($1)",
                vec![Some(table)],
            ),
            Dialect::Click => (
                "select name, type, if(startsWith(type, 'Nullable('), 'YES', 'NO'), nullIf(default_expression, ''), \
                 if(is_in_primary_key, 'PRI', '') from system.columns \
                 where database = currentDatabase() and table = {p1:String} order by position",
                vec![Some(table)],
            ),
            Dialect::Mssql => {
                let (schema, name) = table.split_once('.').unwrap_or(("dbo", table));
                (
                    "select c.column_name, c.data_type + case \
                     when c.character_maximum_length is not null then '(' + case when c.character_maximum_length = -1 \
                       then 'max' else cast(c.character_maximum_length as varchar(10)) end + ')' \
                     when c.data_type in ('decimal', 'numeric') then '(' + cast(c.numeric_precision as varchar(10)) + ',' \
                       + cast(c.numeric_scale as varchar(10)) + ')' else '' end, \
                     c.is_nullable, c.column_default, \
                     case when exists (select 1 from information_schema.table_constraints t \
                       join information_schema.key_column_usage k on k.constraint_name = t.constraint_name \
                       and k.table_schema = t.table_schema and k.table_name = t.table_name \
                       where t.constraint_type = 'PRIMARY KEY' and t.table_schema = c.table_schema \
                       and t.table_name = c.table_name and k.column_name = c.column_name) then 'PRI' else '' end \
                     from information_schema.columns c where c.table_schema = @P1 and c.table_name = @P2 \
                     order by c.ordinal_position",
                    vec![Some(schema), Some(name)],
                )
            }
            Dialect::Duck => {
                let (schema, name) = table.split_once('.').unwrap_or(("main", table));
                (
                    "select c.column_name, c.data_type, c.is_nullable, c.column_default, \
                     case when exists (select 1 from duckdb_constraints() k where k.constraint_type = 'PRIMARY KEY' \
                     and k.database_name = c.table_catalog and k.schema_name = c.table_schema \
                     and k.table_name = c.table_name and list_contains(k.constraint_column_names, c.column_name)) \
                     then 'PRI' else '' end \
                     from information_schema.columns c where c.table_catalog = current_database() \
                     and c.table_schema = $1 and c.table_name = $2 order by c.ordinal_position",
                    vec![Some(schema), Some(name)],
                )
            }
        };
        Ok(self
            .fetch_with(sql, &params)
            .await?
            .into_iter()
            .map(|mut r| {
                r.resize(5, None);
                let mut cell = |i: usize| r[i].take();
                Column {
                    name: cell(0).unwrap_or_default(),
                    data_type: cell(1).unwrap_or_default(),
                    nullable: cell(2).as_deref() == Some("YES"),
                    default: cell(3),
                    primary: cell(4).as_deref() == Some("PRI"),
                }
            })
            .collect())
    }

    /// Tables and views in the current database (`schema.table` for Postgres).
    /// `system` adds Postgres's `pg_catalog` and `information_schema` tables.
    pub async fn tables(&mut self, system: bool) -> Result<Vec<String>> {
        let sql = match self.dialect() {
            Dialect::Pg if system => {
                "select table_schema || '.' || table_name from information_schema.tables order by 1"
            }
            Dialect::Pg => {
                "select table_schema || '.' || table_name from information_schema.tables \
                 where table_schema not in ('pg_catalog', 'information_schema', 'crdb_internal', 'pg_extension') order by 1"
            }
            Dialect::My => {
                "select table_name from information_schema.tables where table_schema = database() order by 1"
            }
            // D1 keeps tables of its own (`_cf_KV`) that it won't let anyone read.
            Dialect::Lite => {
                "select name from sqlite_master where type in ('table', 'view') and name not like '\\_cf\\_%' escape '\\' \
                 order by 1"
            }
            Dialect::Duck => {
                "select case when table_schema = 'main' then table_name else table_schema || '.' || table_name end \
                 from information_schema.tables where table_catalog = current_database() \
                 and table_schema not in ('information_schema', 'pg_catalog') order by table_schema <> 'main', 1"
            }
            Dialect::Mssql => {
                "select case when table_schema = 'dbo' then table_name else table_schema + '.' + table_name end \
                 from information_schema.tables where table_type in ('BASE TABLE', 'VIEW') \
                 order by case when table_schema = 'dbo' then 0 else 1 end, 1"
            }
            Dialect::Click => "select name from system.tables where database = currentDatabase() order by name",
        };
        let r = self.query(sql).await?;
        Ok(r.rows.into_iter().filter_map(|mut row| row.swap_remove(0)).collect())
    }
}

#[cfg(test)]
mod tests {
    /// Quoting can't be broken out of: whatever the name or value, the SQL parser reads back exactly it.
    mod quoting {
        use crate::db::Dialect;
        use proptest::prelude::*;
        use sqlparser::ast::{Expr, ObjectNamePart, SelectItem, SetExpr, Statement, TableFactor, Value};
        use sqlparser::dialect::{
            Dialect as Parsing, DuckDbDialect, MsSqlDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect,
        };
        use sqlparser::parser::Parser;

        // ClickHouse isn't here: sqlparser reads its backslash escapes differently, so a real server checks its quoting
        // (`local_clickhouse_should_quote_names_and_values_so_the_server_reads_them_back`).
        fn dialects() -> [(Dialect, Box<dyn Parsing>); 5] {
            [
                (Dialect::Pg, Box::new(PostgreSqlDialect {})),
                (Dialect::My, Box::new(MySqlDialect {})),
                (Dialect::Lite, Box::new(SQLiteDialect {})),
                (Dialect::Duck, Box::new(DuckDbDialect {})),
                (Dialect::Mssql, Box::new(MsSqlDialect {})),
            ]
        }

        /// The one SELECT in `sql`, parsed.
        fn select(p: &dyn Parsing, sql: &str) -> Box<sqlparser::ast::Select> {
            let mut stmts = Parser::parse_sql(p, sql).unwrap_or_else(|e| panic!("{sql}: {e}"));
            assert_eq!(stmts.len(), 1, "{sql}");
            let Some(Statement::Query(q)) = stmts.pop() else { panic!("{sql}: not a query") };
            let SetExpr::Select(s) = *q.body else { panic!("{sql}: not a select") };
            s
        }

        proptest! {
            #[test]
            fn ident_reads_back_as_the_same_name(name in "\\PC{1,30}") {
                for (d, p) in dialects() {
                    let sql = format!("SELECT 1 FROM {}", d.ident(&name));
                    let s = select(p.as_ref(), &sql);
                    let TableFactor::Table { name: read, .. } = &s.from[0].relation else { panic!("{sql}") };
                    let parts: Vec<_> = read.0.iter().filter_map(ObjectNamePart::as_ident).map(|i| i.value.as_str()).collect();
                    prop_assert_eq!(parts, [name.as_str()], "{}", sql);
                }
            }

            #[test]
            fn literal_reads_back_as_the_same_value(value in "\\PC{0,40}") {
                for (d, p) in dialects() {
                    let sql = format!("SELECT {}", d.literal(&value));
                    let s = select(p.as_ref(), &sql);
                    let SelectItem::UnnamedExpr(Expr::Value(v)) = &s.projection[0] else { panic!("{sql}") };
                    let read = match &v.value {
                        Value::SingleQuotedString(t) | Value::EscapedStringLiteral(t) | Value::NationalStringLiteral(t) => t,
                        other => panic!("{sql}: {other:?}"),
                    };
                    prop_assert_eq!(read, &value, "{}", sql);
                }
            }
        }
    }

    #[test]
    fn refused_as_read_only_should_recognise_sqlite_refusing_a_write_in_bounds() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            c.query("create table t (a int)").await.unwrap();
            let (start, end) = c.read_only_bounds();
            c.query(start).await.unwrap();
            let refused = c.query("insert into t values (1)").await.unwrap_err();
            c.query(end).await.unwrap();
            assert!(Conn::refused_as_read_only(&refused), "{refused}");
            let other = c.query("select * from nope").await.unwrap_err();
            assert!(!Conn::refused_as_read_only(&other), "{other}");
        });
    }

    /// A function that writes passes the read-only parse; the bounds still stop it. `DBM_PG_URL` points elsewhere.
    #[test]
    #[ignore = "needs a local Postgres"]
    fn refused_as_read_only_should_recognise_postgres_refusing_a_writing_function() {
        tauri::async_runtime::block_on(async {
            let url =
                std::env::var("DBM_PG_URL").unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5432/postgres".into());
            let mut c = open(Engine::Postgres, &url).await;
            // A real table: Postgres lets a read-only transaction write temporary ones.
            let t = format!("dbm_bumped_{}", std::process::id());
            c.query(&format!(
                "create table {t} (n int); create function pg_temp.bump() returns int language sql as \
                 'insert into {t} values (1) returning 1'"
            ))
            .await
            .unwrap();
            let sql = "select pg_temp.bump()";
            let parsed = crate::readonly::is_read_only(Engine::Postgres, sql);
            let (start, end) = c.read_only_bounds();
            c.query(start).await.unwrap();
            let refused = c.query(sql).await;
            c.query(end).await.unwrap();
            c.query(&format!("drop table {t}")).await.unwrap();
            assert!(parsed, "the parse can't tell");
            let refused = refused.unwrap_err();
            assert!(Conn::refused_as_read_only(&refused), "{refused}");
        });
    }

    #[test]
    #[ignore = "needs a local MySQL"]
    fn refused_as_read_only_should_recognise_mysql_refusing_a_write_in_bounds() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Mysql, "mysql://root@127.0.0.1:3306").await;
            let db = format!("dbm_ro_{}", std::process::id());
            c.query(&format!("create database {db}; use {db}; create table t (a int)")).await.unwrap();
            let (start, end) = c.read_only_bounds();
            c.query(start).await.unwrap();
            let refused = c.query("insert into t values (1)").await.unwrap_err();
            c.query(end).await.unwrap();
            c.query(&format!("drop database {db}")).await.unwrap();
            assert!(Conn::refused_as_read_only(&refused), "{refused}");
        });
    }

    #[test]
    fn read_only_bounds_should_make_sqlite_refuse_writes_until_ended() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            c.query("create table t (a int)").await.unwrap();
            let (start, end) = c.read_only_bounds();
            c.query(start).await.unwrap();
            assert!(c.query("insert into t values (1)").await.is_err());
            c.query(end).await.unwrap();
            c.query("insert into t values (1)").await.unwrap();
        });
    }

    use super::{Conn, Engine};
    use crate::tls::Tls;

    async fn open(engine: Engine, url: &str) -> Conn {
        Conn::open(engine, url, None, &Tls::default(), &super::Options::default(), None).await.unwrap()
    }

    #[test]
    fn sqlite_round_trip() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            assert_eq!(c.query("create table t(id integer, name text)").await.unwrap().affected, 0);
            assert_eq!(c.query("insert into t values (1, 'a'), (2, null)").await.unwrap().affected, 2);
            let r = c.query("select id, name from t order by id").await.unwrap();
            assert_eq!(r.columns, ["id", "name"]);
            assert_eq!(r.rows, [vec![Some("1".into()), Some("a".into())], vec![Some("2".into()), None]]);
            assert_eq!(c.tables(false).await.unwrap(), ["t"]);
            assert_eq!(c.databases().await.unwrap(), Vec::<String>::new());
            assert!(c.server_version().await.unwrap().starts_with("SQLite 3."));
        });
    }

    #[test]
    fn columns_handles_quotes_in_table_names() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            c.query("create table \"it's\"(id integer primary key, v text not null default 'x')").await.unwrap();
            let cols = c.columns("it's").await.unwrap();
            assert_eq!((cols[0].name.as_str(), cols[0].primary, cols[1].nullable), ("id", true, false));
            assert_eq!(cols[1].default.as_deref(), Some("'x'"));
        });
    }

    #[test]
    fn query_limited_should_cap_rows_and_say_so() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            let sql = "with recursive n(x) as (select 1 union all select x + 1 from n where x < 50) select x from n";
            let r = c.query_limited(sql, Some(10)).await.unwrap();
            assert_eq!((r.rows.len(), r.truncated), (10, true));
            let r = c.query_limited(sql, Some(50)).await.unwrap();
            assert_eq!((r.rows.len(), r.truncated), (50, false));
        });
    }

    #[test]
    fn query_results_should_return_every_statements_result_in_order() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            let sql = "create table t(a); insert into t values (1), (2); select a from t; select 'x' as b";
            let r = c.query_results(sql, None).await.unwrap();
            assert_eq!(r.len(), 4);
            assert_eq!((r[1].columns.len(), r[1].affected), (0, 2), "the insert's count");
            assert_eq!((r[2].columns.as_slice(), r[2].rows.len()), (&["a".to_string()][..], 2));
            assert_eq!(r[3].rows[0][0].as_deref(), Some("x"));
        });
    }

    #[test]
    #[ignore = "needs a local Postgres; creates and drops a scratch schema"]
    fn local_postgres_columns_should_list_a_materialized_views_columns() {
        tauri::async_runtime::block_on(async {
            let url =
                std::env::var("DBM_PG_URL").unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5432/postgres".into());
            let mut c = open(Engine::Postgres, &url).await;
            c.batch(
                "drop schema if exists tm_mv cascade; create schema tm_mv; \
                 create materialized view tm_mv.m as select 1 as id, 'x'::varchar(5) as label",
            )
            .await
            .unwrap();
            let cols = c.columns("tm_mv.m").await;
            c.batch("drop schema tm_mv cascade").await.unwrap();
            let cols = cols.unwrap();
            let seen: Vec<_> = cols.iter().map(|c| (c.name.as_str(), c.data_type.as_str())).collect();
            assert_eq!(seen, [("id", "int4"), ("label", "varchar(5)")]);
        });
    }

    #[test]
    #[ignore = "needs a local Postgres"]
    fn local_postgres_query_results_should_return_every_statements_result() {
        tauri::async_runtime::block_on(async {
            let mut c = open(
                Engine::Postgres,
                &std::env::var("DBM_PG_URL").unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5432/postgres".into()),
            )
            .await;
            let sql = "create temporary table tm_multi(a int); insert into tm_multi values (1), (2), (3); \
                       select a from tm_multi order by a; select 'x' as b";
            let r = c.query_results(sql, Some(2)).await.unwrap();
            assert_eq!(r.len(), 4);
            assert_eq!((r[1].columns.len(), r[1].affected), (0, 3), "the insert's count");
            assert_eq!((r[2].rows.len(), r[2].truncated), (2, true));
            assert_eq!((r[3].rows.len(), r[3].truncated), (0, true));
        });
    }

    #[test]
    fn query_results_should_share_the_row_limit_across_results() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            let r = c.query_results("select 1 union all select 2; select 3 union all select 4", Some(3)).await.unwrap();
            assert_eq!((r[0].rows.len(), r[0].truncated), (2, false));
            assert_eq!((r[1].rows.len(), r[1].truncated), (1, true));
        });
    }

    #[test]
    fn query_results_should_keep_at_most_max_results() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            let sql = "select 1;".repeat(crate::driver::MAX_RESULTS + 5);
            assert_eq!(c.query_results(&sql, None).await.unwrap().len(), crate::driver::MAX_RESULTS);
        });
    }

    #[test]
    fn query_results_should_give_one_empty_result_for_a_comment() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            let r = c.query_results("-- nothing", None).await.unwrap();
            assert_eq!((r.len(), r[0].columns.len()), (1, 0));
        });
    }

    #[test]
    #[ignore = "needs a local MySQL"]
    fn local_mysql_query_results_should_return_every_result_set() {
        tauri::async_runtime::block_on(async {
            // A temporary table, gone with the session; it needs a database selected.
            let mut c = open(Engine::Mysql, "mysql://root@127.0.0.1:3306/mysql").await;
            let sql = "create temporary table tm_multi(a int); insert into tm_multi values (1), (2), (3); \
                       select a from tm_multi; select 'x' as b, 'y' as c";
            let r = c.query_results(sql, Some(2)).await.unwrap();
            assert_eq!(r.len(), 4);
            assert_eq!((r[1].columns.len(), r[1].affected), (0, 3), "the insert's count");
            assert_eq!((r[2].rows.len(), r[2].truncated), (2, true));
            assert_eq!((r[3].columns.len(), r[3].rows.len(), r[3].truncated), (2, 0, true));
            assert_eq!(c.query("select 1").await.unwrap().rows[0][0].as_deref(), Some("1"), "still in step");
        });
    }

    #[test]
    fn schema_should_list_columns_per_table_in_order() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            c.query("create table b(y text, x text)").await.unwrap();
            c.query("create table a(id integer)").await.unwrap();
            let pairs = c.schema().await.unwrap();
            let pairs: Vec<(&str, &str)> = pairs.iter().map(|(t, col)| (t.as_str(), col.as_str())).collect();
            assert_eq!(pairs, [("a", "id"), ("b", "y"), ("b", "x")]);
        });
    }

    #[test]
    fn open_runs_the_startup_sql() {
        tauri::async_runtime::block_on(async {
            let options = super::Options {
                startup_sql: "create table t(x); insert into t values (7);".into(),
                ..Default::default()
            };
            let mut c = Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &options, None).await.unwrap();
            assert_eq!(c.query("select x from t").await.unwrap().rows[0][0].as_deref(), Some("7"));
        });
    }

    #[test]
    fn open_reports_failing_startup_sql() {
        tauri::async_runtime::block_on(async {
            let options = super::Options { startup_sql: "not sql".into(), ..Default::default() };
            let r = Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &options, None).await;
            let err = r.err().unwrap().to_string();
            assert!(err.starts_with("startup SQL: "), "{err}");
        });
    }

    #[test]
    fn startup_script_should_pass_when_it_exits_zero() {
        tauri::async_runtime::block_on(async { super::run_startup_script("true").await.unwrap() });
    }

    #[test]
    fn startup_script_should_report_its_status_and_last_error_line() {
        tauri::async_runtime::block_on(async {
            let err = super::run_startup_script("echo first >&2; echo vpn down >&2; exit 3").await.unwrap_err();
            assert_eq!(err.to_string(), "startup script exited with status 3: vpn down");
        });
    }

    #[test]
    fn sqlcipher_key_should_unlock_only_with_the_right_passphrase() {
        tauri::async_runtime::block_on(async {
            let path = std::env::temp_dir().join(format!("tableminus-{}-cipher.db", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let path = path.to_str().unwrap();
            let opts = super::Options::default();
            let key = |k: &str| secrecy::SecretString::from(k);

            let mut c =
                Conn::open(Engine::Sqlite, path, Some(&key("hunter2")), &Tls::default(), &opts, None).await.unwrap();
            c.query("create table secret(v text)").await.unwrap();
            c.query("insert into secret values ('kept')").await.unwrap();
            drop(c);
            assert!(!std::fs::read(path).unwrap().starts_with(b"SQLite format 3"), "the file is encrypted on disk");

            let err = Conn::open(Engine::Sqlite, path, Some(&key("wrong")), &Tls::default(), &opts, None)
                .await
                .err()
                .unwrap();
            assert_eq!(err.to_string(), "wrong passphrase, or the file isn't an SQLCipher database");

            let mut c =
                Conn::open(Engine::Sqlite, path, Some(&key("hunter2")), &Tls::default(), &opts, None).await.unwrap();
            assert_eq!(c.query("select v from secret").await.unwrap().rows[0][0].as_deref(), Some("kept"));
            let _ = std::fs::remove_file(path);
        });
    }

    #[test]
    fn open_reports_bad_sqlite_path() {
        tauri::async_runtime::block_on(async {
            let r = Conn::open(
                Engine::Sqlite,
                "/no/such/dir/x.db",
                None,
                &Tls::default(),
                &super::Options::default(),
                None,
            )
            .await;
            assert!(r.is_err());
        });
    }

    /// The session's TLS cipher on the local MySQL in `mode`; empty when the session is plain.
    async fn local_mysql_cipher(mode: crate::tls::TlsMode) -> crate::error::Result<String> {
        let tls = Tls { mode, ..Tls::default() };
        let options = super::Options::default();
        let mut c = Conn::open(Engine::Mysql, "mysql://root@127.0.0.1:3306", None, &tls, &options, None).await?;
        let r = c.query("show status like 'Ssl_cipher'").await?;
        Ok(r.rows[0][1].clone().unwrap_or_default())
    }

    /// TLS modes against a local MySQL with its auto-generated (self-signed) certificate.
    #[test]
    #[ignore = "needs a local MySQL with TLS on"]
    fn local_mysql_tls_modes() {
        use crate::tls::TlsMode;
        tauri::async_runtime::block_on(async {
            assert_eq!(local_mysql_cipher(TlsMode::Disable).await.unwrap(), "", "Disable: plain");
            assert_ne!(local_mysql_cipher(TlsMode::Prefer).await.unwrap(), "", "Prefer: encrypted");
            assert_ne!(local_mysql_cipher(TlsMode::Require).await.unwrap(), "", "Require: no certificate check");
            let err = local_mysql_cipher(TlsMode::VerifyCa).await.unwrap_err();
            println!("VerifyCa refused as expected: {err}");
        });
    }

    #[test]
    #[ignore = "needs a local MySQL with local_infile on; creates and drops a scratch database"]
    fn local_mysql_load_data_local_sends_the_named_file() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Mysql, "mysql://root@127.0.0.1:3306").await;
            c.query("create database if not exists tm_infile").await.unwrap();
            c.query("create table if not exists tm_infile.t (a int, b text)").await.unwrap();
            let path = std::env::temp_dir().join(format!("tableminus-{}-infile.csv", std::process::id()));
            std::fs::write(&path, "1,one\n2,two\n").unwrap();
            let sql =
                format!("load data local infile '{}' into table tm_infile.t fields terminated by ','", path.display());
            let r = c.query_limited(&sql, None).await;
            let n = c.scalar("select count(*) from tm_infile.t").await;
            c.query("drop database tm_infile").await.unwrap();
            let _ = std::fs::remove_file(&path);
            r.unwrap();
            assert_eq!(n.unwrap().as_deref(), Some("2"));
        });
    }

    /// A MySQL session killed from another connection reads as lost, not as a failed statement.
    #[test]
    #[ignore = "needs a local MySQL"]
    fn local_mysql_killed_session_is_lost() {
        tauri::async_runtime::block_on(async {
            let mut victim = open(Engine::Mysql, "mysql://root@127.0.0.1:3306").await;
            let id = victim.scalar("select connection_id()").await.unwrap().unwrap();
            let mut killer = open(Engine::Mysql, "mysql://root@127.0.0.1:3306").await;
            killer.query(&format!("kill {id}")).await.unwrap();
            let err = victim.query("select 1").await.unwrap_err();
            assert!(victim.lost_by(&err), "{err:?}");
            let syntax = killer.query("selec 1").await.unwrap_err();
            assert!(!killer.lost_by(&syntax), "a bad statement keeps the connection");
        });
    }

    /// Checks against local servers with default credentials: `cargo test -- --ignored`.
    #[test]
    #[ignore = "needs local MySQL and Postgres servers"]
    fn local_servers_without_database() {
        tauri::async_runtime::block_on(async {
            let mut my = open(Engine::Mysql, "mysql://root@127.0.0.1:3306").await;
            assert_eq!(my.current_database().await.unwrap(), None);
            let r = my.query_limited("select * from information_schema.columns", Some(5)).await.unwrap();
            assert_eq!((r.rows.len(), r.truncated), (5, true));
            assert_eq!(my.query("select 1").await.unwrap().rows[0][0].as_deref(), Some("1"), "still in step");
            assert!(my.databases().await.unwrap().contains(&"mysql".to_string()));
            my.use_database("mysql").await.unwrap();
            assert_eq!(my.current_database().await.unwrap().as_deref(), Some("mysql"));
            assert!(my.tables(false).await.unwrap().contains(&"user".to_string()));
            assert!(my.columns("user").await.unwrap().iter().any(|c| c.name == "Host" && c.primary));
            my.query("create temporary table dbm_edit(id int primary key, note text)").await.unwrap();
            my.query("insert into dbm_edit values (1, 'x')").await.unwrap();
            let edit = crate::rows::Update {
                key: vec![("id".into(), "1".into())],
                id: None,
                original: vec![],
                set: vec![("note".into(), crate::rows::Value::Text(Some("a\\b'c".into())))],
            };
            let changes = crate::rows::Changes { updates: vec![edit], ..Default::default() };
            assert_eq!(my.save_rows("dbm_edit", &changes).await.unwrap(), 1);
            let r = my.query("select note from dbm_edit").await.unwrap();
            assert_eq!(r.rows[0][0].as_deref(), Some("a\\b'c"), "backslashes and quotes stored as typed");

            let mut pg = open(Engine::Postgres, "postgres://postgres@127.0.0.1:5432?sslmode=prefer").await;
            assert!(pg.databases().await.unwrap().contains(&"postgres".to_string()));
            assert!(pg.columns("information_schema.tables").await.unwrap().iter().any(|c| c.name == "table_name"));

            // Text parameters into typed columns, on a session-only temp table (nothing persists).
            pg.query(
                "create temporary table dbm_edit(id int primary key, ok bool, at timestamp, n numeric(6,2), note text)",
            )
            .await
            .unwrap();
            pg.query("insert into dbm_edit values (1, false, '2026-01-01', 1.5, 'x')").await.unwrap();
            let set = |c: &str, v: Option<&str>| (c.to_string(), crate::rows::Value::Text(v.map(String::from)));
            let edit = crate::rows::Update {
                key: vec![("id".into(), "1".into())],
                id: None,
                original: vec![],
                set: vec![
                    set("ok", Some("t")),
                    set("at", Some("2026-10-13 09:30:00")),
                    set("n", Some("42.25")),
                    set("note", None),
                ],
            };
            let changes = crate::rows::Changes { updates: vec![edit], ..Default::default() };
            assert_eq!(pg.save_rows("pg_temp.dbm_edit", &changes).await.unwrap(), 1);
            let r = pg.query("select ok, at, n, note from dbm_edit").await.unwrap();
            assert_eq!(
                r.rows[0],
                vec![Some("t".into()), Some("2026-10-13 09:30:00".into()), Some("42.25".into()), None]
            );
        });
    }
}

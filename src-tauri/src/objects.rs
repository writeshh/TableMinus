//! Views, materialized views, functions, procedures and sequences: their definitions for the editors (a view's
//! structure, the function editor, the sequence editor), the sidebar's Copy Script As, and the DDL that saves or
//! drops them. Statements go back to the UI for the preview and then run through `run_ddl`, as all DDL does.

use crate::catalog::ItemKind;
use crate::db::{Column, Conn, Dialect};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use tauri::State;

/// A Copy Script As template. Tables have them all; other items only Create and Drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Script {
    Create,
    Drop,
    Select,
    /// The first 100 rows, for the sidebar's "Select top 100".
    SelectTop,
    Insert,
    Update,
    Delete,
}

/// A Postgres sequence's settings. Numbers travel as text: a sequence's bounds are bigints, past what a JSON number
/// keeps exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sequence {
    /// The value `nextval` returned last; None until the sequence is first used (or without the privilege to see it).
    pub last_value: Option<String>,
    pub increment: String,
    pub min: String,
    pub max: String,
    pub start: String,
    pub cache: String,
    pub cycle: bool,
    pub comment: Option<String>,
}

const NO_ROUTINES: Error = Error::Invalid("SQLite has no functions or procedures");

/// `FUNCTION` or `PROCEDURE`.
fn routine_word(kind: ItemKind) -> Result<&'static str> {
    match kind {
        ItemKind::Function => Ok("FUNCTION"),
        ItemKind::Procedure => Ok("PROCEDURE"),
        _ => Err(Error::Invalid("not a function or procedure")),
    }
}

/// SQL typed in an editor, without the trailing semicolons a statement sent on its own doesn't take.
fn statement(text: &str, empty: &'static str) -> Result<String> {
    let s = text.trim().trim_end_matches(';').trim_end();
    if s.is_empty() { Err(Error::Invalid(empty)) } else { Ok(s.to_string()) }
}

/// Saves a view's SELECT. Postgres and MySQL replace a view in place; a materialized view (Postgres) and a SQLite
/// view are dropped and created again, in one transaction (`run_ddl`). Dropping a materialized view also drops its
/// indexes.
pub fn view_sql(d: Dialect, view: &str, matview: bool, body: &str, create: bool) -> Result<Vec<String>> {
    if matview && d != Dialect::Pg {
        return Err(Error::Invalid("only Postgres has materialized views"));
    }
    let body = statement(body, "the view needs a SELECT")?;
    let t = d.table_ident(view);
    let what = if matview { "MATERIALIZED VIEW" } else { "VIEW" };
    // SQL Server says CREATE OR ALTER, and takes it on views from 2016 SP1.
    let or = if d == Dialect::Mssql { "OR ALTER " } else { "OR REPLACE " };
    let new = |replace: bool| format!("CREATE {}{what} {t} AS\n{body}", if replace { or } else { "" });
    Ok(if create {
        vec![new(false)]
    } else if matview || d == Dialect::Lite {
        vec![format!("DROP {what} {t}"), new(false)]
    } else {
        vec![new(true)]
    })
}

/// Saves a function or procedure from its whole CREATE statement. Postgres's is CREATE OR REPLACE. MySQL can't
/// replace a routine, so it's dropped first; MySQL commits each statement, so the UI passes the original along to
/// `run_ddl` to be put back if the new one fails.
/// A Postgres routine's definition as one that replaces the routine. Postgres writes `CREATE OR REPLACE` already;
/// `CockroachDB` writes plain `CREATE`, which fails on save because the routine exists.
fn or_replace(def: &str) -> String {
    match def.strip_prefix("CREATE ") {
        Some(rest) if !rest.starts_with("OR REPLACE ") => format!("CREATE OR REPLACE {rest}"),
        _ => def.into(),
    }
}

pub fn routine_sql(d: Dialect, kind: ItemKind, name: &str, definition: &str, create: bool) -> Result<Vec<String>> {
    let what = routine_word(kind)?;
    if d == Dialect::Ora {
        return Ok(vec![plsql(definition)?]);
    }
    let def = statement(definition, "the definition is empty")?;
    match d {
        Dialect::Pg => Ok(vec![def]),
        Dialect::Bq => Ok(vec![or_replace(&def)]),
        Dialect::Mssql => Ok(vec![or_alter(&def)]),
        Dialect::My if create => Ok(vec![def]),
        Dialect::My => Ok(vec![format!("DROP {what} IF EXISTS {}", d.ident(name)), def]),
        // ClickHouse keeps the plain CREATE FUNCTION it was made with; saving replaces it.
        Dialect::Click if kind == ItemKind::Function => Ok(vec![if create { def } else { or_replace(&def) }]),
        Dialect::Lite | Dialect::Duck | Dialect::Click | Dialect::Ora => Err(NO_ROUTINES),
    }
}

/// A SQL Server routine's definition as one that creates or replaces it (`CREATE OR ALTER`, from 2016 SP1), however
/// its CREATE is spelled.
fn or_alter(def: &str) -> String {
    let def = def.trim_start();
    let Some(rest) = def.get(..6).filter(|w| w.eq_ignore_ascii_case("create")).and_then(|_| def.get(6..)) else {
        return def.to_string();
    };
    let next: Vec<String> = rest.split_whitespace().take(2).map(str::to_ascii_lowercase).collect();
    if next == ["or", "alter"] { def.to_string() } else { format!("CREATE OR ALTER {}", rest.trim_start()) }
}

/// An Oracle routine as one CREATE OR REPLACE: PL/SQL keeps the semicolon after its END, and loses the `/` line
/// SQL*Plus ends it with.
fn plsql(definition: &str) -> Result<String> {
    let def = definition.trim();
    let def = def.strip_suffix('/').unwrap_or(def).trim_end();
    if def.is_empty() {
        return Err(Error::Invalid("the definition is empty"));
    }
    let def = or_replace(def);
    Ok(if def.ends_with(';') { def } else { format!("{def};") })
}

/// The starting text for a new function or procedure.
pub fn routine_template(d: Dialect, kind: ItemKind, name: &str) -> Result<String> {
    let t = d.table_ident(name);
    Ok(match (d, kind) {
        (Dialect::Pg, ItemKind::Function) => {
            format!("CREATE FUNCTION {t}()\n RETURNS integer\n LANGUAGE sql\nAS $function$\n  SELECT 1\n$function$")
        }
        (Dialect::Pg, ItemKind::Procedure) => {
            format!("CREATE PROCEDURE {t}()\n LANGUAGE plpgsql\nAS $procedure$\nBEGIN\n  NULL;\nEND\n$procedure$")
        }
        (Dialect::My, ItemKind::Function) => format!("CREATE FUNCTION {t}()\nRETURNS int\nDETERMINISTIC\nRETURN 1"),
        (Dialect::My | Dialect::Bq, ItemKind::Procedure) => format!("CREATE PROCEDURE {t}()\nBEGIN\n  SELECT 1;\nEND"),
        (Dialect::Click, ItemKind::Function) => format!("CREATE FUNCTION {t} AS (x) -> x + 1"),
        (Dialect::Ora, ItemKind::Function) => {
            format!("CREATE FUNCTION {t}\nRETURN NUMBER IS\nBEGIN\n  RETURN 1;\nEND;")
        }
        (Dialect::Ora, ItemKind::Procedure) => format!("CREATE PROCEDURE {t} IS\nBEGIN\n  NULL;\nEND;"),
        (Dialect::Bq, ItemKind::Function) => format!("CREATE FUNCTION {t}(x INT64) AS (x + 1)"),
        (Dialect::Mssql, ItemKind::Function) => {
            format!("CREATE FUNCTION {t}()\nRETURNS int\nAS\nBEGIN\n  RETURN 1\nEND")
        }
        (Dialect::Mssql, ItemKind::Procedure) => {
            format!("CREATE PROCEDURE {t}\nAS\nBEGIN\n  SET NOCOUNT ON;\n  SELECT 1\nEND")
        }
        (Dialect::Lite | Dialect::Duck | Dialect::Click, _) => return Err(NO_ROUTINES),
        _ => return Err(Error::Invalid("not a function or procedure")),
    })
}

/// A whole number from the sequence form.
fn number(s: &str, what: &'static str) -> Result<i64> {
    s.trim().parse().map_err(|_| Error::Invalid(what))
}

/// The statements that take sequence `seq` from `cur` to `want`: one ALTER SEQUENCE for the settings that changed,
/// a new current value (Postgres's `setval`; SQL Server's `RESTART WITH` the value after it), and the comment.
/// Empty when nothing changed. A setting left as it was is never parsed, so a blank one (SQL Server's default
/// cache) stays blank.
pub fn sequence_sql(d: Dialect, seq: &str, cur: &Sequence, want: &Sequence) -> Result<Vec<String>> {
    if !matches!(d, Dialect::Pg | Dialect::Mssql) {
        return Err(Error::Invalid("sequences are edited on Postgres and SQL Server"));
    }
    let mssql = d == Dialect::Mssql;
    let t = d.table_ident(seq);
    let mut clauses = Vec::new();
    let fields: [(&str, &str, &str, &'static str); 5] = [
        ("INCREMENT BY", &cur.increment, &want.increment, "the increment must be a whole number"),
        ("MINVALUE", &cur.min, &want.min, "the minimum must be a whole number"),
        ("MAXVALUE", &cur.max, &want.max, "the maximum must be a whole number"),
        ("START WITH", &cur.start, &want.start, "the start must be a whole number"),
        ("CACHE", &cur.cache, &want.cache, "the cache size must be a whole number"),
    ];
    for (clause, was, now, what) in fields {
        if was.trim() == now.trim() {
            continue;
        }
        let now = number(now, what)?;
        if number(was, what).ok() == Some(now) {
            continue;
        }
        match clause {
            "START WITH" if mssql => {
                return Err(Error::Invalid("SQL Server can't change where a sequence starts; set its current value"));
            }
            "CACHE" if mssql && now == 0 => clauses.push("NO CACHE".into()),
            _ => clauses.push(format!("{clause} {now}")),
        }
    }
    if cur.cycle != want.cycle {
        clauses.push(if want.cycle { "CYCLE".into() } else { "NO CYCLE".into() });
    }
    let mut out = Vec::new();
    if !clauses.is_empty() {
        out.push(format!("ALTER SEQUENCE {t} {}", clauses.join(" ")));
    }
    let value = |v: &Option<String>| v.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(String::from);
    if let Some(v) = value(&want.last_value) {
        let v = number(&v, "the current value must be a whole number")?;
        if value(&cur.last_value).and_then(|c| c.parse::<i64>().ok()) != Some(v) {
            if mssql {
                // The next value is the one after `v`, as after Postgres's setval.
                let step = number(&want.increment, "the increment must be a whole number")?;
                let next = v.checked_add(step).ok_or(Error::Invalid("the current value is at the sequence's limit"))?;
                let restart = format!("ALTER SEQUENCE {t} RESTART WITH {next}");
                out.push(restart);
            } else {
                out.push(format!("SELECT setval({}, {v})", d.literal(&t)));
            }
        }
    }
    let comment = |c: &Option<String>| c.as_deref().map(str::trim).filter(|c| !c.is_empty()).map(String::from);
    if comment(&cur.comment) != comment(&want.comment) {
        if mssql {
            let had = comment(&cur.comment).is_some();
            out.extend(crate::ddl::mssql_description(seq, "SEQUENCE", None, had, comment(&want.comment).as_deref()));
        } else {
            let is = comment(&want.comment).map_or_else(|| "NULL".into(), |c| d.literal(&c));
            out.push(format!("COMMENT ON SEQUENCE {t} IS {is}"));
        }
    }
    Ok(out)
}

/// The SELECT of a SQLite `CREATE VIEW name AS select` as `sqlite_master` keeps it: the text after the first bare
/// AS, skipping quoted names (which may themselves contain " as ").
fn sqlite_view_body(sql: &str) -> Option<&str> {
    let b = sql.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'`' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
            }
            b'[' => {
                while i < b.len() && b[i] != b']' {
                    i += 1;
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$') {
                    i += 1;
                }
                // Both ends sit on ASCII bytes or the end, so they're char boundaries.
                if sql.get(start..i).is_some_and(|w| w.eq_ignore_ascii_case("as")) {
                    return sql.get(i..).map(str::trim);
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// A table's DML templates, `?` for each value as `TablePlus` writes them. The WHERE uses the primary key.
fn dml_script(d: Dialect, table: &str, columns: &[Column], script: Script) -> String {
    let t = d.table_ident(table);
    let names = |cols: &mut dyn Iterator<Item = &Column>| cols.map(|c| d.ident(&c.name)).collect::<Vec<_>>();
    let all = names(&mut columns.iter());
    let keys = names(&mut columns.iter().filter(|c| c.primary));
    let rest = names(&mut columns.iter().filter(|c| !c.primary));
    let set = if rest.is_empty() { &all } else { &rest };
    let filter = if keys.is_empty() {
        "<condition>".into()
    } else {
        keys.iter().map(|k| format!("{k} = ?")).collect::<Vec<_>>().join(" AND ")
    };
    match script {
        Script::Select => format!("SELECT\n  {}\nFROM {t};", all.join(",\n  ")),
        Script::SelectTop => format!("SELECT * FROM {t} LIMIT 100;"),
        Script::Insert => {
            format!("INSERT INTO {t} ({})\nVALUES ({});", all.join(", "), vec!["?"; all.len()].join(", "))
        }
        Script::Update => {
            let set: Vec<String> = set.iter().map(|c| format!("{c} = ?")).collect();
            format!("UPDATE {t}\nSET {}\nWHERE {filter};", set.join(", "))
        }
        Script::Delete => format!("DELETE FROM {t}\nWHERE {filter};"),
        Script::Create | Script::Drop => String::new(),
    }
}

/// Postgres names are `schema.name`; a name without a schema is in `public`.
fn split(name: &str) -> (&str, &str) {
    name.split_once('.').unwrap_or(("public", name))
}

impl Conn {
    /// A view's SELECT (Postgres's materialized views too), without a trailing semicolon.
    pub async fn view_body(&mut self, view: &str) -> Result<String> {
        let body = match self.dialect() {
            // The table resource holds a view's query as written.
            Dialect::Bq => {
                let query = self.describe(view).await?["view"]["query"].as_str().map(String::from);
                vec![vec![query]]
            }
            Dialect::Pg => {
                let (schema, name) = split(view);
                self.fetch_with(
                    "select pg_get_viewdef(c.oid, true) from pg_class c join pg_namespace n on n.oid = c.relnamespace \
                     where n.nspname = $1 and c.relname = $2 and c.relkind in ('v', 'm')",
                    &[Some(schema), Some(name)],
                )
                .await?
            }
            Dialect::My => {
                self.fetch_with(
                    "select view_definition from information_schema.views where table_schema = database() and table_name = ?",
                    &[Some(view)],
                )
                .await?
            }
            Dialect::Lite => {
                self.fetch_with("select sql from sqlite_master where type = 'view' and name = $1", &[Some(view)]).await?
            }
            Dialect::Click => {
                self.fetch_with(
                    "select as_select from system.tables where database = currentDatabase() and name = {p1:String}",
                    &[Some(view)],
                )
                .await?
            }
            Dialect::Mssql => {
                let name = self.table_ident(view);
                self.fetch_with("select definition from sys.sql_modules where object_id = object_id(@P1)", &[Some(&name)])
                    .await?
            }
            Dialect::Duck => {
                let (schema, name) = view.split_once('.').unwrap_or(("main", view));
                self.fetch_with(
                    "select sql from duckdb_views() where database_name = current_database() \
                     and schema_name = $1 and view_name = $2",
                    &[Some(schema), Some(name)],
                )
                .await?
            }
            // all_views keeps the text as a LONG; DBMS_METADATA writes the whole statement as a CLOB.
            Dialect::Ora => {
                self.fetch_with(
                    "select dbms_metadata.get_ddl(object_type, object_name, owner) from all_objects \
                     where owner = sys_context('USERENV', 'CURRENT_SCHEMA') and object_name = :1 \
                     and object_type in ('VIEW', 'MATERIALIZED VIEW')",
                    &[Some(view)],
                )
                .await?
            }
        };
        let text = body.into_iter().next().and_then(|mut r| r.pop().flatten()).ok_or(Error::Invalid("no such view"))?;
        // SQLite, DuckDB, SQL Server and Oracle keep the whole CREATE VIEW statement.
        let text = if matches!(self.dialect(), Dialect::Lite | Dialect::Duck | Dialect::Mssql | Dialect::Ora) {
            sqlite_view_body(&text).ok_or(Error::Invalid("can't find the SELECT in this view's definition"))?
        } else {
            &text
        };
        let body = text.trim().trim_end_matches(';').trim_end();
        if body.is_empty() {
            // MySQL shows a blank definition to users without SHOW VIEW.
            return Err(Error::Invalid("the server doesn't show this view's definition to this user"));
        }
        Ok(body.to_string())
    }

    /// A function's or procedure's CREATE statement; on Postgres one for each overload of the name.
    pub async fn routine_definitions(&mut self, kind: ItemKind, name: &str) -> Result<Vec<String>> {
        let what = routine_word(kind)?;
        let rows = match self.dialect() {
            Dialect::Pg => {
                let (schema, proc) = split(name);
                let prokind = if kind == ItemKind::Procedure { "p" } else { "f" };
                self.fetch_with(
                    "select pg_get_functiondef(p.oid) from pg_proc p join pg_namespace n on n.oid = p.pronamespace \
                     where n.nspname = $1 and p.proname = $2 and p.prokind::text = $3 order by p.oid",
                    &[Some(schema), Some(proc), Some(prokind)],
                )
                .await?
            }
            Dialect::My => {
                let sql = format!("SHOW CREATE {what} {}", self.ident(name));
                // The statement is the third column; NULL without the privilege to read it.
                self.query(&sql)
                    .await?
                    .rows
                    .into_iter()
                    .map(|mut r| vec![r.get_mut(2).and_then(Option::take)])
                    .collect()
            }
            Dialect::Click if kind == ItemKind::Function => {
                self.fetch_with(
                    "select create_query from system.functions where origin = 'SQLUserDefined' and name = {p1:String}",
                    &[Some(name)],
                )
                .await?
            }
            Dialect::Ora => {
                self.fetch_with(
                    "select dbms_metadata.get_ddl(:1, :2, sys_context('USERENV', 'CURRENT_SCHEMA')) from dual",
                    &[Some(what), Some(name)],
                )
                .await?
            }
            Dialect::Bq => {
                self.fetch_with(
                    "select ddl from INFORMATION_SCHEMA.ROUTINES where routine_name = @p1 and routine_type = @p2",
                    &[Some(name), Some(what)],
                )
                .await?
            }
            Dialect::Mssql => {
                let name = self.table_ident(name);
                self.fetch_with(
                    "select definition from sys.sql_modules where object_id = object_id(@P1)",
                    &[Some(&name)],
                )
                .await?
            }
            Dialect::Lite | Dialect::Duck | Dialect::Click => return Err(NO_ROUTINES),
        };
        let d = self.dialect();
        let defs: Vec<String> = rows
            .into_iter()
            .filter_map(|mut r| r.pop().flatten())
            .map(|def| match d {
                Dialect::Pg => or_replace(def.trim()),
                Dialect::Mssql => or_alter(def.trim()),
                _ => def.trim().into(),
            })
            .collect();
        if defs.is_empty() {
            return Err(Error::Invalid("no such routine, or the server doesn't show its definition to this user"));
        }
        Ok(defs)
    }

    /// A sequence's settings and comment (Postgres, SQL Server).
    pub async fn sequence(&mut self, seq: &str) -> Result<Sequence> {
        let rows = match self.dialect() {
            Dialect::Pg => self.pg_sequence(seq).await?,
            // last_used_value is SQL Server 2017's; the cache is blank when the server picks its size.
            Dialect::Mssql => {
                let name = self.table_ident(seq);
                self.fetch_with(
                    "select cast(s.last_used_value as nvarchar(40)), cast(s.increment as nvarchar(40)), \
                     cast(s.minimum_value as nvarchar(40)), cast(s.maximum_value as nvarchar(40)), \
                     cast(s.start_value as nvarchar(40)), case when s.is_cached = 0 then '0' \
                       else coalesce(cast(s.cache_size as nvarchar(20)), '') end, \
                     case when s.is_cycling = 1 then 'true' else 'false' end, cast(ep.value as nvarchar(max)) \
                     from sys.sequences s left join sys.extended_properties ep on ep.class = 1 \
                       and ep.major_id = s.object_id and ep.minor_id = 0 and ep.name = 'MS_Description' \
                     where s.object_id = object_id(@P1)",
                    &[Some(&name)],
                )
                .await?
            }
            _ => return Err(Error::Invalid("sequences are edited on Postgres and SQL Server")),
        };
        let mut r = rows.into_iter().next().ok_or(Error::Invalid("no such sequence"))?;
        r.resize(8, None);
        let mut cell = |i: usize| r[i].take();
        Ok(Sequence {
            last_value: cell(0),
            increment: cell(1).unwrap_or_default(),
            min: cell(2).unwrap_or_default(),
            max: cell(3).unwrap_or_default(),
            start: cell(4).unwrap_or_default(),
            cache: cell(5).unwrap_or_default(),
            cycle: cell(6).as_deref() == Some("true"),
            comment: cell(7),
        })
    }

    async fn pg_sequence(&mut self, seq: &str) -> Result<Vec<Vec<Option<String>>>> {
        let (schema, name) = split(seq);
        self.fetch_with(
            "select s.last_value::text, s.increment_by::text, s.min_value::text, s.max_value::text, \
                 s.start_value::text, s.cache_size::text, s.cycle::text, obj_description(c.oid, 'pg_class') \
                 from pg_sequences s join pg_namespace n on n.nspname = s.schemaname \
                 join pg_class c on c.relnamespace = n.oid and c.relname = s.sequencename \
                 where s.schemaname = $1 and s.sequencename = $2",
            &[Some(schema), Some(name)],
        )
        .await
    }

    /// The statements that drop a sidebar item. `cascade` is Postgres's.
    pub async fn drop_item_sql(&mut self, kind: ItemKind, name: &str, cascade: bool) -> Result<Vec<String>> {
        let d = self.dialect();
        let cascade = if cascade && d == Dialect::Pg { " CASCADE" } else { "" };
        let t = d.table_ident(name);
        Ok(match kind {
            ItemKind::Schema => vec![format!("DROP SCHEMA {}{cascade}", d.ident(name))],
            ItemKind::View => vec![format!("DROP VIEW {t}{cascade}")],
            ItemKind::Matview => vec![format!("DROP MATERIALIZED VIEW {t}{cascade}")],
            ItemKind::Sequence => vec![format!("DROP SEQUENCE {t}{cascade}")],
            ItemKind::Type => vec![format!("DROP TYPE {t}{cascade}")],
            ItemKind::Function | ItemKind::Procedure => {
                let what = routine_word(kind)?;
                match d {
                    // Each overload by its argument types, which the server writes out.
                    Dialect::Pg => {
                        let (schema, proc) = split(name);
                        let prokind = if kind == ItemKind::Procedure { "p" } else { "f" };
                        let rows = self
                            .fetch_with(
                                "select pg_get_function_identity_arguments(p.oid) from pg_proc p \
                                 join pg_namespace n on n.oid = p.pronamespace \
                                 where n.nspname = $1 and p.proname = $2 and p.prokind::text = $3 order by p.oid",
                                &[Some(schema), Some(proc), Some(prokind)],
                            )
                            .await?;
                        rows.into_iter()
                            .map(|mut r| format!("DROP {what} {t}({}){cascade}", r.pop().flatten().unwrap_or_default()))
                            .collect()
                    }
                    Dialect::My | Dialect::Ora | Dialect::Bq | Dialect::Mssql => vec![format!("DROP {what} {t}")],
                    Dialect::Click if kind == ItemKind::Function => vec![format!("DROP FUNCTION {t}")],
                    Dialect::Lite | Dialect::Duck | Dialect::Click => return Err(NO_ROUTINES),
                }
            }
        })
    }

    /// Copy Script As: `script` for a table or view (`kind` None), or for another sidebar item (Create and Drop).
    pub async fn script(&mut self, kind: Option<ItemKind>, name: &str, script: Script) -> Result<String> {
        let d = self.dialect();
        let t = d.table_ident(name);
        let end = |list: Vec<String>| list.into_iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n\n");
        match (kind, script) {
            // No columns needed, so it works on Postgres's materialized views too.
            (None | Some(ItemKind::View | ItemKind::Matview), Script::SelectTop) => Ok(if d == Dialect::Mssql {
                format!("SELECT TOP 100 * FROM {t};")
            } else {
                format!("SELECT * FROM {t} LIMIT 100;")
            }),
            (None, Script::Create) => self.ddl(name).await,
            (None, Script::Drop) => {
                Ok(format!("DROP {} {t};", if self.is_view(name).await? { "VIEW" } else { "TABLE" }))
            }
            (None, _) => {
                let columns = self.columns(name).await?;
                if columns.is_empty() {
                    return Err(Error::Invalid("no such table, or it has no columns"));
                }
                Ok(dml_script(d, name, &columns, script))
            }
            (Some(kind @ (ItemKind::View | ItemKind::Matview)), Script::Create) => {
                let what = if kind == ItemKind::Matview { "MATERIALIZED VIEW" } else { "VIEW" };
                Ok(format!("CREATE {what} {t} AS\n{};", self.view_body(name).await?))
            }
            (Some(kind @ (ItemKind::Function | ItemKind::Procedure)), Script::Create) => {
                Ok(end(self.routine_definitions(kind, name).await?))
            }
            (Some(ItemKind::Sequence), Script::Create) => {
                let s = self.sequence(name).await?;
                Ok(format!(
                    "CREATE SEQUENCE {t} START WITH {} INCREMENT BY {} MINVALUE {} MAXVALUE {} {}{};",
                    s.start,
                    s.increment,
                    s.min,
                    s.max,
                    // SQL Server's NO CACHE is a cache of 0 here.
                    if s.cache == "0" && d == Dialect::Mssql {
                        "NO CACHE".to_string()
                    } else {
                        format!("CACHE {}", s.cache)
                    },
                    if s.cycle { " CYCLE" } else { "" }
                ))
            }
            (Some(kind), Script::Drop) => Ok(end(self.drop_item_sql(kind, name, false).await?)),
            (Some(_), _) => Err(Error::Invalid("this script isn't available for this item")),
        }
    }
}

/// Copy Script As for a table or view (`kind` None) or another sidebar item.
#[tauri::command]
pub async fn item_script(
    open: State<'_, crate::Open>,
    name: String,
    kind: Option<ItemKind>,
    item: String,
    script: Script,
) -> Result<String> {
    let mut live = open.get(&name).await?;
    let r = live.conn.script(kind, &item, script).await;
    live.check(r)
}

/// The statements that drop a sidebar item other than a table, for the preview.
#[tauri::command]
pub async fn drop_item_sql(
    open: State<'_, crate::Open>,
    name: String,
    kind: ItemKind,
    item: String,
    cascade: bool,
) -> Result<Vec<String>> {
    let mut live = open.get(&name).await?;
    let r = live.conn.drop_item_sql(kind, &item, cascade).await;
    live.check(r)
}

/// A view's SELECT, for its structure tab.
#[tauri::command]
pub async fn view_definition(open: State<'_, crate::Open>, name: String, view: String) -> Result<String> {
    let mut live = open.get(&name).await?;
    let r = live.conn.view_body(&view).await;
    live.check(r)
}

/// The statements that save (or, with `create`, create) a view from its SELECT.
#[tauri::command]
pub async fn save_view_sql(
    open: State<'_, crate::Open>,
    name: String,
    view: String,
    matview: bool,
    body: String,
    create: bool,
) -> Result<Vec<String>> {
    let live = open.get(&name).await?;
    view_sql(live.conn.dialect(), &view, matview, &body, create)
}

/// A function's or procedure's CREATE statements (one per overload on Postgres), for the function editor.
#[tauri::command]
pub async fn routine_definitions(
    open: State<'_, crate::Open>,
    name: String,
    kind: ItemKind,
    item: String,
) -> Result<Vec<String>> {
    let mut live = open.get(&name).await?;
    let r = live.conn.routine_definitions(kind, &item).await;
    live.check(r)
}

/// The starting text for a new function or procedure named `item`.
#[tauri::command]
pub async fn new_routine(open: State<'_, crate::Open>, name: String, kind: ItemKind, item: String) -> Result<String> {
    let live = open.get(&name).await?;
    routine_template(live.conn.dialect(), kind, &item)
}

/// The statements that save a function or procedure from its edited CREATE statement.
#[tauri::command]
pub async fn save_routine_sql(
    open: State<'_, crate::Open>,
    name: String,
    kind: ItemKind,
    item: String,
    definition: String,
    create: bool,
) -> Result<Vec<String>> {
    let live = open.get(&name).await?;
    routine_sql(live.conn.dialect(), kind, &item, &definition, create)
}

/// A Postgres sequence's settings, for the sequence editor.
#[tauri::command]
pub async fn sequence(open: State<'_, crate::Open>, name: String, sequence: String) -> Result<Sequence> {
    let mut live = open.get(&name).await?;
    let r = live.conn.sequence(&sequence).await;
    live.check(r)
}

/// The statements that give a sequence the settings in `want`; empty when nothing changed.
#[tauri::command]
pub async fn save_sequence_sql(
    open: State<'_, crate::Open>,
    name: String,
    sequence: String,
    want: Sequence,
) -> Result<Vec<String>> {
    let mut live = open.get(&name).await?;
    let cur = live.conn.sequence(&sequence).await;
    let cur = live.check(cur)?;
    sequence_sql(live.conn.dialect(), &sequence, &cur, &want)
}

#[cfg(test)]
mod tests {
    use super::{
        Script, Sequence, dml_script, or_alter, or_replace, routine_sql, routine_template, sequence_sql,
        sqlite_view_body, view_sql,
    };

    #[test]
    fn or_replace_should_make_a_plain_create_replace_and_leave_the_rest() {
        assert_eq!(or_replace("CREATE FUNCTION public.f(x INT8)"), "CREATE OR REPLACE FUNCTION public.f(x INT8)");
        assert_eq!(or_replace("CREATE OR REPLACE PROCEDURE p()"), "CREATE OR REPLACE PROCEDURE p()");
    }
    use crate::catalog::ItemKind;
    use crate::db::{Column, Conn, Dialect, Engine, Options};
    use crate::tls::Tls;

    fn column(name: &str, primary: bool) -> Column {
        Column { name: name.into(), data_type: "int".into(), nullable: true, default: None, primary }
    }

    fn seq() -> Sequence {
        Sequence {
            last_value: Some("5".into()),
            increment: "1".into(),
            min: "1".into(),
            max: "9223372036854775807".into(),
            start: "1".into(),
            cache: "1".into(),
            cycle: false,
            comment: None,
        }
    }

    #[test]
    fn or_alter_should_make_any_create_spelling_create_or_alter_once() {
        assert_eq!(or_alter("  create   PROCEDURE p AS SELECT 1"), "CREATE OR ALTER PROCEDURE p AS SELECT 1");
        assert_eq!(or_alter("CREATE OR ALTER FUNCTION f() RETURNS int"), "CREATE OR ALTER FUNCTION f() RETURNS int");
        assert_eq!(or_alter("ALTER PROCEDURE p AS SELECT 1"), "ALTER PROCEDURE p AS SELECT 1");
    }

    #[test]
    fn sequence_sql_should_restart_a_sql_server_sequence_after_the_value_set() {
        let want = Sequence { last_value: Some("100".into()), increment: "5".into(), ..seq() };
        let cur = Sequence { increment: "5".into(), ..seq() };
        assert_eq!(sequence_sql(Dialect::Mssql, "s", &cur, &want).unwrap(), ["ALTER SEQUENCE [s] RESTART WITH 105"]);
    }

    #[test]
    fn sequence_sql_should_keep_a_blank_cache_and_write_no_cache_for_zero() {
        let cur = Sequence { cache: String::new(), ..seq() };
        assert_eq!(sequence_sql(Dialect::Mssql, "s", &cur, &cur).unwrap(), Vec::<String>::new());
        let want = Sequence { cache: "0".into(), ..seq() };
        assert_eq!(sequence_sql(Dialect::Mssql, "s", &cur, &want).unwrap(), ["ALTER SEQUENCE [s] NO CACHE"]);
    }

    #[test]
    fn view_sql_should_replace_a_postgres_view_in_place() {
        assert_eq!(
            view_sql(Dialect::Pg, "public.v", false, "select 1;\n", false).unwrap(),
            ["CREATE OR REPLACE VIEW \"public\".\"v\" AS\nselect 1"]
        );
    }

    #[test]
    fn view_sql_should_drop_and_create_a_materialized_view_and_a_sqlite_view() {
        assert_eq!(
            view_sql(Dialect::Pg, "public.m", true, "select 1", false).unwrap(),
            ["DROP MATERIALIZED VIEW \"public\".\"m\"", "CREATE MATERIALIZED VIEW \"public\".\"m\" AS\nselect 1"]
        );
        assert_eq!(
            view_sql(Dialect::Lite, "v", false, "select 1", false).unwrap(),
            ["DROP VIEW \"v\"", "CREATE VIEW \"v\" AS\nselect 1"]
        );
    }

    #[test]
    fn view_sql_should_create_without_replacing_and_refuse_an_empty_body() {
        assert_eq!(view_sql(Dialect::My, "v`x", false, "select 1", true).unwrap(), ["CREATE VIEW `v``x` AS\nselect 1"]);
        assert!(view_sql(Dialect::Pg, "public.v", false, " ; ", false).is_err());
        assert!(view_sql(Dialect::My, "m", true, "select 1", true).is_err(), "MySQL has no materialized views");
    }

    #[test]
    fn routine_sql_should_drop_first_on_mysql_only() {
        let def = "CREATE FUNCTION f() RETURNS int RETURN 2;";
        assert_eq!(
            routine_sql(Dialect::My, ItemKind::Function, "f", def, false).unwrap(),
            ["DROP FUNCTION IF EXISTS `f`", "CREATE FUNCTION f() RETURNS int RETURN 2"]
        );
        assert_eq!(routine_sql(Dialect::My, ItemKind::Function, "f", def, true).unwrap().len(), 1);
        assert_eq!(
            routine_sql(Dialect::Pg, ItemKind::Procedure, "public.p", "CREATE OR REPLACE …", false).unwrap().len(),
            1
        );
        assert!(routine_sql(Dialect::Lite, ItemKind::Function, "f", def, false).is_err());
        assert!(routine_sql(Dialect::My, ItemKind::View, "f", def, false).is_err());
    }

    #[test]
    fn routine_template_should_quote_the_name() {
        let t = routine_template(Dialect::Pg, ItemKind::Function, "public.add one").unwrap();
        assert!(t.starts_with("CREATE FUNCTION \"public\".\"add one\"()"), "{t}");
        assert!(routine_template(Dialect::Lite, ItemKind::Function, "f").is_err());
    }

    #[test]
    fn sequence_sql_should_emit_only_what_changed() {
        let want = Sequence { increment: "5".into(), cycle: true, ..seq() };
        assert_eq!(
            sequence_sql(Dialect::Pg, "public.s", &seq(), &want).unwrap(),
            ["ALTER SEQUENCE \"public\".\"s\" INCREMENT BY 5 CYCLE"]
        );
        assert_eq!(sequence_sql(Dialect::Pg, "public.s", &seq(), &seq()).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn sequence_sql_should_set_the_value_and_comment_as_literals() {
        let want = Sequence { last_value: Some("42".into()), comment: Some("it's".into()), ..seq() };
        assert_eq!(
            sequence_sql(Dialect::Pg, "public.s", &seq(), &want).unwrap(),
            ["SELECT setval(E'\"public\".\"s\"', 42)", "COMMENT ON SEQUENCE \"public\".\"s\" IS E'it''s'"]
        );
        let cleared = Sequence { comment: Some(" ".into()), ..seq() };
        let cur = Sequence { comment: Some("old".into()), ..seq() };
        assert_eq!(
            sequence_sql(Dialect::Pg, "public.s", &cur, &cleared).unwrap(),
            ["COMMENT ON SEQUENCE \"public\".\"s\" IS NULL"]
        );
    }

    #[test]
    fn sequence_sql_should_refuse_numbers_that_are_not_whole() {
        let want = Sequence { cache: "1; drop table t".into(), ..seq() };
        let err = sequence_sql(Dialect::Pg, "public.s", &seq(), &want).unwrap_err();
        assert_eq!(err.to_string(), "the cache size must be a whole number");
    }

    #[test]
    fn sqlite_view_body_should_skip_quoted_names_containing_as() {
        assert_eq!(sqlite_view_body("CREATE VIEW \"a as b\" AS select 1"), Some("select 1"));
        assert_eq!(sqlite_view_body("create view [x as] as\n  select 2"), Some("select 2"));
        assert_eq!(sqlite_view_body("create view v"), None);
    }

    #[test]
    fn dml_script_should_use_placeholders_and_the_primary_key() {
        let cols = [column("id", true), column("name", false)];
        assert_eq!(
            dml_script(Dialect::Pg, "public.t", &cols, Script::Insert),
            "INSERT INTO \"public\".\"t\" (\"id\", \"name\")\nVALUES (?, ?);"
        );
        assert_eq!(dml_script(Dialect::My, "t", &cols, Script::Update), "UPDATE `t`\nSET `name` = ?\nWHERE `id` = ?;");
        assert_eq!(
            dml_script(Dialect::Lite, "t", &[column("a", false)], Script::Delete),
            "DELETE FROM \"t\"\nWHERE <condition>;"
        );
    }

    #[test]
    fn view_body_should_round_trip_a_sqlite_view_through_its_save_sql() {
        tauri::async_runtime::block_on(async {
            let mut c =
                Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &Options::default(), None).await.unwrap();
            c.query("create table t(a int)").await.unwrap();
            c.query("create view \"v as w\" as select a from t").await.unwrap();
            assert_eq!(c.view_body("v as w").await.unwrap(), "select a from t");
            let sql = view_sql(Dialect::Lite, "v as w", false, "select a, a * 2 as b from t", false).unwrap();
            c.run_ddl(&sql).await.unwrap();
            assert_eq!(c.columns("v as w").await.unwrap().len(), 2);
            let drop = c.script(Some(ItemKind::View), "v as w", Script::Drop).await.unwrap();
            assert_eq!(drop, "DROP VIEW \"v as w\";");
        });
    }

    /// Postgres: a view, a function and a sequence, edited the way the UI does. Uses a scratch schema.
    #[test]
    #[ignore = "needs a local Postgres; creates and drops a scratch schema"]
    fn local_postgres_objects_round_trip() {
        tauri::async_runtime::block_on(async {
            let url =
                std::env::var("DBM_PG_URL").unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5432/postgres".into());
            let mut c =
                Conn::open(Engine::Postgres, &url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let sch = format!("tm_objects_{}", std::process::id());
            c.query(&format!(
                "create schema {sch}; create table {sch}.t(a int); create view {sch}.v as select a from {sch}.t; \
                create sequence {sch}.q; create function {sch}.f(x int) returns int language sql as 'select x + 1'"
            ))
            .await
            .unwrap();
            let result: crate::error::Result<()> = async {
                let view = format!("{sch}.v");
                assert!(c.view_body(&view).await?.contains("SELECT"));
                c.run_ddl(&view_sql(Dialect::Pg, &view, false, &format!("select a, 1 as b from {sch}.t"), false)?)
                    .await?;
                assert_eq!(c.columns(&view).await?.len(), 2);
                let func = format!("{sch}.f");
                let defs = c.routine_definitions(ItemKind::Function, &func).await?;
                assert_eq!(defs.len(), 1);
                c.run_ddl(&routine_sql(
                    Dialect::Pg,
                    ItemKind::Function,
                    &func,
                    &defs[0].replace("x + 1", "x + 2"),
                    false,
                )?)
                .await?;
                assert_eq!(c.query(&format!("select {sch}.f(1)::text")).await?.rows[0][0].as_deref(), Some("3"));
                assert!(c.drop_item_sql(ItemKind::Function, &func, false).await?[0].ends_with("(x integer)"));
                let seq_name = format!("{sch}.q");
                let cur = c.sequence(&seq_name).await?;
                let want = Sequence {
                    last_value: Some("41".into()),
                    increment: "2".into(),
                    comment: Some("ids".into()),
                    ..cur.clone()
                };
                c.run_ddl(&sequence_sql(Dialect::Pg, &seq_name, &cur, &want)?).await?;
                let now = c.sequence(&seq_name).await?;
                assert_eq!(
                    (now.last_value.as_deref(), now.increment.as_str(), now.comment.as_deref()),
                    (Some("41"), "2", Some("ids"))
                );
                assert!(
                    c.script(Some(ItemKind::Sequence), &seq_name, Script::Create).await?.contains("INCREMENT BY 2")
                );
                Ok(())
            }
            .await;
            c.query(&format!("drop schema {sch} cascade")).await.unwrap();
            result.unwrap();
        });
    }

    /// MySQL: a routine whose new definition fails is put back by `run_ddl_or_restore`.
    #[test]
    #[ignore = "needs a local MySQL; creates and drops a scratch database"]
    fn local_mysql_routine_restores_after_a_failed_save() {
        tauri::async_runtime::block_on(async {
            let url = "mysql://root@127.0.0.1:3306";
            let mut c = Conn::open(Engine::Mysql, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let db = format!("tm_objects_{}", std::process::id());
            c.query(&format!("create database {db}")).await.unwrap();
            c.use_database(&db).await.unwrap();
            let result: crate::error::Result<()> = async {
                c.query("create function f() returns int deterministic return 1").await?;
                let original = c.routine_definitions(ItemKind::Function, "f").await?;
                let bad = routine_sql(
                    Dialect::My,
                    ItemKind::Function,
                    "f",
                    "CREATE FUNCTION f() RETURNS int nonsense",
                    false,
                )?;
                let err = c.run_ddl_or_restore(&bad, &original).await.unwrap_err();
                assert!(err.to_string().contains("put back"), "{err}");
                assert_eq!(c.query("select f()").await?.rows[0][0].as_deref(), Some("1"));
                Ok(())
            }
            .await;
            c.query(&format!("drop database {db}")).await.unwrap();
            result.unwrap();
        });
    }
}

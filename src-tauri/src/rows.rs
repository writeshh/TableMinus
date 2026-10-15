//! Saving the grid's staged changes: deleted, updated and inserted rows in one transaction, each existing row
//! found by its primary key, its row identity (ctid / rowid), or on MySQL, for a table without a key, by every
//! column's value with `LIMIT 1`. The same plan renders as SQL for the preview.
//! Also the catalog lookups the cell editors need: enum values and the bytes of binary cells.

use crate::Open;
use crate::db::{Conn, Dialect};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use tauri::State;

/// The largest file a cell's value may be loaded from.
const MAX_CELL_FILE: u64 = 20 * 1024 * 1024;
/// How much of a binary cell the hex viewer is sent.
const HEX_PREVIEW: u32 = 64 * 1024;

/// A cell's new value.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Value {
    /// Text the server converts to the column's type; None is NULL.
    Text(Option<String>),
    /// The bytes of a file, for binary columns.
    File(FileValue),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileValue {
    pub file: PathBuf,
}

/// An existing row: its primary-key values, or with `id` its row identity (ctid / rowid) from the page.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    pub key: Vec<(String, String)>,
    #[serde(default)]
    pub id: Option<String>,
    /// Every column's value as loaded: what the row is matched on when it has neither key nor identity (MySQL
    /// tables without a primary key), and what [`Changes::log_originals`] logs.
    #[serde(default)]
    pub original: Vec<(String, Option<String>)>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub key: Vec<(String, String)>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub original: Vec<(String, Option<String>)>,
    pub set: Vec<(String, Value)>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Changes {
    pub deletes: Vec<Row>,
    pub updates: Vec<Update>,
    /// New rows: values for the columns given; the others get their defaults.
    pub inserts: Vec<Vec<(String, Value)>>,
    /// Write each updated or deleted row's original values to the console log before changing it.
    pub log_originals: bool,
}

impl Changes {
    /// Whether some row is found by its values, which needs the table's column types.
    fn matches_values(&self) -> bool {
        let by_values = |key: &[(String, String)], id: &Option<String>, original: &[(String, Option<String>)]| {
            key.is_empty() && id.is_none() && !original.is_empty()
        };
        self.deletes.iter().any(|r| by_values(&r.key, &r.id, &r.original))
            || self.updates.iter().any(|u| by_values(&u.key, &u.id, &u.original))
    }
}

/// MySQL column types whose values don't survive the trip through text exactly (FLOAT reads back rounded, JSON
/// compares as a JSON string, binary cells only show as placeholders), so a row can't be matched on them.
fn inexact(column_type: &str) -> bool {
    let base = column_type.split(['(', ' ']).next().unwrap_or_default().to_ascii_lowercase();
    matches!(
        base.as_str(),
        "float"
            | "json"
            | "bit"
            | "binary"
            | "varbinary"
            | "tinyblob"
            | "blob"
            | "mediumblob"
            | "longblob"
            | "geometry"
            | "point"
            | "linestring"
            | "polygon"
            | "multipoint"
            | "multilinestring"
            | "multipolygon"
            | "geometrycollection"
    )
}

/// MySQL text types, compared case- and space-exactly when matching a row.
fn texty(column_type: &str) -> bool {
    let t = column_type.to_ascii_lowercase();
    ["char", "varchar", "tinytext", "text", "mediumtext", "longtext", "enum", "set"]
        .iter()
        .any(|p| t == *p || t.starts_with(&format!("{p}(")) || t.starts_with(&format!("{p} ")))
}

#[derive(Clone, Copy)]
enum Param<'a> {
    Text(Option<&'a str>),
    File(&'a Path),
    RowId(&'a str),
}

enum Part<'a> {
    Sql(String),
    Param(Param<'a>),
}

/// One planned statement, kept as parts so the same plan gives bound SQL to run and literal SQL to show.
struct Statement<'a> {
    parts: Vec<Part<'a>>,
    /// An UPDATE or DELETE, which must touch at most one row.
    one_row: bool,
    /// The row's values before the change, for the log.
    original: &'a [(String, Option<String>)],
}

/// The console log's note of a row's values before it changes.
fn original_note(d: Dialect, original: &[(String, Option<String>)]) -> String {
    let values: Vec<String> = original
        .iter()
        .map(|(c, v)| {
            format!("{} = {}", d.ident(c), v.as_deref().map_or_else(|| "NULL".into(), |v| shown_literal(d, v)))
        })
        .collect();
    // A newline in a value can't end the comment early: the note is one console entry, never run.
    format!("-- original values: {}", values.join(", "))
}

impl<'a> From<&'a Value> for Param<'a> {
    fn from(v: &'a Value) -> Self {
        match v {
            Value::Text(t) => Param::Text(t.as_deref()),
            Value::File(f) => Param::File(&f.file),
        }
    }
}

/// How a planned UPDATE or DELETE finds its row.
struct Target<'a> {
    key: &'a [(String, String)],
    id: Option<&'a str>,
    original: &'a [(String, Option<String>)],
}

/// `where …` finding one existing row; matching on values (MySQL only) ends in `limit 1`. `types` holds the
/// table's column types, needed only for matching.
fn find<'a>(d: Dialect, t: &Target<'a>, types: &HashMap<String, String>, parts: &mut Vec<Part<'a>>) -> Result<()> {
    let Target { key, id, original } = *t;
    if key.is_empty() && id.is_none() && !original.is_empty() {
        if d != Dialect::My {
            return Err(Error::Invalid("only MySQL rows are matched on their values; this engine has row identities"));
        }
        for (i, (column, value)) in original.iter().enumerate() {
            let column_type = types.get(column).ok_or(Error::Invalid("a row names a column the table doesn't have"))?;
            if inexact(column_type) {
                return Err(Error::Invalid(
                    "without a primary key, rows are matched on every value, and FLOAT, JSON, binary and spatial \
                     values can't be matched exactly; add a primary key to edit this table",
                ));
            }
            let c = d.ident(column);
            let joint = if i == 0 { " where " } else { " and " };
            let value = Param::Text(value.as_deref());
            if texty(column_type) {
                // The column's own collation may ignore case and trailing spaces; two rows differing only there are
                // different rows.
                parts.push(Part::Sql(format!("{joint}{c} <=> ")));
                parts.push(Part::Param(value));
                parts.push(Part::Sql(format!(" collate utf8mb4_bin and char_length({c}) <=> char_length(")));
                parts.push(Part::Param(value));
                parts.push(Part::Sql(")".into()));
            } else {
                parts.push(Part::Sql(format!("{joint}{c} <=> ")));
                parts.push(Part::Param(value));
            }
        }
        // Rows equal in every value are interchangeable: changing any one of them is right.
        parts.push(Part::Sql(" limit 1".into()));
        return Ok(());
    }
    if let Some(id) = id {
        let column = match d {
            Dialect::Pg => "ctid",
            Dialect::Lite => "rowid",
            Dialect::My => return Err(Error::Invalid("MySQL rows have no row identity")),
        };
        parts.push(Part::Sql(format!(" where {column} = ")));
        parts.push(Part::Param(Param::RowId(id)));
        return Ok(());
    }
    if key.is_empty() {
        return Err(Error::Invalid("every changed or deleted row needs its primary key"));
    }
    for (i, (column, value)) in key.iter().enumerate() {
        let joint = if i == 0 { " where " } else { " and " };
        parts.push(Part::Sql(format!("{joint}{} = ", d.ident(column))));
        parts.push(Part::Param(Param::Text(Some(value))));
    }
    Ok(())
}

/// The statements for `changes`: deletes first (so a new row may reuse a deleted row's key), then updates, then
/// inserts.
fn plan<'a>(
    d: Dialect,
    table: &str,
    changes: &'a Changes,
    types: &HashMap<String, String>,
) -> Result<Vec<Statement<'a>>> {
    let t = d.table_ident(table);
    let mut plan = Vec::new();
    for row in &changes.deletes {
        let mut parts = vec![Part::Sql(format!("delete from {t}"))];
        find(d, &Target { key: &row.key, id: row.id.as_deref(), original: &row.original }, types, &mut parts)?;
        plan.push(Statement { parts, one_row: true, original: &row.original });
    }
    for u in &changes.updates {
        if u.set.is_empty() {
            return Err(Error::Invalid("an update needs at least one value"));
        }
        let mut parts = vec![Part::Sql(format!("update {t} set "))];
        for (i, (column, value)) in u.set.iter().enumerate() {
            let comma = if i == 0 { "" } else { ", " };
            parts.push(Part::Sql(format!("{comma}{} = ", d.ident(column))));
            parts.push(Part::Param(value.into()));
        }
        find(d, &Target { key: &u.key, id: u.id.as_deref(), original: &u.original }, types, &mut parts)?;
        plan.push(Statement { parts, one_row: true, original: &u.original });
    }
    for values in &changes.inserts {
        let parts = if values.is_empty() {
            let rest = if d == Dialect::My { "() values ()" } else { "default values" };
            vec![Part::Sql(format!("insert into {t} {rest}"))]
        } else {
            let columns: Vec<String> = values.iter().map(|(c, _)| d.ident(c)).collect();
            let mut parts = vec![Part::Sql(format!("insert into {t} ({}) values (", columns.join(", ")))];
            for (i, (_, value)) in values.iter().enumerate() {
                if i > 0 {
                    parts.push(Part::Sql(", ".into()));
                }
                parts.push(Part::Param(value.into()));
            }
            parts.push(Part::Sql(")".into()));
            parts
        };
        plan.push(Statement { parts, one_row: false, original: &[] });
    }
    Ok(plan)
}

/// A string literal for showing: plain quotes unless the value has a backslash, where engines differ.
fn shown_literal(d: Dialect, v: &str) -> String {
    if v.contains('\\') { d.literal(v) } else { Dialect::Lite.literal(v) }
}

impl Statement<'_> {
    /// The statement with its values written in, for the preview. Never run: [`Statement::bind`] is.
    fn show(&self, d: Dialect) -> String {
        self.parts
            .iter()
            .map(|p| match p {
                Part::Sql(s) => s.clone(),
                Part::Param(Param::Text(None)) => "NULL".into(),
                Part::Param(Param::Text(Some(v)) | Param::RowId(v)) => shown_literal(d, v),
                Part::Param(Param::File(path)) => format!("<the bytes of {}>", path.display()),
            })
            .collect()
    }

    /// The statement with placeholders, and its parameters. `files` holds each file's bytes as hex, which the
    /// SQL decodes, so every parameter stays text.
    fn bind<'s>(
        &'s self,
        d: Dialect,
        files: &'s HashMap<PathBuf, String>,
    ) -> Result<(String, Vec<Option<Cow<'s, str>>>)> {
        let mut sql = String::new();
        let mut params = Vec::new();
        for p in &self.parts {
            let p = match p {
                Part::Sql(s) => {
                    sql.push_str(s);
                    continue;
                }
                Part::Param(p) => *p,
            };
            let mark = if d == Dialect::My { "?".into() } else { format!("${}", params.len() + 1) };
            let (mark, param) = match p {
                Param::Text(v) => (mark, v.map(Cow::Borrowed)),
                // SQLite compares a text parameter with rowid as text, which never matches.
                Param::RowId(v) if d == Dialect::Lite => (format!("cast({mark} as integer)"), Some(Cow::Borrowed(v))),
                Param::RowId(v) => (mark, Some(Cow::Borrowed(v))),
                Param::File(path) => {
                    let hex = files.get(path).ok_or(Error::Invalid("a file was not read before saving"))?;
                    let decode =
                        if d == Dialect::Pg { format!("decode({mark}, 'hex')") } else { format!("unhex({mark})") };
                    (decode, Some(Cow::Borrowed(hex.as_str())))
                }
            };
            sql.push_str(&mark);
            params.push(param);
        }
        Ok((sql, params))
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    s
}

/// A file's bytes as hex; refuses files over [`MAX_CELL_FILE`].
fn read_hex(path: &Path) -> Result<String> {
    let failed = |source| Error::File { path: path.display().to_string(), source };
    let mut bytes = Vec::new();
    // Read at most one byte past the cap, so a file that grows meanwhile can't blow up memory either.
    std::fs::File::open(path).map_err(failed)?.take(MAX_CELL_FILE + 1).read_to_end(&mut bytes).map_err(failed)?;
    if bytes.len() as u64 > MAX_CELL_FILE {
        return Err(Error::Invalid("the file is over 20 MB, the most a cell loads"));
    }
    Ok(hex(&bytes))
}

/// Reads every file the plan loads into a cell, off the async runtime.
async fn read_files(plan: &[Statement<'_>]) -> Result<HashMap<PathBuf, String>> {
    let paths: Vec<PathBuf> = plan
        .iter()
        .flat_map(|s| &s.parts)
        .filter_map(|p| match p {
            Part::Param(Param::File(path)) => Some(path.to_path_buf()),
            _ => None,
        })
        .collect();
    if paths.is_empty() {
        return Ok(HashMap::new());
    }
    tauri::async_runtime::spawn_blocking(move || {
        paths.into_iter().map(|p| Ok((p.clone(), read_hex(&p)?))).collect::<Result<HashMap<_, _>>>()
    })
    .await?
}

/// The start of a binary cell as hex, and its full size in bytes.
#[derive(Debug, Serialize)]
pub struct CellBytes {
    pub hex: String,
    pub size: u64,
}

/// MySQL's `enum('a','it''s')` column type as its values.
fn mysql_enum(column_type: &str) -> Vec<String> {
    let Some(body) = column_type.strip_prefix("enum(").and_then(|s| s.strip_suffix(')')) else { return vec![] };
    let (mut values, mut value, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (false, '\'') => quoted = true,
            (true, '\'') if chars.peek() == Some(&'\'') => {
                chars.next();
                value.push('\'');
            }
            (true, '\'') => {
                quoted = false;
                values.push(std::mem::take(&mut value));
            }
            (true, c) => value.push(c),
            (false, _) => {} // the commas between values
        }
    }
    values
}

impl Conn {
    /// Applies `changes` to `table` in one transaction; returns the rows affected. Rolls everything back when a
    /// statement fails or an update or delete touches more than one row ([`Error::SeveralRows`]).
    // ponytail: MySQL reports rows changed, not matched, so an update of a vanished row (or one matching several
    // rows with the same new value) isn't detected there; set CLIENT_FOUND_ROWS on the connection if that matters.
    pub async fn save_rows(&mut self, table: &str, changes: &Changes) -> Result<u64> {
        let d = self.dialect();
        let types = self.match_types(table, changes).await?;
        let plan = plan(d, table, changes, &types)?;
        let files = read_files(&plan).await?; // before the transaction: a bad file fails before anything runs
        let statements = plan.iter().map(|s| s.bind(d, &files)).collect::<Result<Vec<_>>>()?;
        self.query(if d == Dialect::My { "start transaction" } else { "begin" }).await?;
        let mut total = 0;
        for (n, ((sql, params), s)) in (1..).zip(statements.iter().zip(&plan)) {
            let params: Vec<Option<&str>> = params.iter().map(Option::as_deref).collect();
            if changes.log_originals && !s.original.is_empty() {
                crate::console::record(&original_note(d, s.original));
            }
            let outcome = match self.execute_with(sql, &params).await {
                Ok(rows) if s.one_row && rows > 1 => Err(Error::SeveralRows),
                Ok(rows) => Ok(rows),
                Err(e) => Err(Error::Statement { n, source: Box::new(e) }),
            };
            match outcome {
                Ok(rows) => total += rows,
                Err(e) => {
                    // The statement's own error is the useful one; a failed rollback ends with the session anyway.
                    let _ = self.query("rollback").await;
                    return Err(e);
                }
            }
        }
        self.query("commit").await?;
        Ok(total)
    }

    /// The statements [`Conn::save_rows`] would run, with values written in, for the preview.
    pub async fn rows_sql(&mut self, table: &str, changes: &Changes) -> Result<Vec<String>> {
        let d = self.dialect();
        let types = self.match_types(table, changes).await?;
        Ok(plan(d, table, changes, &types)?.iter().map(|s| s.show(d)).collect())
    }

    /// The table's column types when some row is matched on its values; nothing is asked otherwise.
    async fn match_types(&mut self, table: &str, changes: &Changes) -> Result<HashMap<String, String>> {
        if !changes.matches_values() {
            return Ok(HashMap::new());
        }
        Ok(self.columns(table).await?.into_iter().map(|c| (c.name, c.data_type)).collect())
    }

    /// The first [`HEX_PREVIEW`] bytes of one binary cell, for the hex viewer.
    pub async fn cell_bytes(&mut self, table: &str, column: &str, row: &Row) -> Result<CellBytes> {
        let d = self.dialect();
        let c = d.ident(column);
        let select = match d {
            Dialect::Pg => {
                format!("select encode(substring({c} from 1 for {HEX_PREVIEW}), 'hex'), octet_length({c})::text")
            }
            Dialect::My => format!("select hex(substring({c}, 1, {HEX_PREVIEW})), octet_length({c})"),
            Dialect::Lite => format!("select hex(substr({c}, 1, {HEX_PREVIEW})), length(cast({c} as blob))"),
        };
        let mut parts = vec![Part::Sql(format!("{select} from {}", d.table_ident(table)))];
        // Binary cells can't be matched on, so a row without key or identity never gets here (find refuses).
        find(d, &Target { key: &row.key, id: row.id.as_deref(), original: &[] }, &HashMap::new(), &mut parts)?;
        let statement = Statement { parts, one_row: true, original: &[] };
        let files = HashMap::new();
        let (sql, params) = statement.bind(d, &files)?;
        let params: Vec<Option<&str>> = params.iter().map(Option::as_deref).collect();
        let mut rows = self.fetch_with(&sql, &params).await?.into_iter();
        let mut first = rows.next().ok_or(Error::Invalid("the row is no longer there"))?.into_iter();
        let hex = first.next().flatten().unwrap_or_default();
        let size = first.next().flatten().and_then(|s| s.parse().ok()).unwrap_or_default();
        Ok(CellBytes { hex: hex.to_lowercase(), size })
    }

    /// Each enum column of `table` with its values in order, for the grid's dropdowns.
    pub async fn enum_values(&mut self, table: &str) -> Result<Vec<(String, Vec<String>)>> {
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        match self.dialect() {
            Dialect::Pg => {
                // regclass parses the name like SQL does, so it gets the quoted form.
                let name = Dialect::Pg.table_ident(table);
                let sql = "select a.attname::text, e.enumlabel::text from pg_attribute a \
                           join pg_enum e on e.enumtypid = a.atttypid \
                           where a.attrelid = $1::regclass and a.attnum > 0 and not a.attisdropped \
                           order by a.attnum, e.enumsortorder";
                for row in self.fetch_with(sql, &[Some(&name)]).await? {
                    let [Some(column), Some(label)] = <[Option<String>; 2]>::try_from(row).unwrap_or_default() else {
                        continue;
                    };
                    match out.last_mut() {
                        Some((c, values)) if *c == column => values.push(label),
                        _ => out.push((column, vec![label])),
                    }
                }
            }
            Dialect::My => {
                let sql = "select column_name, column_type from information_schema.columns \
                           where table_schema = database() and table_name = ? and data_type = 'enum' \
                           order by ordinal_position";
                for row in self.fetch_with(sql, &[Some(table)]).await? {
                    if let [Some(column), Some(t)] = row.as_slice() {
                        out.push((column.clone(), mysql_enum(t)));
                    }
                }
            }
            Dialect::Lite => {}
        }
        Ok(out)
    }
}

/// Saves the grid's staged changes. Safe mode applies: [`Error::ConfirmRequired`] until resent with
/// `confirmed: true`, which saving from the preview does.
#[tauri::command]
pub async fn save_rows(
    open: State<'_, Open>,
    name: String,
    table: String,
    changes: Changes,
    confirmed: bool,
    unlock: Option<crate::safety::Unlock>,
) -> Result<u64> {
    crate::guard(&open, &name, |_| false, confirmed, unlock.as_ref()).await?;
    let mut live = open.get(&name).await?;
    let r = live.conn.save_rows(&table, &changes).await;
    live.check(r)
}

/// The statements `save_rows` would run, for the preview.
#[tauri::command]
pub async fn rows_sql(open: State<'_, Open>, name: String, table: String, changes: Changes) -> Result<Vec<String>> {
    let mut live = open.get(&name).await?;
    let r = live.conn.rows_sql(&table, &changes).await;
    live.check(r)
}

#[tauri::command]
pub async fn cell_bytes(
    open: State<'_, Open>,
    name: String,
    table: String,
    column: String,
    row: Row,
) -> Result<CellBytes> {
    let mut live = open.get(&name).await?;
    let r = live.conn.cell_bytes(&table, &column, &row).await;
    live.check(r)
}

#[tauri::command]
pub async fn enum_values(open: State<'_, Open>, name: String, table: String) -> Result<Vec<(String, Vec<String>)>> {
    let mut live = open.get(&name).await?;
    let r = live.conn.enum_values(&table).await;
    live.check(r)
}

#[cfg(test)]
mod tests {
    use super::{Changes, Row, Update, Value, mysql_enum};
    use crate::db::{Conn, Dialect, Engine, Options};
    use crate::tls::Tls;

    async fn sqlite(setup: &str) -> Conn {
        let mut c =
            Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &Options::default(), None).await.unwrap();
        for sql in setup.split(';').filter(|s| !s.trim().is_empty()) {
            c.query(sql).await.unwrap();
        }
        c
    }

    fn text(v: Option<&str>) -> Value {
        Value::Text(v.map(String::from))
    }

    fn key(id: &str) -> Vec<(String, String)> {
        vec![("id".into(), id.into())]
    }

    fn update(id: &str, column: &str, v: Option<&str>) -> Update {
        Update { key: key(id), id: None, original: vec![], set: vec![(column.into(), text(v))] }
    }

    async fn column(c: &mut Conn, sql: &str) -> Vec<Option<String>> {
        c.query(sql).await.unwrap().rows.into_iter().map(|mut r| r.remove(0)).collect()
    }

    const PEOPLE: &str = "create table t(id integer primary key, v text not null default 'dflt'); \
                          insert into t values (1, 'a'), (2, 'b')";

    #[test]
    fn save_rows_should_delete_update_and_insert_in_one_go() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite(PEOPLE).await;
            let changes = Changes {
                deletes: vec![Row { key: key("2"), id: None, original: vec![] }],
                updates: vec![update("1", "v", Some("O'Brien \\ ;--"))],
                inserts: vec![vec![("id".into(), text(Some("2"))), ("v".into(), text(Some("again")))], vec![]],
                log_originals: false,
            };
            assert_eq!(c.save_rows("t", &changes).await.unwrap(), 4);
            let rows = column(&mut c, "select id || ':' || v from t order by id").await;
            let rows: Vec<&str> = rows.iter().flatten().map(String::as_str).collect();
            assert_eq!(
                rows,
                ["1:O'Brien \\ ;--", "2:again", "3:dflt"],
                "a deleted key can be reused; defaults fill in"
            );
        });
    }

    #[test]
    fn save_rows_should_roll_back_everything_when_a_statement_fails() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite(PEOPLE).await;
            let changes = Changes {
                deletes: vec![Row { key: key("2"), id: None, original: vec![] }],
                updates: vec![update("1", "v", None)], // v is NOT NULL
                inserts: vec![],
                log_originals: false,
            };
            let err = c.save_rows("t", &changes).await.unwrap_err();
            assert!(err.to_string().starts_with("statement 2: "), "{err}");
            assert_eq!(column(&mut c, "select count(*) from t").await, [Some("2".into())], "the delete was undone");
        });
    }

    #[test]
    fn save_rows_should_refuse_a_key_that_matches_several_rows() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite("create table dup(k integer, v text); insert into dup values (1, 'a'), (1, 'b')").await;
            let changes = Changes {
                deletes: vec![Row { key: vec![("k".into(), "1".into())], id: None, original: vec![] }],
                ..Changes::default()
            };
            let err = c.save_rows("dup", &changes).await.unwrap_err();
            assert_eq!(err.to_string(), "a key matched more than one row; nothing was saved");
            assert_eq!(column(&mut c, "select count(*) from dup").await, [Some("2".into())]);
        });
    }

    #[test]
    fn save_rows_should_find_a_row_by_its_rowid_when_asked() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite("create table dup(k integer, v text); insert into dup values (1, 'a'), (1, 'b')").await;
            let id = column(&mut c, "select rowid from dup where v = 'b'").await.remove(0).unwrap();
            let changes = Changes {
                updates: vec![Update {
                    key: vec![("k".into(), "1".into())],
                    id: Some(id),
                    original: vec![],
                    set: vec![("v".into(), text(None))],
                }],
                ..Changes::default()
            };
            assert_eq!(c.save_rows("dup", &changes).await.unwrap(), 1);
            assert_eq!(column(&mut c, "select v from dup order by rowid").await, [Some("a".into()), None]);
        });
    }

    #[test]
    fn save_rows_should_load_a_file_as_bytes() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite("create table b(id integer primary key, data blob)").await;
            let path = std::env::temp_dir().join(format!("tableminus-{}-cell.bin", std::process::id()));
            std::fs::write(&path, [0u8, 0xff, b'\'']).unwrap();
            let changes = Changes {
                inserts: vec![vec![("data".into(), Value::File(super::FileValue { file: path.clone() }))]],
                ..Changes::default()
            };
            c.save_rows("b", &changes).await.unwrap();
            let _ = std::fs::remove_file(&path);
            let r = column(&mut c, "select typeof(data) || ':' || hex(data) from b").await;
            assert_eq!(r, [Some("blob:00FF27".into())]);
        });
    }

    #[test]
    fn save_rows_should_refuse_a_file_over_the_cap() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite("create table b(id integer primary key, data blob)").await;
            let path = std::env::temp_dir().join(format!("tableminus-{}-big.bin", std::process::id()));
            std::fs::File::create(&path).unwrap().set_len(super::MAX_CELL_FILE + 1).unwrap();
            let changes = Changes {
                inserts: vec![vec![("data".into(), Value::File(super::FileValue { file: path.clone() }))]],
                ..Changes::default()
            };
            let err = c.save_rows("b", &changes).await.unwrap_err();
            let _ = std::fs::remove_file(&path);
            assert_eq!(err.to_string(), "the file is over 20 MB, the most a cell loads");
        });
    }

    #[test]
    fn rows_sql_should_show_values_written_in_and_quotes_doubled() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite("").await;
            let changes = Changes {
                deletes: vec![Row { key: key("3"), id: None, original: vec![] }],
                updates: vec![update("1", "na\"me", Some("it's"))],
                inserts: vec![vec![("v".into(), text(None))]],
                log_originals: false,
            };
            assert_eq!(
                c.rows_sql("t", &changes).await.unwrap(),
                [
                    r#"delete from "t" where "id" = '3'"#,
                    r#"update "t" set "na""me" = 'it''s' where "id" = '1'"#,
                    r#"insert into "t" ("v") values (NULL)"#,
                ]
            );
        });
    }

    #[test]
    fn rows_sql_should_refuse_a_change_without_a_key() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite("").await;
            let changes =
                Changes { deletes: vec![Row { key: vec![], id: None, original: vec![] }], ..Changes::default() };
            assert_eq!(
                c.rows_sql("t", &changes).await.unwrap_err().to_string(),
                "every changed or deleted row needs its primary key"
            );
        });
    }

    fn original(values: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        values.iter().map(|(c, v)| ((*c).to_string(), v.map(String::from))).collect()
    }

    fn types(list: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        list.iter().map(|(c, t)| ((*c).to_string(), (*t).to_string())).collect()
    }

    #[test]
    fn plan_should_match_a_mysql_row_on_every_value_exactly_and_change_one() {
        let changes = Changes {
            deletes: vec![Row { original: original(&[("n", Some("1")), ("s", Some("A b"))]), ..Row::default() }],
            ..Changes::default()
        };
        let plan = super::plan(Dialect::My, "t", &changes, &types(&[("n", "int"), ("s", "varchar(10)")])).unwrap();
        assert_eq!(
            plan[0].show(Dialect::My),
            "delete from `t` where `n` <=> '1' and `s` <=> 'A b' collate utf8mb4_bin \
             and char_length(`s`) <=> char_length('A b') limit 1"
        );
    }

    #[test]
    fn plan_should_refuse_to_match_a_row_on_a_float_value() {
        let changes = Changes {
            updates: vec![Update {
                original: original(&[("f", Some("0.1"))]),
                set: vec![("f".into(), text(Some("1")))],
                ..Update::default()
            }],
            ..Changes::default()
        };
        let Err(err) = super::plan(Dialect::My, "t", &changes, &types(&[("f", "float")])) else { panic!() };
        assert!(err.to_string().contains("can't be matched exactly"), "{err}");
    }

    #[test]
    fn plan_should_match_on_values_only_on_mysql() {
        let changes = Changes {
            deletes: vec![Row { original: original(&[("n", Some("1"))]), ..Row::default() }],
            ..Changes::default()
        };
        assert!(super::plan(Dialect::Pg, "t", &changes, &types(&[("n", "int4")])).is_err());
    }

    #[test]
    fn save_rows_should_log_the_original_values_when_asked() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite(PEOPLE).await;
            let changes = Changes {
                updates: vec![Update {
                    original: original(&[("id", Some("1")), ("v", Some("it's"))]),
                    ..update("1", "v", Some("x"))
                }],
                log_originals: true,
                ..Changes::default()
            };
            c.save_rows("t", &changes).await.unwrap();
            let log = crate::console::console_log(0);
            assert!(log.iter().any(|e| e.sql == r#"-- original values: "id" = '1', "v" = 'it''s'"#), "{log:?}");
        });
    }

    /// A table without a primary key: duplicates change one at a time, and case or trailing spaces tell rows apart.
    #[test]
    #[ignore = "needs a local MySQL; creates and drops a scratch database"]
    fn local_mysql_save_rows_should_match_rows_without_a_key() {
        tauri::async_runtime::block_on(async {
            let url = "mysql://root@127.0.0.1:3306";
            let mut c = Conn::open(Engine::Mysql, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let db = format!("dbm_nokey_{}", std::process::id());
            c.query(&format!("create database {db}")).await.unwrap();
            let result = async {
                c.use_database(&db).await?;
                c.query("create table t(n int, s varchar(10), d datetime(3), x double) collate utf8mb4_general_ci").await?;
                c.query(
                    "insert into t values (1, 'a', '2026-01-02 03:04:05.678', 1.5), (1, 'a', '2026-01-02 03:04:05.678', 1.5), \
                     (1, 'A', null, null), (1, 'a ', null, null)",
                )
                .await?;
                let row = original(&[("n", Some("1")), ("s", Some("a")), ("d", Some("2026-01-02 03:04:05.678")), ("x", Some("1.5"))]);
                let changes = Changes {
                    updates: vec![Update { original: row, set: vec![("n".into(), text(Some("2")))], ..Update::default() }],
                    deletes: vec![Row { original: original(&[("n", Some("1")), ("s", Some("A")), ("d", None), ("x", None)]), ..Row::default() }],
                    ..Changes::default()
                };
                let saved = c.save_rows("t", &changes).await?;
                let rows = c.query("select n, concat('[', s, ']') from t order by n, char_length(s)").await?.rows;
                c.query("create table f(v float)").await?;
                let refused = Changes {
                    deletes: vec![Row { original: original(&[("v", Some("0.1"))]), ..Row::default() }],
                    ..Changes::default()
                };
                let refused = c.save_rows("f", &refused).await.is_err();
                Ok::<_, crate::error::Error>((saved, rows, refused))
            }
            .await;
            c.query(&format!("drop database {db}")).await.unwrap();
            let (saved, rows, refused) = result.unwrap();
            let s = |n: &str, v: &str| vec![Some(n.to_string()), Some(v.to_string())];
            assert_eq!(saved, 2);
            assert_eq!(rows, [s("1", "[a]"), s("1", "[a ]"), s("2", "[a]")]);
            assert!(refused, "a FLOAT column can't be matched");
        });
    }

    #[test]
    fn changes_should_read_null_text_and_file_values_from_json() {
        let c: Changes =
            serde_json::from_str(r#"{"inserts": [[["a", null], ["b", "x"], ["c", {"file": "/tmp/f"}]]]}"#).unwrap();
        let kinds: Vec<&str> = c.inserts[0]
            .iter()
            .map(|(_, v)| match v {
                Value::Text(None) => "null",
                Value::Text(Some(_)) => "text",
                Value::File(_) => "file",
            })
            .collect();
        assert_eq!(kinds, ["null", "text", "file"]);
        assert!(serde_json::from_str::<Changes>(r#"{"inserts": [[["a", {"file": "/f", "x": 1}]]]}"#).is_err());
    }

    #[test]
    fn cell_bytes_should_return_hex_and_size() {
        tauri::async_runtime::block_on(async {
            let mut c =
                sqlite("create table b(id integer primary key, data blob); insert into b values (1, x'00ff10')").await;
            let bytes = c.cell_bytes("b", "data", &Row { key: key("1"), id: None, original: vec![] }).await.unwrap();
            assert_eq!((bytes.hex.as_str(), bytes.size), ("00ff10", 3));
        });
    }

    #[test]
    fn mysql_enum_should_read_quoted_values() {
        assert_eq!(mysql_enum("enum('a','it''s','x,y')"), ["a", "it's", "x,y"]);
        assert_eq!(mysql_enum("varchar(10)"), Vec::<String>::new());
    }

    /// Updates, deletes, inserts with a file, and enum values on MySQL, in a scratch database dropped afterwards.
    #[test]
    #[ignore = "needs a local MySQL; creates and drops a scratch database"]
    fn local_mysql_save_rows_and_enum_values() {
        tauri::async_runtime::block_on(async {
            let url = "mysql://root@127.0.0.1:3306";
            let mut c = Conn::open(Engine::Mysql, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let db = format!("dbm_rows_{}", std::process::id());
            c.query(&format!("create database {db}")).await.unwrap();
            let result = async {
                c.use_database(&db).await?;
                c.query("create table t(id int auto_increment primary key, mood enum('ok','it''s'), b blob)").await?;
                c.query("insert into t(mood) values ('ok'), ('ok')").await?;
                let path = std::env::temp_dir().join(format!("tableminus-{}-my.bin", std::process::id()));
                std::fs::write(&path, [0u8, 0xff])?;
                let changes = Changes {
                    deletes: vec![Row { key: key("2"), id: None, original: vec![] }],
                    updates: vec![update("1", "mood", Some("it's"))],
                    inserts: vec![vec![("b".into(), Value::File(super::FileValue { file: path.clone() }))], vec![]],
                    log_originals: false,
                };
                let saved = c.save_rows("t", &changes).await;
                let _ = std::fs::remove_file(&path);
                saved?;
                let rows = c.query("select id, mood, hex(b) from t order by id").await?.rows;
                let enums = c.enum_values("t").await?;
                let bytes = c.cell_bytes("t", "b", &Row { key: key("3"), id: None, original: vec![] }).await?;
                Ok::<_, crate::error::Error>((rows, enums, bytes.hex))
            }
            .await;
            c.query(&format!("drop database {db}")).await.unwrap();
            let (rows, enums, hex) = result.unwrap();
            let s = |v: &str| Some(v.to_string());
            assert_eq!(rows, [vec![s("1"), s("it's"), None], vec![s("3"), None, s("00FF")], vec![s("4"), None, None]]);
            assert_eq!(enums, [("mood".to_string(), vec!["ok".to_string(), "it's".to_string()])]);
            assert_eq!(hex, "00ff");
        });
    }
}

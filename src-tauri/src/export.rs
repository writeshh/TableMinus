//! Export: a table, a filtered result, one page or a query's result, streamed to CSV, JSON or SQL files.
//! Rows go from the database to the file as they arrive; each file is written under a temporary name and renamed
//! into place only once it is complete.

use crate::browse::{Browse, Combine, MAX_PAGE};
use crate::db::{Conn, Dialect, RowSink, quote_ident};
use crate::error::{Error, Result};
use crate::jobs::{Job, Progress};
use serde::Deserialize;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// Rows per INSERT statement in SQL exports.
const INSERT_BATCH: u64 = 100;
/// How often (in rows) progress is reported.
const REPORT_EVERY: u64 = 1000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub items: Vec<Item>,
    pub format: Format,
    #[serde(default)]
    pub gzip: bool,
}

/// One file to write.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub path: PathBuf,
    pub source: Source,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    /// Every row of a table.
    Table(String),
    /// A table filtered and sorted as in its tab; with `page`, only the page the tab shows.
    Rows { request: Browse, page: bool },
    /// The result of one statement from the editor. `table` names the target of SQL INSERTs.
    Query { sql: String, table: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Format {
    Csv(Csv),
    /// An array of objects, every value a string or null.
    Json,
    Sql(Sql),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(clippy::struct_excessive_bools, reason = "independent on/off settings, one per checkbox in the dialog")]
pub struct Csv {
    pub delimiter: char,
    /// Quote every field, not only those holding the delimiter, a quote or a line break.
    pub quote_all: bool,
    /// Windows line breaks.
    pub crlf: bool,
    /// Field names in the first row.
    pub header: bool,
    /// NULL as an empty field; otherwise as the text NULL.
    pub null_empty: bool,
    /// The quote around fields that need one (doubled inside them).
    #[serde(default = "double_quote")]
    pub quote: char,
    /// The decimal separator for values that are plain decimal numbers (`-12.5`, `3.0e2`): `.` keeps them as the
    /// database gives them, `,` is for spreadsheets set up that way.
    #[serde(default = "point")]
    pub decimal: char,
}

fn double_quote() -> char {
    '"'
}

fn point() -> char {
    '.'
}

impl Csv {
    /// Options that would make a file nobody can read back are refused.
    fn check(&self) -> Result<()> {
        if !['"', '\''].contains(&self.quote) {
            return Err(Error::Invalid("the CSV quote is a double or a single quote"));
        }
        if !['.', ','].contains(&self.decimal) {
            return Err(Error::Invalid("the CSV decimal separator is a point or a comma"));
        }
        if self.delimiter == self.quote || ['\n', '\r'].contains(&self.delimiter) {
            return Err(Error::Invalid("the CSV delimiter can't be the quote or a line break"));
        }
        Ok(())
    }
}

/// Whether `v` is a plain decimal number, as databases print numeric and float values.
fn is_decimal(v: &str) -> bool {
    let digits = v.strip_prefix('-').unwrap_or(v);
    let (mantissa, exponent) = digits.split_once(['e', 'E']).unwrap_or((digits, ""));
    let exponent_ok = exponent.is_empty() || {
        let e = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit())
    };
    let Some((whole, fraction)) = mantissa.split_once('.') else { return false };
    !whole.is_empty()
        && !fraction.is_empty()
        && whole.bytes().chain(fraction.bytes()).all(|b| b.is_ascii_digit())
        && exponent_ok
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sql {
    /// CREATE TABLE before the rows (table sources only).
    pub structure: bool,
    /// DROP TABLE IF EXISTS before the structure.
    pub drop: bool,
    /// The rows as INSERT statements.
    pub content: bool,
}

/// Writes each item of `export` to its file. Returns the number of rows written.
pub async fn run(conn: &mut Conn, export: &Export, job: &Job) -> Result<u64> {
    if export.items.is_empty() {
        return Err(Error::Invalid("nothing to export"));
    }
    if let Format::Csv(o) = &export.format {
        o.check()?;
    }
    let mut rows = 0;
    for (i, item) in export.items.iter().enumerate() {
        let progress = Progress { item: i, items: export.items.len(), rows, ..Progress::default() };
        rows += write_item(conn, export, item, job, progress).await?;
    }
    Ok(rows)
}

/// The query behind an item: SQL, bound values, the table it reads (if any) and how many rows it will return.
struct Plan {
    sql: String,
    params: Vec<String>,
    table: Option<String>,
    total: Option<u64>,
}

async fn plan(conn: &mut Conn, source: &Source) -> Result<Plan> {
    let (request, page) = match source {
        Source::Query { sql, .. } => {
            return Ok(Plan { sql: sql.clone(), params: vec![], table: None, total: None });
        }
        Source::Rows { request, page } => (request.clone(), *page),
        Source::Table(table) => {
            let all = Browse {
                table: table.clone(),
                filters: vec![],
                combine: Combine::And,
                sort: vec![],
                limit: 0,
                offset: 0,
                count: false,
                estimate_over: None,
                identity: false,
                wkt: false,
            };
            (all, false)
        }
    };
    let s = conn.select(&request).await?;
    let (sql, total) = if page {
        let limit = request.limit.min(MAX_PAGE);
        (format!("{} limit {limit} offset {}", s.rows, request.offset), None)
    } else {
        let total = conn.count(&s).await?;
        (s.rows, total)
    };
    Ok(Plan { sql, params: s.params, table: Some(request.table), total })
}

async fn write_item(conn: &mut Conn, export: &Export, item: &Item, job: &Job, progress: Progress) -> Result<u64> {
    let plan = plan(conn, &item.source).await?;
    let mysql = conn.dialect() == Dialect::My;
    let target = match (&item.source, &plan.table) {
        (Source::Query { table, .. }, _) | (_, Some(table)) => conn.table_ident(table),
        (_, None) => String::new(),
    };
    let mut head = String::new();
    if let (Format::Sql(o), Some(table)) = (&export.format, &plan.table) {
        if o.drop {
            let _ = writeln!(head, "drop table if exists {target};"); // writing to a String can't fail
        }
        if o.structure {
            head += &structure(conn, table).await?;
            head += "\n";
        }
    }
    let content = !matches!(&export.format, Format::Sql(o) if !o.content);

    let tmp = temp_path(&item.path);
    let written = async {
        let file = File::create(&tmp)?;
        let buf = BufWriter::new(file);
        let out = if export.gzip {
            Out::Gzip(flate2::write::GzEncoder::new(buf, flate2::Compression::default()))
        } else {
            Out::Plain(buf)
        };
        let mut w = Writer { out, format: &export.format, mysql, target, keys: vec![], rows: 0, job, progress };
        w.progress.total = plan.total;
        w.out.write_all(head.as_bytes())?;
        if content {
            let bound: Vec<Option<&str>> = plan.params.iter().map(|p| Some(p.as_str())).collect();
            conn.stream(&plan.sql, &bound, &mut w).await?;
            w.end()?;
        }
        job.report(Progress { done: w.rows, rows: w.progress.rows + w.rows, ..w.progress });
        let rows = w.rows;
        w.out.finish()?;
        Ok::<_, Error>(rows)
    }
    .await;
    match written {
        Ok(rows) => {
            std::fs::rename(&tmp, &item.path)
                .map_err(|source| Error::Write { path: item.path.display().to_string(), source })?;
            Ok(rows)
        }
        Err(e) => {
            // The export's own error is the one worth reporting; a leftover temp file is only clutter.
            let _ = std::fs::remove_file(&tmp);
            match e {
                Error::Io(source) => Err(Error::Write { path: item.path.display().to_string(), source }),
                e => Err(e),
            }
        }
    }
}

/// Where a file is written before it is complete.
fn temp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tableminus-tmp");
    tmp.into()
}

/// A CREATE TABLE statement for `table`, ending in a semicolon and a line break.
async fn structure(conn: &mut Conn, table: &str) -> Result<String> {
    match conn.dialect() {
        Dialect::Lite => {
            let rows = conn.fetch_with("select sql from sqlite_master where name = $1", &[Some(table)]).await?;
            let sql = rows.into_iter().next().and_then(|mut r| r.swap_remove(0));
            Ok(format!("{};\n", sql.ok_or(Error::Invalid("no such table"))?))
        }
        Dialect::My => {
            let r = conn.query(&format!("show create table {}", conn.table_ident(table))).await?;
            let sql = r.rows.into_iter().next().and_then(|mut row| row.pop().flatten());
            Ok(format!("{};\n", sql.ok_or(Error::Invalid("no such table"))?))
        }
        // DuckDB keeps each CREATE statement (and its indexes), which `ddl` gathers.
        Dialect::Duck => Ok(format!("{}\n", conn.ddl(table).await?)),
        // ponytail: Postgres has no SHOW CREATE TABLE, so this is rebuilt from the columns: types, NOT NULL,
        // defaults and the primary key, without other constraints or indexes. Backups (pg_dump) copy everything.
        Dialect::Pg => {
            let columns = conn.columns(table).await?;
            let mut lines: Vec<String> = columns
                .iter()
                .map(|c| {
                    // A sequence default names a sequence the target won't have; serial creates its own.
                    let serial = c.default.as_deref().is_some_and(|d| d.starts_with("nextval("));
                    let ty = match (serial, c.data_type.as_str()) {
                        (true, "int2") => "smallserial",
                        (true, "int4") => "serial",
                        (true, "int8") => "bigserial",
                        (_, other) => other,
                    };
                    let mut line = format!("  {} {ty}", conn.ident(&c.name));
                    if !c.nullable {
                        line += " not null";
                    }
                    if let Some(d) = c.default.as_ref().filter(|_| !ty.ends_with("serial")) {
                        let _ = write!(line, " default {d}"); // writing to a String can't fail
                    }
                    line
                })
                .collect();
            let key: Vec<String> = columns.iter().filter(|c| c.primary).map(|c| conn.ident(&c.name)).collect();
            if !key.is_empty() {
                lines.push(format!("  primary key ({})", key.join(", ")));
            }
            Ok(format!("create table {} (\n{}\n);\n", conn.table_ident(table), lines.join(",\n")))
        }
    }
}

/// The file being written: plain or gzip.
enum Out {
    Plain(BufWriter<File>),
    Gzip(flate2::write::GzEncoder<BufWriter<File>>),
}

impl Write for Out {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Out::Plain(w) => w.write(buf),
            Out::Gzip(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Out::Plain(w) => w.flush(),
            Out::Gzip(w) => w.flush(),
        }
    }
}

impl Out {
    /// Ends the gzip stream and gets everything onto the disk, so a rename afterwards publishes a whole file.
    fn finish(self) -> std::io::Result<()> {
        let buf = match self {
            Out::Plain(w) => w,
            Out::Gzip(w) => w.finish()?,
        };
        buf.into_inner().map_err(std::io::IntoInnerError::into_error)?.sync_all()
    }
}

/// Turns streamed rows into the chosen format.
struct Writer<'a> {
    out: Out,
    format: &'a Format,
    mysql: bool,
    /// The quoted table INSERTs go into; once the columns are known, the start of each INSERT.
    target: String,
    /// JSON: the column names, quoted.
    keys: Vec<String>,
    rows: u64,
    job: &'a Job,
    progress: Progress,
}

impl RowSink for Writer<'_> {
    fn columns(&mut self, names: Vec<String>) -> Result<()> {
        match self.format {
            Format::Csv(o) if o.header => {
                let line: Vec<String> = names.iter().map(|n| csv_field(o, Some(n))).collect();
                write!(self.out, "{}{}", line.join(&o.delimiter.to_string()), eol(o))?;
            }
            Format::Csv(_) => {}
            Format::Json => {
                self.out.write_all(b"[")?;
                self.keys = names.iter().map(serde_json::to_string).collect::<serde_json::Result<_>>()?;
            }
            Format::Sql(_) => {
                let cols: Vec<String> = names.iter().map(|n| quote_ident(self.mysql, n)).collect();
                self.target = format!("insert into {} ({}) values", self.target, cols.join(", "));
            }
        }
        Ok(())
    }

    fn row(&mut self, row: Vec<Option<String>>) -> Result<()> {
        self.job.check()?;
        match self.format {
            Format::Csv(o) => {
                let fields: Vec<String> = row.iter().map(|v| csv_field(o, v.as_deref())).collect();
                write!(self.out, "{}{}", fields.join(&o.delimiter.to_string()), eol(o))?;
            }
            Format::Json => {
                let sep = if self.rows == 0 { "\n" } else { ",\n" };
                let pairs: Vec<String> = self
                    .keys
                    .iter()
                    .zip(&row)
                    .map(|(k, v)| Ok(format!("{k}: {}", serde_json::to_string(v)?)))
                    .collect::<serde_json::Result<_>>()?;
                write!(self.out, "{sep}  {{{}}}", pairs.join(", "))?;
            }
            Format::Sql(_) => {
                if self.rows.is_multiple_of(INSERT_BATCH) {
                    let end = if self.rows == 0 { "" } else { ";\n" };
                    writeln!(self.out, "{end}{}", self.target)?;
                } else {
                    self.out.write_all(b",\n")?;
                }
                let values: Vec<String> = row.iter().map(|v| literal(self.mysql, v.as_deref())).collect();
                write!(self.out, "({})", values.join(", "))?;
            }
        }
        self.rows += 1;
        if self.rows.is_multiple_of(REPORT_EVERY) {
            self.job.report(Progress { done: self.rows, rows: self.progress.rows + self.rows, ..self.progress });
        }
        Ok(())
    }
}

impl Writer<'_> {
    /// Closes what the rows left open.
    fn end(&mut self) -> Result<()> {
        match self.format {
            Format::Json => self.out.write_all(if self.rows == 0 { b"]\n" } else { b"\n]\n" })?,
            Format::Sql(_) if self.rows > 0 => self.out.write_all(b";\n")?,
            _ => {}
        }
        Ok(())
    }
}

fn eol(o: &Csv) -> &'static str {
    if o.crlf { "\r\n" } else { "\n" }
}

/// One CSV field, quoted when it must be (or always, with `quote_all`), its decimal point swapped when asked.
fn csv_field(o: &Csv, v: Option<&str>) -> String {
    let Some(v) = v else { return if o.null_empty { String::new() } else { "NULL".into() } };
    let v = if o.decimal == '.' || !is_decimal(v) { v.to_string() } else { v.replacen('.', &o.decimal.to_string(), 1) };
    if o.quote_all || v.contains(o.delimiter) || v.contains([o.quote, '\n', '\r']) {
        let q = o.quote.to_string();
        format!("{q}{}{q}", v.replace(o.quote, &format!("{q}{q}")))
    } else {
        v
    }
}

/// A SQL string literal. MySQL also reads backslashes as escapes by default, so they're doubled there. Every
/// value is quoted; the server converts it to the column's type, as it does for bound text parameters.
fn literal(mysql: bool, v: Option<&str>) -> String {
    let Some(v) = v else { return "NULL".into() };
    let s = v.replace('\'', "''");
    if mysql { format!("'{}'", s.replace('\\', "\\\\")) } else { format!("'{s}'") }
}

#[cfg(test)]
mod tests {
    use super::{Csv, Export, Format, Item, Source, Sql};
    use crate::browse::{Browse, Combine, Filter, Op, Sort};
    use crate::db::{Conn, Engine, Options};
    use crate::jobs::Job;
    use crate::tls::Tls;
    use std::path::{Path, PathBuf};

    async fn people() -> Conn {
        let mut c =
            Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &Options::default(), None).await.unwrap();
        c.query("create table people(id integer primary key, name text, note text)").await.unwrap();
        c.query("insert into people values (1, 'Ada', 'says \"hi\", twice'), (2, 'O''Hara', null), (3, 'Cy', 'a\nb')")
            .await
            .unwrap();
        c
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tableminus-{}-export-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn csv() -> Format {
        Format::Csv(Csv {
            delimiter: ',',
            quote_all: false,
            crlf: false,
            header: true,
            null_empty: true,
            quote: '"',
            decimal: '.',
        })
    }

    async fn export_one(c: &mut Conn, source: Source, format: Format, gzip: bool, path: &Path) -> u64 {
        let export = Export { items: vec![Item { path: path.to_path_buf(), source }], format, gzip };
        super::run(c, &export, &Job::quiet()).await.unwrap()
    }

    #[test]
    fn csv_export_should_quote_only_fields_that_need_it() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let path = dir("csv").join("people.csv");
            assert_eq!(export_one(&mut c, Source::Table("people".into()), csv(), false, &path).await, 3);
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "id,name,note\n1,Ada,\"says \"\"hi\"\", twice\"\n2,O'Hara,\n3,Cy,\"a\nb\"\n"
            );
        });
    }

    #[test]
    fn csv_export_should_honour_delimiter_quote_all_crlf_and_null_text() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let path = dir("csv-opts").join("people.csv");
            let format = Format::Csv(Csv {
                delimiter: ';',
                quote_all: true,
                crlf: true,
                header: false,
                null_empty: false,
                quote: '"',
                decimal: '.',
            });
            let source = Source::Query { sql: "select id, note from people where id = 2".into(), table: "q".into() };
            export_one(&mut c, source, format, false, &path).await;
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "\"2\";NULL\r\n");
        });
    }

    #[test]
    fn json_export_should_write_an_array_of_objects() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let path = dir("json").join("people.json");
            export_one(&mut c, Source::Table("people".into()), Format::Json, false, &path).await;
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(v[0]["note"], "says \"hi\", twice");
            assert_eq!(v[1]["note"], serde_json::Value::Null);
            assert_eq!(v.as_array().unwrap().len(), 3);
        });
    }

    #[test]
    fn json_export_of_no_rows_should_be_an_empty_array() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let path = dir("json-empty").join("none.json");
            let source = Source::Query { sql: "select * from people where 0".into(), table: "q".into() };
            export_one(&mut c, source, Format::Json, false, &path).await;
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]\n");
        });
    }

    #[test]
    fn sql_export_should_recreate_the_table_when_run() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let path = dir("sql").join("people.sql");
            let format = Format::Sql(Sql { structure: true, drop: true, content: true });
            export_one(&mut c, Source::Table("people".into()), format, false, &path).await;
            let script = std::fs::read_to_string(&path).unwrap();
            assert!(script.starts_with("drop table if exists \"people\";\nCREATE TABLE people"), "{script}");
            c.query("insert into people values (9, 'gone', null)").await.unwrap();
            c.query(&script).await.unwrap();
            let r = c.query("select id, name, note from people order by id").await.unwrap();
            assert_eq!(r.rows.len(), 3, "the dropped table's extra row is gone");
            assert_eq!(r.rows[1], vec![Some("2".into()), Some("O'Hara".into()), None]);
        });
    }

    #[test]
    fn sql_export_should_batch_inserts() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            c.query(
                "with recursive n(x) as (select 10 union all select x + 1 from n where x < 259) \
                 insert into people(id, name) select x, 'p' || x from n",
            )
            .await
            .unwrap();
            let path = dir("sql-batch").join("people.sql");
            let format = Format::Sql(Sql { structure: false, drop: false, content: true });
            assert_eq!(export_one(&mut c, Source::Table("people".into()), format, false, &path).await, 253);
            let script = std::fs::read_to_string(&path).unwrap();
            assert_eq!(script.matches("insert into").count(), 3, "100 + 100 + 53 rows");
        });
    }

    /// The requests importexport.ts sends, as captured from the dialog.
    #[test]
    fn export_requests_from_the_ui_should_deserialize() {
        let tables = r#"{"items":[{"path":"/x/people.sql","source":{"table":"people"}}],
            "format":{"sql":{"structure":true,"drop":true,"content":true}},"gzip":false}"#;
        assert!(matches!(serde_json::from_str::<Export>(tables).unwrap().format, Format::Sql(_)));
        let rows = r#"{"items":[{"path":"/x/p.json","source":{"rows":{"request":{"table":"people","filters":[],
            "combine":"and","sort":[],"limit":300,"offset":0,"count":false},"page":true}}}],"format":"json","gzip":true}"#;
        assert!(serde_json::from_str::<Export>(rows).unwrap().gzip);
        let query = r#"{"items":[{"path":"/x/q.csv","source":{"query":{"sql":"select 1","table":"t"}}}],
            "format":{"csv":{"delimiter":"\t","quote_all":false,"crlf":true,"header":true,"null_empty":true}},"gzip":false}"#;
        assert!(matches!(
            serde_json::from_str::<Export>(query).unwrap().format,
            Format::Csv(Csv { delimiter: '\t', .. })
        ));
    }

    #[test]
    fn mysql_literals_should_double_backslashes() {
        assert_eq!(super::literal(true, Some("a\\'b")), "'a\\\\''b'");
        assert_eq!(super::literal(false, Some("a\\'b")), "'a\\''b'");
        assert_eq!(super::literal(false, None), "NULL");
    }

    #[test]
    fn export_should_apply_filters_and_sort_and_optionally_one_page() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let request = Browse {
                table: "people".into(),
                filters: vec![Filter {
                    column: Some("id".into()),
                    op: Op::Ge,
                    value: "2".into(),
                    value2: String::new(),
                }],
                combine: Combine::And,
                sort: vec![Sort { column: "id".into(), desc: true }],
                limit: 1,
                offset: 1,
                count: false,
                estimate_over: None,
                identity: false,
                wkt: false,
            };
            let d = dir("rows");
            let format = || {
                Format::Csv(Csv {
                    delimiter: ',',
                    quote_all: false,
                    crlf: false,
                    header: false,
                    null_empty: true,
                    quote: '"',
                    decimal: '.',
                })
            };
            let all = d.join("all.csv");
            export_one(&mut c, Source::Rows { request: request.clone(), page: false }, format(), false, &all).await;
            assert_eq!(std::fs::read_to_string(all).unwrap(), "3,Cy,\"a\nb\"\n2,O'Hara,\n");
            let page = d.join("page.csv");
            export_one(&mut c, Source::Rows { request, page: true }, format(), false, &page).await;
            assert_eq!(std::fs::read_to_string(page).unwrap(), "2,O'Hara,\n");
        });
    }

    #[test]
    fn gzip_export_should_decompress_to_the_same_text() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let path = dir("gz").join("people.csv.gz");
            export_one(&mut c, Source::Table("people".into()), csv(), true, &path).await;
            let mut text = String::new();
            std::io::Read::read_to_string(
                &mut flate2::read::GzDecoder::new(std::fs::File::open(&path).unwrap()),
                &mut text,
            )
            .unwrap();
            assert!(text.starts_with("id,name,note\n1,Ada,"), "{text}");
        });
    }

    #[test]
    fn several_tables_should_go_to_their_own_files() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            c.query("create table pets(name text)").await.unwrap();
            let d = dir("many");
            let items = ["people", "pets"]
                .iter()
                .map(|t| Item { path: d.join(format!("{t}.csv")), source: Source::Table((*t).into()) })
                .collect();
            let n = super::run(&mut c, &Export { items, format: csv(), gzip: false }, &Job::quiet()).await.unwrap();
            assert_eq!(n, 3);
            assert_eq!(std::fs::read_to_string(d.join("pets.csv")).unwrap(), "name\n");
        });
    }

    #[test]
    fn a_failed_export_should_leave_no_file_behind() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let d = dir("fail");
            let path = d.join("bad.csv");
            std::fs::write(&path, "kept").unwrap();
            let export = Export {
                items: vec![Item {
                    path: path.clone(),
                    source: Source::Query { sql: "select nope".into(), table: "q".into() },
                }],
                format: csv(),
                gzip: false,
            };
            assert!(super::run(&mut c, &export, &Job::quiet()).await.is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "kept", "the old file is untouched");
            assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "no temporary file left");
        });
    }

    #[test]
    fn a_cancelled_export_should_stop_and_leave_no_file() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let d = dir("cancel");
            let job = Job::quiet();
            job.cancel();
            let export = Export {
                items: vec![Item { path: d.join("p.csv"), source: Source::Table("people".into()) }],
                format: csv(),
                gzip: false,
            };
            let err = super::run(&mut c, &export, &job).await.unwrap_err();
            assert_eq!(err.to_string(), "cancelled");
            assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
        });
    }

    /// Creates and drops a scratch database on the local MySQL.
    #[test]
    #[ignore = "needs a local MySQL; creates and drops a scratch database"]
    fn local_mysql_sql_export_round_trips() {
        tauri::async_runtime::block_on(async {
            let url = "mysql://root@127.0.0.1:3306";
            let mut c = Conn::open(Engine::Mysql, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let db = format!("dbm_export_{}", std::process::id());
            c.query(&format!("create database {db}")).await.unwrap();
            c.use_database(&db).await.unwrap();
            let result = async {
                c.query("create table t(id int primary key, s varchar(20), d date)").await?;
                c.query("insert into t values (1, 'a\\\\b''c', '2026-01-02'), (2, null, null)").await?;
                let path = dir("mysql").join("t.sql");
                let format = Format::Sql(Sql { structure: true, drop: true, content: true });
                export_one(&mut c, Source::Table("t".into()), format, false, &path).await;
                for stmt in std::fs::read_to_string(&path).unwrap().split(";\n").filter(|s| !s.trim().is_empty()) {
                    c.query(stmt).await?;
                }
                c.query("select s, d from t order by id").await
            }
            .await;
            c.query(&format!("drop database {db}")).await.unwrap();
            let r = result.unwrap();
            assert_eq!(r.rows[0], vec![Some("a\\b'c".into()), Some("2026-01-02".into())]);
            assert_eq!(r.rows[1], vec![None, None]);
        });
    }

    fn csv_with(quote: char, decimal: char, delimiter: char) -> super::Csv {
        super::Csv { delimiter, quote_all: false, crlf: false, header: false, null_empty: true, quote, decimal }
    }

    #[test]
    fn csv_field_should_quote_with_the_chosen_quote() {
        let o = csv_with('\'', '.', ',');
        assert_eq!(super::csv_field(&o, Some("it's, here")), "'it''s, here'");
        assert_eq!(super::csv_field(&o, Some(r#"say "hi""#)), r#"say "hi""#, "a double quote needs no quoting now");
    }

    #[test]
    fn csv_field_should_swap_the_decimal_point_of_numbers_only() {
        let o = csv_with('"', ',', ';');
        assert_eq!(super::csv_field(&o, Some("-12.50")), "-12,50");
        assert_eq!(super::csv_field(&o, Some("3.0e-2")), "3,0e-2");
        assert_eq!(super::csv_field(&o, Some("1.2.3")), "1.2.3");
        assert_eq!(super::csv_field(&o, Some("12")), "12");
        assert_eq!(super::csv_field(&o, Some("v1.5")), "v1.5");
        assert_eq!(super::csv_field(&o, Some(".5")), ".5");
    }

    #[test]
    fn csv_field_should_quote_a_comma_decimal_when_comma_is_the_delimiter() {
        assert_eq!(super::csv_field(&csv_with('"', ',', ','), Some("1.5")), "\"1,5\"");
    }

    #[test]
    fn csv_check_should_refuse_a_delimiter_equal_to_the_quote() {
        assert!(csv_with('"', '.', '"').check().is_err());
        assert!(csv_with('`', '.', ',').check().is_err());
        assert!(csv_with('"', ';', ',').check().is_err());
        assert!(csv_with('"', ',', ';').check().is_ok());
    }
}

//! Import: CSV and JSON files into a table as batched INSERTs with bound values, in one transaction; `.sql` files
//! statement by statement. Files are read as they go, never whole (JSON aside, which is capped).

use crate::db::{Conn, Engine};
use crate::error::{Error, Result};
use crate::jobs::{Job, Progress};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// Rows the wizard shows.
const PREVIEW_ROWS: usize = 100;
/// The longest CSV record or SQL statement read; anything longer is most likely a wrong quote character.
const MAX_RECORD: usize = 64 * 1024 * 1024;
/// Fields per record; MySQL's column limit.
const MAX_FIELDS: usize = 4096;
/// JSON files are parsed whole, so they are capped.
const MAX_JSON: u64 = 100 * 1024 * 1024;
/// Rows per INSERT at most; fewer for wide tables, to stay under SQLite's 32,766 bound parameters.
const BATCH_ROWS: usize = 500;
const MAX_PARAMS: usize = 30_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum FileFormat {
    Csv(CsvFormat),
    /// An array of objects; the fields are every key, in the order first seen.
    Json,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CsvFormat {
    pub delimiter: char,
    pub quote: char,
    /// The first record names the fields.
    pub header: bool,
    /// Empty fields become NULL.
    pub empty_is_null: bool,
}

/// What to do with a row whose key is already in the table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Conflict {
    /// Stop and roll back everything (as any other error does).
    #[default]
    Stop,
    /// Keep the existing row.
    Skip,
    /// Overwrite the existing row's imported columns.
    Update,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Import {
    pub path: std::path::PathBuf,
    pub format: FileFormat,
    /// `schema.table` on Postgres.
    pub table: String,
    /// For each field of the [`Preview`], the column it goes into; None skips the field.
    pub columns: Vec<Option<String>>,
    /// Create the table first, with a text column per imported field.
    #[serde(default)]
    pub create: bool,
    #[serde(default)]
    pub conflict: Conflict,
}

#[derive(Debug, Serialize)]
pub struct Preview {
    pub fields: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

/// Reads CSV records from a file as it goes. Quoted fields may hold delimiters, doubled quotes and line breaks.
struct CsvReader {
    r: BufReader<File>,
    f: CsvFormat,
    line: String,
    /// Bytes read so far, for progress.
    read: u64,
}

impl CsvReader {
    fn open(path: &Path, f: CsvFormat) -> Result<CsvReader> {
        if f.delimiter == f.quote || ['\r', '\n'].contains(&f.delimiter) || ['\r', '\n'].contains(&f.quote) {
            return Err(Error::Invalid("the delimiter and quote must differ and can't be line breaks"));
        }
        Ok(CsvReader { r: BufReader::new(open(path)?), f, line: String::new(), read: 0 })
    }

    /// The next record, skipping blank lines; None at the end of the file.
    fn record(&mut self) -> Result<Option<Vec<String>>> {
        let (delim, quote) = (self.f.delimiter, self.f.quote);
        let mut fields = Vec::new();
        let mut field = String::new();
        let mut quoted = false; // inside quotes, where line breaks are part of the field
        let mut size = 0;
        loop {
            self.line.clear();
            let n = (&mut self.r).take((MAX_RECORD + 1) as u64).read_line(&mut self.line)?;
            if self.read == 0 && self.line.starts_with('\u{feff}') {
                self.line.remove(0); // a UTF-8 byte order mark
            }
            self.read += n as u64;
            size += n;
            if size > MAX_RECORD {
                return Err(Error::Invalid("a CSV record is over 64 MB; check the quote character"));
            }
            if n == 0 {
                if quoted {
                    return Err(Error::Invalid("the file ends inside a quoted field; check the quote character"));
                }
                if fields.is_empty() && field.is_empty() {
                    return Ok(None);
                }
                fields.push(field);
                return Ok(Some(fields));
            }
            let mut chars = self.line.chars().peekable();
            while let Some(c) = chars.next() {
                if quoted {
                    if c == quote && chars.peek() == Some(&quote) {
                        chars.next();
                        field.push(quote);
                    } else if c == quote {
                        quoted = false;
                    } else {
                        field.push(c);
                    }
                } else if c == quote && field.is_empty() {
                    quoted = true;
                } else if c == delim {
                    fields.push(std::mem::take(&mut field));
                    if fields.len() > MAX_FIELDS {
                        return Err(Error::Invalid("a CSV record has over 4,096 fields; check the delimiter"));
                    }
                } else if c == '\n' || (c == '\r' && matches!(chars.peek(), Some('\n') | None)) {
                    if fields.is_empty() && field.is_empty() {
                        break; // a blank line
                    }
                    fields.push(field);
                    return Ok(Some(fields));
                } else {
                    field.push(c);
                }
            }
        }
    }
}

fn open(path: &Path) -> Result<File> {
    File::open(path).map_err(|source| Error::File { path: path.display().to_string(), source })
}

fn read_json(path: &Path) -> Result<Vec<serde_json::Map<String, serde_json::Value>>> {
    let file = open(path)?;
    if file.metadata()?.len() > MAX_JSON {
        return Err(Error::Invalid("JSON files over 100 MB can't be imported; convert it to CSV"));
    }
    serde_json::from_reader(BufReader::new(file)).map_err(Error::JsonImport)
}

/// Every key in `objects`, in the order first seen.
fn json_keys(objects: &[serde_json::Map<String, serde_json::Value>]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for key in objects.iter().flat_map(serde_json::Map::keys) {
        if !keys.contains(key) {
            keys.push(key.clone());
            if keys.len() > MAX_FIELDS {
                break;
            }
        }
    }
    keys
}

/// A JSON value as the text the server will convert: strings as they are, other values as JSON.
fn json_text(v: Option<&serde_json::Value>) -> Option<String> {
    match v? {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

fn csv_cell(f: &CsvFormat, v: String) -> Option<String> {
    if f.empty_is_null && v.is_empty() { None } else { Some(v) }
}

/// The fields and first rows of a file, as the import would read them.
pub fn preview(path: &Path, format: &FileFormat) -> Result<Preview> {
    match format {
        FileFormat::Csv(f) => {
            let mut r = CsvReader::open(path, *f)?;
            let mut fields = if f.header { r.record()?.unwrap_or_default() } else { vec![] };
            let mut rows = Vec::new();
            while rows.len() < PREVIEW_ROWS {
                let Some(record) = r.record()? else { break };
                rows.push(record);
            }
            let width = rows.iter().map(Vec::len).max().unwrap_or(0).max(fields.len());
            fields.resize(width, String::new());
            for (i, name) in fields.iter_mut().enumerate() {
                if name.trim().is_empty() {
                    *name = format!("column{}", i + 1);
                }
            }
            let rows = rows.into_iter().map(|r| r.into_iter().map(|v| csv_cell(f, v)).collect()).collect();
            Ok(Preview { fields, rows })
        }
        FileFormat::Json => {
            let objects = read_json(path)?;
            let fields = json_keys(&objects);
            let rows = objects
                .iter()
                .take(PREVIEW_ROWS)
                .map(|o| fields.iter().map(|k| json_text(o.get(k))).collect())
                .collect();
            Ok(Preview { fields, rows })
        }
    }
}

/// Batched INSERTs into the target table.
struct Insert {
    /// `insert … into t (columns) values`
    head: String,
    /// What to do on a duplicate key.
    tail: String,
    width: usize,
    rows: usize,
    /// The statement for a full batch, built once.
    full: String,
}

impl Insert {
    async fn new(conn: &mut Conn, imp: &Import, columns: &[&str]) -> Result<Insert> {
        let mysql = matches!(conn, Conn::My(_));
        let quoted: Vec<String> = columns.iter().map(|c| conn.ident(c)).collect();
        let set = |value: &dyn Fn(&str) -> String| -> Vec<String> { quoted.iter().map(|q| value(q)).collect() };
        let (verb, tail) = match imp.conflict {
            Conflict::Stop => ("insert", String::new()),
            Conflict::Skip if mysql => ("insert ignore", String::new()),
            Conflict::Skip => ("insert", " on conflict do nothing".into()),
            Conflict::Update if mysql => {
                ("insert", format!(" on duplicate key update {}", set(&|q| format!("{q} = values({q})")).join(", ")))
            }
            Conflict::Update => {
                let table = conn.columns(&imp.table).await?;
                let key: Vec<&str> = table.iter().filter(|c| c.primary).map(|c| c.name.as_str()).collect();
                if key.is_empty() {
                    return Err(Error::Invalid("updating duplicates needs a primary key on the table"));
                }
                let changes: Vec<String> = columns
                    .iter()
                    .filter(|c| !key.contains(c))
                    .map(|c| format!("{0} = excluded.{0}", conn.ident(c)))
                    .collect();
                let target: Vec<String> = key.iter().map(|k| conn.ident(k)).collect();
                let action =
                    if changes.is_empty() { "nothing".into() } else { format!("update set {}", changes.join(", ")) };
                ("insert", format!(" on conflict ({}) do {action}", target.join(", ")))
            }
        };
        let head = format!("{verb} into {} ({}) values", conn.table_ident(&imp.table), quoted.join(", "));
        let width = columns.len();
        let rows = (MAX_PARAMS / width).clamp(1, BATCH_ROWS);
        let mut insert = Insert { head, tail, width, rows, full: String::new() };
        insert.full = insert.sql(conn, rows);
        Ok(insert)
    }

    fn sql(&self, conn: &Conn, rows: usize) -> String {
        let mut n = 0;
        let tuples: Vec<String> = (0..rows)
            .map(|_| {
                let params: Vec<String> = (0..self.width)
                    .map(|_| {
                        n += 1;
                        conn.param(n)
                    })
                    .collect();
                format!("({})", params.join(", "))
            })
            .collect();
        format!("{} {}{}", self.head, tuples.join(", "), self.tail)
    }

    /// Sends the rows in `batch` (flattened, `width` values each) and empties it.
    async fn flush(&self, conn: &mut Conn, batch: &mut Vec<Option<String>>) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let rows = batch.len() / self.width;
        let partial;
        let sql = if rows == self.rows {
            &self.full
        } else {
            partial = self.sql(conn, rows);
            &partial
        };
        let params: Vec<Option<&str>> = batch.iter().map(Option::as_deref).collect();
        conn.execute_with(sql, &params).await?;
        batch.clear();
        Ok(())
    }
}

/// Imports a CSV or JSON file into `imp.table` in one transaction: on any error or a cancel, nothing is kept
/// (the table aside, when `create` made it on MySQL, whose DDL commits by itself). Returns the rows imported.
pub async fn run(conn: &mut Conn, imp: &Import, job: &Job) -> Result<u64> {
    let picked: Vec<(usize, &str)> =
        imp.columns.iter().enumerate().filter_map(|(i, c)| Some((i, c.as_deref()?))).collect();
    if picked.is_empty() {
        return Err(Error::Invalid("choose at least one column to import"));
    }
    let columns: Vec<&str> = picked.iter().map(|(_, c)| *c).collect();
    if columns.iter().enumerate().any(|(i, c)| columns[..i].contains(c)) {
        return Err(Error::Invalid("two fields go into the same column"));
    }
    if imp.create {
        let defs: Vec<String> = columns.iter().map(|c| format!("{} text", conn.ident(c))).collect();
        conn.execute_with(&format!("create table {} ({})", conn.table_ident(&imp.table), defs.join(", ")), &[]).await?;
    } else {
        let existing: Vec<String> = conn.columns(&imp.table).await?.into_iter().map(|c| c.name).collect();
        if existing.is_empty() {
            return Err(Error::Invalid("no such table"));
        }
        if !columns.iter().all(|c| existing.iter().any(|e| e == c)) {
            return Err(Error::Invalid("a field is matched to a column the table doesn't have"));
        }
    }
    let insert = Insert::new(conn, imp, &columns).await?;
    conn.query(if matches!(conn, Conn::My(_)) { "start transaction" } else { "begin" }).await?;
    let result = match &imp.format {
        FileFormat::Csv(f) => import_csv(conn, imp, *f, &picked, &insert, job).await,
        FileFormat::Json => import_json(conn, imp, &picked, &insert, job).await,
    };
    match result {
        Ok(rows) => {
            conn.query("commit").await?;
            Ok(rows)
        }
        Err(e) => {
            // The import's own error is the useful one; a failed rollback ends with the job's connection anyway.
            let _ = conn.query("rollback").await;
            Err(e)
        }
    }
}

async fn import_csv(
    conn: &mut Conn,
    imp: &Import,
    f: CsvFormat,
    picked: &[(usize, &str)],
    insert: &Insert,
    job: &Job,
) -> Result<u64> {
    let mut r = CsvReader::open(&imp.path, f)?;
    let size = r.r.get_ref().metadata()?.len();
    if f.header {
        r.record()?;
    }
    let mut batch = Vec::with_capacity(insert.rows * insert.width);
    let mut rows = 0;
    while let Some(mut record) = r.record()? {
        job.check()?;
        batch.extend(picked.iter().map(|&(i, _)| record.get_mut(i).and_then(|v| csv_cell(&f, std::mem::take(v)))));
        rows += 1;
        if batch.len() == insert.rows * insert.width {
            insert.flush(conn, &mut batch).await?;
            job.report(Progress { items: 1, done: r.read, total: Some(size), rows, ..Progress::default() });
        }
    }
    insert.flush(conn, &mut batch).await?;
    Ok(rows)
}

async fn import_json(
    conn: &mut Conn,
    imp: &Import,
    picked: &[(usize, &str)],
    insert: &Insert,
    job: &Job,
) -> Result<u64> {
    let objects = read_json(&imp.path)?;
    let keys = json_keys(&objects);
    let total = objects.len() as u64;
    let mut batch = Vec::with_capacity(insert.rows * insert.width);
    let mut rows = 0;
    for o in &objects {
        job.check()?;
        batch.extend(picked.iter().map(|&(i, _)| json_text(keys.get(i).and_then(|k| o.get(k)))));
        rows += 1;
        if batch.len() == insert.rows * insert.width {
            insert.flush(conn, &mut batch).await?;
            job.report(Progress { items: 1, done: rows, total: Some(total), rows, ..Progress::default() });
        }
    }
    insert.flush(conn, &mut batch).await?;
    Ok(rows)
}

/// Where the SQL splitter is: in code, or inside something a semicolon doesn't end.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Code,
    LineComment,
    BlockComment,
    Quote(char),
    /// A Postgres dollar quote, with its tag (`$$` or `$tag$`).
    Dollar(String),
}

/// Splits SQL at semicolons outside strings, quoted names, comments and dollar quotes, fed a line at a time.
/// The same rules as the editor's splitter (statements.ts).
// ponytail: MySQL's DELIMITER command (mysqldump's routines and triggers) isn't understood; files using it
// need the MySQL client.
struct Splitter {
    mysql: bool,
    postgres: bool,
    state: State,
    buf: String,
    /// The statement so far holds more than whitespace and comments.
    code: bool,
}

impl Splitter {
    fn new(engine: Engine) -> Splitter {
        Splitter {
            mysql: matches!(engine, Engine::Mysql | Engine::Mariadb),
            postgres: engine == Engine::Postgres,
            state: State::Code,
            buf: String::new(),
            code: false,
        }
    }

    /// The statements `line` completes.
    fn feed(&mut self, line: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut i = 0;
        while let Some(c) = line[i..].chars().next() {
            let rest = &line[i..];
            let mut step = c.len_utf8();
            match &self.state {
                State::Code => {
                    if rest.starts_with("--") || (self.mysql && c == '#') {
                        self.state = State::LineComment;
                    } else if rest.starts_with("/*") {
                        self.state = State::BlockComment;
                        step = 2;
                    } else if c == ';' {
                        if self.code {
                            out.push(self.buf.trim().to_string());
                        }
                        self.buf.clear();
                        self.code = false;
                        i += 1;
                        continue;
                    } else if let Some(tag) = dollar_tag(rest).filter(|_| self.postgres) {
                        step = tag.len();
                        self.state = State::Dollar(tag.to_string());
                        self.code = true;
                    } else if ['\'', '"', '`'].contains(&c) {
                        self.state = State::Quote(c);
                        self.code = true;
                    } else if !c.is_whitespace() {
                        self.code = true;
                    }
                }
                State::LineComment => {
                    if c == '\n' {
                        self.state = State::Code;
                    }
                }
                State::BlockComment => {
                    if rest.starts_with("*/") {
                        self.state = State::Code;
                        step = 2;
                    }
                }
                &State::Quote(q) => {
                    if c == '\\' && self.mysql && q != '`' {
                        step += rest[1..].chars().next().map_or(0, char::len_utf8);
                    } else if c == q && rest[1..].starts_with(q) {
                        step = 2;
                    } else if c == q {
                        self.state = State::Code;
                    }
                }
                State::Dollar(tag) => {
                    if rest.starts_with(tag.as_str()) {
                        step = tag.len();
                        self.state = State::Code;
                    }
                }
            }
            self.buf.push_str(&rest[..step]);
            i += step;
        }
        if self.buf.len() > MAX_RECORD {
            return Err(Error::Invalid("a statement is over 64 MB; check for an unclosed quote"));
        }
        Ok(out)
    }

    /// The last statement, when the file doesn't end with a semicolon.
    fn finish(self) -> Option<String> {
        self.code.then(|| self.buf.trim().to_string())
    }
}

/// `$$` or `$tag$` at the start of `s`; None when the `$` is something else, such as a `$1` parameter.
fn dollar_tag(s: &str) -> Option<&str> {
    let body = s.strip_prefix('$')?;
    let end = body.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    let ok = body[end..].starts_with('$') && !body.starts_with(|c: char| c.is_ascii_digit());
    ok.then(|| &s[..end + 2])
}

/// Runs a `.sql` file statement by statement, stopping at the first that fails. Returns the statements run.
pub async fn run_sql(conn: &mut Conn, engine: Engine, path: &Path, job: &Job) -> Result<u64> {
    let file = open(path)?;
    let size = file.metadata()?.len();
    let mut r = BufReader::new(file);
    let mut split = Splitter::new(engine);
    let mut line = String::new();
    let mut read = 0;
    let mut n = 0;
    loop {
        line.clear();
        let got = (&mut r).take((MAX_RECORD + 1) as u64).read_line(&mut line)?;
        read += got as u64;
        let statements = if got == 0 {
            std::mem::replace(&mut split, Splitter::new(engine)).finish().into_iter().collect()
        } else {
            split.feed(&line)?
        };
        for sql in statements {
            job.check()?;
            n += 1;
            // Rows a SELECT returns aren't wanted; keeping none bounds memory.
            conn.query_limited(&sql, Some(0)).await.map_err(|e| Error::Statement { n, source: Box::new(e) })?;
            job.report(Progress { items: 1, done: read, total: Some(size), rows: n, ..Progress::default() });
        }
        if got == 0 {
            return Ok(n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Conflict, CsvFormat, FileFormat, Import, Splitter};
    use crate::db::{Conn, Engine, Options};
    use crate::jobs::Job;
    use crate::tls::Tls;
    use std::path::PathBuf;

    fn file(name: &str, text: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dbmanager-{}-import", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    fn csv(header: bool) -> CsvFormat {
        CsvFormat { delimiter: ',', quote: '"', header, empty_is_null: true }
    }

    async fn db() -> Conn {
        let mut c =
            Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &Options::default(), None).await.unwrap();
        c.query("create table people(id integer primary key, name text, note text)").await.unwrap();
        c
    }

    fn import(path: PathBuf, format: FileFormat, columns: &[Option<&str>]) -> Import {
        Import {
            path,
            format,
            table: "people".into(),
            columns: columns.iter().map(|c| c.map(String::from)).collect(),
            create: false,
            conflict: Conflict::Stop,
        }
    }

    async fn rows(c: &mut Conn) -> Vec<Vec<Option<String>>> {
        c.query("select id, name, note from people order by id").await.unwrap().rows
    }

    fn row(id: &str, name: &str, note: Option<&str>) -> Vec<Option<String>> {
        vec![Some(id.into()), Some(name.into()), note.map(String::from)]
    }

    /// The requests importexport.ts sends, as captured from the wizard.
    #[test]
    fn import_requests_from_the_ui_should_deserialize() {
        let csv = r#"{"path":"/x/people.csv","format":{"csv":{"delimiter":",","quote":"\"","header":true,"empty_is_null":true}},
            "table":"people","columns":["id","Name",null],"create":false,"conflict":"stop"}"#;
        assert_eq!(serde_json::from_str::<Import>(csv).unwrap().columns[2], None);
        let json = r#"{"path":"/x/o.json","format":"json","table":"orders","columns":["id","name"],"create":true,"conflict":"update"}"#;
        let imp = serde_json::from_str::<Import>(json).unwrap();
        assert!(imp.create && imp.conflict == Conflict::Update);
    }

    #[test]
    fn csv_preview_should_read_quotes_line_breaks_and_a_bom() {
        let p = file("preview.csv", "\u{feff}id,name,note\r\n1,\"O\"\"Hara, Jr\",\"a\nb\"\r\n\r\n2,Bo,\n");
        let pv = super::preview(&p, &FileFormat::Csv(csv(true))).unwrap();
        assert_eq!(pv.fields, ["id", "name", "note"]);
        assert_eq!(pv.rows, [row("1", "O\"Hara, Jr", Some("a\nb")), row("2", "Bo", None)]);
    }

    #[test]
    fn csv_preview_without_header_should_name_fields_by_position() {
        let p = file("noheader.csv", "1;x\n2;y;extra\n");
        let f = CsvFormat { delimiter: ';', quote: '\'', header: false, empty_is_null: false };
        let pv = super::preview(&p, &FileFormat::Csv(f)).unwrap();
        assert_eq!(pv.fields, ["column1", "column2", "column3"]);
        assert_eq!(pv.rows[1][2].as_deref(), Some("extra"));
    }

    #[test]
    fn csv_preview_should_stop_at_100_rows() {
        let text: String = (0..250).fold(String::new(), |t, i| t + &i.to_string() + "\n");
        let pv = super::preview(&file("long.csv", &text), &FileFormat::Csv(csv(false))).unwrap();
        assert_eq!(pv.rows.len(), 100);
    }

    #[test]
    fn csv_reader_should_report_an_unclosed_quote() {
        let err = super::preview(&file("open.csv", "a,\"b\n"), &FileFormat::Csv(csv(false))).unwrap_err();
        assert_eq!(err.to_string(), "the file ends inside a quoted field; check the quote character");
    }

    #[test]
    fn csv_import_should_map_fields_by_position_and_skip_unpicked_ones() {
        tauri::async_runtime::block_on(async {
            let mut c = db().await;
            let p = file("map.csv", "name,ignored,id\nAda,x,1\nO'Hara,y,2\n");
            let imp = import(p, FileFormat::Csv(csv(true)), &[Some("name"), None, Some("id")]);
            assert_eq!(super::run(&mut c, &imp, &Job::quiet()).await.unwrap(), 2);
            assert_eq!(rows(&mut c).await, [row("1", "Ada", None), row("2", "O'Hara", None)]);
        });
    }

    #[test]
    fn csv_import_should_send_many_batches() {
        tauri::async_runtime::block_on(async {
            let mut c = db().await;
            let text: String = (1..=1234).fold(String::new(), |t, i| t + &format!("{i},n{i}\n"));
            let imp = import(file("many.csv", &text), FileFormat::Csv(csv(false)), &[Some("id"), Some("name")]);
            assert_eq!(super::run(&mut c, &imp, &Job::quiet()).await.unwrap(), 1234);
            let r = c.query("select count(*), max(name) from people").await.unwrap();
            assert_eq!(r.rows[0], [Some("1234".into()), Some("n999".into())]);
        });
    }

    #[test]
    fn import_should_roll_back_everything_when_a_row_fails() {
        tauri::async_runtime::block_on(async {
            let mut c = db().await;
            c.query("insert into people values (2, 'kept', null)").await.unwrap();
            let p = file("dup.csv", "1,a\n2,b\n");
            let imp = import(p, FileFormat::Csv(csv(false)), &[Some("id"), Some("name")]);
            assert!(super::run(&mut c, &imp, &Job::quiet()).await.is_err());
            assert_eq!(rows(&mut c).await, [row("2", "kept", None)], "row 1 was rolled back");
        });
    }

    #[test]
    fn import_should_skip_or_update_duplicates_when_asked() {
        tauri::async_runtime::block_on(async {
            let mut c = db().await;
            c.query("insert into people values (2, 'old', 'n')").await.unwrap();
            let p = file("upsert.csv", "1,a\n2,b\n");
            let skip = Import {
                conflict: Conflict::Skip,
                ..import(p.clone(), FileFormat::Csv(csv(false)), &[Some("id"), Some("name")])
            };
            super::run(&mut c, &skip, &Job::quiet()).await.unwrap();
            assert_eq!(rows(&mut c).await, [row("1", "a", None), row("2", "old", Some("n"))]);
            let update = Import { conflict: Conflict::Update, ..skip };
            super::run(&mut c, &update, &Job::quiet()).await.unwrap();
            assert_eq!(rows(&mut c).await[1], row("2", "b", Some("n")), "only imported columns change");
        });
    }

    #[test]
    fn import_should_create_a_table_of_text_columns() {
        tauri::async_runtime::block_on(async {
            let mut c = db().await;
            let p = file("new.csv", "a b,c\n1,2\n");
            let imp = Import {
                table: "fresh".into(),
                create: true,
                ..import(p, FileFormat::Csv(csv(true)), &[Some("a b"), Some("c")])
            };
            super::run(&mut c, &imp, &Job::quiet()).await.unwrap();
            let r = c.query("select \"a b\", typeof(c) from fresh").await.unwrap();
            assert_eq!(r.rows[0], [Some("1".into()), Some("text".into())]);
        });
    }

    #[test]
    fn import_should_refuse_unknown_or_repeated_columns() {
        tauri::async_runtime::block_on(async {
            let mut c = db().await;
            let p = file("cols.csv", "1,2\n");
            let unknown = import(p.clone(), FileFormat::Csv(csv(false)), &[Some("id"), Some("nope")]);
            let err = super::run(&mut c, &unknown, &Job::quiet()).await.unwrap_err();
            assert_eq!(err.to_string(), "a field is matched to a column the table doesn't have");
            let twice = import(p, FileFormat::Csv(csv(false)), &[Some("id"), Some("id")]);
            let err = super::run(&mut c, &twice, &Job::quiet()).await.unwrap_err();
            assert_eq!(err.to_string(), "two fields go into the same column");
        });
    }

    #[test]
    fn a_cancelled_import_should_keep_nothing() {
        tauri::async_runtime::block_on(async {
            let mut c = db().await;
            let job = Job::quiet();
            job.cancel();
            let imp = import(file("cancel.csv", "1,a\n"), FileFormat::Csv(csv(false)), &[Some("id"), Some("name")]);
            assert_eq!(super::run(&mut c, &imp, &job).await.unwrap_err().to_string(), "cancelled");
            assert_eq!(rows(&mut c).await, Vec::<Vec<Option<String>>>::new());
        });
    }

    #[test]
    fn json_import_should_take_keys_in_first_seen_order() {
        tauri::async_runtime::block_on(async {
            let mut c = db().await;
            let p = file("people.json", r#"[{"id": 1, "name": "Ada"}, {"name": "Bo", "id": 2, "note": {"x": true}}]"#);
            let pv = super::preview(&p, &FileFormat::Json).unwrap();
            assert_eq!(pv.fields, ["id", "name", "note"]);
            let imp = import(p, FileFormat::Json, &[Some("id"), Some("name"), Some("note")]);
            super::run(&mut c, &imp, &Job::quiet()).await.unwrap();
            assert_eq!(rows(&mut c).await, [row("1", "Ada", None), row("2", "Bo", Some(r#"{"x":true}"#))]);
        });
    }

    #[test]
    fn json_preview_should_refuse_anything_but_an_array_of_objects() {
        let err = super::preview(&file("bad.json", "[1, 2]"), &FileFormat::Json).unwrap_err();
        assert!(err.to_string().starts_with("not a JSON array of objects"), "{err}");
    }

    fn split(engine: Engine, sql: &str) -> Vec<String> {
        let mut s = Splitter::new(engine);
        let mut out: Vec<String> = sql.split_inclusive('\n').flat_map(|l| s.feed(l).unwrap()).collect();
        out.extend(s.finish());
        out
    }

    #[test]
    fn splitter_should_ignore_semicolons_in_strings_comments_and_dollar_quotes() {
        let sql = "select ';' -- a;b\n; /* x; */ select 2;\ncreate function f() returns int as $body$ select 1; $body$ language sql;\nselect 3";
        assert_eq!(
            split(Engine::Postgres, sql),
            [
                "select ';' -- a;b",
                "/* x; */ select 2",
                "create function f() returns int as $body$ select 1; $body$ language sql",
                "select 3"
            ]
        );
    }

    #[test]
    fn splitter_should_follow_mysql_quoting_rules() {
        let sql = "insert into t values ('it\\'s; fine', \"a;b\"); # c;\nselect `x;y` from t;";
        assert_eq!(
            split(Engine::Mysql, sql),
            ["insert into t values ('it\\'s; fine', \"a;b\")", "# c;\nselect `x;y` from t"]
        );
    }

    #[test]
    fn splitter_should_drop_comment_only_statements_and_keep_strings_across_lines() {
        assert_eq!(split(Engine::Sqlite, "-- only a comment\n;\nselect 'a\n;b';\n"), ["select 'a\n;b'"]);
    }

    #[test]
    fn dollar_tags_should_not_match_parameters() {
        assert_eq!(super::dollar_tag("$$ x"), Some("$$"));
        assert_eq!(super::dollar_tag("$fn$ x"), Some("$fn$"));
        assert_eq!(super::dollar_tag("$1 + $2"), None);
    }

    #[test]
    fn sql_import_should_run_each_statement_and_name_the_one_that_fails() {
        tauri::async_runtime::block_on(async {
            let mut c = db().await;
            let p = file(
                "ok.sql",
                "insert into people values (1, 'a;b', null);\ninsert into people values (2, 'c', null)\n",
            );
            assert_eq!(super::run_sql(&mut c, Engine::Sqlite, &p, &Job::quiet()).await.unwrap(), 2);
            assert_eq!(rows(&mut c).await.len(), 2);
            let p = file("bad.sql", "select 1;\nselec 2;\nselect 3;");
            let err = super::run_sql(&mut c, Engine::Sqlite, &p, &Job::quiet()).await.unwrap_err();
            assert!(err.to_string().starts_with("statement 2: "), "{err}");
        });
    }

    /// Creates and drops a scratch database on the local MySQL.
    #[test]
    #[ignore = "needs a local MySQL; creates and drops a scratch database"]
    fn local_mysql_import_should_upsert_and_roll_back() {
        tauri::async_runtime::block_on(async {
            let url = "mysql://root@127.0.0.1:3306";
            let mut c = Conn::open(Engine::Mysql, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let db = format!("dbm_import_{}", std::process::id());
            c.query(&format!("create database {db}")).await.unwrap();
            c.use_database(&db).await.unwrap();
            let result = async {
                c.query("create table people(id int primary key, name varchar(20), note text)").await?;
                c.query("insert into people values (2, 'old', 'n')").await?;
                let p = file("my.csv", "1,a\\b\n2,b\n");
                let cols = [Some("id"), Some("name")];
                let update =
                    Import { conflict: Conflict::Update, ..import(p.clone(), FileFormat::Csv(csv(false)), &cols) };
                super::run(&mut c, &update, &Job::quiet()).await?;
                let stop = import(p, FileFormat::Csv(csv(false)), &cols);
                let failed = super::run(&mut c, &stop, &Job::quiet()).await.is_err();
                Ok::<_, crate::error::Error>((failed, c.query("select id, name, note from people order by id").await?))
            }
            .await;
            c.query(&format!("drop database {db}")).await.unwrap();
            let (failed, r) = result.unwrap();
            assert!(failed, "duplicates stop a plain insert");
            assert_eq!(r.rows, [row("1", "a\\b", None), row("2", "b", Some("n"))]);
        });
    }
}

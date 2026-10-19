//! libSQL (`sqld`, Turso) over Hrana 3 over HTTP: JSON requests to `/v3/pipeline`. The server is SQLite, so
//! everything above the driver is SQLite's ([`Dialect::Lite`]).
//!
//! Each pipeline asks `get_autocommit` last. Outside a transaction the stream is closed
//! with it, so nothing waits on the server between calls; inside one, the baton the server hands back keeps the
//! stream, and with it the transaction, for the next call.

use super::http::{Endpoint, bearer, https_connector};
use super::{Driver, Results, RowSink, Stopper, first_word, only_comments};
use crate::connect::tls::{Tls, TlsMode};
use crate::error::{Error, Result};
use crate::sql::db::{Dialect, Options};
use async_trait::async_trait;
use hyper::header;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use std::any::Any;
use std::sync::Arc;
use tokio::sync::Notify;

/// The most an answer may hold. The server sends a whole result at once, so this bounds what one query can return.
const MAX_ANSWER: usize = 512 * 1024 * 1024;
/// `sqld` refuses `pragma query_only`, so read-only rests on the app's own parse of the SQL, as for SQL Server.
/// These are comments, which `run` drops.
const READ_ONLY_ON: &str = "-- read-only on";
const READ_ONLY_OFF: &str = "-- read-only off";

pub struct Libsql {
    endpoint: Endpoint,
    auth: Option<header::HeaderValue>,
    /// The open stream, while a transaction needs it.
    baton: Option<String>,
    cancel: Arc<Notify>,
}

/// Splits `sql` into its statements at the semicolons outside quotes, quoted names and comments, keeping a
/// `CREATE TRIGGER` whole up to its `END`. The server runs one statement per step.
pub fn split_statements(sql: &str) -> Vec<&str> {
    let b = sql.as_bytes();
    let (mut out, mut start, mut i) = (vec![], 0, 0);
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"' | b'`' | b'[') => {
                let close = if q == b'[' { b']' } else { q };
                i += 1;
                // A doubled quote stays inside the string; `[…]` names have no escape.
                while i < b.len() && !(b[i] == close && (q == b'[' || b.get(i + 1) != Some(&close))) {
                    i += if b[i] == close { 2 } else { 1 };
                }
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b';' if !inside_trigger(&sql[start..i]) => {
                out.push(&sql[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&sql[start.min(sql.len())..]);
    out.into_iter().filter(|s| !only_comments(s)).collect()
}

/// Whether `so_far` is a `CREATE TRIGGER` whose body hasn't reached its `END` yet.
fn inside_trigger(so_far: &str) -> bool {
    if first_word(so_far) != "create" {
        return false;
    }
    let words: Vec<String> = so_far.split_whitespace().take(3).map(str::to_ascii_lowercase).collect();
    let trigger = words.get(1).map(String::as_str) == Some("trigger")
        || (matches!(words.get(1).map(String::as_str), Some("temp" | "temporary"))
            && words.get(2).map(String::as_str) == Some("trigger"));
    let end = so_far.trim_end();
    let ended = end.len() >= 3
        && end[end.len() - 3..].eq_ignore_ascii_case("end")
        && !end[..end.len() - 3].ends_with(|c: char| c.is_alphanumeric() || c == '_');
    trigger && !ended
}

/// A Hrana value as a cell: text as is, numbers as SQLite prints them, a blob by its size (as the SQLite driver
/// shows it).
fn cell(v: &Value) -> Option<String> {
    match v.get("type")?.as_str()? {
        "integer" | "text" => v.get("value")?.as_str().map(String::from),
        "float" => v.get("value")?.as_f64().map(|f| f.to_string()),
        "blob" => {
            let encoded = v.get("base64").and_then(Value::as_str).unwrap_or_default().trim_end_matches('=');
            Some(format!("<{} bytes>", encoded.len() * 3 / 4))
        }
        _ => None,
    }
}

/// `sql` with the app's `$n` placeholders written as SQLite's positional `?n`: the server reads `$1` as a named
/// parameter, and takes positional values only for positional ones.
fn positional(sql: &str) -> String {
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let (mut from, mut i) = (0, 0);
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"' | b'`' | b'[') => {
                let close = if q == b'[' { b']' } else { q };
                i += 1;
                while i < b.len() && b[i] != close {
                    i += 1;
                }
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'$' if b.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                out.push_str(&sql[from..i]);
                out.push('?');
                from = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push_str(&sql[from.min(sql.len())..]);
    out
}

fn statement(sql: &str, params: &[Option<&str>]) -> Value {
    let args: Vec<Value> = params
        .iter()
        .map(|p| match p {
            Some(v) => json!({ "type": "text", "value": v }),
            None => json!({ "type": "null" }),
        })
        .collect();
    let sql = if params.is_empty() { sql.to_string() } else { positional(sql) };
    json!({ "sql": sql, "args": args, "want_rows": true })
}

/// One statement's outcome: its columns (none for a statement that returns no rows), rows and rows changed.
struct Outcome {
    columns: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
    affected: u64,
}

fn outcome(result: &Value) -> Outcome {
    let columns = result
        .get("cols")
        .and_then(Value::as_array)
        .map(|c| c.iter().map(|c| c.get("name").and_then(Value::as_str).unwrap_or_default().to_string()).collect())
        .unwrap_or_default();
    let rows = result
        .get("rows")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter().map(|r| r.as_array().map(|r| r.iter().map(cell).collect()).unwrap_or_default()).collect()
        })
        .unwrap_or_default();
    let affected = result.get("affected_row_count").and_then(Value::as_u64).unwrap_or(0);
    Outcome { columns, rows, affected }
}

/// The server's error message from a failed request or step.
fn message(error: &Value) -> String {
    error.get("message").and_then(Value::as_str).unwrap_or("the server refused the request").to_string()
}

impl Libsql {
    /// Opens `libsql://host:port` with `token` as the bearer token, if any. The TLS setting decides between HTTPS and
    /// plain HTTP; Prefer (the default) is HTTPS, except to a server on this computer, which `sqld` serves plain.
    pub async fn open(
        url: &str,
        token: Option<&SecretString>,
        tls: &Tls,
        _options: &Options,
        tunnel_port: Option<u16>,
    ) -> Result<Libsql> {
        let u = url::Url::parse(url)?;
        let host = u.host_str().unwrap_or("localhost");
        let local = matches!(host, "127.0.0.1" | "localhost" | "[::1]");
        let tls =
            if local && tls.mode == TlsMode::Prefer && tunnel_port.is_none() { None } else { https_connector(tls)? };
        let port = u.port().unwrap_or(if tls.is_some() { 443 } else { 8080 });
        let endpoint = Endpoint::new("libSQL", host, port, tls, tunnel_port);
        let auth = token.map(ExposeSecret::expose_secret).filter(|t| !t.is_empty()).map(bearer).transpose()?;
        let mut c = Libsql { endpoint, auth, baton: None, cancel: Arc::new(Notify::new()) };
        // Nothing connects until a request: say now whether the server and the token are right.
        c.ping().await?;
        Ok(c)
    }

    /// Sends `requests` with `get_autocommit` after them on the current stream, keeps the stream only while it's
    /// needed, and returns each request's result (`Err` with the server's message for one that failed).
    async fn pipeline(&mut self, requests: Vec<Value>) -> Result<Vec<std::result::Result<Value, String>>> {
        let n = requests.len();
        let mut all = requests;
        all.push(json!({ "type": "get_autocommit" }));
        let body = json!({ "baton": self.baton, "requests": all }).to_string().into_bytes();
        let headers: Vec<_> = self.auth.iter().map(|a| (header::AUTHORIZATION, a.clone())).collect();
        let cancel = self.cancel.clone();
        let send = self.endpoint.post("/v3/pipeline", &headers, body, MAX_ANSWER);
        let (status, bytes) = tokio::select! {
            r = send => r?,
            () = cancel.notified() => {
                self.baton = None; // the stream may be mid-statement; a fresh one starts next time
                return Err(Error::Cancelled);
            }
        };
        if !(200..300).contains(&status) {
            self.baton = None;
            let text = String::from_utf8_lossy(&bytes);
            let text = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("message")?.as_str().map(String::from))
                .unwrap_or_else(|| text.trim().chars().take(500).collect());
            return Err(self.endpoint.error(match status {
                401 | 403 => format!("the server refused the token ({status}): {text}"),
                _ => format!("{status}: {text}"),
            }));
        }
        let answer: Value =
            serde_json::from_slice(&bytes).map_err(|_| self.endpoint.error("the answer wasn't JSON"))?;
        let results = answer.get("results").and_then(Value::as_array).cloned().unwrap_or_default();
        let autocommit =
            results.get(n).and_then(|r| r.get("response")?.get("is_autocommit")?.as_bool()).unwrap_or(true);
        self.baton = answer.get("baton").and_then(Value::as_str).map(String::from);
        if autocommit {
            self.close().await;
        }
        Ok(results
            .into_iter()
            .take(n)
            .map(|r| match r.get("type").and_then(Value::as_str) {
                Some("ok") => Ok(r.get("response").cloned().unwrap_or_default()),
                _ => Err(r.get("error").map_or_else(|| "the server sent no answer".into(), message)),
            })
            .collect())
    }

    /// Closes the stream, if one is open. Best effort: the server drops an idle stream on its own.
    async fn close(&mut self) {
        let Some(baton) = self.baton.take() else { return };
        let headers: Vec<_> = self.auth.iter().map(|a| (header::AUTHORIZATION, a.clone())).collect();
        let body = json!({ "baton": baton, "requests": [{ "type": "close" }] }).to_string().into_bytes();
        let _ = self.endpoint.post("/v3/pipeline", &headers, body, 64 * 1024).await; // see above
    }

    /// Runs one statement and returns what it did.
    async fn execute(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Outcome> {
        let request = json!({ "type": "execute", "stmt": statement(sql, params) });
        let response = self.pipeline(vec![request]).await?.pop().unwrap_or_else(|| Err("no answer".into()));
        let response = response.map_err(|m| self.endpoint.error(m))?;
        Ok(outcome(response.get("result").unwrap_or(&Value::Null)))
    }
}

#[async_trait]
impl Driver for Libsql {
    fn dialect(&self) -> Dialect {
        Dialect::Lite
    }

    async fn batch(&mut self, sql: &str) -> Result<()> {
        let response = self.pipeline(vec![json!({ "type": "sequence", "sql": sql })]).await?.pop();
        response.unwrap_or_else(|| Err("no answer".into())).map(|_| ()).map_err(|m| self.endpoint.error(m))
    }

    async fn run(&mut self, sql: &str, out: &mut Results) -> Result<()> {
        let mut steps = Vec::new();
        for s in split_statements(sql) {
            // `sqld` refuses this pragma, which a SQLite table rebuild sets around its rename so views naming the table
            // aren't checked while it's gone. Without it such a rebuild fails, and its transaction rolls back.
            if s.trim().to_ascii_lowercase().starts_with("pragma legacy_alter_table") {
                continue;
            }
            // Each statement runs only when the one before it succeeded, as a script stops at its first error.
            let condition = steps.len().checked_sub(1).map(|prev| json!({ "type": "ok", "step": prev }));
            steps.push(json!({ "stmt": statement(s, &[]), "condition": condition }));
        }
        if steps.is_empty() {
            return Ok(());
        }
        let count = steps.len();
        let response = self.pipeline(vec![json!({ "type": "batch", "batch": { "steps": steps } })]).await?.pop();
        let response = response.unwrap_or_else(|| Err("no answer".into())).map_err(|m| self.endpoint.error(m))?;
        let result = response.get("result").unwrap_or(&Value::Null);
        let results = result.get("step_results").and_then(Value::as_array).cloned().unwrap_or_default();
        let errors = result.get("step_errors").and_then(Value::as_array).cloned().unwrap_or_default();
        for i in 0..count {
            if let Some(e) = errors.get(i).filter(|e| !e.is_null()) {
                let error = self.endpoint.error(message(e));
                return Err(if count > 1 {
                    Error::Statement { n: i as u64 + 1, source: Box::new(error) }
                } else {
                    error
                });
            }
            let Some(step) = results.get(i).filter(|r| !r.is_null()) else { break };
            let o = outcome(step);
            if o.columns.is_empty() {
                out.begin(vec![]);
                out.end(o.affected);
                continue;
            }
            out.begin(o.columns);
            for row in o.rows {
                if !out.keep() {
                    break;
                }
                out.push(row);
            }
            out.end(0);
        }
        Ok(())
    }

    async fn execute_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<u64> {
        Ok(self.execute(sql, params).await?.affected)
    }

    async fn fetch_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Vec<Vec<Option<String>>>> {
        Ok(self.execute(sql, params).await?.rows)
    }

    async fn stream(&mut self, sql: &str, params: &[Option<&str>], sink: &mut dyn RowSink) -> Result<()> {
        let o = self.execute(sql, params).await?;
        sink.columns(o.columns)?;
        for row in o.rows {
            sink.row(row)?;
        }
        Ok(())
    }

    async fn ping(&mut self) -> Result<()> {
        self.execute("select 1", &[]).await.map(|_| ())
    }

    fn stopper(&self) -> Stopper {
        Stopper::Notify(self.cancel.clone())
    }

    fn read_only_bounds(&self) -> (&'static str, &'static str) {
        (READ_ONLY_ON, READ_ONLY_OFF)
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{cell, positional, split_statements};
    use crate::connect::tls::{Tls, TlsMode};
    use crate::sql::db::{Conn, Engine, Options};
    use serde_json::json;

    async fn local() -> Conn {
        let tls = Tls { mode: TlsMode::Disable, ..Tls::default() };
        Conn::open(Engine::Libsql, "libsql://127.0.0.1:8080", None, &tls, &Options::default(), None).await.unwrap()
    }

    #[test]
    #[ignore = "needs a local libSQL server on 8080 (scripts/test-env.sh up); replaces its table t"]
    fn local_libsql_should_run_scripts_bind_values_and_keep_a_transaction_across_calls() {
        tauri::async_runtime::block_on(async {
            let mut c = local().await;
            let r = c
                .query_results(
                    "drop table if exists t; create table t(id integer primary key, s text, f real, b blob); \
                     insert into t values (1, 'a;b', 2.5, x'0102'), (2, null, 3, null); select * from t order by id",
                    None,
                )
                .await
                .unwrap();
            assert_eq!(r.len(), 4);
            assert_eq!(r[2].affected, 2, "the insert's count");
            assert_eq!(
                r[3].rows[0],
                [Some("1".into()), Some("a;b".into()), Some("2.5".into()), Some("<2 bytes>".into())]
            );
            assert_eq!(r[3].rows[1][2].as_deref(), Some("3"), "as the SQLite driver prints a whole REAL");
            assert_eq!(
                c.execute_with("update t set s = $1 where id = $2", &[Some("it's"), Some("2")]).await.unwrap(),
                1
            );
            assert_eq!(
                c.fetch_with("select s from t where id = $1", &[Some("2")]).await.unwrap(),
                [[Some("it's".into())]]
            );

            c.query("begin").await.unwrap();
            c.query("insert into t(id) values (3)").await.unwrap();
            c.query("rollback").await.unwrap();
            assert_eq!(c.query("select count(*) from t").await.unwrap().rows[0][0].as_deref(), Some("2"));

            let e = c.query("select 1; selec 2").await.unwrap_err().to_string();
            assert!(e.contains("syntax error"), "{e}");
            assert!(c.tables(false).await.unwrap().contains(&"t".to_string()));
            assert!(c.server_version().await.unwrap().starts_with("SQLite 3."));
        });
    }

    #[test]
    fn positional_should_rewrite_placeholders_outside_strings_and_comments() {
        assert_eq!(
            positional("select $1, '$2', \"$3\" -- $4\n from t where a = $10"),
            "select ?1, '$2', \"$3\" -- $4\n from t where a = ?10"
        );
    }

    #[test]
    fn split_statements_should_keep_a_trigger_body_whole() {
        let sql = "create table t(a); CREATE TEMP TRIGGER tr AFTER INSERT ON t BEGIN select 1; select ';'; END; \
                   select [a;b], `c;`, \"d;\" from t; -- x;\n";
        assert_eq!(
            split_statements(sql),
            [
                "create table t(a)",
                " CREATE TEMP TRIGGER tr AFTER INSERT ON t BEGIN select 1; select ';'; END",
                " select [a;b], `c;`, \"d;\" from t",
            ]
        );
    }

    #[test]
    fn split_statements_should_not_end_a_trigger_at_a_word_ending_in_end() {
        let sql = "create trigger tr after insert on t begin insert into legend values (1); end; select 2";
        assert_eq!(split_statements(sql).len(), 2);
    }

    #[test]
    fn cell_should_read_each_hrana_type_as_the_sqlite_driver_shows_it() {
        assert_eq!(cell(&json!({ "type": "integer", "value": "42" })).as_deref(), Some("42"));
        assert_eq!(cell(&json!({ "type": "float", "value": 2.5 })).as_deref(), Some("2.5"));
        assert_eq!(cell(&json!({ "type": "text", "value": "x" })).as_deref(), Some("x"));
        assert_eq!(cell(&json!({ "type": "blob", "base64": "qrvM" })).as_deref(), Some("<3 bytes>"));
        assert_eq!(cell(&json!({ "type": "null" })), None);
    }
}

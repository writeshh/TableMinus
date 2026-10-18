//! Cloudflare D1 through Cloudflare's HTTP API (`…/d1/database/{id}/raw`). D1 is SQLite, so everything above the
//! driver is SQLite's ([`Dialect::Lite`]).
//!
//! Each request is a transaction of its own, and D1 refuses `BEGIN`: so a transaction the app opens is kept here
//! instead. From `begin` to `commit` statements are queued, then sent together as one batch, which D1 runs as one
//! transaction; `rollback` drops the queue, and nothing has reached the server.
// ponytail: a queued statement reports one row changed, so `Conn::save_rows` can't catch a key matching several rows
// here (on SQLite a key or rowid always finds one); send each one alone if that ever matters.

use super::http::{Endpoint, bearer, https_connector};
use super::libsql::split_statements;
use super::{Driver, Results, RowSink, Stopper, first_word};
use crate::db::Dialect;
use crate::error::{Error, Result};
use crate::tls::Tls;
use async_trait::async_trait;
use hyper::header;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use std::any::Any;
use std::sync::Arc;
use tokio::sync::Notify;

const API_HOST: &str = "api.cloudflare.com";
/// D1 answers a whole result at once; this bounds what one request may return.
const MAX_ANSWER: usize = 512 * 1024 * 1024;
/// D1 takes no `pragma query_only`: read-only rests on the app's own parse of the SQL, as for SQL Server. These are
/// comments, which `run` drops.
const READ_ONLY_ON: &str = "-- read-only on";
const READ_ONLY_OFF: &str = "-- read-only off";

pub struct D1 {
    endpoint: Endpoint,
    path: String,
    auth: header::HeaderValue,
    /// Statements queued since `begin`, sent at `commit`.
    queued: Option<Vec<Value>>,
    cancel: Arc<Notify>,
}

/// A D1 value as a cell: numbers as SQLite prints them, a blob (an array of bytes) by its size.
fn cell(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) if n.is_f64() => n.as_f64().map(|f| f.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(i32::from(*b).to_string()),
        Value::Array(bytes) => Some(format!("<{} bytes>", bytes.len())),
        Value::Object(_) => Some(v.to_string()),
    }
}

fn query(sql: &str, params: &[Option<&str>]) -> Value {
    json!({ "sql": sql, "params": params })
}

/// One statement's outcome: its columns (none when it returns no rows), rows, and rows changed.
struct Outcome {
    columns: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
    affected: u64,
}

fn outcome(result: &Value) -> Outcome {
    let results = result.get("results").unwrap_or(&Value::Null);
    let columns: Vec<String> = results
        .get("columns")
        .and_then(Value::as_array)
        .map(|c| c.iter().map(|c| c.as_str().unwrap_or_default().to_string()).collect())
        .unwrap_or_default();
    let rows = results
        .get("rows")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter().map(|r| r.as_array().map(|r| r.iter().map(cell).collect()).unwrap_or_default()).collect()
        })
        .unwrap_or_default();
    let affected = if columns.is_empty() {
        result.get("meta").and_then(|m| m.get("changes")).and_then(Value::as_u64).unwrap_or(0)
    } else {
        0
    };
    Outcome { columns, rows, affected }
}

/// Cloudflare's error messages from an answer, joined.
fn errors(answer: &Value) -> Option<String> {
    let list = answer.get("errors")?.as_array()?;
    let text: Vec<&str> = list.iter().filter_map(|e| e.get("message")?.as_str()).collect();
    (!text.is_empty()).then(|| text.join("; "))
}

impl D1 {
    /// Opens `d1://{account id}/{database id}` with `token`, an API token allowed to edit D1.
    pub async fn open(url: &str, token: Option<&SecretString>, tls: &Tls) -> Result<D1> {
        let u = url::Url::parse(url)?;
        let account =
            u.host_str().filter(|a| !a.is_empty()).ok_or(Error::Invalid("enter the Cloudflare account ID"))?;
        let database = u.path().trim_matches('/');
        let ids_ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !ids_ok(account) || !ids_ok(database) {
            return Err(Error::Invalid("the account ID and database ID are letters, digits and dashes"));
        }
        let token = token
            .map(ExposeSecret::expose_secret)
            .filter(|t| !t.is_empty())
            .ok_or(Error::Invalid("enter a Cloudflare API token that may edit D1"))?;
        let endpoint = Endpoint::new("Cloudflare D1", API_HOST, 443, https_connector(tls)?, None);
        let path = format!("/client/v4/accounts/{account}/d1/database/{database}/raw");
        let mut c = D1 { endpoint, path, auth: bearer(token)?, queued: None, cancel: Arc::new(Notify::new()) };
        c.ping().await?;
        Ok(c)
    }

    /// Sends one request (`{sql, params}` or `{batch}`) and returns one result per statement.
    async fn send(&self, body: &Value) -> Result<Vec<Value>> {
        let headers = [(header::AUTHORIZATION, self.auth.clone())];
        let send = self.endpoint.post(&self.path, &headers, body.to_string().into_bytes(), MAX_ANSWER);
        let cancel = self.cancel.clone();
        let (status, bytes) = tokio::select! {
            r = send => r?,
            () = cancel.notified() => return Err(Error::Cancelled),
        };
        let answer: Value = serde_json::from_slice(&bytes).map_err(|_| {
            let text: String = String::from_utf8_lossy(&bytes).trim().chars().take(300).collect();
            self.endpoint.error(format!("{status}: {text}"))
        })?;
        if !(200..300).contains(&status) || answer.get("success") == Some(&Value::Bool(false)) {
            let text = errors(&answer).unwrap_or_else(|| format!("the request failed ({status})"));
            return Err(self.endpoint.error(match status {
                401 | 403 => format!("the API token was refused ({status}): {text}"),
                _ => text,
            }));
        }
        Ok(answer.get("result").and_then(Value::as_array).cloned().unwrap_or_default())
    }

    /// Runs `statements` (each `{sql, params}`) now, or queues them while a transaction is open.
    async fn execute(&mut self, statements: Vec<Value>) -> Result<Vec<Outcome>> {
        if let Some(queue) = &mut self.queued {
            let n = statements.len();
            queue.extend(statements);
            // Each reports one row changed: it runs at commit.
            return Ok((0..n).map(|_| Outcome { columns: vec![], rows: vec![], affected: 1 }).collect());
        }
        let body = match <[Value; 1]>::try_from(statements) {
            Ok([one]) => one,
            Err(many) => json!({ "batch": many }),
        };
        Ok(self.send(&body).await?.iter().map(outcome).collect())
    }

    /// Handles the transaction statements the app sends; false for anything else.
    async fn transaction(&mut self, sql: &str) -> Result<bool> {
        let words: Vec<String> = sql.split_whitespace().map(str::to_ascii_lowercase).collect();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["begin"] | ["begin", "transaction"] | ["begin", "deferred" | "immediate" | "exclusive", ..] => {
                if self.queued.is_some() {
                    return Err(Error::Invalid("a transaction is already open"));
                }
                self.queued = Some(vec![]);
            }
            ["commit" | "end"] | ["commit" | "end", "transaction"] => {
                let queued = self.queued.take().unwrap_or_default();
                if !queued.is_empty() {
                    self.send(&json!({ "batch": queued })).await?;
                }
            }
            ["rollback"] | ["rollback", "transaction"] => self.queued = None,
            _ => return Ok(false),
        }
        Ok(true)
    }
}

#[async_trait]
impl Driver for D1 {
    fn dialect(&self) -> Dialect {
        Dialect::Lite
    }

    async fn batch(&mut self, sql: &str) -> Result<()> {
        self.run(sql, &mut Results::new(Some(0))).await
    }

    async fn run(&mut self, sql: &str, out: &mut Results) -> Result<()> {
        let mut pending = Vec::new();
        for s in split_statements(sql) {
            if matches!(first_word(s).as_str(), "begin" | "commit" | "end" | "rollback") {
                // Statements before it run first, as they would in order.
                if !pending.is_empty() {
                    show(out, self.execute(std::mem::take(&mut pending)).await?);
                }
                if self.transaction(s).await? {
                    out.begin(vec![]);
                    out.end(0);
                    continue;
                }
            }
            pending.push(query(s, &[]));
        }
        if !pending.is_empty() {
            show(out, self.execute(pending).await?);
        }
        Ok(())
    }

    async fn execute_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<u64> {
        Ok(self.execute(vec![query(sql, params)]).await?.first().map_or(0, |o| o.affected))
    }

    async fn fetch_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Vec<Vec<Option<String>>>> {
        Ok(self.execute(vec![query(sql, params)]).await?.into_iter().next().map(|o| o.rows).unwrap_or_default())
    }

    async fn stream(&mut self, sql: &str, params: &[Option<&str>], sink: &mut dyn RowSink) -> Result<()> {
        let o = self.execute(vec![query(sql, params)]).await?.into_iter().next();
        let o = o.unwrap_or(Outcome { columns: vec![], rows: vec![], affected: 0 });
        sink.columns(o.columns)?;
        for row in o.rows {
            sink.row(row)?;
        }
        Ok(())
    }

    async fn ping(&mut self) -> Result<()> {
        self.send(&query("select 1", &[])).await.map(|_| ())
    }

    fn stopper(&self) -> Stopper {
        Stopper::Notify(self.cancel.clone())
    }

    fn read_only_bounds(&self) -> (&'static str, &'static str) {
        (READ_ONLY_ON, READ_ONLY_OFF)
    }

    // D1 caps a statement at 100 bound parameters.
    fn max_params(&self) -> usize {
        100
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// Adds the statements' outcomes to `out`.
fn show(out: &mut Results, outcomes: Vec<Outcome>) {
    for o in outcomes {
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
}

#[cfg(test)]
mod tests {
    use super::{D1, Endpoint, cell, errors, outcome};
    use crate::driver::{Driver, Results};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Notify;

    /// A server that answers every request with one successful result per statement, and hands back the bodies.
    async fn fake_d1() -> (u16, tokio::sync::mpsc::UnboundedReceiver<Value>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tauri::async_runtime::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let body = loop {
                    let n = s.read(&mut chunk).await.unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let Some((head, body)) = text.split_once("\r\n\r\n") else { continue };
                    let len: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())
                        })
                        .unwrap();
                    if body.len() >= len {
                        break serde_json::from_str::<Value>(body).unwrap();
                    }
                };
                let n = body.get("batch").and_then(Value::as_array).map_or(1, Vec::len);
                let one =
                    json!({ "success": true, "results": { "columns": [], "rows": [] }, "meta": { "changes": 1 } });
                let answer = json!({ "success": true, "errors": [], "result": vec![one; n] }).to_string();
                tx.send(body).unwrap();
                let reply = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{answer}", answer.len());
                s.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        (port, rx)
    }

    #[test]
    fn a_transaction_should_reach_d1_as_one_batch_at_commit_and_not_at_all_on_rollback() {
        tauri::async_runtime::block_on(async {
            let (port, mut seen) = fake_d1().await;
            let mut c = D1 {
                endpoint: Endpoint::new("Cloudflare D1", "127.0.0.1", port, None, None),
                path: "/raw".into(),
                auth: super::bearer("t").unwrap(),
                queued: None,
                cancel: Arc::new(Notify::new()),
            };
            c.run("begin transaction", &mut Results::new(None)).await.unwrap();
            assert_eq!(c.execute_with("update t set a = $1", &[Some("x")]).await.unwrap(), 1);
            c.run("delete from t; insert into t values (1)", &mut Results::new(None)).await.unwrap();
            assert!(seen.try_recv().is_err(), "nothing sent before commit");
            c.run("commit", &mut Results::new(None)).await.unwrap();
            let batch = seen.recv().await.unwrap();
            let sql: Vec<&str> =
                batch["batch"].as_array().unwrap().iter().map(|q| q["sql"].as_str().unwrap()).collect();
            assert_eq!(sql, ["update t set a = $1", "delete from t", " insert into t values (1)"]);
            assert_eq!(batch["batch"][0]["params"], json!(["x"]));

            c.run("begin", &mut Results::new(None)).await.unwrap();
            c.execute_with("delete from t", &[]).await.unwrap();
            c.run("rollback", &mut Results::new(None)).await.unwrap();
            c.execute_with("select 1", &[]).await.unwrap();
            assert_eq!(seen.recv().await.unwrap()["sql"], "select 1", "the rolled-back delete was never sent");
        });
    }

    #[test]
    fn outcome_should_read_rows_and_changes_from_a_raw_result() {
        let rows = outcome(
            &json!({ "results": { "columns": ["a", "b"], "rows": [[1, "x"], [2.5, null]] }, "meta": { "changes": 0 } }),
        );
        assert_eq!(rows.columns, ["a", "b"]);
        assert_eq!(rows.rows, [[Some("1".into()), Some("x".into())], [Some("2.5".into()), None]]);
        let write = outcome(&json!({ "results": { "columns": [], "rows": [] }, "meta": { "changes": 3 } }));
        assert_eq!(write.affected, 3);
    }

    #[test]
    fn cell_should_show_numbers_as_sqlite_does_and_blobs_by_size() {
        assert_eq!(cell(&json!(3.0)).as_deref(), Some("3"));
        assert_eq!(cell(&json!(7)).as_deref(), Some("7"));
        assert_eq!(cell(&json!([1, 2, 3])).as_deref(), Some("<3 bytes>"));
    }

    #[test]
    fn errors_should_join_cloudflares_messages() {
        let answer = json!({ "success": false, "errors": [{ "code": 7500, "message": "no such table: t" }] });
        assert_eq!(errors(&answer).as_deref(), Some("no such table: t"));
    }
}

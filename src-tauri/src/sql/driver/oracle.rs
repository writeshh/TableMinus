//! Oracle through the `oracle` crate (ODPI-C), which loads the user's Oracle Instant Client when the first Oracle
//! connection opens: the app builds and runs without it. Its calls block, so each runs in `spawn_blocking` and sends
//! its rows back over a channel. Oracle has no `BEGIN TRANSACTION`: each statement is committed after it runs unless
//! the app opened a transaction (its `begin transaction`, kept here), which `commit` and `rollback` end. The
//! session's date and number formats are set at connect, so values read as text go back in as text unchanged.

use super::{Driver, Results, RowSink, Stopper, first_word, only_comments};
use crate::connect::tls::{Tls, TlsMode};
use crate::error::{Error, Result};
use crate::sql::db::{Dialect, Options};
use async_trait::async_trait;
use oracle::{Connection, Connector, InitParams, sql_type::ToSql};
use secrecy::{ExposeSecret, SecretString};
use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;

/// Rows fetched per round trip.
const FETCH: u32 = 500;
/// How dates and timestamps read and are written as text, in every session.
const FORMATS: &str = "ALTER SESSION SET NLS_DATE_FORMAT = 'YYYY-MM-DD HH24:MI:SS' \
     NLS_TIMESTAMP_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF6' NLS_TIMESTAMP_TZ_FORMAT = 'YYYY-MM-DD HH24:MI:SS.FF6 TZH:TZM' \
     NLS_NUMERIC_CHARACTERS = '.,'";
/// Where Instant Client's own installer puts it, and other usual places; the system's library path is tried first.
const CLIENT_DIRS: [&str; 4] = ["~/Downloads", "~", "/opt/oracle", "/usr/local/lib"];
const NO_CLIENT: &str = "Oracle connections need Oracle Instant Client (Basic or Basic Light, version 19 or later) from \
     oracle.com/database/technologies/instant-client. Put its folder (instantclient_…) in Downloads, your home folder or \
     /opt/oracle, or set ORACLE_CLIENT_LIB_DIR to it";

pub struct Oracle {
    conn: Arc<Connection>,
    /// A transaction the app opened; statements aren't committed one by one while it lasts.
    in_transaction: bool,
}

#[expect(clippy::needless_pass_by_value, reason = "it's handed to map_err, which passes the error by value")]
fn failed(e: oracle::Error) -> Error {
    Error::Oracle(e.to_string())
}

/// The newest `instantclient_*` folder in `dir`.
fn instant_client_in(dir: &str) -> Option<PathBuf> {
    let dir = crate::connect::ssh::expand_home(dir).ok()?;
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("instantclient")))
        .collect();
    found.sort();
    found.pop()
}

/// Loads Instant Client once: from `ORACLE_CLIENT_LIB_DIR`, the system's library path, or the usual folders.
fn load_client() -> Result<()> {
    static LOADED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    LOADED
        .get_or_init(|| {
            let dir = std::env::var_os("ORACLE_CLIENT_LIB_DIR")
                .map(PathBuf::from)
                .or_else(|| CLIENT_DIRS.iter().find_map(|d| instant_client_in(d)));
            let mut params = InitParams::new();
            if let Some(dir) = dir {
                params.oracle_client_lib_dir(dir).map_err(|e| e.to_string())?;
            }
            params.init().map(|_| ()).map_err(|e| {
                // DPI-1047: the library wasn't found or wouldn't load.
                if e.to_string().contains("DPI-1047") { format!("{NO_CLIENT}. ({e})") } else { e.to_string() }
            })
        })
        .clone()
        .map_err(Error::Oracle)
}

/// Splits a script into statements: at semicolons outside quotes and comments, except inside a PL/SQL block (an
/// anonymous block, or CREATE of a procedure, function, package, trigger or type), which ends at a line holding only
/// `/`, as SQL*Plus has it. A block keeps its own semicolons; other statements lose their last one.
pub fn split_statements(sql: &str) -> Vec<String> {
    let b = sql.as_bytes();
    let mut out: Vec<String> = vec![];
    let (mut start, mut i) = (0, 0);
    let push = |out: &mut Vec<String>, s: &str| {
        let s = s.trim();
        if !only_comments(s) {
            out.push(if is_block(s) { s.to_string() } else { s.trim_end_matches(';').trim_end().to_string() });
        }
    };
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < b.len() && b[i] != q {
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
            // A line of only `/` ends a block (or whatever comes before it).
            b'/' if line_is_slash(sql, i) => {
                push(&mut out, &sql[start..i]);
                start = i + 1;
            }
            b';' if !is_block(&sql[start..i]) => {
                push(&mut out, &sql[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    push(&mut out, &sql[start.min(sql.len())..]);
    out
}

fn line_is_slash(sql: &str, at: usize) -> bool {
    let line_start = sql[..at].rfind('\n').map_or(0, |n| n + 1);
    let line_end = sql[at..].find('\n').map_or(sql.len(), |n| at + n);
    sql[line_start..line_end].trim() == "/"
}

/// Whether `sql` starts a PL/SQL block, whose semicolons don't end it.
fn is_block(sql: &str) -> bool {
    let words: Vec<String> = sql.split_whitespace().take(5).map(str::to_ascii_lowercase).collect();
    let w = |i: usize| words.get(i).map_or("", String::as_str);
    match w(0) {
        "begin" | "declare" => w(1) != "transaction",
        "create" => {
            let mut k = 1;
            if w(k) == "or" && w(k + 1) == "replace" {
                k += 2;
            }
            if matches!(w(k), "editionable" | "noneditionable") {
                k += 1;
            }
            matches!(w(k), "procedure" | "function" | "package" | "trigger" | "type")
        }
        _ => false,
    }
}

/// What the blocking side of a statement hands back while it runs.
enum Msg {
    Columns(Vec<String>),
    Row(Vec<Option<String>>),
}

/// Runs one statement on this thread, sending its rows to `tx` until they end or the receiver goes away; commits a
/// statement that changed something when `commit`. Returns the rows changed.
fn execute(
    conn: &Connection,
    sql: &str,
    params: &[Option<String>],
    commit: bool,
    tx: &mpsc::Sender<Msg>,
) -> Result<Option<u64>> {
    let mut stmt = conn.statement(sql).fetch_array_size(FETCH).build().map_err(failed)?;
    let binds: Vec<&dyn ToSql> = params.iter().map(|p| p as &dyn ToSql).collect();
    if stmt.is_query() {
        let rows = stmt.query(&binds).map_err(failed)?;
        let names = rows.column_info().iter().map(|c| c.name().to_string()).collect();
        if tx.blocking_send(Msg::Columns(names)).is_err() {
            return Ok(None);
        }
        for row in rows {
            let row = row.map_err(failed)?;
            let mut cells = Vec::with_capacity(row.sql_values().len());
            for v in row.sql_values() {
                // Every type has a text form: numbers as written, RAW and BLOB as hex, dates by the formats above.
                cells.push(if v.is_null().map_err(failed)? { None } else { Some(v.get::<String>().map_err(failed)?) });
            }
            if tx.blocking_send(Msg::Row(cells)).is_err() {
                break;
            }
        }
        return Ok(None);
    }
    stmt.execute(&binds).map_err(failed)?;
    let changed = stmt.row_count().map_err(failed)?;
    if commit {
        conn.commit().map_err(failed)?;
    }
    Ok(Some(changed))
}

/// Runs `work` on a blocking thread.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(work).await.map_err(|e| Error::Oracle(e.to_string()))?
}

impl Oracle {
    /// Opens `oracle://user@host:port/service`.
    pub async fn open(
        url: &str,
        password: Option<&SecretString>,
        tls: &Tls,
        _options: &Options,
        tunnel_port: Option<u16>,
    ) -> Result<Oracle> {
        let u = url::Url::parse(url)?;
        let host = u.host_str().ok_or(Error::Invalid("enter the server's host"))?.to_string();
        let service = u.path().trim_matches('/').to_string();
        if service.is_empty() {
            return Err(Error::Invalid("enter the service name, e.g. FREEPDB1 or ORCLPDB1"));
        }
        let user = url::form_urlencoded::parse(format!("u={}", u.username()).as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        let (dial, port) = match tunnel_port {
            Some(p) => ("127.0.0.1".to_string(), p),
            None => (host, u.port().unwrap_or(1521)),
        };
        // Easy Connect Plus. TLS (tcps) checks the server against the system's certificates, or a wallet folder in
        // the CA field; the host name only for Verify Full.
        let mut options = vec![format!("connect_timeout={}", crate::sql::db::CONNECT_TIMEOUT.as_secs())];
        if !tls.cert.is_empty() || !tls.key.is_empty() {
            return Err(Error::Invalid(
                "Oracle reads client certificates from a wallet: leave Client cert and key empty and put the wallet's folder in SSL CA",
            ));
        }
        let protocol = if matches!(tls.mode, TlsMode::Disable | TlsMode::Prefer) {
            ""
        } else {
            options.push(format!("ssl_server_dn_match={}", if tls.mode == TlsMode::VerifyFull { "on" } else { "off" }));
            options.push(if tls.ca.is_empty() {
                "wallet_location=system".to_string()
            } else {
                format!("wallet_location={}", crate::connect::ssh::expand_home(&tls.ca)?.display())
            });
            "tcps://"
        };
        let connect = format!("{protocol}{dial}:{port}/{service}?{}", options.join("&"));
        let pw = password.map(|p| p.expose_secret().to_string()).unwrap_or_default();
        let conn = tokio::time::timeout(
            crate::sql::db::CONNECT_TIMEOUT + std::time::Duration::from_secs(5),
            blocking(move || {
                load_client()?;
                let mut connector = Connector::new(user, pw, connect);
                connector.driver_name("TableMinus");
                let conn = connector.connect().map_err(failed)?;
                conn.execute(FORMATS, &[]).map_err(failed)?;
                Ok(conn)
            }),
        )
        .await
        .map_err(|_| Error::Oracle("the server didn't answer in time".into()))??;
        Ok(Oracle { conn: Arc::new(conn), in_transaction: false })
    }

    /// Runs one statement and hands its rows to `on_row` (columns first); returns the rows it changed.
    async fn one(
        &mut self,
        sql: &str,
        params: &[Option<&str>],
        on_columns: &mut (dyn FnMut(Vec<String>) -> Result<()> + Send),
        on_row: &mut (dyn FnMut(Vec<Option<String>>) -> Result<bool> + Send),
    ) -> Result<Option<u64>> {
        let word = first_word(sql);
        let lower = sql.trim().to_ascii_lowercase();
        if matches!(lower.as_str(), "begin transaction" | "start transaction") {
            self.in_transaction = true;
            return Ok(None);
        }
        let conn = self.conn.clone();
        if word == "commit" || word == "rollback" {
            self.in_transaction = false;
            let commit = word == "commit";
            blocking(move || if commit { conn.commit() } else { conn.rollback() }.map_err(failed)).await?;
            return Ok(None);
        }
        if lower.starts_with("set transaction") {
            self.in_transaction = true; // read only lasts until the rollback that ends it
        }
        let (sql, commit) = (sql.to_string(), !self.in_transaction);
        let params: Vec<Option<String>> = params.iter().map(|p| p.map(String::from)).collect();
        let (tx, mut rx) = mpsc::channel(256);
        let task = tokio::task::spawn_blocking(move || execute(&conn, &sql, &params, commit, &tx));
        let mut handed = Ok(());
        while let Some(msg) = rx.recv().await {
            let more = match msg {
                Msg::Columns(names) => on_columns(names).map(|()| true),
                Msg::Row(row) => on_row(row),
            };
            match more {
                Ok(true) => {}
                Ok(false) => break,
                Err(e) => {
                    handed = Err(e);
                    break;
                }
            }
        }
        // The statement stops at its next row once nothing receives; wait for it so the connection is free again.
        drop(rx);
        let changed = task.await.map_err(|e| Error::Oracle(e.to_string()))??;
        handed.map(|()| changed)
    }
}

#[async_trait]
impl Driver for Oracle {
    fn dialect(&self) -> Dialect {
        Dialect::Ora
    }

    async fn batch(&mut self, sql: &str) -> Result<()> {
        self.run(sql, &mut Results::new(Some(0))).await
    }

    async fn run(&mut self, sql: &str, out: &mut Results) -> Result<()> {
        for statement in split_statements(sql) {
            // Both callbacks write to `out`, so they share it through a lock.
            let shared = std::sync::Mutex::new((&mut *out, false));
            let lock = || shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let changed = self
                .one(
                    &statement,
                    &[],
                    &mut |names| {
                        let mut g = lock();
                        g.1 = true;
                        g.0.begin(names);
                        Ok(())
                    },
                    &mut |row| {
                        let mut g = lock();
                        if g.0.keep() {
                            g.0.push(row);
                            Ok(true)
                        } else {
                            Ok(false)
                        }
                    },
                )
                .await?;
            let (out, began) = shared.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !began {
                out.begin(vec![]);
            }
            out.end(changed.unwrap_or(0));
        }
        Ok(())
    }

    async fn execute_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<u64> {
        Ok(self.one(sql, params, &mut |_| Ok(()), &mut |_| Ok(true)).await?.unwrap_or(0))
    }

    async fn fetch_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Vec<Vec<Option<String>>>> {
        let mut rows = vec![];
        self.one(sql, params, &mut |_| Ok(()), &mut |r| {
            rows.push(r);
            Ok(true)
        })
        .await?;
        Ok(rows)
    }

    async fn stream(&mut self, sql: &str, params: &[Option<&str>], sink: &mut dyn RowSink) -> Result<()> {
        let sink = std::sync::Mutex::new(sink);
        let lock = || sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.one(sql, params, &mut |names| lock().columns(names), &mut |row| lock().row(row).map(|()| true))
            .await
            .map(|_| ())
    }

    async fn ping(&mut self) -> Result<()> {
        let conn = self.conn.clone();
        blocking(move || conn.ping().map_err(failed)).await
    }

    fn stopper(&self) -> Stopper {
        Stopper::Ora(self.conn.clone())
    }

    fn lost_by(&self, _err: &Error) -> bool {
        // Reads the state the last call left; no round trip.
        !matches!(self.conn.status(), Ok(oracle::ConnStatus::Normal))
    }

    fn read_only_bounds(&self) -> (&'static str, &'static str) {
        ("SET TRANSACTION READ ONLY", "ROLLBACK")
    }

    async fn use_database(&mut self, db: &str) -> Result<()> {
        let (conn, db) = (self.conn.clone(), db.to_string());
        blocking(move || conn.set_current_schema(&db).map_err(failed)).await
    }

    // Oracle caps a statement at 65,535 bind variables; batches stay far below that.
    fn max_params(&self) -> usize {
        30_000
    }

    fn param(&self, n: usize) -> String {
        format!(":{n}")
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::split_statements;

    #[test]
    fn split_statements_should_keep_plsql_blocks_whole_until_their_slash() {
        let sql = "create table t (a number);\ninsert into t values (1);\n\
                   CREATE OR REPLACE PROCEDURE p AS\nBEGIN\n  NULL;\nEND;\n/\n\
                   begin\n  insert into t values (';');\nend;\n/\nselect * from t";
        assert_eq!(
            split_statements(sql),
            [
                "create table t (a number)",
                "insert into t values (1)",
                "CREATE OR REPLACE PROCEDURE p AS\nBEGIN\n  NULL;\nEND;",
                "begin\n  insert into t values (';');\nend;",
                "select * from t",
            ]
        );
        assert_eq!(split_statements("select 1 / 2 from dual; -- a / b\n"), ["select 1 / 2 from dual"]);
        assert_eq!(split_statements("begin transaction"), ["begin transaction"]);
    }
}

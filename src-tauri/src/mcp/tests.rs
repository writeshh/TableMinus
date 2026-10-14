//! End-to-end tests of the MCP server: a real server on a free port, called over HTTP, against SQLite.

use super::{Event, Shared, bridge_file, new_token};
use crate::db::{Conn, Engine, Options};
use crate::error::Error;
use crate::{Live, Open, Prefs, store, tls::Tls};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

const READ_WRITE: &str = "dbm_read_write";
const READ_ONLY: &str = "dbm_read_only";
/// Lists only "other", which isn't open.
const ELSEWHERE: &str = "dbm_elsewhere";

struct Server {
    shared: Arc<Shared>,
    port: u16,
    events: Receiver<Event>,
    dir: PathBuf,
    _running: super::server::Running,
}

fn saved(name: &str, url: &str) -> store::Saved {
    store::Saved {
        name: name.into(),
        kind: Engine::Sqlite,
        url: url.into(),
        tag: "local".into(),
        safety: crate::safety::Safety::Silent,
        ssh: None,
        tls: Tls::default(),
        options: Options::default(),
        password_mode: crate::store::PasswordMode::Keychain,
        color: None,
        group: String::new(),
    }
}

/// A server whose "lite" connection is open on an in-memory database with table `t` (one row).
async fn server(test: &str) -> Server {
    let dir = std::env::temp_dir().join(format!("tableminus-{}-mcp-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir); // left over from an earlier run, or not there at all
    for c in [saved("lite", ":memory:"), saved("other", ":memory:")] {
        store::save(&dir, c, None, None, None).unwrap();
    }
    let mut conn =
        Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &Options::default(), None).await.unwrap();
    for sql in ["create table t(id integer primary key, name text)", "insert into t values (1, 'a')"] {
        conn.query(sql).await.unwrap();
    }
    let open = Open::default();
    let live = Live { saved: saved("lite", ":memory:"), url: ":memory:".into(), password: None, conn, tunnel: None };
    open.insert("lite".into(), live).await;

    let (tx, events) = channel();
    let notify = Box::new(move |e| {
        let _ = tx.send(e); // the test may have stopped listening
    });
    let shared = Shared::new(dir.clone(), open, Prefs::default(), notify).unwrap();
    shared
        .update(|c| {
            c.tokens = vec![
                new_token("rw", READ_WRITE, None, false, false).unwrap(),
                new_token("ro", READ_ONLY, None, true, false).unwrap(),
                new_token("elsewhere", ELSEWHERE, Some(vec!["other".into()]), false, false).unwrap(),
            ];
        })
        .unwrap();
    let running = super::server::start(shared.clone(), 0).await.unwrap();
    Server { shared, port: running.port, events, dir, _running: running }
}

/// POSTs a JSON-RPC message and returns the status and the reply.
async fn post(port: u16, token: Option<&str>, body: &Value) -> (u16, Value) {
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.unwrap();
    tauri::async_runtime::spawn(conn);
    let mut req = hyper::Request::post("/mcp")
        .header("host", format!("127.0.0.1:{port}"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let res = sender.send_request(req.body(Full::new(Bytes::from(body.to_string()))).unwrap()).await.unwrap();
    let status = res.status().as_u16();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Calls a tool and returns whether it failed, and its text.
async fn call(port: u16, token: &str, tool: &str, args: Value) -> (bool, String) {
    let body =
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": tool, "arguments": args } });
    let (status, reply) = post(port, Some(token), &body).await;
    assert_eq!(status, 200, "{reply}");
    let result = &reply["result"];
    (result["isError"] == true, result["content"][0]["text"].as_str().unwrap_or_default().to_string())
}

/// Waits for the next approval request, answers it with `allow` and returns the SQL it asked about. The call
/// that asked runs on the async runtime's workers, so blocking here is fine.
fn answer_next(s: &Server, allow: bool) -> String {
    loop {
        if let Event::Approval { id, sql, .. } = s.events.recv_timeout(std::time::Duration::from_secs(5)).unwrap() {
            s.shared.answer(id, allow);
            return sql;
        }
    }
}

#[test]
fn requests_without_a_known_token_are_refused() {
    tauri::async_runtime::block_on(async {
        let s = server("unauthorized").await;
        let list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        assert_eq!(post(s.port, None, &list).await.0, 401);
        assert_eq!(post(s.port, Some("dbm_wrong"), &list).await.0, 401);
        let (status, reply) = post(s.port, Some(READ_WRITE), &list).await;
        assert_eq!(status, 200);
        let names: Vec<_> = reply["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].clone()).collect();
        assert_eq!(names, ["list_connections", "list_tables", "describe_table", "run_query"]);
    });
}

#[test]
fn list_connections_shows_only_reachable_ones_without_urls() {
    tauri::async_runtime::block_on(async {
        let s = server("list").await;
        let (_, all) = call(s.port, READ_WRITE, "list_connections", json!({})).await;
        assert_eq!(all, r#"[{"engine":"sqlite","name":"lite","open":true,"tag":"local"}]"#);
        let (_, listed) = call(s.port, ELSEWHERE, "list_connections", json!({})).await;
        assert_eq!(listed, r#"[{"engine":"sqlite","name":"other","open":false,"tag":"local"}]"#);
    });
}

#[test]
fn tools_refuse_a_connection_the_token_does_not_reach() {
    tauri::async_runtime::block_on(async {
        let s = server("reach").await;
        let (error, text) = call(s.port, ELSEWHERE, "list_tables", json!({ "connection": "lite" })).await;
        assert!(error && text.contains("can't use \"lite\""), "{text}");
    });
}

#[test]
fn list_tables_and_describe_table_read_the_schema() {
    tauri::async_runtime::block_on(async {
        let s = server("schema").await;
        assert_eq!(
            call(s.port, READ_ONLY, "list_tables", json!({ "connection": "lite" })).await,
            (false, r#"["t"]"#.into())
        );
        let (error, columns) =
            call(s.port, READ_ONLY, "describe_table", json!({ "connection": "lite", "table": "t" })).await;
        let columns: Value = serde_json::from_str(&columns).unwrap();
        assert!(!error);
        assert_eq!(columns[0]["name"], "id");
        assert_eq!(columns[0]["primary"], true);
        assert_eq!(columns[1]["name"], "name");
    });
}

#[test]
fn run_query_returns_rows_of_a_read_only_query_without_asking() {
    tauri::async_runtime::block_on(async {
        let s = server("select").await;
        let (error, text) =
            call(s.port, READ_ONLY, "run_query", json!({ "connection": "lite", "sql": "select * from t" })).await;
        let result: Value = serde_json::from_str(&text).unwrap();
        assert!(!error);
        assert_eq!(result["rows"], json!([["1", "a"]]));
        assert!(s.events.try_iter().all(|e| !matches!(e, Event::Approval { .. })));
        let log = super::lock(&s.shared.activity).clone();
        assert_eq!(
            (log[0].tool.as_str(), log[0].outcome.as_str(), log[0].token.as_str()),
            ("run_query", "1 rows", "ro")
        );
    });
}

#[test]
fn read_only_tokens_never_run_writes() {
    tauri::async_runtime::block_on(async {
        let s = server("read-only").await;
        let sql = "delete from t";
        let (error, text) = call(s.port, READ_ONLY, "run_query", json!({ "connection": "lite", "sql": sql })).await;
        assert!(error && text.contains("read-only"), "{text}");
        assert!(s.events.try_iter().all(|e| !matches!(e, Event::Approval { .. })), "refused without asking");
        let (_, rows) =
            call(s.port, READ_ONLY, "run_query", json!({ "connection": "lite", "sql": "select count(*) from t" }))
                .await;
        assert!(rows.contains(r#"[["1"]]"#), "{rows}");
    });
}

#[test]
fn writes_run_only_once_the_user_allows_them() {
    tauri::async_runtime::block_on(async {
        let s = server("approve").await;
        let port = s.port;
        let insert = json!({ "connection": "lite", "sql": "insert into t values (2, 'b')" });
        let pending = tauri::async_runtime::spawn(async move { call(port, READ_WRITE, "run_query", insert).await });
        let asked = answer_next(&s, true);
        assert_eq!(asked, "insert into t values (2, 'b')");
        assert_eq!(
            pending.await.unwrap(),
            (false, r#"{"columns":[],"rows":[],"affected":1,"truncated":false}"#.into())
        );
    });
}

#[test]
fn denied_writes_are_not_run() {
    tauri::async_runtime::block_on(async {
        let s = server("deny").await;
        let port = s.port;
        let delete = json!({ "connection": "lite", "sql": "delete from t" });
        let pending = tauri::async_runtime::spawn(async move { call(port, READ_WRITE, "run_query", delete).await });
        answer_next(&s, false);
        let (error, text) = pending.await.unwrap();
        assert!(error && text.contains("denied"), "{text}");
        let (_, rows) =
            call(s.port, READ_WRITE, "run_query", json!({ "connection": "lite", "sql": "select count(*) from t" }))
                .await;
        assert!(rows.contains(r#"[["1"]]"#), "{rows}");
    });
}

#[test]
fn writes_skip_approval_when_it_is_off_and_safe_mode_is_too() {
    tauri::async_runtime::block_on(async {
        let s = server("no-approval").await;
        s.shared.update(|c| c.approval = false).unwrap();
        let (error, text) =
            call(s.port, READ_WRITE, "run_query", json!({ "connection": "lite", "sql": "delete from t" })).await;
        assert!(!error, "{text}");
        assert!(text.contains(r#""affected":1"#), "{text}");
    });
}

#[test]
fn the_bridge_file_is_private_and_holds_a_working_token() {
    tauri::async_runtime::block_on(async {
        let s = server("bridge").await;
        s.shared.write_bridge_file(s.port).unwrap();
        let path = bridge_file(&s.dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let file: super::BridgeFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(file.port, s.port);
        assert!(
            !std::fs::read_to_string(super::config_file(&s.dir)).unwrap().contains(&file.token),
            "only the hash is kept"
        );
        assert_eq!(call(s.port, &file.token, "list_tables", json!({ "connection": "lite" })).await.1, r#"["t"]"#);
        s.shared.write_bridge_file(s.port).unwrap();
        let again: super::BridgeFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(again.token, file.token, "a restart keeps the bridge's token");
    });
}

#[test]
fn a_connection_in_safe_mode_asks_even_with_approval_off() {
    tauri::async_runtime::block_on(async {
        let s = server("safe-mode").await;
        s.shared.update(|c| c.approval = false).unwrap();
        s.shared.open.get("lite").await.unwrap().saved.safety = crate::safety::Safety::Alert2;
        let port = s.port;
        let delete = json!({ "connection": "lite", "sql": "delete from t" });
        let pending = tauri::async_runtime::spawn(async move { call(port, READ_WRITE, "run_query", delete).await });
        assert_eq!(answer_next(&s, false), "delete from t");
        assert!(pending.await.unwrap().0);
    });
}

#[test]
fn alert1_asks_even_before_a_read() {
    tauri::async_runtime::block_on(async {
        let s = server("alert1").await;
        s.shared.update(|c| c.approval = false).unwrap();
        s.shared.open.get("lite").await.unwrap().saved.safety = crate::safety::Safety::Alert1;
        let port = s.port;
        let select = json!({ "connection": "lite", "sql": "select count(*) from t" });
        let pending = tauri::async_runtime::spawn(async move { call(port, READ_WRITE, "run_query", select).await });
        assert_eq!(answer_next(&s, false), "select count(*) from t");
        assert!(pending.await.unwrap().0);
    });
}

#[test]
fn a_safe_level_allows_only_with_the_right_password() {
    tauri::async_runtime::block_on(async {
        let s = server("safe1").await;
        {
            let mut live = s.shared.open.get("lite").await.unwrap();
            live.saved.safety = crate::safety::Safety::Safe2;
            live.password = Some("pw".into());
        }
        let port = s.port;
        let insert = json!({ "connection": "lite", "sql": "insert into t values (3, 'c')" });
        let pending = tauri::async_runtime::spawn(async move { call(port, READ_WRITE, "run_query", insert).await });
        let id = loop {
            if let Event::Approval { id, unlock, .. } =
                s.events.recv_timeout(std::time::Duration::from_secs(5)).unwrap()
            {
                assert!(unlock, "the dialog should ask for the password");
                break id;
            }
        };
        let wrong = crate::safety::Unlock::Password("nope".into());
        assert!(matches!(s.shared.answer_checked(id, true, Some(&wrong)).await, Err(Error::WrongPassword)));
        assert!(matches!(s.shared.answer_checked(id, true, None).await, Err(Error::PasswordRequired)));
        let right = crate::safety::Unlock::Password("pw".into());
        s.shared.answer_checked(id, true, Some(&right)).await.unwrap();
        assert!(!pending.await.unwrap().0, "allowed, so it ran");
    });
}

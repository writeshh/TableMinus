//! `tableminus-mcp`: the stdio bridge for agents that start MCP servers as a command. It forwards each JSON-RPC
//! message read from stdin to the running app's MCP server and writes the replies to stdout, one per line.
//!
//! The port and token come from `mcp-bridge.json` in the app's config folder, which the app writes while its MCP
//! server runs. `TABLEMINUS_MCP_TOKEN` overrides the token, to give an agent a narrower one from Settings › MCP.

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Request, header};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

/// Must match `identifier` in tauri.conf.json: the app keeps its files in a folder of that name.
const APP_ID: &str = "dev.ritesh.dbmanager";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(serde::Deserialize)]
struct BridgeFile {
    port: u16,
    token: String,
}

/// The negotiated protocol version, sent as `MCP-Protocol-Version` once `initialize` has answered.
type Version = Arc<Mutex<Option<String>>>;
type Stdout = Arc<tokio::sync::Mutex<tokio::io::Stdout>>;

fn main() -> std::process::ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tableminus-mcp: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tableminus-mcp: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> std::io::Result<()> {
    let stdout: Stdout = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
    let version: Version = Arc::default();
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut in_flight = tokio::task::JoinSet::new();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        // Each message on its own task: a query waiting for the user's approval mustn't hold up a ping.
        in_flight.spawn(forward(line, stdout.clone(), version.clone()));
    }
    // stdin closed: answer what was already asked before exiting.
    while in_flight.join_next().await.is_some() {}
    Ok(())
}

/// Sends one message and writes whatever comes back. A request that fails gets a JSON-RPC error, so the agent
/// can tell the user why.
async fn forward(line: String, stdout: Stdout, version: Version) {
    let message: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    let replies = match post(line, &version).await {
        Ok(replies) => replies,
        Err(why) => match message.get("id") {
            Some(id) => vec![json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": why } })],
            None => vec![], // a notification: nobody to tell
        },
    };
    if message.get("method").and_then(Value::as_str) == Some("initialize") {
        let negotiated = replies.iter().find_map(|r| r.pointer("/result/protocolVersion")?.as_str().map(String::from));
        *version.lock().unwrap_or_else(PoisonError::into_inner) = negotiated;
    }
    let mut out = stdout.lock().await;
    for reply in replies {
        // stdout gone means the agent quit; there's no one left to tell.
        let _ = out.write_all(format!("{reply}\n").as_bytes()).await;
    }
    let _ = out.flush().await;
}

fn bridge_file() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join(APP_ID).join("mcp-bridge.json"))
}

/// Read for every message, so restarting the app (maybe on another port) needs no agent restart.
fn target() -> Result<(u16, String), String> {
    let off = "TableMinus isn't running, or its MCP server is off: turn it on in Settings › MCP";
    let path = bridge_file().ok_or(off)?;
    let file: BridgeFile = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).ok_or(off)?;
    let token = std::env::var("TABLEMINUS_MCP_TOKEN").ok().filter(|t| !t.trim().is_empty()).unwrap_or(file.token);
    Ok((file.port, token))
}

/// POSTs one message to the server and returns the JSON-RPC messages in the reply (none for a notification).
// The reply has no timeout of its own: the server bounds it with the approval and query timeouts.
async fn post(body: String, version: &Version) -> Result<Vec<Value>, String> {
    let (port, token) = target()?;
    let off = |_| "TableMinus isn't running, or its MCP server is off: turn it on in Settings › MCP".to_string();
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(("127.0.0.1", port)))
        .await
        .map_err(|_| "TableMinus's MCP server didn't answer".to_string())?
        .map_err(off)?;
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await.map_err(|e| e.to_string())?;
    tokio::spawn(conn);
    let mut request = Request::post("/mcp")
        .header(header::HOST, format!("127.0.0.1:{port}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    if let Some(v) = version.lock().unwrap_or_else(PoisonError::into_inner).clone() {
        request = request.header("MCP-Protocol-Version", v);
    }
    let request = request.body(Full::new(Bytes::from(body))).map_err(|e| e.to_string())?;
    let response = sender.send_request(request).await.map_err(|e| e.to_string())?;
    let status = response.status();
    let sse = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    let bytes = response.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    let messages: Vec<Value> = if sse {
        text.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .filter_map(|d| serde_json::from_str(d.trim()).ok())
            .collect()
    } else {
        serde_json::from_str(&text).ok().into_iter().collect()
    };
    if messages.is_empty() && !status.is_success() {
        return Err(if status == hyper::StatusCode::UNAUTHORIZED {
            "TableMinus refused the token: it may have been revoked (Settings › MCP)".into()
        } else {
            format!("TableMinus's MCP server answered {status}: {}", text.trim())
        });
    }
    Ok(messages)
}

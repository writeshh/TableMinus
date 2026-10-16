//! Assistant agents over the Agent Client Protocol (ACP): Claude Code, Codex or any agent that speaks it, started as
//! a child process and spoken to in JSON-RPC, one message per line, over its standard input and output.
//!
//! The agent reaches the database through TableMinus's MCP server: its session gets the `tableminus-mcp` bridge with
//! a token that lives in memory only, reaches only the workspace's connection and is read-only. Whatever the agent
//! asks permission for (its own tools: files, shell) goes to the assistant pane; file and terminal access through
//! TableMinus aren't offered.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

const PROTOCOL_VERSION: u64 = 1;
/// A message larger than this isn't one; the agent is stopped.
const MAX_LINE: u64 = 16 * 1024 * 1024;
/// Starting may download the agent first (`npx`).
const START_TIMEOUT: Duration = Duration::from_secs(180);
/// A whole prompt turn, tool calls and permission questions included.
const TURN_TIMEOUT: Duration = Duration::from_mins(30);
/// How much of the end of the agent's standard error is kept, for the message when it fails.
const STDERR_TAIL: usize = 2000;
/// Where `npx`, `node` and agents live on a Mac or a Linux desktop; an app started from the Dock or a launcher
/// doesn't get the shell's PATH.
const MORE_PATH: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];
const MORE_HOME_PATH: [&str; 4] = [".local/bin", ".volta/bin", ".bun/bin", ".npm-global/bin"];

/// The command that starts the agent, without a shell: `npx` with `["-y", "@zed-industries/claude-code-acp"]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    pub command: String,
    pub args: Vec<String>,
}

/// What the assistant pane hears from an agent, as the `acp` event.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcpEvent {
    /// A `session/update` (message chunks, tool calls, plans), passed on as the agent sent it.
    Update { agent: u64, update: Value },
    /// The agent asks before a tool call; `acp_permission` answers with one of `options`.
    Permission { agent: u64, key: u64, tool_call: Value, options: Value },
    /// The process ended.
    Exited { agent: u64, message: String },
}

type Sink = Arc<dyn Fn(AcpEvent) + Send + Sync>;
type Reply = std::result::Result<Value, String>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the agent and the task reading its messages share.
#[derive(Clone)]
struct Pipes {
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    /// Our requests waiting for their response, by JSON-RPC id.
    waiting: Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>,
    /// The agent's permission requests waiting for the user: our key, then the agent's request id.
    asks: Arc<Mutex<HashMap<u64, Value>>>,
    stderr: Arc<Mutex<String>>,
}

/// One running agent and its session.
pub struct Agent {
    session: Mutex<String>,
    pipes: Pipes,
    child: tokio::sync::Mutex<Child>,
    next: AtomicU64,
    /// The temporary MCP token's id, dropped with the agent.
    token: String,
}

async fn send(stdin: &tokio::sync::Mutex<ChildStdin>, message: &Value) -> Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    let mut stdin = stdin.lock().await;
    stdin.write_all(&line).await?;
    Ok(stdin.flush().await?)
}

/// Reads the agent's messages until it closes its output: responses go to whoever waits for them, permission requests
/// and session updates to `sink`, and any other request is answered "not offered".
async fn read_messages(
    id: u64,
    stdout: tokio::process::ChildStdout,
    pipes: Pipes,
    stderr_done: oneshot::Receiver<()>,
    sink: Sink,
) {
    let Pipes { stdin, waiting, asks, stderr } = pipes;
    let mut reader = BufReader::new(stdout);
    let mut next_key = 1u64;
    let ended = loop {
        let mut line = Vec::new();
        match (&mut reader).take(MAX_LINE).read_until(b'\n', &mut line).await {
            Ok(0) => break "the agent stopped".to_string(),
            Ok(_) if !line.ends_with(b"\n") && line.len() as u64 == MAX_LINE => {
                break "the agent sent a message too large to read".to_string();
            }
            Ok(_) => {}
            Err(e) => break format!("reading from the agent failed: {e}"),
        }
        let Ok(message) = serde_json::from_slice::<Value>(&line) else { continue }; // a stray log line
        let method = message.get("method").and_then(Value::as_str);
        let request_id = message.get("id").cloned().filter(|v| !v.is_null());
        match (method, request_id) {
            (None, Some(rid)) => {
                let Some(sender) = rid.as_u64().and_then(|n| lock(&waiting).remove(&n)) else { continue };
                let reply = match message.get("error") {
                    Some(e) => Err(error_text(e)),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                // The waiting request may have timed out and gone; nobody else wants the reply.
                let _ = sender.send(reply);
            }
            (Some("session/request_permission"), Some(rid)) => {
                let key = next_key;
                next_key += 1;
                lock(&asks).insert(key, rid);
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                sink(AcpEvent::Permission {
                    agent: id,
                    key,
                    tool_call: params.get("toolCall").cloned().unwrap_or(Value::Null),
                    options: params.get("options").cloned().unwrap_or(Value::Null),
                });
            }
            (Some(_), Some(rid)) => {
                let refusal = json!({ "jsonrpc": "2.0", "id": rid,
                    "error": { "code": -32601, "message": "TableMinus doesn't offer this to agents" } });
                if send(&stdin, &refusal).await.is_err() {
                    break "the agent stopped".to_string();
                }
            }
            (Some("session/update"), None) => {
                if let Some(update) = message.get("params").and_then(|p| p.get("update")) {
                    sink(AcpEvent::Update { agent: id, update: update.clone() });
                }
            }
            _ => {}
        }
    };
    // An agent that fails says why on its standard error, which may close a moment after its output.
    let _ = tokio::time::timeout(Duration::from_secs(1), stderr_done).await; // without it, the message is shorter
    let tail = lock(&stderr).trim().to_string();
    let message = if tail.is_empty() { ended } else { format!("{ended}: {tail}") };
    for (_, sender) in lock(&waiting).drain() {
        // Each waiter learns why; one that gave up already doesn't need to.
        let _ = sender.send(Err(message.clone()));
    }
    sink(AcpEvent::Exited { agent: id, message });
}

/// A JSON-RPC error as one line: its message, and the details agents put in `data`.
fn error_text(e: &Value) -> String {
    let message = e.get("message").and_then(Value::as_str).unwrap_or("the agent refused");
    let details = e.get("data").and_then(|d| d.get("details").and_then(Value::as_str).or_else(|| d.as_str()));
    let text = details.map_or_else(|| message.to_string(), |d| format!("{message}: {d}"));
    text.chars().take(500).collect()
}

/// Keeps the end of the agent's standard error; `done` fires when it closes.
async fn keep_stderr(stream: tokio::process::ChildStderr, tail: Arc<Mutex<String>>, done: oneshot::Sender<()>) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut t = lock(&tail);
        t.push_str(&line);
        t.push('\n');
        if t.len() > STDERR_TAIL {
            let cut = t.len() - STDERR_TAIL;
            let at = (cut..t.len()).find(|&i| t.is_char_boundary(i)).unwrap_or(t.len());
            t.drain(..at);
        }
    }
    // The reader may have stopped waiting for this already.
    let _ = done.send(());
}

/// PATH for the agent: this process's, then the usual places for `npx` and agents.
fn agent_path() -> std::ffi::OsString {
    let mut dirs: Vec<std::path::PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(MORE_PATH.iter().map(std::path::PathBuf::from));
    if let Some(home) = dirs::home_dir() {
        dirs.extend(MORE_HOME_PATH.iter().map(|d| home.join(d)));
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

impl Agent {
    /// Starts the agent in `cwd`, initializes it and opens a session with `mcp_servers`.
    async fn start(
        id: u64,
        spec: &AgentSpec,
        cwd: &Path,
        mcp_servers: Value,
        token: String,
        sink: Sink,
    ) -> Result<Arc<Agent>> {
        if spec.command.trim().is_empty() {
            return Err(Error::Invalid("choose the agent's command in Settings › AI"));
        }
        let mut child = Command::new(spec.command.trim())
            .args(&spec.args)
            .current_dir(cwd)
            .env("PATH", agent_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    Error::Agent(format!("couldn't find {}; is it installed?", spec.command))
                }
                _ => Error::Io(e),
            })?;
        let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(Error::Agent("the agent's input and output couldn't be opened".into()));
        };
        let pipes = Pipes {
            stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
            waiting: Arc::default(),
            asks: Arc::default(),
            stderr: Arc::default(),
        };
        let agent = Arc::new(Agent {
            session: Mutex::new(String::new()),
            pipes: pipes.clone(),
            child: tokio::sync::Mutex::new(child),
            next: AtomicU64::new(1),
            token,
        });
        let (stderr_done, stderr_closed) = oneshot::channel();
        tauri::async_runtime::spawn(keep_stderr(stderr, pipes.stderr.clone(), stderr_done));
        tauri::async_runtime::spawn(read_messages(id, stdout, pipes, stderr_closed, sink));
        let setup = async {
            let init = agent
                .request(
                    "initialize",
                    json!({
                        "protocolVersion": PROTOCOL_VERSION,
                        "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
                        "clientInfo": { "name": "TableMinus", "version": env!("CARGO_PKG_VERSION") },
                    }),
                )
                .await?;
            if init.get("protocolVersion").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
                return Err(Error::Agent("the agent speaks another version of ACP".into()));
            }
            let session = agent.request("session/new", json!({ "cwd": cwd, "mcpServers": mcp_servers })).await?;
            let id = session
                .get("sessionId")
                .and_then(Value::as_str)
                .ok_or(Error::Agent("the agent opened no session".into()))?;
            *lock(&agent.session) = id.to_string();
            Ok(())
        };
        match tokio::time::timeout(START_TIMEOUT, setup).await {
            Ok(Ok(())) => Ok(agent),
            Ok(Err(e)) => {
                agent.kill().await;
                Err(e)
            }
            Err(_) => {
                agent.kill().await;
                Err(Error::Agent(format!("the agent didn't start within {} seconds", START_TIMEOUT.as_secs())))
            }
        }
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.pipes.waiting).insert(id, tx);
        send(&self.pipes.stdin, &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        rx.await.map_err(|_| Error::Agent("the agent stopped".into()))?.map_err(Error::Agent)
    }

    fn session(&self) -> String {
        lock(&self.session).clone()
    }

    /// Sends the user's message and waits for the turn to end; returns why it ended (`end_turn`, `cancelled`…).
    async fn prompt(&self, text: &str) -> Result<String> {
        let params = json!({ "sessionId": self.session(), "prompt": [{ "type": "text", "text": text }] });
        let done = tokio::time::timeout(TURN_TIMEOUT, self.request("session/prompt", params))
            .await
            .map_err(|_| Error::Agent("the agent's turn took over 30 minutes; stopped waiting".into()))??;
        Ok(done.get("stopReason").and_then(Value::as_str).unwrap_or("end_turn").to_string())
    }

    /// Answers a permission request: with an option's id, or cancelled.
    async fn answer(&self, key: u64, option: Option<&str>) -> Result<()> {
        let Some(rid) = lock(&self.pipes.asks).remove(&key) else { return Ok(()) }; // answered already, or cancelled
        let outcome = match option {
            Some(o) => json!({ "outcome": "selected", "optionId": o }),
            None => json!({ "outcome": "cancelled" }),
        };
        send(&self.pipes.stdin, &json!({ "jsonrpc": "2.0", "id": rid, "result": { "outcome": outcome } })).await
    }

    /// Stops the turn: the agent is told, and its open permission questions are answered cancelled, as ACP asks.
    async fn cancel(&self) -> Result<()> {
        let keys: Vec<u64> = lock(&self.pipes.asks).keys().copied().collect();
        for key in keys {
            self.answer(key, None).await?;
        }
        send(
            &self.pipes.stdin,
            &json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": self.session() } }),
        )
        .await
    }

    async fn kill(&self) {
        // Already gone is as good as killed.
        let _ = self.child.lock().await.kill().await;
    }
}

/// The running agents, by the id the pane knows them by.
#[derive(Default)]
pub struct Agents {
    map: Mutex<HashMap<u64, Arc<Agent>>>,
    next: AtomicU64,
}

impl Agents {
    fn get(&self, id: u64) -> Result<Arc<Agent>> {
        lock(&self.map).get(&id).cloned().ok_or(Error::Agent("the agent isn't running; start a new chat".into()))
    }
}

/// Kills every agent: the app is quitting, and an agent left running would outlive what it works for.
pub fn stop_all(agents: &Agents) {
    for (_, a) in lock(&agents.map).drain() {
        if let Ok(mut child) = a.child.try_lock() {
            // The app quits whether or not the agent is still there to kill.
            let _ = child.start_kill();
        }
    }
}

/// Starts an agent for the workspace's `connection` and returns its id. Session updates and permission questions
/// arrive as `acp` events.
#[tauri::command]
pub async fn acp_start(
    app: AppHandle,
    agents: State<'_, Agents>,
    mcp: State<'_, crate::mcp::Mcp>,
    connection: String,
    agent: AgentSpec,
) -> Result<u64> {
    mcp.0
        .running_port()
        .ok_or(Error::Invalid("turn on the MCP server in Settings › MCP: the agent reaches the database through it"))?;
    let bridge = crate::mcp::bridge_path();
    if !bridge.exists() {
        return Err(Error::Invalid(
            "the MCP bridge (tableminus-mcp) isn't next to the app; build it with cargo build --bin tableminus-mcp",
        ));
    }
    let cwd = app.path().app_config_dir()?.join("agent");
    std::fs::create_dir_all(&cwd)?;
    let (token, value) = mcp.0.temporary_token("Assistant agent", vec![connection], true)?;
    let servers = json!([{ "name": "tableminus", "command": bridge, "args": [],
        "env": [{ "name": "TABLEMINUS_MCP_TOKEN", "value": value }] }]);
    let id = agents.next.fetch_add(1, Ordering::Relaxed) + 1;
    let sink: Sink = Arc::new(move |e| {
        // The window may be closing; then nobody is listening.
        let _ = app.emit("acp", e);
    });
    match Agent::start(id, &agent, &cwd, servers, token.clone(), sink).await {
        Ok(a) => {
            lock(&agents.map).insert(id, a);
            Ok(id)
        }
        Err(e) => {
            mcp.0.drop_temporary(&token);
            Err(e)
        }
    }
}

#[tauri::command]
pub async fn acp_prompt(agents: State<'_, Agents>, agent: u64, text: String) -> Result<String> {
    let a = agents.get(agent)?;
    a.prompt(&text).await
}

#[tauri::command]
pub async fn acp_cancel(agents: State<'_, Agents>, agent: u64) -> Result<()> {
    agents.get(agent)?.cancel().await
}

/// The user's answer to a permission question: an option's id, or None for cancelled.
#[tauri::command]
pub async fn acp_permission(agents: State<'_, Agents>, agent: u64, key: u64, option: Option<String>) -> Result<()> {
    agents.get(agent)?.answer(key, option.as_deref()).await
}

/// Stops the agent and forgets its token.
#[tauri::command]
pub async fn acp_stop(agents: State<'_, Agents>, mcp: State<'_, crate::mcp::Mcp>, agent: u64) -> Result<()> {
    let Some(a) = lock(&agents.map).remove(&agent) else { return Ok(()) };
    a.kill().await;
    mcp.0.drop_temporary(&a.token);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AcpEvent, Agent, AgentSpec, Sink, error_text};
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};

    /// A fake agent in Node: answers initialize and session/new, and on a prompt sends a message chunk, asks
    /// permission, then ends the turn with the option chosen as its text. It also asks for a file, which must be
    /// refused.
    const FAKE: &str = r#"
const rl = require("readline").createInterface({ input: process.stdin });
const out = (m) => process.stdout.write(JSON.stringify(m) + "\n");
let prompt = null;
rl.on("line", (line) => {
  const m = JSON.parse(line);
  if (m.method === "initialize") out({ jsonrpc: "2.0", id: m.id, result: { protocolVersion: 1, agentCapabilities: {} } });
  else if (m.method === "session/new") out({ jsonrpc: "2.0", id: m.id, result: { sessionId: "s1", seen: m.params } });
  else if (m.method === "session/prompt") {
    prompt = m.id;
    process.stdout.write("not json, a log line\n");
    out({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1", update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "Hi " + m.params.prompt[0].text } } } });
    out({ jsonrpc: "2.0", id: "read-1", method: "fs/read_text_file", params: { sessionId: "s1", path: "/etc/passwd" } });
  } else if (m.id === "read-1") {
    out({ jsonrpc: "2.0", method: "session/update", params: { sessionId: "s1", update: { sessionUpdate: "agent_thought_chunk", content: { type: "text", text: m.error ? "refused" : "read" } } } });
    out({ jsonrpc: "2.0", id: "perm-1", method: "session/request_permission", params: { sessionId: "s1", toolCall: { toolCallId: "t", title: "Run" }, options: [{ optionId: "yes", name: "Allow", kind: "allow_once" }] } });
  } else if (m.id === "perm-1") {
    out({ jsonrpc: "2.0", id: prompt, result: { stopReason: m.result.outcome.optionId ?? m.result.outcome.outcome } });
  }
});
"#;

    fn fake_spec(name: &str) -> (AgentSpec, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tableminus-acp-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("agent.js");
        std::fs::write(&script, FAKE).unwrap();
        (AgentSpec { command: "node".into(), args: vec![script.display().to_string()] }, dir)
    }

    fn sink() -> (Sink, Arc<Mutex<Vec<AcpEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let kept = events.clone();
        (Arc::new(move |e| kept.lock().unwrap().push(e)), events)
    }

    #[test]
    fn an_agent_should_stream_updates_ask_permission_and_end_the_turn() {
        tauri::async_runtime::block_on(async {
            let (spec, dir) = fake_spec("turn");
            let (sink, events) = sink();
            let servers = json!([{ "name": "tableminus", "command": "/bin/true", "args": [], "env": [] }]);
            let agent = Agent::start(7, &spec, &dir, servers, "t".into(), sink).await.unwrap();
            assert_eq!(agent.session(), "s1");
            let turn = {
                let agent = agent.clone();
                tokio::spawn(async move { agent.prompt("there").await })
            };
            // Wait for the permission question, then allow.
            let key = loop {
                let found = events.lock().unwrap().iter().find_map(|e| match e {
                    AcpEvent::Permission { key, .. } => Some(*key),
                    _ => None,
                });
                if let Some(k) = found {
                    break k;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            };
            agent.answer(key, Some("yes")).await.unwrap();
            assert_eq!(turn.await.unwrap().unwrap(), "yes");
            let updates: Vec<Value> = events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    AcpEvent::Update { update, .. } => Some(update["content"]["text"].clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(updates, [json!("Hi there"), json!("refused")], "the file read was refused");
            agent.kill().await;
        });
    }

    #[test]
    fn an_agent_that_cant_be_found_should_say_so() {
        tauri::async_runtime::block_on(async {
            let (_, dir) = fake_spec("missing");
            let (sink, _) = sink();
            let spec = AgentSpec { command: "no-such-agent-here".into(), args: vec![] };
            let e = Agent::start(1, &spec, &dir, json!([]), "t".into(), sink).await.err().unwrap().to_string();
            assert!(e.contains("couldn't find no-such-agent-here"), "{e}");
        });
    }

    #[test]
    fn an_agent_that_exits_should_fail_its_start_with_its_error_output() {
        tauri::async_runtime::block_on(async {
            let (_, dir) = fake_spec("exits");
            let (sink, _) = sink();
            let spec = AgentSpec {
                command: "node".into(),
                args: vec!["-e".into(), "console.error('no login'); process.exit(1)".into()],
            };
            let e = Agent::start(1, &spec, &dir, json!([]), "t".into(), sink).await.err().unwrap().to_string();
            assert!(e.contains("no login"), "{e}");
        });
    }

    #[test]
    fn error_text_should_add_the_agents_details() {
        let e = json!({ "code": -32603, "message": "Internal error", "data": { "details": "Query closed" } });
        assert_eq!(error_text(&e), "Internal error: Query closed");
        assert_eq!(error_text(&json!({ "code": 1, "message": "Nope" })), "Nope");
    }
}

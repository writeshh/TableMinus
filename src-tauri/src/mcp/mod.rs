//! The MCP server agents use to reach the user's databases: settings and access tokens in `mcp.json`, the
//! activity log, query approval, and the Tauri commands behind Settings › MCP. The HTTP side is in [`server`].

mod server;

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::oneshot;

/// How long a query waits for Allow before it counts as denied.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
/// Activity entries kept; older ones drop off.
const ACTIVITY_CAP: usize = 500;

/// What Settings › MCP edits, kept in `mcp.json` in the app config folder. Tokens are stored as SHA-256 hashes:
/// they are 256 random bits, so a slow password hash would add nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default)]
    enabled: bool,
    #[serde(default = "default_port")]
    port: u16,
    /// Ask in the app before running anything that isn't provably read-only.
    #[serde(default = "yes")]
    approval: bool,
    #[serde(default)]
    tokens: Vec<Token>,
}

impl Default for Config {
    fn default() -> Self {
        Config { enabled: false, port: default_port(), approval: true, tokens: vec![] }
    }
}

fn default_port() -> u16 {
    47321
}

fn yes() -> bool {
    true
}

/// An access token: who may call, which connections it reaches and whether it may write.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Token {
    id: String,
    name: String,
    /// Hex SHA-256 of the token value; the value itself is shown once and never kept.
    hash: String,
    /// `None` reaches every open connection; a list also reaches those connections when they aren't open.
    connections: Option<Vec<String>>,
    read_only: bool,
    created: u64,
    /// The token written for the stdio bridge; revoking it gives the bridge a new one.
    #[serde(default)]
    bridge: bool,
}

impl Token {
    /// Whether this token may use the connection. "All connections" means the ones the user has open; a
    /// connection named in the token's list is reachable even when closed (the server opens it).
    fn reaches(&self, name: &str, open: bool) -> bool {
        match &self.connections {
            None => open,
            Some(list) => list.iter().any(|c| c == name),
        }
    }
}

/// One tool call, for Settings › MCP › Activity.
#[derive(Debug, Clone, Serialize)]
pub struct Activity {
    at: u64,
    token: String,
    tool: String,
    connection: Option<String>,
    sql: Option<String>,
    outcome: String,
}

/// What the server tells the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// An agent wants to run this SQL; answer with `mcp_answer`.
    Approval {
        id: u64,
        token: String,
        connection: String,
        sql: String,
    },
    /// The approval was answered or timed out; the dialog can go.
    ApprovalClosed {
        id: u64,
    },
    Activity(Activity),
}

type Notify = Box<dyn Fn(Event) + Send + Sync>;

/// Everything the server and the commands share.
pub struct Shared {
    dir: PathBuf,
    open: crate::Open,
    prefs: crate::Prefs,
    config: Mutex<Config>,
    activity: Mutex<VecDeque<Activity>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    next_id: AtomicU64,
    server: Mutex<Option<server::Running>>,
    notify: Notify,
}

/// The Tauri-managed handle on [`Shared`].
pub struct Mcp(pub Arc<Shared>);

/// A std mutex guard; a panic elsewhere while holding it leaves data that is still usable here.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}"); // writing to a String can't fail
        s
    })
}

fn hash(value: &str) -> String {
    hex(&Sha256::digest(value.as_bytes()))
}

fn random_hex(len: usize) -> Result<String> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).map_err(|_| Error::Invalid("the system random number generator failed"))?;
    Ok(hex(&bytes))
}

fn config_file(dir: &Path) -> PathBuf {
    dir.join("mcp.json")
}

/// The file the stdio bridge reads: the port and the bridge's token. The bridge binary has the same path built in.
fn bridge_file(dir: &Path) -> PathBuf {
    dir.join("mcp-bridge.json")
}

/// The settings, or the defaults when there is no file yet. An unreadable file is an error, so a save can't
/// overwrite tokens the user still has.
fn load(dir: &Path) -> Result<Config> {
    let path = config_file(dir);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(Error::McpConfig),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(source) => Err(Error::File { path: path.display().to_string(), source }),
    }
}

/// Writes to a temporary file readable only by the user, then renames it over `path`.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let failed = |source| Error::File { path: path.display().to_string(), source };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    // A leftover temporary file would keep its old permissions, as `mode` only applies on creation.
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&tmp).and_then(|mut f| f.write_all(bytes)).map_err(failed)?;
    std::fs::rename(&tmp, path).map_err(failed)
}

#[derive(Serialize, Deserialize)]
struct BridgeFile {
    port: u16,
    token: String,
}

impl Shared {
    pub fn new(dir: PathBuf, open: crate::Open, prefs: crate::Prefs, notify: Notify) -> Result<Arc<Self>> {
        let config = load(&dir)?;
        Ok(Arc::new(Shared {
            dir,
            open,
            prefs,
            config: Mutex::new(config),
            activity: Mutex::new(VecDeque::new()),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            server: Mutex::new(None),
            notify,
        }))
    }

    /// Changes the settings and saves them; nothing changes in memory if the save fails.
    fn update(&self, change: impl FnOnce(&mut Config)) -> Result<Config> {
        let mut config = lock(&self.config);
        let mut next = config.clone();
        change(&mut next);
        write_private(&config_file(&self.dir), &serde_json::to_vec_pretty(&next)?)?;
        *config = next.clone();
        Ok(next)
    }

    /// The token presented as `Authorization: Bearer …`, if it is one of ours.
    fn caller(&self, bearer: &str) -> Option<Token> {
        use subtle::ConstantTimeEq;
        let given = hash(bearer);
        lock(&self.config).tokens.iter().find(|t| bool::from(t.hash.as_bytes().ct_eq(given.as_bytes()))).cloned()
    }

    fn log(&self, entry: Activity) {
        let mut log = lock(&self.activity);
        if log.len() == ACTIVITY_CAP {
            log.pop_back();
        }
        log.push_front(entry.clone());
        drop(log);
        (self.notify)(Event::Activity(entry));
    }

    /// Asks the user to allow `sql`; no answer within [`APPROVAL_TIMEOUT`] is a denial.
    async fn approve(&self, token: &Token, connection: &str, sql: &str) -> bool {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        (self.notify)(Event::Approval {
            id,
            token: token.name.clone(),
            connection: connection.to_string(),
            sql: sql.to_string(),
        });
        let allowed = matches!(tokio::time::timeout(APPROVAL_TIMEOUT, rx).await, Ok(Ok(true)));
        lock(&self.pending).remove(&id);
        (self.notify)(Event::ApprovalClosed { id });
        allowed
    }

    fn answer(&self, id: u64, allow: bool) {
        if let Some(tx) = lock(&self.pending).remove(&id) {
            // The request may have timed out meanwhile; then the answer has nowhere to go.
            let _ = tx.send(allow);
        }
    }

    /// Makes sure the bridge token exists and its file names the current port.
    fn write_bridge_file(&self, port: u16) -> Result<()> {
        let path = bridge_file(&self.dir);
        let existing = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<BridgeFile>(&b).ok());
        let known = |token: &str| self.caller(token).is_some_and(|t| t.bridge);
        let token = match existing {
            Some(f) if known(&f.token) => f.token,
            _ => {
                let value = format!("dbm_{}", random_hex(32)?);
                let entry = new_token("Local bridge", &value, None, false, true)?;
                self.update(|c| {
                    c.tokens.retain(|t| !t.bridge);
                    c.tokens.push(entry);
                })?;
                value
            }
        };
        write_private(&path, &serde_json::to_vec_pretty(&BridgeFile { port, token })?)
    }

    /// Starts or stops the server to match the settings.
    async fn apply(self: &Arc<Self>) -> Result<()> {
        let config = lock(&self.config).clone();
        let running = lock(&self.server).take();
        let keep = running.filter(|r| config.enabled && r.preferred == config.port);
        let running = match keep {
            Some(r) => Some(r),
            None if config.enabled => {
                let r = server::start(self.clone(), config.port).await?;
                self.write_bridge_file(r.port)?;
                Some(r)
            }
            None => None,
        };
        *lock(&self.server) = running;
        Ok(())
    }
}

fn new_token(
    name: &str,
    value: &str,
    connections: Option<Vec<String>>,
    read_only: bool,
    bridge: bool,
) -> Result<Token> {
    Ok(Token {
        id: random_hex(8)?,
        name: name.to_string(),
        hash: hash(value),
        connections,
        read_only,
        created: now_ms(),
        bridge,
    })
}

/// Sets up the MCP state and starts the server if it's enabled. A failure here is reported, not fatal: the
/// rest of the app works without MCP.
pub fn setup(app: &AppHandle, open: crate::Open, prefs: crate::Prefs) -> Result<()> {
    let emitter = app.clone();
    let notify: Notify = Box::new(move |event| {
        if matches!(event, Event::Approval { .. })
            && let Some(window) = emitter.get_webview_window("main")
        {
            // Best effort: the approval dialog still shows when the window can't take focus.
            let _ = window.set_focus();
        }
        // Nobody listening (no window yet) is fine; activity is also kept in the log.
        let _ = emitter.emit("mcp", event);
    });
    let shared = Shared::new(app.path().app_config_dir()?, open, prefs, notify)?;
    app.manage(Mcp(shared.clone()));
    tauri::async_runtime::spawn(async move {
        if let Err(e) = shared.apply().await {
            eprintln!("MCP server could not start: {e}");
        }
    });
    Ok(())
}

/// What Settings › MCP shows.
#[derive(Debug, Serialize)]
pub struct Status {
    enabled: bool,
    port: u16,
    /// The port actually in use: the preferred one, or another when that was taken.
    running_port: Option<u16>,
    approval: bool,
    tokens: Vec<Token>,
    /// The stdio bridge binary agents launch, and whether it is there.
    bridge: String,
    bridge_exists: bool,
}

fn status(shared: &Shared) -> Status {
    let config = lock(&shared.config).clone();
    let bridge = std::env::current_exe()
        .map(|exe| exe.with_file_name(format!("dbmanager-mcp{}", std::env::consts::EXE_SUFFIX)))
        .unwrap_or_default();
    Status {
        enabled: config.enabled,
        port: config.port,
        running_port: lock(&shared.server).as_ref().map(|r| r.port),
        approval: config.approval,
        tokens: config.tokens,
        bridge_exists: bridge.is_file(),
        bridge: bridge.display().to_string(),
    }
}

#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
pub fn mcp_status(mcp: State<'_, Mcp>) -> Status {
    status(&mcp.0)
}

/// Saves the server settings and starts, stops or moves the server to match.
#[tauri::command]
pub async fn mcp_configure(mcp: State<'_, Mcp>, enabled: bool, port: u16, approval: bool) -> Result<Status> {
    if port < 1024 {
        return Err(Error::Invalid("choose a port from 1024 to 65535"));
    }
    mcp.0.update(|c| (c.enabled, c.port, c.approval) = (enabled, port, approval))?;
    mcp.0.apply().await?;
    Ok(status(&mcp.0))
}

/// Creates a token and returns its value, the only time it is shown.
#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
pub fn mcp_create_token(
    mcp: State<'_, Mcp>,
    name: String,
    connections: Option<Vec<String>>,
    read_only: bool,
) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Invalid("give the token a name"));
    }
    let value = format!("dbm_{}", random_hex(32)?);
    let token = new_token(name, &value, connections, read_only, false)?;
    mcp.0.update(|c| c.tokens.push(token))?;
    Ok(value)
}

#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
pub fn mcp_revoke_token(mcp: State<'_, Mcp>, id: String) -> Result<()> {
    mcp.0.update(|c| c.tokens.retain(|t| t.id != id))?;
    // When it was the bridge's token, the bridge gets a new one straight away.
    let port = lock(&mcp.0.server).as_ref().map(|r| r.port);
    port.map_or(Ok(()), |port| mcp.0.write_bridge_file(port))
}

#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
pub fn mcp_activity(mcp: State<'_, Mcp>) -> Vec<Activity> {
    lock(&mcp.0.activity).iter().cloned().collect()
}

#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
pub fn mcp_clear_activity(mcp: State<'_, Mcp>) {
    lock(&mcp.0.activity).clear();
}

#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
pub fn mcp_answer(mcp: State<'_, Mcp>, id: u64, allow: bool) {
    mcp.0.answer(id, allow);
}

#[cfg(test)]
mod tests;

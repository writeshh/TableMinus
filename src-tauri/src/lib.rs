//! TableMinus's Tauri commands: the only surface the UI can call.

mod acp;
mod ai;
mod backup;
mod browse;
mod catalog;
mod console;
mod db;
mod ddl;
mod diagram;
mod error;
mod export;
mod import;
mod jobs;
mod launcher;
mod locations;
mod mcp;
mod objects;
mod passcode;
mod plugins;
mod processes;
mod readonly;
mod redis;
mod rows;
mod safety;
mod secrets;
mod ssh;
mod store;
mod structure;
mod themes;
mod tls;
mod users;

/// What `cargo fuzz` drives (`fuzz/fuzz_targets/read_only.rs`): safe mode's read-only check on arbitrary text, for
/// every engine. Built only with `--cfg fuzzing`, which cargo-fuzz sets, so the app's surface stays as it is.
#[cfg(fuzzing)]
#[doc(hidden)]
pub fn fuzz_read_only(sql: &str) {
    for engine in [Engine::Postgres, Engine::Mysql, Engine::Sqlite] {
        readonly::is_read_only(engine, sql);
    }
}

use db::Engine;
use error::{Error, Result};
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State, async_runtime::Mutex};

/// Settings the UI keeps and hands over at startup and on every change. Clones share the values (the MCP
/// server holds one).
#[derive(Clone)]
struct Prefs {
    keep_alive: Arc<AtomicBool>,
    /// 0 means no limit.
    query_timeout_secs: Arc<AtomicU64>,
    /// The editor's row limit, which MCP queries follow too; 0 means no limit.
    max_rows: Arc<AtomicU64>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            keep_alive: Arc::new(AtomicBool::new(true)),
            query_timeout_secs: Arc::new(AtomicU64::new(300)),
            max_rows: Arc::new(AtomicU64::new(1000)),
        }
    }
}

#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
fn set_preferences(prefs: State<'_, Prefs>, keep_alive: bool, query_timeout_secs: u64, max_rows: Option<u64>) {
    prefs.keep_alive.store(keep_alive, Ordering::Relaxed);
    prefs.query_timeout_secs.store(query_timeout_secs, Ordering::Relaxed);
    if let Some(n) = max_rows {
        prefs.max_rows.store(n, Ordering::Relaxed);
    }
}

/// Every 30 s, pings each open connection when keep-alive is on. A connection that doesn't answer is reported
/// to the UI as `connection-lost`, so the reconnect banner shows before the next query fails.
async fn keep_alive(app: AppHandle) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if !app.state::<Prefs>().keep_alive.load(Ordering::Relaxed) {
            continue;
        }
        let conns: Vec<_> = app.state::<Open>().conns.lock().await.clone().into_iter().collect();
        for (name, live) in conns {
            // A connection busy with a request is in use, which is all a ping would check.
            let Ok(mut live) = live.try_lock() else { continue };
            let answered = matches!(tokio::time::timeout(Duration::from_secs(10), live.conn.ping()).await, Ok(Ok(())));
            if !answered {
                // The UI only listens while a workspace is open; nobody listening is fine.
                let _ = app.emit("connection-lost", name);
            }
        }
    }
}

struct Live {
    saved: store::Saved,
    /// The saved URL, or with another database when Postgres switched (Postgres switches by reconnecting).
    url: String,
    password: Option<SecretString>,
    conn: db::Conn,
    tunnel: Option<ssh::Tunnel>, // lives exactly as long as the connection
}

impl Live {
    /// Turns an error that means the connection dropped into [`Error::Disconnected`], so the UI can offer to
    /// reconnect instead of showing a driver message.
    fn check<T>(&self, result: Result<T>) -> Result<T> {
        result.map_err(|e| if self.conn.lost_by(&e) { Error::Disconnected } else { e })
    }
}

/// Runs the startup script, opens the SSH tunnel if there is one, then opens the database.
async fn open_db(
    saved: &store::Saved,
    password: Option<&SecretString>,
    ssh_secret: Option<SecretString>,
) -> Result<(db::Conn, Option<ssh::Tunnel>)> {
    let url = saved.url.as_str();
    if !saved.options.startup_script.trim().is_empty() {
        db::run_startup_script(&saved.options.startup_script).await?;
    }
    let tunnel = match &saved.ssh {
        Some(ssh) => Some(ssh::open_for(saved.kind, url, ssh, ssh_secret.as_ref()).await?),
        None => None,
    };
    let port = tunnel.as_ref().map(|t| t.port);
    let conn = db::Conn::open(saved.kind, url, password, &saved.tls, &saved.options, port).await?;
    Ok((conn, tunnel))
}

/// Separates a saved connection's name from the session number in a session key; see [`Open`].
const SESSION_SEP: char = '\u{1f}';

/// The saved connection a session key belongs to: `name` or `name\u{1f}n`.
fn saved_name(key: &str) -> &str {
    key.split_once(SESSION_SEP).map_or(key, |(name, _)| name)
}

/// Opens a saved connection by name (or a session key of it), with its keychain secrets. Refused while the
/// passcode lock is on.
async fn open_saved(open: &Open, dir: &std::path::Path, key: &str) -> Result<Live> {
    open.unlocked()?;
    let name = saved_name(key);
    let saved = store::load(dir)?
        .into_iter()
        .find(|c| c.name == name)
        .ok_or_else(|| Error::NoSuchConnection(name.to_string()))?;
    let password = store::password_for(&saved)?;
    let ssh_secret = if saved.ssh.is_some() { store::stored(store::Secret::Ssh, &saved.name)? } else { None };
    let (conn, tunnel) = open_db(&saved, password.as_ref(), ssh_secret).await?;
    Ok(Live { url: saved.url.clone(), saved, password, conn, tunnel })
}

/// What the workspace header and sidebar show for an open connection.
#[derive(Debug, serde::Serialize)]
struct Info {
    server: String,
    database: Option<String>,
    can_switch: bool,
    tables: Vec<String>,
}

async fn info(live: &mut Live) -> Result<Info> {
    let database = live.conn.current_database().await?;
    let file_based = live.saved.kind == Engine::Sqlite;
    // No database selected yet (MySQL connected without one): nothing to list.
    let tables = if database.is_none() && !file_based {
        vec![]
    } else {
        live.conn.tables(live.saved.options.system_schemas).await?
    };
    Ok(Info { server: live.conn.server_version().await?, database, can_switch: !file_based, tables })
}

// ponytail: SQLite's calls block the async worker thread they run on. Each connection has its own lock, so that
// stalls only its own connection's requests (the runtime has a worker per core); spawn_blocking if it hurts.
/// The open connections, each behind its own lock so a slow query holds up only its connection. Clones share
/// everything (the MCP server holds one).
///
/// They're keyed by session: a workspace's first session of a saved connection is keyed by the connection's
/// name, further ones (another database on the same server, each in its own tab) by `name\u{1f}n`. Every
/// command's `name` is such a key; the MCP server uses the plain name, so it shares the first session.
#[derive(Default, Clone)]
struct Open {
    conns: Arc<Mutex<HashMap<String, Arc<Mutex<Live>>>>>,
    /// The passcode lock. While it's set nothing reaches a database: not the UI's commands, not the MCP server.
    locked: Arc<AtomicBool>,
}

impl Open {
    fn unlocked(&self) -> Result<()> {
        if self.locked.load(Ordering::SeqCst) { Err(Error::Locked) } else { Ok(()) }
    }

    /// The open connection, locked for this request. The map's lock is let go before waiting on the connection's.
    async fn get(&self, name: &str) -> Result<tokio::sync::OwnedMutexGuard<Live>> {
        self.unlocked()?;
        let live = self.conns.lock().await.get(name).cloned().ok_or(Error::NotConnected)?;
        Ok(live.lock_owned().await)
    }

    async fn insert(&self, name: String, live: Live) {
        self.conns.lock().await.insert(name, Arc::new(Mutex::new(live)));
    }

    /// Adds `live` unless another request opened the session meanwhile; then that one stays and `live` closes.
    async fn insert_new(&self, name: String, live: Live) {
        self.conns.lock().await.entry(name).or_insert_with(|| Arc::new(Mutex::new(live)));
    }

    /// Takes the connection out of the map; it closes once a request still using it finishes.
    async fn remove(&self, name: &str) -> Option<Arc<Mutex<Live>>> {
        self.conns.lock().await.remove(name)
    }

    async fn is_open(&self, name: &str) -> bool {
        self.conns.lock().await.contains_key(name)
    }

    /// Whether any session of the saved connection is open.
    async fn any_open(&self, name: &str) -> bool {
        self.conns.lock().await.keys().any(|k| saved_name(k) == name)
    }

    /// Takes every session of the saved connection out of the map.
    async fn remove_all(&self, name: &str) {
        self.conns.lock().await.retain(|k, _| saved_name(k) != name);
    }
}

fn dir(app: &AppHandle) -> Result<std::path::PathBuf> {
    Ok(app.path().app_config_dir()?)
}

#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
fn list_connections(app: AppHandle) -> Result<Vec<store::Saved>> {
    store::load(&dir(&app)?)
}

/// Leaving a Safe level wants the password (or Touch ID), or changing the level some other way would get around it.
async fn may_change_safety(old: &store::Saved, new: safety::Safety, unlock: Option<&safety::Unlock>) -> Result<()> {
    if !old.safety.wants_password() || new == old.safety {
        return Ok(());
    }
    let unlock = unlock.ok_or(Error::PasswordRequired)?;
    let stored = store::stored(store::Secret::Database, &old.name)?;
    safety::verify(unlock, stored.as_ref()).await
}

/// Saves a connection from the form. Changing a Safe level here wants the password, as the toolbar lock does.
#[tauri::command]
async fn save_connection(
    app: AppHandle,
    open: State<'_, Open>,
    conn: store::Saved,
    password: Option<SecretString>,
    ssh_password: Option<SecretString>,
    previous: Option<String>,
    unlock: Option<safety::Unlock>,
) -> Result<()> {
    let dir = dir(&app)?;
    let old_name = previous.as_deref().unwrap_or(&conn.name);
    if let Some(old) = store::load(&dir)?.into_iter().find(|c| c.name == old_name) {
        may_change_safety(&old, conn.safety, unlock.as_ref()).await?;
    }
    // Drop live connections so the next connect uses the new settings.
    open.remove_all(&conn.name).await;
    if let Some(old) = &previous {
        open.remove_all(old).await;
    }
    store::save(&dir, conn, password.as_ref(), ssh_password.as_ref(), previous.as_deref())
}

#[tauri::command]
async fn delete_connection(app: AppHandle, open: State<'_, Open>, name: String) -> Result<()> {
    open.remove_all(&name).await;
    store::delete(&dir(&app)?, &name)
}

/// Opens a throwaway connection from unsaved form values. A blank password falls back to the one
/// stored for `saved_name` (editing an existing connection); nothing is written.
#[tauri::command]
async fn test_connection(
    open: State<'_, Open>,
    conn: store::Saved,
    password: SecretString,
    ssh_password: SecretString,
    saved_name: Option<String>,
) -> Result<String> {
    open.unlocked()?;
    let fallback = |given: SecretString, which| -> Result<Option<SecretString>> {
        if !given.expose_secret().is_empty() {
            return Ok(Some(given));
        }
        Ok(saved_name.as_deref().map(|name| store::stored(which, name)).transpose()?.flatten())
    };
    let password = fallback(password, store::Secret::Database)?;
    let ssh_secret = if conn.ssh.is_some() { fallback(ssh_password, store::Secret::Ssh)? } else { None };
    if conn.kind == Engine::Redis {
        return redis::test(conn, password, ssh_secret).await;
    }
    let (mut db, _tunnel) = open_db(&conn, password.as_ref(), ssh_secret).await?;
    let n = db.tables(conn.options.system_schemas).await?.len();
    Ok(format!("Connected · {n} tables"))
}

/// Opens (or reuses) a session of a saved connection; `name` is its session key (see [`Open`]). With `database`,
/// the session switches to it.
#[tauri::command]
async fn connect(app: AppHandle, open: State<'_, Open>, name: String, database: Option<String>) -> Result<Info> {
    open.unlocked()?;
    if open.is_open(&name).await {
        let mut live = open.get(&name).await?;
        let r = async {
            if let Some(db) = &database {
                switch_database(&mut live, db).await?;
            }
            info(&mut live).await
        }
        .await;
        match r {
            // Dropped while the workspace was closed: open a fresh one rather than show an error.
            Err(e) if live.conn.lost_by(&e) => {
                drop(live);
                open.remove(&name).await;
            }
            other => return other,
        }
    }
    let mut live = open_saved(&open, &dir(&app)?, &name).await?;
    if let Some(db) = &database {
        switch_database(&mut live, db).await?;
    }
    let r = info(&mut live).await;
    // Two connects can race here (the session wasn't open when both checked); the first one's session stays.
    open.insert_new(name, live).await;
    r
}

/// Opens the connection again from its saved settings and replaces the old one (and its tunnel), which stays when
/// reopening fails. The database chosen after connecting is chosen again.
#[tauri::command]
async fn reconnect(app: AppHandle, open: State<'_, Open>, name: String) -> Result<Info> {
    open.unlocked()?;
    let old = open.conns.lock().await.get(&name).cloned();
    let database = match old {
        // A dead connection can't say.
        Some(old) => old.lock().await.conn.current_database().await.ok().flatten(),
        None => None,
    };
    let mut live = open_saved(&open, &dir(&app)?, &name).await?;
    if let Some(db) = database {
        // A database dropped meanwhile isn't worth failing the reconnect over; the default one stays.
        let _ = switch_database(&mut live, &db).await;
    }
    let r = info(&mut live).await;
    open.insert(name, live).await;
    r
}

/// Safe mode's check for a request on an open connection; see [`safety::check`]. `reads` says whether the request
/// is read-only for the connection's engine. The connection lock is let go before checking, since Touch ID waits for
/// the user. Returns the connection's level.
async fn guard(
    open: &Open,
    name: &str,
    reads: impl FnOnce(Engine) -> bool,
    confirmed: bool,
    unlock: Option<&safety::Unlock>,
) -> Result<safety::Safety> {
    let (level, kind, password) = {
        let live = open.get(name).await?;
        (live.saved.safety, live.saved.kind, live.password.clone())
    };
    safety::check(level, reads(kind), confirmed, unlock, password.as_ref()).await?;
    Ok(level)
}

/// Drops the live connection; the next connect opens a fresh one.
#[tauri::command]
async fn disconnect(open: State<'_, Open>, redis: State<'_, redis::Sessions>, name: String) -> Result<()> {
    redis.remove(&name).await;
    open.remove(&name).await;
    Ok(())
}

#[tauri::command]
async fn columns(open: State<'_, Open>, name: String, table: String) -> Result<Vec<db::Column>> {
    let mut live = open.get(&name).await?;
    let r = live.conn.columns(&table).await;
    live.check(r)
}

#[tauri::command]
async fn list_databases(open: State<'_, Open>, name: String) -> Result<Vec<String>> {
    let mut live = open.get(&name).await?;
    let r = live.conn.databases().await;
    live.check(r)
}

#[tauri::command]
async fn use_database(open: State<'_, Open>, name: String, database: String) -> Result<Info> {
    let mut live = open.get(&name).await?;
    let r = switch_database(&mut live, &database).await;
    live.check(r)?;
    let r = info(&mut live).await;
    live.check(r)
}

async fn switch_database(live: &mut Live, database: &str) -> Result<()> {
    if live.saved.kind == Engine::Postgres {
        let mut u = url::Url::parse(&live.url)?;
        u.set_path(&format!("/{database}")); // set_path percent-encodes
        let url = u.to_string();
        // Borrow only the settings across the await: `Live` holds a SQLite handle, which isn't Sync.
        let (s, port) = (&live.saved, live.tunnel.as_ref().map(|t| t.port));
        let conn = db::Conn::open(s.kind, &url, live.password.as_ref(), &s.tls, &s.options, port).await?; // swap only once it works
        (live.url, live.conn) = (url, conn);
        Ok(())
    } else {
        live.conn.use_database(database).await
    }
}

/// A database call borrowing the connection, as [`with_limit`] runs it.
type ConnCall<'c, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'c>>;

/// Runs `call` on the connection, stopping it on the server if it outlasts `limit`.
async fn with_limit<T>(
    live: &mut Live,
    limit: Option<Duration>,
    call: impl for<'c> FnOnce(&'c mut db::Conn) -> ConnCall<'c, T>,
) -> Result<T> {
    let Some(limit) = limit else { return call(&mut live.conn).await };
    let timed_out = || Error::QueryTimeout(limit.as_secs());
    let Live { saved, url, password, conn, tunnel } = live;
    match conn.stopper() {
        db::Stopper::Lite(handle) => {
            // The query blocks this thread, so a timer thread interrupts it; finishing first drops `done`.
            let (done, finished) = std::sync::mpsc::channel::<()>();
            std::thread::spawn(move || {
                if finished.recv_timeout(limit) == Err(std::sync::mpsc::RecvTimeoutError::Timeout) {
                    handle.interrupt();
                }
            });
            let r = call(conn).await;
            drop(done);
            r.map_err(|e| if db::Conn::interrupted_by(&e) { timed_out() } else { e })
        }
        stopper => {
            let running = call(conn);
            tokio::pin!(running);
            if let Ok(r) = tokio::time::timeout(limit, &mut running).await {
                return r;
            }
            // Best effort: if the stop request fails, the wait below still ends things.
            let _ = stop_query(stopper, saved, url, password.as_ref(), tunnel.as_ref().map(|t| t.port)).await;
            // The server answers the stopped query with an error; wait for it so the connection stays in step.
            let _ = tokio::time::timeout(Duration::from_secs(10), running).await;
            Err(timed_out())
        }
    }
}

fn query_limit(prefs: &Prefs) -> Option<Duration> {
    let secs = prefs.query_timeout_secs.load(Ordering::Relaxed);
    (secs > 0).then(|| Duration::from_secs(secs))
}

async fn stop_query(
    stopper: db::Stopper,
    saved: &store::Saved,
    url: &str,
    password: Option<&SecretString>,
    port: Option<u16>,
) -> Result<()> {
    match stopper {
        db::Stopper::Pg(token) => Ok(token.cancel_query(saved.tls.postgres()?.1).await?),
        db::Stopper::My(id) => {
            // A side connection with the same settings, minus startup SQL, which isn't wanted here.
            let options = db::Options { startup_sql: String::new(), ..saved.options.clone() };
            let mut side = db::Conn::open(saved.kind, url, password, &saved.tls, &options, port).await?;
            side.query(&format!("kill query {id}")).await.map(|_| ())
        }
        db::Stopper::Lite(handle) => {
            handle.interrupt();
            Ok(())
        }
    }
}

/// How the active connection is set up, for the connection info popover.
#[derive(Debug, serde::Serialize)]
struct ConnectionInfo {
    server: String,
    driver: &'static str,
    cipher: Option<String>,
    ssh: Option<String>,
}

#[tauri::command]
async fn connection_info(open: State<'_, Open>, name: String) -> Result<ConnectionInfo> {
    let mut live = open.get(&name).await?;
    let r = async {
        Ok(ConnectionInfo {
            server: live.conn.server_version().await?,
            driver: match live.saved.kind {
                Engine::Postgres => "tokio-postgres",
                Engine::Mysql | Engine::Mariadb => "mysql_async",
                Engine::Sqlite => "rusqlite (bundled SQLite)",
                Engine::Redis => "TableMinus RESP2 client",
            },
            cipher: live.conn.tls_cipher().await?,
            ssh: live.saved.ssh.as_ref().map(|s| s.host.clone()),
        })
    }
    .await;
    live.check(r)
}

/// What editor SQL returned.
#[derive(Debug, serde::Serialize)]
struct Ran {
    /// One per statement, in order.
    results: Vec<db::QueryResult>,
    /// Whether the SQL only reads, so the UI may run it again to load more rows.
    read_only: bool,
}

/// Runs editor SQL. Safe mode applies; see [`guard`].
#[tauri::command]
async fn run_query(
    open: State<'_, Open>,
    prefs: State<'_, Prefs>,
    name: String,
    sql: String,
    confirmed: bool,
    unlock: Option<safety::Unlock>,
    max_rows: Option<u32>,
) -> Result<Ran> {
    let mut read_only = false;
    let reads = |kind| {
        read_only = readonly::is_read_only(kind, &sql);
        read_only
    };
    let level = guard(&open, &name, reads, confirmed, unlock.as_ref()).await?;
    // The level asks before writes and the parse saw none, so nobody was asked: the database makes sure, refusing
    // what the parse can't see (a function that writes). Once the user has said yes, it runs as it is.
    let bounded = read_only && level.asks(false) && !confirmed && unlock.is_none();
    let mut live = open.get(&name).await?;
    let (start, end) = live.conn.read_only_bounds();
    if bounded {
        let r = live.conn.query(start).await;
        live.check(r)?;
    }
    let max = max_rows.map(|n| n as usize);
    let r =
        with_limit(&mut live, query_limit(&prefs), move |c| Box::pin(async move { c.query_results(&sql, max).await }))
            .await;
    if bounded {
        let ended = live.conn.query(end).await;
        if r.is_ok() {
            live.check(ended)?;
        }
    }
    match r {
        // It did try to write: ask as for any write, and the answer sends it again unbounded.
        Err(e) if bounded && db::Conn::refused_as_read_only(&e) => {
            Err(if level.wants_password() { Error::PasswordRequired } else { Error::ConfirmRequired })
        }
        r => Ok(Ran { results: live.check(r)?, read_only }),
    }
}

/// Whether safe mode counts `sql` as only reading. The metrics board asks before running anything else, and never
/// runs it on its own (auto-refresh).
#[tauri::command]
async fn is_read_only(open: State<'_, Open>, name: String, sql: String) -> Result<bool> {
    let kind = open.get(&name).await?.saved.kind;
    Ok(readonly::is_read_only(kind, &sql))
}

/// The most rows the AI assistant's query shows the model.
const AI_ROWS: usize = 50;

/// Runs a query the AI assistant asked for, once the user approved it in the pane (which counts as the safe mode
/// confirmation; password levels still want `unlock`).
#[tauri::command]
async fn ai_run(
    open: State<'_, Open>,
    prefs: State<'_, Prefs>,
    name: String,
    sql: String,
    unlock: Option<safety::Unlock>,
) -> Result<db::QueryResult> {
    refuse_writes(&open, &name, &sql).await?; // before safe mode asks for anything
    guard(&open, &name, |_| true, true, unlock.as_ref()).await?;
    run_read_only(&open, &prefs, &name, &sql, AI_ROWS).await
}

async fn refuse_writes(open: &Open, name: &str, sql: &str) -> Result<()> {
    let kind = open.get(name).await?.saved.kind;
    if readonly::is_read_only(kind, sql) {
        Ok(())
    } else {
        Err(Error::Invalid("only read-only SQL runs here; insert it into the editor to run it"))
    }
}

/// Runs SQL for the assistant or a plugin: only what the parse calls read-only, and inside read-only bounds, so the
/// database refuses a function that writes. At most `max_rows` rows come back. Safe mode is the caller's to check.
pub(crate) async fn run_read_only(
    open: &Open,
    prefs: &Prefs,
    name: &str,
    sql: &str,
    max_rows: usize,
) -> Result<db::QueryResult> {
    refuse_writes(open, name, sql).await?;
    let mut live = open.get(name).await?;
    let (start, end) = live.conn.read_only_bounds();
    let r = live.conn.query(start).await;
    live.check(r)?;
    let sql = sql.to_string();
    let r = with_limit(&mut live, query_limit(prefs), move |c| {
        Box::pin(async move { c.query_limited(&sql, Some(max_rows)).await })
    })
    .await;
    let ended = live.conn.query(end).await;
    if r.is_ok() {
        live.check(ended)?;
    }
    match r {
        Err(e) if db::Conn::refused_as_read_only(&e) => Err(Error::Invalid("the database refused it as a write")),
        r => live.check(r),
    }
}

/// The largest SQL file the editor opens.
const MAX_SQL_FILE: u64 = 20 * 1024 * 1024;

/// A text file chosen in the open dialog, for a SQL tab.
#[tauri::command]
async fn read_text_file(path: std::path::PathBuf) -> Result<String> {
    let failed = |source| Error::File { path: path.display().to_string(), source };
    let size = std::fs::metadata(&path).map_err(failed)?.len();
    if size > MAX_SQL_FILE {
        return Err(Error::Invalid("the file is over 20 MB; run it with the SQL import instead"));
    }
    let bytes = std::fs::read(&path).map_err(failed)?;
    String::from_utf8(bytes).map_err(|_| Error::Invalid("the file isn't UTF-8 text"))
}

/// Saves a SQL tab to the path chosen in the save dialog: written to a temporary file, then renamed over.
#[tauri::command]
async fn write_text_file(path: std::path::PathBuf, text: String) -> Result<()> {
    let failed = |source| Error::File { path: path.display().to_string(), source };
    let mut tmp = path.clone().into_os_string();
    tmp.push(".tableminus-tmp");
    std::fs::write(&tmp, text).map_err(failed)?;
    std::fs::rename(&tmp, &path).map_err(failed)
}

/// Every table's columns, for editor autocomplete: (table, column) pairs.
#[tauri::command]
async fn schema(open: State<'_, Open>, name: String) -> Result<Vec<(String, String)>> {
    let mut live = open.get(&name).await?;
    let r = live.conn.schema().await;
    live.check(r)
}

/// One page of a table, filtered and sorted; see [`browse::Browse`].
#[tauri::command]
async fn browse(
    open: State<'_, Open>,
    prefs: State<'_, Prefs>,
    name: String,
    request: browse::Browse,
) -> Result<browse::Page> {
    let mut live = open.get(&name).await?;
    let r =
        with_limit(&mut live, query_limit(&prefs), move |c| Box::pin(async move { c.browse(&request).await })).await;
    live.check(r)
}

/// A table's full structure for the structure view; see [`structure::Structure`].
#[tauri::command]
async fn table_structure(open: State<'_, Open>, name: String, table: String) -> Result<structure::Structure> {
    let mut live = open.get(&name).await?;
    let r = live.conn.structure(&table).await;
    live.check(r)
}

/// The statements a structure edit would run, for the preview; `create` makes `table` as a new table.
#[tauri::command]
async fn structure_sql(
    open: State<'_, Open>,
    name: String,
    table: String,
    create: bool,
    changes: Vec<ddl::Change>,
) -> Result<Vec<String>> {
    let mut live = open.get(&name).await?;
    let r = live.conn.alter_sql(&table, create, &changes).await;
    live.check(r)
}

/// The filter bar's "SQL": the page a browse request reads, with its values written in as literals for display.
#[tauri::command]
async fn filter_sql(open: State<'_, Open>, name: String, request: browse::Browse) -> Result<String> {
    let mut live = open.get(&name).await?;
    let r = live.conn.filter_sql(&request).await;
    live.check(r)
}

/// The statements a table operation (rename, clone, truncate, drop…) would run, for the confirmation.
#[tauri::command]
async fn table_op_sql(open: State<'_, Open>, name: String, op: ddl::TableOp) -> Result<Vec<String>> {
    let mut live = open.get(&name).await?;
    let r = live.conn.op_sql(&op).await;
    live.check(r)
}

/// The statements a database operation (create, rename, drop) would run, for the preview.
#[tauri::command]
async fn database_sql(open: State<'_, Open>, name: String, op: ddl::DbOp) -> Result<Vec<String>> {
    let mut live = open.get(&name).await?;
    let r = live.conn.database_sql(&op).await;
    live.check(r)
}

/// The encodings and collations for the new database form.
#[tauri::command]
async fn database_options(open: State<'_, Open>, name: String) -> Result<ddl::DbOptions> {
    let mut live = open.get(&name).await?;
    let r = live.conn.database_options().await;
    live.check(r)
}

/// Runs the statements the user saw in a preview. Safe mode applies (see [`guard`]); applying from the preview
/// counts as confirmed, but the Safe levels still want the password.
#[tauri::command]
async fn run_ddl(
    open: State<'_, Open>,
    name: String,
    statements: Vec<String>,
    confirmed: bool,
    unlock: Option<safety::Unlock>,
    restore: Option<Vec<String>>,
) -> Result<()> {
    guard(&open, &name, |_| false, confirmed, unlock.as_ref()).await?;
    let mut live = open.get(&name).await?;
    let r = live.conn.run_ddl_or_restore(&statements, restore.as_deref().unwrap_or_default()).await;
    live.check(r)
}

/// Changes a connection's safe mode level (the toolbar lock). Leaving a Safe level wants the password, or the lock
/// would be a way around it.
#[tauri::command]
async fn set_safety(
    app: AppHandle,
    open: State<'_, Open>,
    redis: State<'_, redis::Sessions>,
    name: String,
    safety: safety::Safety,
    unlock: Option<safety::Unlock>,
) -> Result<()> {
    open.unlocked()?;
    let dir = dir(&app)?;
    let mut saved =
        store::load(&dir)?.into_iter().find(|c| c.name == name).ok_or_else(|| Error::NoSuchConnection(name.clone()))?;
    may_change_safety(&saved, safety, unlock.as_ref()).await?;
    saved.safety = safety;
    store::save(&dir, saved, None, None, None)?;
    // Every session of the connection, the map's lock let go before waiting on theirs.
    let sessions: Vec<_> =
        open.conns.lock().await.iter().filter(|(k, _)| saved_name(k) == name).map(|(_, l)| l.clone()).collect();
    for live in sessions {
        live.lock().await.saved.safety = safety;
    }
    redis.set_safety(&name, safety).await;
    Ok(())
}

/// The CREATE statement of a table or view.
#[tauri::command]
async fn table_ddl(open: State<'_, Open>, name: String, table: String) -> Result<String> {
    let mut live = open.get(&name).await?;
    let r = live.conn.ddl(&table).await;
    live.check(r)
}

/// The sidebar's items other than tables: schemas, views, functions, sequences, types.
#[tauri::command]
async fn items(open: State<'_, Open>, name: String) -> Result<Vec<catalog::Item>> {
    let mut live = open.get(&name).await?;
    let system = live.saved.options.system_schemas;
    let r = live.conn.items(system).await;
    live.check(r)
}

/// "Search in database", one table at a time so the UI can stop between tables and other commands get the
/// connection in between: how many rows match, up to [`catalog::SEARCH_CAP`].
#[tauri::command]
async fn search_table(
    open: State<'_, Open>,
    prefs: State<'_, Prefs>,
    name: String,
    table: String,
    mode: catalog::SearchMode,
    text: String,
) -> Result<u64> {
    let mut live = open.get(&name).await?;
    let r = with_limit(&mut live, query_limit(&prefs), move |c| {
        Box::pin(async move { c.search_count(&table, mode, &text).await })
    })
    .await;
    live.check(r)
}

/// Rows and size of a table or view, for the sidebar's Overview.
#[tauri::command]
async fn item_overview(open: State<'_, Open>, name: String, table: String) -> Result<catalog::Overview> {
    let mut live = open.get(&name).await?;
    let r = live.conn.overview(&table).await;
    live.check(r)
}

/// A schema's tables, columns and foreign keys for the ER diagram; `schema` is Postgres's (None: the current one).
#[tauri::command]
async fn diagram(open: State<'_, Open>, name: String, schema: Option<String>) -> Result<diagram::Diagram> {
    let mut live = open.get(&name).await?;
    let r = live.conn.diagram(schema.as_deref()).await;
    live.check(r)
}

/// Starts the app. Returns only when it quits.
///
/// # Errors
/// When the window or webview can't be created.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
#[expect(clippy::too_many_lines, reason = "the command list, one line per command")]
pub fn run() -> tauri::Result<()> {
    let (open, prefs) = (Open::default(), Prefs::default());
    open.locked.store(true, Ordering::SeqCst); // until lock_at_startup knows whether a passcode is set
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(open.clone())
        .manage(prefs.clone())
        .manage(backup::Jobs::default())
        .manage(processes::Sessions::default())
        .manage(jobs::Jobs::default())
        .manage(acp::Agents::default())
        .manage(redis::Sessions::default())
        .manage(launcher::Opened::default())
        .setup(|app| {
            secrets::init(&app.path().app_config_dir()?);
            tauri::async_runtime::spawn(keep_alive(app.handle().clone()));
            tauri::async_runtime::spawn(passcode::lock_at_startup(open.clone()));
            mcp::setup(app.handle(), open, prefs)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_connections,
            save_connection,
            delete_connection,
            test_connection,
            connect,
            reconnect,
            connection_info,
            set_preferences,
            list_databases,
            use_database,
            columns,
            disconnect,
            run_query,
            browse,
            filter_sql,
            schema,
            read_text_file,
            write_text_file,
            is_read_only,
            table_structure,
            structure_sql,
            table_op_sql,
            database_sql,
            database_options,
            run_ddl,
            table_ddl,
            rows::save_rows,
            rows::rows_sql,
            rows::cell_bytes,
            rows::enum_values,
            items,
            search_table,
            item_overview,
            diagram,
            diagram::write_image,
            console::console_log,
            console::console_clear,
            set_safety,
            safety::touch_id_available,
            safety::batch_grant,
            mcp::mcp_install,
            ai::ai_chat,
            ai::ai_has_key,
            ai::ai_set_key,
            ai_run,
            secrets::vault_status,
            secrets::vault_unlock,
            secrets::vault_reset,
            themes::theme_git_fetch,
            acp::acp_start,
            acp::acp_prompt,
            acp::acp_cancel,
            acp::acp_permission,
            acp::acp_stop,
            plugins::plugin_list,
            plugins::plugin_fetch,
            plugins::plugin_install,
            plugins::plugin_remove,
            plugins::plugin_enable,
            plugins::plugin_run,
            redis::redis_connect,
            redis::redis_databases,
            redis::redis_scan,
            redis::redis_value,
            redis::redis_edit,
            redis::redis_command,
            redis::redis_connection_info,
            passcode::passcode_set,
            passcode::set_passcode,
            passcode::remove_passcode,
            passcode::lock,
            passcode::unlock,
            passcode::unlock_with_touch_id,
            passcode::forgot_passcode,
            locations::locations,
            locations::set_location,
            locations::read_doc,
            locations::write_doc,
            backup::backup_setup,
            backup::start_backup,
            backup::start_restore,
            backup::cancel_backup,
            processes::processes,
            processes::kill_process,
            processes::close_processes,
            mcp::mcp_status,
            mcp::mcp_configure,
            mcp::mcp_create_token,
            mcp::mcp_revoke_token,
            mcp::mcp_activity,
            mcp::mcp_clear_activity,
            mcp::mcp_clients,
            mcp::mcp_answer,
            jobs::start_export,
            jobs::start_import,
            jobs::start_sql_import,
            jobs::preview_import,
            jobs::cancel_job,
            launcher::launcher,
            launcher::duplicate_connection,
            launcher::move_to_group,
            launcher::save_group,
            launcher::delete_group,
            launcher::save_tag,
            launcher::delete_tag,
            launcher::set_session_password,
            launcher::export_connections,
            launcher::import_connections,
            launcher::take_opened,
            users::users,
            users::user_privileges,
            users::user_sql,
            users::apply_user_edit,
            objects::item_script,
            objects::drop_item_sql,
            objects::view_definition,
            objects::save_view_sql,
            objects::routine_definitions,
            objects::new_routine,
            objects::save_routine_sql,
            objects::sequence,
            objects::save_sequence_sql
        ])
        .build(tauri::generate_context!())?
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                acp::stop_all(&app.state::<acp::Agents>());
            }
            launcher::on_event(app, event);
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Live, with_limit};
    use std::time::Duration as D;

    async fn query_with_limit(
        live: &mut Live,
        sql: &str,
        limit: Option<D>,
    ) -> crate::error::Result<crate::db::QueryResult> {
        let sql = sql.to_string();
        with_limit(live, limit, move |c| Box::pin(async move { c.query(&sql).await })).await
    }
    use crate::db::{Conn, Engine, Options};
    use crate::error::Error;
    use crate::{store, tls::Tls};
    use std::time::{Duration, Instant};

    async fn live(engine: Engine, url: &str) -> Live {
        let saved = store::Saved {
            name: "t".into(),
            kind: engine,
            url: url.into(),
            tag: String::new(),
            safety: crate::safety::Safety::Silent,
            ssh: None,
            tls: Tls::default(),
            options: Options::default(),
            password_mode: crate::store::PasswordMode::Keychain,
            color: None,
            group: String::new(),
        };
        let conn = Conn::open(engine, url, None, &saved.tls, &saved.options, None).await.unwrap();
        Live { url: url.into(), saved, password: None, conn, tunnel: None }
    }

    #[test]
    fn guard_should_apply_the_open_connections_level() {
        use crate::safety::{Safety, Unlock};
        tauri::async_runtime::block_on(async {
            let mut l = live(Engine::Sqlite, ":memory:").await;
            l.saved.safety = Safety::Safe2;
            l.password = Some("pw".into());
            let open = super::Open::default();
            open.insert("t".into(), l).await;
            let reads = |sql: &'static str| move |kind| crate::readonly::is_read_only(kind, sql);
            assert!(super::guard(&open, "t", reads("select 1"), false, None).await.is_ok());
            let write = super::guard(&open, "t", reads("delete from x"), true, None).await;
            assert!(matches!(write, Err(Error::PasswordRequired)), "confirming isn't enough at Safe 2");
            let wrong = Unlock::Password("nope".into());
            assert!(matches!(
                super::guard(&open, "t", reads("delete from x"), true, Some(&wrong)).await,
                Err(Error::WrongPassword)
            ));
            let right = Unlock::Password("pw".into());
            assert!(super::guard(&open, "t", reads("delete from x"), true, Some(&right)).await.is_ok());
            assert!(matches!(
                super::guard(&open, "gone", reads("select 1"), true, None).await,
                Err(Error::NotConnected)
            ));
        });
    }

    #[test]
    fn may_change_safety_should_want_the_password_only_to_leave_a_safe_level() {
        use crate::safety::Safety;
        tauri::async_runtime::block_on(async {
            let mut old = live(Engine::Sqlite, ":memory:").await.saved;
            old.safety = Safety::Safe2;
            let leaving = super::may_change_safety(&old, Safety::Silent, None).await;
            assert!(matches!(leaving, Err(Error::PasswordRequired)));
            assert!(super::may_change_safety(&old, Safety::Safe2, None).await.is_ok(), "unchanged");
            old.safety = Safety::Silent;
            assert!(super::may_change_safety(&old, Safety::Safe1, None).await.is_ok(), "entering one is free");
        });
    }

    #[test]
    fn open_should_key_sessions_by_connection_and_remove_them_together() {
        tauri::async_runtime::block_on(async {
            assert_eq!(super::saved_name("prod\u{1f}2"), "prod");
            assert_eq!(super::saved_name("prod"), "prod");
            let open = super::Open::default();
            for key in ["prod", "prod\u{1f}2", "prod 2"] {
                open.insert(key.into(), live(Engine::Sqlite, ":memory:").await).await;
            }
            open.remove("prod").await;
            assert!(open.any_open("prod").await, "its second session is still open");
            assert!(!open.is_open("prod").await);
            open.remove_all("prod").await;
            assert!(!open.any_open("prod").await);
            assert!(open.is_open("prod 2").await, "another connection's name only looks alike");
        });
    }

    #[test]
    fn insert_new_should_keep_the_session_that_was_there_first() {
        tauri::async_runtime::block_on(async {
            let open = super::Open::default();
            open.insert_new("a".into(), live(Engine::Sqlite, ":memory:").await).await;
            let mut late = live(Engine::Sqlite, ":memory:").await;
            late.url = "late".into();
            open.insert_new("a".into(), late).await;
            assert_eq!(open.get("a").await.unwrap().url, ":memory:");
        });
    }

    #[test]
    fn open_should_let_one_connection_work_while_another_is_busy() {
        tauri::async_runtime::block_on(async {
            let open = super::Open::default();
            open.insert("a".into(), live(Engine::Sqlite, ":memory:").await).await;
            open.insert("b".into(), live(Engine::Sqlite, ":memory:").await).await;
            let _busy = open.get("a").await.unwrap();
            let other = tokio::time::timeout(Duration::from_secs(1), open.get("b")).await;
            assert!(other.is_ok_and(|g| g.is_ok()), "b waited on a");
        });
    }

    #[test]
    fn open_should_refuse_every_request_while_locked() {
        tauri::async_runtime::block_on(async {
            let open = super::Open::default();
            open.insert("a".into(), live(Engine::Sqlite, ":memory:").await).await;
            open.locked.store(true, std::sync::atomic::Ordering::SeqCst);
            assert!(matches!(open.get("a").await, Err(Error::Locked)));
            let dir = std::env::temp_dir();
            assert!(matches!(super::open_saved(&open, &dir, "a").await, Err(Error::Locked)));
        });
    }

    #[test]
    fn text_files_should_round_trip_and_refuse_huge_or_binary_ones() {
        tauri::async_runtime::block_on(async {
            let dir = std::env::temp_dir().join(format!("tableminus-{}-files", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("q.sql");
            super::write_text_file(path.clone(), "select 'é';\n".into()).await.unwrap();
            assert_eq!(super::read_text_file(path.clone()).await.unwrap(), "select 'é';\n");

            let big = dir.join("big.sql");
            std::fs::write(&big, vec![b' '; 21 * 1024 * 1024]).unwrap();
            assert!(super::read_text_file(big).await.unwrap_err().to_string().contains("over 20 MB"));

            let binary = dir.join("bin.sql");
            std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
            assert_eq!(super::read_text_file(binary).await.unwrap_err().to_string(), "the file isn't UTF-8 text");
            let _ = std::fs::remove_dir_all(dir);
        });
    }

    #[test]
    fn query_with_limit_should_interrupt_a_runaway_sqlite_query() {
        tauri::async_runtime::block_on(async {
            let mut live = live(Engine::Sqlite, ":memory:").await;
            let endless = "with recursive c(x) as (select 1 union all select x + 1 from c) select count(*) from c";
            let started = Instant::now();
            let err = query_with_limit(&mut live, endless, Some(Duration::from_millis(200))).await.unwrap_err();
            assert!(matches!(err, Error::QueryTimeout(0)), "{err}");
            assert!(started.elapsed() < Duration::from_secs(5));
            assert_eq!(live.conn.query("select 1").await.unwrap().rows[0][0].as_deref(), Some("1"));
        });
    }

    #[test]
    fn query_with_limit_should_leave_fast_queries_alone() {
        tauri::async_runtime::block_on(async {
            let mut live = live(Engine::Sqlite, ":memory:").await;
            let r = query_with_limit(&mut live, "select 2", Some(Duration::from_secs(5))).await.unwrap();
            assert_eq!(r.rows[0][0].as_deref(), Some("2"));
        });
    }

    #[test]
    #[ignore = "needs a local MySQL"]
    fn query_with_limit_should_kill_a_slow_mysql_query_and_keep_the_connection() {
        tauri::async_runtime::block_on(async {
            let mut live = live(Engine::Mysql, "mysql://root@127.0.0.1:3306").await;
            let started = Instant::now();
            let err = query_with_limit(&mut live, "select sleep(30)", Some(Duration::from_secs(1))).await.unwrap_err();
            assert!(matches!(err, Error::QueryTimeout(1)), "{err}");
            assert!(started.elapsed() < Duration::from_secs(10), "killed, not waited out");
            assert_eq!(live.conn.query("select 1").await.unwrap().rows[0][0].as_deref(), Some("1"));
        });
    }
}

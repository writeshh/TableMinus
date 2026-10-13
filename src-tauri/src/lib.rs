//! DB Manager's Tauri commands: the only surface the UI can call.

mod db;
mod error;
mod readonly;
mod ssh;
mod store;
mod tls;

use db::Engine;
use error::{Error, Result};
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use tauri::{AppHandle, Manager, State, async_runtime::Mutex};

struct Live {
    saved: store::Saved,
    url: SecretString, // with password; kept in memory only, to reconnect when Postgres switches database
    conn: db::Conn,
    tunnel: Option<ssh::Tunnel>, // lives exactly as long as the connection
}

/// Runs the startup script, opens the SSH tunnel if there is one, then opens the database.
async fn open_db(
    saved: &store::Saved,
    url: &SecretString,
    ssh_secret: Option<SecretString>,
) -> Result<(db::Conn, Option<ssh::Tunnel>)> {
    if !saved.options.startup_script.trim().is_empty() {
        db::run_startup_script(&saved.options.startup_script).await?;
    }
    let tunnel = match &saved.ssh {
        Some(ssh) => Some(ssh::open_for(saved.kind, url, ssh, ssh_secret.as_ref()).await?),
        None => None,
    };
    let conn = db::Conn::open(saved.kind, url, &saved.tls, &saved.options, tunnel.as_ref().map(|t| t.port)).await?;
    Ok((conn, tunnel))
}

/// Opens a saved connection by name, with its keychain secrets.
async fn open_saved(app: &AppHandle, name: &str) -> Result<Live> {
    let saved = store::load(&dir(app)?)?
        .into_iter()
        .find(|c| c.name == name)
        .ok_or_else(|| Error::NoSuchConnection(name.to_string()))?;
    let url = store::connect_url(&saved)?;
    let ssh_secret = if saved.ssh.is_some() { store::stored(store::Secret::Ssh, &saved.name)? } else { None };
    let (conn, tunnel) = open_db(&saved, &url, ssh_secret).await?;
    Ok(Live { saved, url, conn, tunnel })
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

// ponytail: one lock for all open connections serialises queries across them, and SQLite's blocking calls
// run on the async runtime; per-connection locks plus spawn_blocking if either hurts.
#[derive(Default)]
struct Open(Mutex<HashMap<String, Live>>);

fn dir(app: &AppHandle) -> Result<std::path::PathBuf> {
    Ok(app.path().app_config_dir()?)
}

#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
fn list_connections(app: AppHandle) -> Result<Vec<store::Saved>> {
    store::load(&dir(&app)?)
}

#[tauri::command]
async fn save_connection(
    app: AppHandle,
    open: State<'_, Open>,
    conn: store::Saved,
    password: Option<SecretString>,
    ssh_password: Option<SecretString>,
    previous: Option<String>,
) -> Result<()> {
    let mut open = open.0.lock().await; // drop live connections so the next connect uses the new settings
    open.remove(&conn.name);
    if let Some(old) = &previous {
        open.remove(old);
    }
    store::save(&dir(&app)?, conn, password.as_ref(), ssh_password.as_ref(), previous.as_deref())
}

#[tauri::command]
async fn delete_connection(app: AppHandle, open: State<'_, Open>, name: String) -> Result<()> {
    open.0.lock().await.remove(&name);
    store::delete(&dir(&app)?, &name)
}

/// Opens a throwaway connection from unsaved form values. A blank password falls back to the one
/// stored for `saved_name` (editing an existing connection); nothing is written.
#[tauri::command]
async fn test_connection(
    conn: store::Saved,
    password: SecretString,
    ssh_password: SecretString,
    saved_name: Option<String>,
) -> Result<String> {
    let fallback = |given: SecretString, which| -> Result<Option<SecretString>> {
        if !given.expose_secret().is_empty() {
            return Ok(Some(given));
        }
        Ok(saved_name.as_deref().map(|name| store::stored(which, name)).transpose()?.flatten())
    };
    let url = match fallback(password, store::Secret::Database)? {
        Some(pw) => store::with_password(&conn.url, &pw)?,
        None => conn.url.clone().into(),
    };
    let ssh_secret = if conn.ssh.is_some() { fallback(ssh_password, store::Secret::Ssh)? } else { None };
    let (mut db, _tunnel) = open_db(&conn, &url, ssh_secret).await?;
    let n = db.tables(conn.options.system_schemas).await?.len();
    Ok(format!("Connected · {n} tables"))
}

/// Opens (or reuses) the saved connection.
#[tauri::command]
async fn connect(app: AppHandle, open: State<'_, Open>, name: String) -> Result<Info> {
    let mut open = open.0.lock().await;
    let live = match open.entry(name) {
        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
        std::collections::hash_map::Entry::Vacant(e) => {
            let live = open_saved(&app, e.key()).await?;
            e.insert(live)
        }
    };
    info(live).await
}

/// Closes the connection (and its tunnel) and opens it again from its saved settings. The database chosen
/// after connecting is chosen again.
#[tauri::command]
async fn reconnect(app: AppHandle, open: State<'_, Open>, name: String) -> Result<Info> {
    let mut open = open.0.lock().await;
    let database = match open.remove(&name) {
        Some(mut old) => old.conn.current_database().await.ok().flatten(), // a dead connection can't say
        None => None,
    };
    let mut live = open_saved(&app, &name).await?;
    if let Some(db) = database {
        // A database dropped meanwhile isn't worth failing the reconnect over; the default one stays.
        let _ = switch_database(&mut live, &db).await;
    }
    info(open.entry(name).or_insert(live)).await
}

/// Saves inline grid edits. Safe mode applies: [`Error::ConfirmRequired`] until resent with `confirmed: true`.
#[tauri::command]
async fn update_rows(
    open: State<'_, Open>,
    name: String,
    table: String,
    edits: Vec<db::RowEdit>,
    confirmed: bool,
) -> Result<u64> {
    let mut open = open.0.lock().await;
    let live = open.get_mut(&name).ok_or(Error::NotConnected)?;
    if live.saved.confirm_writes && !confirmed {
        return Err(Error::ConfirmRequired);
    }
    live.conn.update_rows(&table, &edits).await
}

/// Drops the live connection; the next connect opens a fresh one.
#[tauri::command]
async fn disconnect(open: State<'_, Open>, name: String) -> Result<()> {
    open.0.lock().await.remove(&name);
    Ok(())
}

#[tauri::command]
async fn columns(open: State<'_, Open>, name: String, table: String) -> Result<Vec<db::Column>> {
    open.0.lock().await.get_mut(&name).ok_or(Error::NotConnected)?.conn.columns(&table).await
}

#[tauri::command]
async fn list_databases(open: State<'_, Open>, name: String) -> Result<Vec<String>> {
    open.0.lock().await.get_mut(&name).ok_or(Error::NotConnected)?.conn.databases().await
}

#[tauri::command]
async fn use_database(open: State<'_, Open>, name: String, database: String) -> Result<Info> {
    let mut open = open.0.lock().await;
    let live = open.get_mut(&name).ok_or(Error::NotConnected)?;
    switch_database(live, &database).await?;
    info(live).await
}

async fn switch_database(live: &mut Live, database: &str) -> Result<()> {
    if live.saved.kind == Engine::Postgres {
        let mut u = url::Url::parse(live.url.expose_secret())?;
        u.set_path(&format!("/{database}")); // set_path percent-encodes
        let url = SecretString::from(u.to_string());
        // Borrow only the settings across the await: `Live` holds a SQLite handle, which isn't Sync.
        let (s, port) = (&live.saved, live.tunnel.as_ref().map(|t| t.port));
        let conn = db::Conn::open(s.kind, &url, &s.tls, &s.options, port).await?; // swap only once it works
        (live.url, live.conn) = (url, conn);
        Ok(())
    } else {
        live.conn.use_database(database).await
    }
}

/// Safe mode: with `confirm_writes` on, anything not provably read-only fails with
/// [`Error::ConfirmRequired`] until it is resent with `confirmed: true`.
#[tauri::command]
async fn run_query(open: State<'_, Open>, name: String, sql: String, confirmed: bool) -> Result<db::QueryResult> {
    let mut open = open.0.lock().await;
    let live = open.get_mut(&name).ok_or(Error::NotConnected)?;
    if live.saved.confirm_writes && !confirmed && !readonly::is_read_only(live.saved.kind, &sql) {
        return Err(Error::ConfirmRequired);
    }
    live.conn.query(&sql).await
}

/// Starts the app. Returns only when it quits.
///
/// # Errors
/// When the window or webview can't be created.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> tauri::Result<()> {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(Open::default())
        .invoke_handler(tauri::generate_handler![
            list_connections,
            save_connection,
            delete_connection,
            test_connection,
            connect,
            reconnect,
            list_databases,
            use_database,
            columns,
            disconnect,
            update_rows,
            run_query
        ])
        .run(tauri::generate_context!())
}

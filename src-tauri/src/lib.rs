//! DB Manager's Tauri commands: the only surface the UI can call.

mod db;
mod error;
mod readonly;
mod store;

use db::Engine;
use error::{Error, Result};
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use tauri::{AppHandle, Manager, State, async_runtime::Mutex};

struct Live {
    engine: Engine,
    confirm_writes: bool,
    url: SecretString, // with password; kept in memory only, to reconnect when Postgres switches database
    conn: db::Conn,
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
    let file_based = live.engine == Engine::Sqlite;
    // No database selected yet (MySQL connected without one): nothing to list.
    let tables = if database.is_none() && !file_based { vec![] } else { live.conn.tables().await? };
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
    previous: Option<String>,
) -> Result<()> {
    let mut open = open.0.lock().await; // drop live connections so the next connect uses the new settings
    open.remove(&conn.name);
    if let Some(old) = &previous {
        open.remove(old);
    }
    store::save(&dir(&app)?, conn, password.as_ref(), previous.as_deref())
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
    kind: Engine,
    url: String,
    password: SecretString,
    saved_name: Option<String>,
) -> Result<String> {
    let url = match saved_name.filter(|_| password.expose_secret().is_empty()) {
        Some(name) => store::connect_url(&store::Saved { name, kind, url, tag: String::new(), confirm_writes: true })?,
        None => store::with_password(&url, &password)?,
    };
    let mut conn = db::Conn::open(kind, &url).await?;
    let n = conn.tables().await?.len();
    Ok(format!("Connected · {n} tables"))
}

/// Opens (or reuses) the saved connection.
#[tauri::command]
async fn connect(app: AppHandle, open: State<'_, Open>, name: String) -> Result<Info> {
    let mut open = open.0.lock().await;
    let live = match open.entry(name) {
        std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
        std::collections::hash_map::Entry::Vacant(e) => {
            let saved = store::load(&dir(&app)?)?
                .into_iter()
                .find(|c| c.name == *e.key())
                .ok_or_else(|| Error::NoSuchConnection(e.key().clone()))?;
            let url = store::connect_url(&saved)?;
            let conn = db::Conn::open(saved.kind, &url).await?;
            e.insert(Live { engine: saved.kind, confirm_writes: saved.confirm_writes, url, conn })
        }
    };
    info(live).await
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
    if live.confirm_writes && !confirmed {
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
    if live.engine == Engine::Postgres {
        let mut u = url::Url::parse(live.url.expose_secret())?;
        u.set_path(&format!("/{database}")); // set_path percent-encodes
        let url = SecretString::from(u.to_string());
        let conn = db::Conn::open(live.engine, &url).await?; // swap only once the new one works
        (live.url, live.conn) = (url, conn);
    } else {
        live.conn.use_database(&database).await?;
    }
    info(live).await
}

/// Safe mode: with `confirm_writes` on, anything not provably read-only fails with
/// [`Error::ConfirmRequired`] until it is resent with `confirmed: true`.
#[tauri::command]
async fn run_query(open: State<'_, Open>, name: String, sql: String, confirmed: bool) -> Result<db::QueryResult> {
    let mut open = open.0.lock().await;
    let live = open.get_mut(&name).ok_or(Error::NotConnected)?;
    if live.confirm_writes && !confirmed && !readonly::is_read_only(live.engine, &sql) {
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
        .manage(Open::default())
        .invoke_handler(tauri::generate_handler![
            list_connections,
            save_connection,
            delete_connection,
            test_connection,
            connect,
            list_databases,
            use_database,
            columns,
            disconnect,
            update_rows,
            run_query
        ])
        .run(tauri::generate_context!())
}

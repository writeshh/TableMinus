mod db;
mod readonly;
mod store;

use std::collections::HashMap;
use tauri::{async_runtime::Mutex, AppHandle, Manager, State};

struct Live {
    kind: String,
    confirm_writes: bool,
    url: String, // with password; kept in memory only, to reconnect when Postgres switches database
    conn: db::Conn,
}

/// What the workspace header and sidebar show for an open connection.
#[derive(serde::Serialize)]
struct Info {
    server: String,
    database: Option<String>,
    can_switch: bool,
    tables: Vec<String>,
}

async fn info(live: &mut Live) -> Result<Info, String> {
    let database = live.conn.current_database().await?;
    // No database selected yet (MySQL connected without one): nothing to list.
    let tables = if database.is_none() && live.kind != "sqlite" { vec![] } else { live.conn.tables().await? };
    Ok(Info { server: live.conn.server_version().await?, database, can_switch: live.kind != "sqlite", tables })
}

// ponytail: one lock for all open connections serialises queries across them; per-connection locks if that hurts.
#[derive(Default)]
struct Open(Mutex<HashMap<String, Live>>);

fn dir(app: &AppHandle) -> std::path::PathBuf {
    app.path().app_config_dir().expect("no config dir")
}

#[tauri::command]
fn list_connections(app: AppHandle) -> Vec<store::Saved> {
    store::load(dir(&app))
}

#[tauri::command]
async fn save_connection(
    app: AppHandle,
    open: State<'_, Open>,
    conn: store::Saved,
    password: Option<String>,
    previous: Option<String>,
) -> Result<(), String> {
    let mut open = open.0.lock().await; // drop live connections so the next connect uses the new settings
    open.remove(&conn.name);
    if let Some(old) = &previous {
        open.remove(old);
    }
    store::save(dir(&app), conn, password, previous)
}

#[tauri::command]
async fn delete_connection(app: AppHandle, open: State<'_, Open>, name: String) -> Result<(), String> {
    open.0.lock().await.remove(&name);
    store::delete(dir(&app), &name)
}

/// Opens a throwaway connection from unsaved form values. A blank password falls back to the one
/// stored for `saved_name` (editing an existing connection); nothing is written.
#[tauri::command]
async fn test_connection(kind: String, url: String, password: String, saved_name: Option<String>) -> Result<String, String> {
    let url = match saved_name.filter(|_| password.is_empty()) {
        Some(name) => store::connect_url(&store::Saved { name, kind: kind.clone(), url, tag: String::new(), confirm_writes: true })?,
        None => store::with_password(&url, &password)?,
    };
    let mut conn = db::Conn::open(&kind, &url).await?;
    let n = conn.tables().await?.len();
    Ok(format!("Connected · {n} tables"))
}

/// Opens (or reuses) the saved connection.
#[tauri::command]
async fn connect(app: AppHandle, open: State<'_, Open>, name: String) -> Result<Info, String> {
    let mut open = open.0.lock().await;
    if !open.contains_key(&name) {
        let saved = store::load(dir(&app)).into_iter().find(|c| c.name == name).ok_or("no such connection")?;
        let url = store::connect_url(&saved)?;
        let conn = db::Conn::open(&saved.kind, &url).await?;
        open.insert(name.clone(), Live { kind: saved.kind, confirm_writes: saved.confirm_writes, url, conn });
    }
    info(open.get_mut(&name).unwrap()).await
}

/// Drops the live connection; the next connect opens a fresh one.
#[tauri::command]
async fn disconnect(open: State<'_, Open>, name: String) -> Result<(), String> {
    open.0.lock().await.remove(&name);
    Ok(())
}

#[tauri::command]
async fn columns(open: State<'_, Open>, name: String, table: String) -> Result<Vec<db::Column>, String> {
    open.0.lock().await.get_mut(&name).ok_or("not connected")?.conn.columns(&table).await
}

#[tauri::command]
async fn list_databases(open: State<'_, Open>, name: String) -> Result<Vec<String>, String> {
    open.0.lock().await.get_mut(&name).ok_or("not connected")?.conn.databases().await
}

#[tauri::command]
async fn use_database(open: State<'_, Open>, name: String, database: String) -> Result<Info, String> {
    let mut open = open.0.lock().await;
    let live = open.get_mut(&name).ok_or("not connected")?;
    if live.kind == "postgres" {
        let mut u = url::Url::parse(&live.url).map_err(|e| e.to_string())?;
        u.set_path(&format!("/{database}")); // set_path percent-encodes
        let conn = db::Conn::open(&live.kind, u.as_str()).await?; // swap only once the new one works
        (live.url, live.conn) = (u.to_string(), conn);
    } else {
        live.conn.use_database(&database).await?;
    }
    info(live).await
}

/// Safe mode: with confirm_writes on, anything not provably read-only fails with "CONFIRM"
/// until it is resent with `confirmed: true`.
#[tauri::command]
async fn run_query(open: State<'_, Open>, name: String, sql: String, confirmed: bool) -> Result<db::QueryResult, String> {
    let mut open = open.0.lock().await;
    let live = open.get_mut(&name).ok_or("not connected")?;
    if live.confirm_writes && !confirmed && !readonly::is_read_only(&live.kind, &sql) {
        return Err("CONFIRM".into());
    }
    live.conn.query(&sql).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
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
            run_query
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

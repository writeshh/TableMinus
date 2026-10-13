mod db;
mod readonly;
mod store;

use std::collections::HashMap;
use tauri::{async_runtime::Mutex, AppHandle, Manager, State};

// ponytail: one lock for all open connections serialises queries across tabs; per-connection locks if that hurts.
#[derive(Default)]
struct Open(Mutex<HashMap<String, (String, db::Conn)>>); // name -> (kind, connection)

fn dir(app: &AppHandle) -> std::path::PathBuf {
    app.path().app_config_dir().expect("no config dir")
}

#[tauri::command]
fn list_connections(app: AppHandle) -> Vec<store::Saved> {
    store::load(dir(&app))
}

#[tauri::command]
fn save_connection(app: AppHandle, conn: store::Saved, password: Option<String>) -> Result<(), String> {
    store::save(dir(&app), conn, password)
}

#[tauri::command]
async fn delete_connection(app: AppHandle, open: State<'_, Open>, name: String) -> Result<(), String> {
    open.0.lock().await.remove(&name);
    store::delete(dir(&app), &name)
}

/// Opens (or reuses) the connection and returns its tables.
#[tauri::command]
async fn connect(app: AppHandle, open: State<'_, Open>, name: String) -> Result<Vec<String>, String> {
    let mut open = open.0.lock().await;
    if !open.contains_key(&name) {
        let saved = store::load(dir(&app)).into_iter().find(|c| c.name == name).ok_or("no such connection")?;
        let conn = db::Conn::open(&saved.kind, &store::connect_url(&saved)?).await?;
        open.insert(name.clone(), (saved.kind, conn));
    }
    open.get_mut(&name).unwrap().1.tables().await
}

/// Safe mode: anything not provably read-only fails with "CONFIRM" until resent with `confirmed: true`.
#[tauri::command]
async fn run_query(open: State<'_, Open>, name: String, sql: String, confirmed: bool) -> Result<db::QueryResult, String> {
    let mut open = open.0.lock().await;
    let (kind, conn) = open.get_mut(&name).ok_or("not connected")?;
    if !confirmed && !readonly::is_read_only(kind, &sql) {
        return Err("CONFIRM".into());
    }
    conn.query(&sql).await
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
            connect,
            run_query
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

//! Updates from the project's GitHub Releases: `latest.json` of the newest release, or of the `beta` release for
//! beta versions. Each update is signed with the key `scripts/build.sh` signs with; the public half is in
//! `tauri.conf.json`, so a file that wasn't signed with it is refused.

use crate::error::{Error, Result};
use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::sync::Mutex;

const STABLE: &str = "https://github.com/writeshh/TableMinus/releases/latest/download/latest.json";
const BETA: &str = "https://github.com/writeshh/TableMinus/releases/download/beta/latest.json";

/// The update found by the last check, kept for `update_install`.
#[derive(Default)]
pub struct Pending(Mutex<Option<Update>>);

/// What the update dialog shows.
#[derive(Debug, Serialize)]
pub struct Available {
    version: String,
    current: String,
    notes: Option<String>,
    date: Option<String>,
}

fn failed(e: impl std::fmt::Display) -> Error {
    Error::Update(e.to_string())
}

/// Looks for a newer version, of the beta channel when `beta`. None when this one is the newest.
#[tauri::command]
pub async fn update_check(app: AppHandle, pending: State<'_, Pending>, beta: bool) -> Result<Option<Available>> {
    crate::db::install_tls_provider();
    let endpoint = url::Url::parse(if beta { BETA } else { STABLE })?;
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(failed)?
        .timeout(crate::db::CONNECT_TIMEOUT)
        .build()
        .map_err(failed)?;
    let found = updater.check().await.map_err(failed)?;
    let shown = found.as_ref().map(|u| Available {
        version: u.version.clone(),
        current: u.current_version.clone(),
        notes: u.body.clone(),
        date: u.date.map(|d| d.to_string()),
    });
    *pending.0.lock().await = found;
    Ok(shown)
}

/// Downloads and installs the update the last check found, then starts the new version.
#[tauri::command]
pub async fn update_install(app: AppHandle, pending: State<'_, Pending>) -> Result<()> {
    let update = pending.0.lock().await.take().ok_or(Error::Invalid("check for updates first"))?;
    update.download_and_install(|_, _| {}, || {}).await.map_err(failed)?;
    app.restart()
}

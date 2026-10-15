//! Where the app keeps its files (Settings › Locations): saved connections, kept SQL tabs, saved queries, metrics
//! boards, query history, and settings (with themes and layouts). MCP settings stay in the config folder: their access
//! tokens belong to this machine.
//! `locations.json` in the app config folder names custom folders, e.g. in iCloud Drive or Dropbox; anything it
//! doesn't name stays in the config folder.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Location {
    Connections,
    Queries,
    History,
    /// Settings, themes, column layouts and other choices.
    Settings,
}

impl Location {
    /// The files that live there, which move with it.
    fn files(self) -> &'static [&'static str] {
        match self {
            Location::Connections => &["connections.json"],
            Location::Queries => &["sql-tabs.json", "favorites.json", "metrics.json"],
            Location::History => &["history.json"],
            Location::Settings => &["settings.json"],
        }
    }
}

/// The custom folders; `None` is the config folder.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Locations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connections: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    queries: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    history: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    settings: Option<PathBuf>,
}

impl Locations {
    fn slot(&mut self, which: Location) -> &mut Option<PathBuf> {
        match which {
            Location::Connections => &mut self.connections,
            Location::Queries => &mut self.queries,
            Location::History => &mut self.history,
            Location::Settings => &mut self.settings,
        }
    }
}

fn settings_file(config: &Path) -> PathBuf {
    config.join("locations.json")
}

/// A missing file means the defaults. One that can't be read is an error rather than the defaults, so nothing gets
/// saved to the config folder while the user's files are somewhere else.
fn load(config: &Path) -> Result<Locations> {
    let path = settings_file(config);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Error::Locations(format!("{} is not valid ({e}); fix or remove it", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Locations::default()),
        Err(source) => Err(Error::File { path: path.display().to_string(), source }),
    }
}

/// The folder `which` lives in, given the app config folder.
pub fn folder(config: &Path, which: Location) -> Result<PathBuf> {
    Ok(load(config)?.slot(which).clone().unwrap_or_else(|| config.to_path_buf()))
}

/// Writes to a temporary file next to `path`, then renames it over, so a crash mid-write leaves the old file whole.
pub fn write_safely(path: &Path, bytes: &[u8]) -> Result<()> {
    let failed = |source| Error::Write { path: path.display().to_string(), source };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, bytes).map_err(failed)?;
    std::fs::rename(&tmp, path).map_err(failed)
}

/// Points `which` at `to` (`None`: back to the config folder). Files already in the new folder are used as they
/// are, e.g. ones synced from another Mac; otherwise the current ones are copied there. The old copies stay.
fn relocate(config: &Path, which: Location, to: Option<PathBuf>) -> Result<()> {
    let mut all = load(config)?;
    if let Some(to) = &to
        && !(to.is_absolute() && to.is_dir())
    {
        return Err(Error::Invalid("choose an existing folder"));
    }
    let from = all.slot(which).clone().unwrap_or_else(|| config.to_path_buf());
    let dest = to.clone().unwrap_or_else(|| config.to_path_buf());
    if from != dest {
        for name in which.files() {
            let (old, new) = (from.join(name), dest.join(name));
            if old.exists() && !new.exists() {
                let bytes =
                    std::fs::read(&old).map_err(|source| Error::File { path: old.display().to_string(), source })?;
                write_safely(&new, &bytes)?;
            }
        }
    }
    *all.slot(which) = to.filter(|t| t != config);
    write_safely(&settings_file(config), &serde_json::to_vec_pretty(&all)?)
}

fn config_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(app.path().app_config_dir()?)
}

/// Each location's folder, plus the default (the config folder), for Settings › Locations.
#[derive(Debug, Serialize)]
pub struct Folders {
    connections: PathBuf,
    queries: PathBuf,
    history: PathBuf,
    settings: PathBuf,
    default: PathBuf,
}

#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
pub fn locations(app: AppHandle) -> Result<Folders> {
    let config = config_dir(&app)?;
    Ok(Folders {
        connections: folder(&config, Location::Connections)?,
        queries: folder(&config, Location::Queries)?,
        history: folder(&config, Location::History)?,
        settings: folder(&config, Location::Settings)?,
        default: config,
    })
}

#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
pub fn set_location(app: AppHandle, which: Location, folder: Option<PathBuf>) -> Result<()> {
    relocate(&config_dir(&app)?, which, folder)
}

/// The JSON documents the UI keeps in files: one object each, keyed by connection name (settings: by setting).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Doc {
    SqlTabs,
    History,
    /// Saved queries, which live with the SQL tabs.
    Favorites,
    /// Metrics boards, which live with the SQL tabs.
    Metrics,
    Settings,
}

impl Doc {
    fn path(self, config: &Path) -> Result<PathBuf> {
        let (which, name) = match self {
            Doc::SqlTabs => (Location::Queries, "sql-tabs.json"),
            Doc::History => (Location::History, "history.json"),
            Doc::Favorites => (Location::Queries, "favorites.json"),
            Doc::Metrics => (Location::Queries, "metrics.json"),
            Doc::Settings => (Location::Settings, "settings.json"),
        };
        Ok(folder(config, which)?.join(name))
    }
}

/// The most a document may hold, so a runaway history can't fill the disk or memory.
const MAX_DOC: usize = 32 * 1024 * 1024;

/// The document, or `None` when there isn't one yet.
fn read(path: &Path) -> Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.len() > MAX_DOC => Err(Error::Invalid("the file is over 32 MB")),
        Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| Error::Invalid("the file isn't UTF-8 text")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::File { path: path.display().to_string(), source }),
    }
}

/// Saves a JSON object. Refuses to replace a file that isn't one, which a failed load would otherwise lose.
fn write(path: &Path, text: &str) -> Result<()> {
    let object = |t: &str| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(t).is_ok();
    if text.len() > MAX_DOC {
        return Err(Error::Invalid("too much to save (over 32 MB)"));
    }
    if !object(text) {
        return Err(Error::Invalid("not a JSON object"));
    }
    if let Some(existing) = read(path)?
        && !object(&existing)
    {
        return Err(Error::Locations(format!("{} is not valid; fix or move it before saving", path.display())));
    }
    write_safely(path, text.as_bytes())
}

#[tauri::command]
pub async fn read_doc(app: AppHandle, doc: Doc) -> Result<Option<String>> {
    let path = doc.path(&config_dir(&app)?)?;
    tauri::async_runtime::spawn_blocking(move || read(&path)).await?
}

#[tauri::command]
pub async fn write_doc(app: AppHandle, doc: Doc, text: String) -> Result<()> {
    let path = doc.path(&config_dir(&app)?)?;
    tauri::async_runtime::spawn_blocking(move || write(&path, &text)).await?
}

#[cfg(test)]
mod tests {
    use super::{Location, folder, read, relocate, write};
    use std::path::PathBuf;

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tableminus-{}-loc-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn folder_defaults_to_the_config_folder() {
        let config = temp_dir("default");
        assert_eq!(folder(&config, Location::History).unwrap(), config);
    }

    #[test]
    fn folder_refuses_an_unreadable_locations_file() {
        let config = temp_dir("corrupt");
        std::fs::write(config.join("locations.json"), "{ nope").unwrap();
        assert!(folder(&config, Location::Connections).is_err());
    }

    #[test]
    fn relocate_copies_the_files_and_restores_the_default() {
        let config = temp_dir("move");
        let elsewhere = temp_dir("move-dest");
        std::fs::write(config.join("history.json"), r#"{"a":[]}"#).unwrap();
        relocate(&config, Location::History, Some(elsewhere.clone())).unwrap();
        assert_eq!(folder(&config, Location::History).unwrap(), elsewhere);
        assert_eq!(std::fs::read_to_string(elsewhere.join("history.json")).unwrap(), r#"{"a":[]}"#);
        assert_eq!(folder(&config, Location::Queries).unwrap(), config, "others stay");
        relocate(&config, Location::History, None).unwrap();
        assert_eq!(folder(&config, Location::History).unwrap(), config);
    }

    #[test]
    fn relocate_moves_saved_queries_with_the_sql_tabs() {
        let config = temp_dir("favorites");
        let elsewhere = temp_dir("favorites-dest");
        std::fs::write(config.join("favorites.json"), r#"{"a":{}}"#).unwrap();
        relocate(&config, Location::Queries, Some(elsewhere.clone())).unwrap();
        assert_eq!(std::fs::read_to_string(elsewhere.join("favorites.json")).unwrap(), r#"{"a":{}}"#);
    }

    #[test]
    fn relocate_moves_settings_on_their_own() {
        let config = temp_dir("settings");
        let elsewhere = temp_dir("settings-dest");
        std::fs::write(config.join("settings.json"), r#"{"settings":{}}"#).unwrap();
        relocate(&config, Location::Settings, Some(elsewhere.clone())).unwrap();
        assert_eq!(std::fs::read_to_string(elsewhere.join("settings.json")).unwrap(), r#"{"settings":{}}"#);
        assert_eq!(folder(&config, Location::Queries).unwrap(), config, "others stay");
    }

    #[test]
    fn relocate_moves_metrics_boards_with_the_sql_tabs() {
        let config = temp_dir("metrics");
        let elsewhere = temp_dir("metrics-dest");
        std::fs::write(config.join("metrics.json"), r#"{"a":[]}"#).unwrap();
        relocate(&config, Location::Queries, Some(elsewhere.clone())).unwrap();
        assert_eq!(std::fs::read_to_string(elsewhere.join("metrics.json")).unwrap(), r#"{"a":[]}"#);
    }

    #[test]
    fn relocate_keeps_files_already_in_the_new_folder() {
        let config = temp_dir("keep");
        let synced = temp_dir("keep-dest");
        std::fs::write(config.join("connections.json"), "[1]").unwrap();
        std::fs::write(synced.join("connections.json"), "[2]").unwrap();
        relocate(&config, Location::Connections, Some(synced.clone())).unwrap();
        assert_eq!(std::fs::read_to_string(synced.join("connections.json")).unwrap(), "[2]");
    }

    #[test]
    fn relocate_refuses_a_folder_that_does_not_exist() {
        let config = temp_dir("missing");
        assert!(relocate(&config, Location::Queries, Some(config.join("nope"))).is_err());
        assert!(relocate(&config, Location::Queries, Some("relative".into())).is_err());
    }

    #[test]
    fn write_saves_objects_and_reads_them_back() {
        let path = temp_dir("doc").join("history.json");
        assert_eq!(read(&path).unwrap(), None);
        write(&path, r#"{"a":[1]}"#).unwrap();
        assert_eq!(read(&path).unwrap().as_deref(), Some(r#"{"a":[1]}"#));
        assert!(write(&path, "[1]").is_err(), "only objects");
    }

    #[test]
    fn write_refuses_to_replace_an_unreadable_file() {
        let path = temp_dir("doc-corrupt").join("history.json");
        std::fs::write(&path, "{ half written").unwrap();
        assert!(write(&path, "{}").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ half written");
    }
}

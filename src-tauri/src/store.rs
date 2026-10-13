use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const SERVICE: &str = "dev.ritesh.dbmanager";

/// A saved connection. `url` never holds the password; that lives in the OS keychain under `name`.
#[derive(Serialize, Deserialize, Clone)]
pub struct Saved {
    pub name: String,
    pub kind: String, // postgres | mysql | mariadb | sqlite
    pub url: String,
    #[serde(default)]
    pub tag: String, // "" | local | development | staging | production
    #[serde(default = "yes")]
    pub confirm_writes: bool, // safe mode
}

fn yes() -> bool {
    true
}

fn file(dir: PathBuf) -> PathBuf {
    dir.join("connections.json")
}

pub fn load(dir: PathBuf) -> Vec<Saved> {
    std::fs::read(file(dir)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write(dir: PathBuf, all: &[Saved]) -> Result<(), String> {
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(file(dir), serde_json::to_vec_pretty(all).unwrap()).map_err(|e| e.to_string())
}

fn entry(name: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, name).map_err(|e| e.to_string())
}

/// Saves or replaces by name. `password: None` keeps the stored one; `Some("")` removes it.
/// `previous` is the old name when renaming; its record goes and its keychain password moves over.
pub fn save(dir: PathBuf, mut c: Saved, password: Option<String>, previous: Option<String>) -> Result<(), String> {
    if let Ok(mut u) = url::Url::parse(&c.url) {
        let _ = u.set_password(None); // never persist a password typed into the URL
        c.url = u.to_string();
    }
    if let Some(old) = previous.filter(|old| *old != c.name) {
        if let Ok(pw) = entry(&old)?.get_password() {
            entry(&c.name)?.set_password(&pw).map_err(|e| e.to_string())?;
        }
        delete(dir.clone(), &old)?;
    }
    match password.as_deref() {
        Some("") => { let _ = entry(&c.name)?.delete_credential(); }
        Some(pw) => entry(&c.name)?.set_password(pw).map_err(|e| e.to_string())?,
        None => {}
    }
    let mut all = load(dir.clone());
    all.retain(|x| x.name != c.name);
    all.push(c);
    all.sort_by(|a, b| a.name.cmp(&b.name));
    write(dir, &all)
}

pub fn delete(dir: PathBuf, name: &str) -> Result<(), String> {
    let _ = entry(name)?.delete_credential();
    let mut all = load(dir.clone());
    all.retain(|x| x.name != name);
    write(dir, &all)
}

/// The URL to connect with: the saved URL plus the keychain password, if any.
pub fn connect_url(c: &Saved) -> Result<String, String> {
    match entry(&c.name)?.get_password() {
        Ok(pw) => with_password(&c.url, &pw),
        Err(_) => Ok(c.url.clone()),
    }
}

pub fn with_password(url: &str, pw: &str) -> Result<String, String> {
    if pw.is_empty() {
        return Ok(url.to_string());
    }
    let mut u = url::Url::parse(url).map_err(|e| e.to_string())?;
    u.set_password(Some(pw)).map_err(|_| "this URL cannot hold a password".to_string())?;
    Ok(u.to_string())
}

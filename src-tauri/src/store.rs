//! Saved connections: `connections.json` in the app config folder, passwords in the OS keychain.

use crate::db::Engine;
use crate::error::{Error, Result};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SERVICE: &str = "dev.ritesh.dbmanager";

/// A saved connection. `url` never holds the password; that lives in the OS keychain under `name`.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    pub name: String,
    pub kind: Engine,
    pub url: String,
    #[serde(default)]
    pub tag: String, // "" | local | development | staging | production
    #[serde(default = "yes")]
    pub confirm_writes: bool, // safe mode
}

fn yes() -> bool {
    true
}

fn file(dir: &Path) -> PathBuf {
    dir.join("connections.json")
}

/// All saved connections. A missing file means none yet; a file that can't be read or parsed is an error,
/// so a later save can't overwrite connections the user still has.
pub fn load(dir: &Path) -> Result<Vec<Saved>> {
    match std::fs::read(file(dir)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e.into()),
    }
}

/// Writes to a temporary file first, then renames it over the old one, so a crash mid-write
/// leaves the previous file intact.
fn write(dir: &Path, all: &[Saved]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("connections.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(all)?)?;
    std::fs::rename(tmp, file(dir))?;
    Ok(())
}

fn entry(name: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, name)?)
}

/// The stored password, or None when there isn't one.
fn stored_password(name: &str) -> Result<Option<SecretString>> {
    match entry(name)?.get_password() {
        Ok(pw) => Ok(Some(pw.into())),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn forget_password(name: &str) -> Result<()> {
    match entry(name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Saves or replaces by name. `password: None` keeps the stored one; an empty password removes it.
/// `previous` is the old name when renaming; its record goes and its keychain password moves over.
pub fn save(dir: &Path, mut c: Saved, password: Option<&SecretString>, previous: Option<&str>) -> Result<()> {
    let mut all = load(dir)?; // first, so an unreadable file stops the save before the keychain changes
    if let Ok(mut u) = url::Url::parse(&c.url) {
        let _ = u.set_password(None); // never persist a password typed into the URL; Err means it had none
        c.url = u.to_string();
    }
    if let Some(old) = previous.filter(|old| *old != c.name) {
        if let Some(pw) = stored_password(old)? {
            entry(&c.name)?.set_password(pw.expose_secret())?;
        }
        forget_password(old)?;
        all.retain(|x| x.name != old);
    }
    match password.map(ExposeSecret::expose_secret) {
        Some("") => forget_password(&c.name)?,
        Some(pw) => entry(&c.name)?.set_password(pw)?,
        None => {}
    }
    all.retain(|x| x.name != c.name);
    all.push(c);
    all.sort_by(|a, b| a.name.cmp(&b.name));
    write(dir, &all)
}

pub fn delete(dir: &Path, name: &str) -> Result<()> {
    let mut all = load(dir)?;
    forget_password(name)?;
    all.retain(|x| x.name != name);
    write(dir, &all)
}

/// The URL to connect with: the saved URL plus the keychain password, if any.
pub fn connect_url(c: &Saved) -> Result<SecretString> {
    match stored_password(&c.name)? {
        Some(pw) => with_password(&c.url, &pw),
        None => Ok(c.url.clone().into()),
    }
}

pub fn with_password(url: &str, pw: &SecretString) -> Result<SecretString> {
    if pw.expose_secret().is_empty() {
        return Ok(url.into());
    }
    let mut u = url::Url::parse(url)?;
    u.set_password(Some(pw.expose_secret())).map_err(|()| Error::Invalid("this URL cannot hold a password"))?;
    Ok(u.to_string().into())
}

#[cfg(test)]
mod tests {
    use super::{Saved, load, save, with_password};
    use crate::db::Engine;
    use secrecy::{ExposeSecret, SecretString};
    use std::path::PathBuf;

    /// A fresh folder for one test, removed first in case a previous run left it behind.
    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dbmanager-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn conn(name: &str, url: &str) -> Saved {
        Saved { name: name.into(), kind: Engine::Postgres, url: url.into(), tag: String::new(), confirm_writes: true }
    }

    #[test]
    fn load_returns_empty_when_file_is_missing() {
        let names: Vec<String> = load(&temp_dir("missing")).unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(names, Vec::<String>::new());
    }

    #[test]
    fn save_refuses_to_overwrite_an_unreadable_file() {
        let dir = temp_dir("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("connections.json"), "{ not json").unwrap();
        assert!(load(&dir).is_err());
        assert!(save(&dir, conn("a", "postgres://u@h/db"), None, None).is_err());
        assert_eq!(std::fs::read_to_string(dir.join("connections.json")).unwrap(), "{ not json");
    }

    #[test]
    fn save_strips_a_password_typed_into_the_url() {
        let dir = temp_dir("strip");
        save(&dir, conn("a", "postgres://u:hunter2@h/db"), None, None).unwrap();
        let saved = load(&dir).unwrap();
        assert_eq!(saved[0].url, "postgres://u@h/db");
        assert!(!std::fs::read_to_string(dir.join("connections.json")).unwrap().contains("hunter2"));
    }

    #[test]
    fn save_replaces_by_name_and_keeps_order() {
        let dir = temp_dir("replace");
        save(&dir, conn("b", "postgres://u@h/one"), None, None).unwrap();
        save(&dir, conn("a", "postgres://u@h/two"), None, None).unwrap();
        save(&dir, conn("b", "postgres://u@h/three"), None, None).unwrap();
        let names: Vec<_> = load(&dir).unwrap().into_iter().map(|c| (c.name, c.url)).collect();
        assert_eq!(names, [("a".into(), "postgres://u@h/two".into()), ("b".into(), "postgres://u@h/three".into())]);
    }

    #[test]
    fn with_password_encodes_special_characters() {
        let url = with_password("postgres://u@h/db", &SecretString::from("p@ss:w/rd")).unwrap();
        assert_eq!(url.expose_secret(), "postgres://u:p%40ss%3Aw%2Frd@h/db");
    }
}

//! Saved connections: `connections.json` in the app config folder, passwords in the OS keychain.

use crate::db::{Engine, Options};
use crate::error::{Error, Result};
use crate::ssh::Ssh;
use crate::tls::Tls;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The two secrets a connection can have, each kept in the keychain under the connection's name.
#[derive(Debug, Clone, Copy)]
pub enum Secret {
    Database,
    /// The SSH password, or the private key's passphrase.
    Ssh,
}

impl Secret {
    fn service(self) -> &'static str {
        match self {
            Secret::Database => "dev.ritesh.dbmanager",
            Secret::Ssh => "dev.ritesh.dbmanager.ssh",
        }
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<Ssh>,
    #[serde(default, skip_serializing_if = "Tls::is_default")]
    pub tls: Tls,
    #[serde(default, skip_serializing_if = "Options::is_default")]
    pub options: Options,
}

impl Saved {
    /// Moves a TLS mode kept in the URL (how connections were saved before `tls` existed) into `tls`.
    fn migrate_legacy_tls(&mut self) {
        let Ok(mut u) = url::Url::parse(&self.url) else { return };
        let mut mode = None;
        let kept: Vec<(String, String)> = u
            .query_pairs()
            .filter(|(k, v)| match Tls::from_legacy_param(k, v) {
                Some(m) => {
                    mode = Some(m);
                    false
                }
                None => true,
            })
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let Some(mode) = mode else { return };
        self.tls.mode = mode;
        if kept.is_empty() {
            u.set_query(None);
        } else {
            u.query_pairs_mut().clear().extend_pairs(kept);
        }
        self.url = u.to_string();
    }
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
    let path = file(dir);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let mut all: Vec<Saved> = serde_json::from_slice(&bytes)?;
            all.iter_mut().for_each(Saved::migrate_legacy_tls);
            Ok(all)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(source) => Err(Error::File { path: path.display().to_string(), source }),
    }
}

/// Writes to a temporary file first, then renames it over the old one, so a crash mid-write
/// leaves the previous file intact.
fn write(dir: &Path, all: &[Saved]) -> Result<()> {
    let path = file(dir);
    let failed = |source| Error::File { path: path.display().to_string(), source };
    std::fs::create_dir_all(dir).map_err(failed)?;
    let tmp = dir.join("connections.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(all)?).map_err(failed)?;
    std::fs::rename(tmp, &path).map_err(failed)?;
    Ok(())
}

fn entry(which: Secret, name: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(which.service(), name)?)
}

/// The stored secret, or None when there isn't one.
pub fn stored(which: Secret, name: &str) -> Result<Option<SecretString>> {
    match entry(which, name)?.get_password() {
        Ok(pw) => Ok(Some(pw.into())),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn forget(which: Secret, name: &str) -> Result<()> {
    match entry(which, name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// `None` keeps the stored secret; an empty one removes it.
fn update(which: Secret, name: &str, secret: Option<&SecretString>) -> Result<()> {
    match secret.map(ExposeSecret::expose_secret) {
        Some("") => forget(which, name),
        Some(s) => Ok(entry(which, name)?.set_password(s)?),
        None => Ok(()),
    }
}

/// Saves or replaces by name. For each secret, `None` keeps the stored one and an empty one removes it.
/// `previous` is the old name when renaming; its record goes and its keychain secrets move over.
pub fn save(
    dir: &Path,
    mut c: Saved,
    password: Option<&SecretString>,
    ssh_password: Option<&SecretString>,
    previous: Option<&str>,
) -> Result<()> {
    let mut all = load(dir)?; // first, so an unreadable file stops the save before the keychain changes
    if let Ok(mut u) = url::Url::parse(&c.url) {
        let _ = u.set_password(None); // never persist a password typed into the URL; Err means it had none
        c.url = u.to_string();
    }
    let old_name = previous.unwrap_or(&c.name);
    let had_ssh = all.iter().any(|x| x.name == old_name && x.ssh.is_some());
    if let Some(old) = previous.filter(|old| *old != c.name) {
        for which in [Secret::Database, Secret::Ssh] {
            if let Some(s) = stored(which, old)? {
                entry(which, &c.name)?.set_password(s.expose_secret())?;
            }
            forget(which, old)?;
        }
        all.retain(|x| x.name != old);
    }
    update(Secret::Database, &c.name, password)?;
    if c.ssh.is_some() {
        update(Secret::Ssh, &c.name, ssh_password)?;
    } else if had_ssh {
        forget(Secret::Ssh, &c.name)?; // SSH turned off: don't leave its password behind
    }
    all.retain(|x| x.name != c.name);
    all.push(c);
    all.sort_by(|a, b| a.name.cmp(&b.name));
    write(dir, &all)
}

pub fn delete(dir: &Path, name: &str) -> Result<()> {
    let mut all = load(dir)?;
    forget(Secret::Database, name)?;
    forget(Secret::Ssh, name)?;
    all.retain(|x| x.name != name);
    write(dir, &all)
}

#[cfg(test)]
mod tests {
    use super::{Saved, load, save};
    use crate::db::{Engine, Options};
    use crate::tls::{Tls, TlsMode};
    use std::path::PathBuf;

    /// A fresh folder for one test, removed first in case a previous run left it behind.
    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dbmanager-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn conn(name: &str, url: &str) -> Saved {
        Saved {
            name: name.into(),
            kind: Engine::Postgres,
            url: url.into(),
            tag: String::new(),
            confirm_writes: true,
            ssh: None,
            tls: Tls::default(),
            options: Options::default(),
        }
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
        assert!(save(&dir, conn("a", "postgres://u@h/db"), None, None, None).is_err());
        assert_eq!(std::fs::read_to_string(dir.join("connections.json")).unwrap(), "{ not json");
    }

    #[test]
    fn save_strips_a_password_typed_into_the_url() {
        let dir = temp_dir("strip");
        save(&dir, conn("a", "postgres://u:hunter2@h/db"), None, None, None).unwrap();
        let saved = load(&dir).unwrap();
        assert_eq!(saved[0].url, "postgres://u@h/db");
        assert!(!std::fs::read_to_string(dir.join("connections.json")).unwrap().contains("hunter2"));
    }

    #[test]
    fn save_replaces_by_name_and_keeps_order() {
        let dir = temp_dir("replace");
        save(&dir, conn("b", "postgres://u@h/one"), None, None, None).unwrap();
        save(&dir, conn("a", "postgres://u@h/two"), None, None, None).unwrap();
        save(&dir, conn("b", "postgres://u@h/three"), None, None, None).unwrap();
        let names: Vec<_> = load(&dir).unwrap().into_iter().map(|c| (c.name, c.url)).collect();
        assert_eq!(names, [("a".into(), "postgres://u@h/two".into()), ("b".into(), "postgres://u@h/three".into())]);
    }

    #[test]
    fn load_moves_a_legacy_url_tls_mode_into_tls() {
        let dir = temp_dir("legacy");
        std::fs::create_dir_all(&dir).unwrap();
        let file = r#"[{"name":"a","kind":"postgres","url":"postgres://u@h:5432/db?sslmode=require&application_name=x"},
                       {"name":"b","kind":"mysql","url":"mysql://root@h:3306?require_ssl=true"}]"#;
        std::fs::write(dir.join("connections.json"), file).unwrap();
        let all = load(&dir).unwrap();
        assert_eq!(
            (all[0].tls.mode, all[0].url.as_str()),
            (TlsMode::Require, "postgres://u@h:5432/db?application_name=x")
        );
        assert_eq!((all[1].tls.mode, all[1].url.as_str()), (TlsMode::Require, "mysql://root@h:3306"));
    }
}

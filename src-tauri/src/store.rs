//! Saved connections: `connections.json` in the app config folder (with the launcher's groups and tags), passwords in
//! the OS keychain, or only in memory for connections that ask every time.

use crate::db::{Engine, Options};
use crate::error::{Error, Result};
use crate::locations::{self, Location};
use crate::safety::Safety;
use crate::ssh::Ssh;
use crate::tls::Tls;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

/// The two secrets a connection can have, each kept in the keychain under the connection's name.
#[derive(Debug, Clone, Copy)]
pub enum Secret {
    Database,
    /// The SSH password, or the private key's passphrase.
    Ssh,
}

impl Secret {
    fn service(self) -> &'static str {
        // Named for the app before it became TableMinus; renaming would orphan saved passwords.
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
    /// Safe mode. Files from before levels existed have `confirm_writes: bool` instead.
    #[serde(default, alias = "confirm_writes", deserialize_with = "crate::safety::level_or_legacy")]
    pub safety: Safety,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<Ssh>,
    #[serde(default, skip_serializing_if = "Tls::is_default")]
    pub tls: Tls,
    #[serde(default, skip_serializing_if = "Options::is_default")]
    pub options: Options,
    #[serde(default, skip_serializing_if = "PasswordMode::is_keychain")]
    pub password_mode: PasswordMode,
    /// The status color on the launcher row and the workspace header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    /// The launcher group's name; empty when ungrouped.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub group: String,
}

/// Where a connection's database password (or SQLite passphrase) is kept.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PasswordMode {
    #[default]
    Keychain,
    /// Asked for at connect and kept in memory only while the app runs; see [`set_session_password`].
    Ask,
    NoPassword,
}

impl PasswordMode {
    #[expect(clippy::trivially_copy_pass_by_ref, reason = "serde's skip_serializing_if passes a reference")]
    fn is_keychain(&self) -> bool {
        *self == PasswordMode::Keychain
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    Red,
    Orange,
    Green,
    Blue,
    Purple,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GroupIcon {
    #[default]
    Folder,
    Server,
    Cloud,
    Laptop,
    Star,
    Lock,
}

/// A launcher group. Connections name theirs in [`Saved::group`].
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub icon: GroupIcon,
}

/// A tag a connection can carry in [`Saved::tag`], with its color as `#rrggbb`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Tag {
    pub name: String,
    pub color: String,
}

impl Tag {
    fn check(&self) -> Result<()> {
        let hex = self.color.strip_prefix('#').unwrap_or_default();
        if self.name.trim().is_empty() {
            return Err(Error::Invalid("a tag needs a name"));
        }
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Invalid("a tag color is #rrggbb"));
        }
        Ok(())
    }
}

/// The tags a connections file starts with; the UI knows these names (production gets a red edge).
fn default_tags() -> Vec<Tag> {
    [("local", "#30a46c"), ("development", "#3e63dd"), ("staging", "#f5a524"), ("production", "#e5484d")]
        .map(|(name, color)| Tag { name: name.into(), color: color.into() })
        .into()
}

/// Everything in the connections file.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ConnectionsFile {
    #[serde(default)]
    pub connections: Vec<Saved>,
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default = "default_tags")]
    pub tags: Vec<Tag>,
}

impl Default for ConnectionsFile {
    fn default() -> Self {
        ConnectionsFile { connections: vec![], groups: vec![], tags: default_tags() }
    }
}

impl ConnectionsFile {
    /// `base`, or `base 2`, `base 3`… when a connection already has that name.
    pub fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| self.connections.iter().any(|c| c.name == n);
        if !taken(base) {
            return base.to_string();
        }
        // n connections can't take all of n + 1 names.
        (2..=self.connections.len() + 1).map(|i| format!("{base} {i}")).find(|n| !taken(n)).unwrap_or_default()
    }
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

fn file(dir: &Path) -> Result<PathBuf> {
    Ok(locations::folder(dir, Location::Connections)?.join("connections.json"))
}

/// The connections file. A missing file means none yet; a file that can't be read or parsed is an error,
/// so a later save can't overwrite connections the user still has.
pub fn load_all(dir: &Path) -> Result<ConnectionsFile> {
    let path = file(dir)?;
    match std::fs::read(&path) {
        Ok(bytes) => {
            // Before groups and tags, the file was just the array of connections.
            let mut all = if bytes.trim_ascii_start().starts_with(b"[") {
                ConnectionsFile { connections: serde_json::from_slice(&bytes)?, ..ConnectionsFile::default() }
            } else {
                serde_json::from_slice(&bytes)?
            };
            all.connections.iter_mut().for_each(Saved::migrate_legacy_tls);
            Ok(all)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ConnectionsFile::default()),
        Err(source) => Err(Error::File { path: path.display().to_string(), source }),
    }
}

/// All saved connections; see [`load_all`].
pub fn load(dir: &Path) -> Result<Vec<Saved>> {
    Ok(load_all(dir)?.connections)
}

/// Writes to a temporary file first, then renames it over the old one, so a crash mid-write
/// leaves the previous file intact.
pub fn write(dir: &Path, all: &ConnectionsFile) -> Result<()> {
    locations::write_safely(&file(dir)?, &serde_json::to_vec_pretty(all)?)
}

/// "Ask every time" passwords, by connection name: memory only, gone when the app quits.
static SESSION: LazyLock<Mutex<HashMap<String, SecretString>>> = LazyLock::new(Mutex::default);

/// Keeps (or with `None` forgets) the password typed for an "ask every time" connection.
pub fn set_session_password(name: &str, password: Option<SecretString>) {
    // A panic elsewhere while holding the lock leaves the map itself intact.
    let mut session = SESSION.lock().unwrap_or_else(PoisonError::into_inner);
    match password {
        Some(p) => session.insert(name.to_string(), p),
        None => session.remove(name),
    };
}

/// The database password to connect with, from wherever the connection keeps it.
pub fn password_for(c: &Saved) -> Result<Option<SecretString>> {
    match c.password_mode {
        PasswordMode::Keychain => stored(Secret::Database, &c.name),
        PasswordMode::NoPassword => Ok(None),
        PasswordMode::Ask => {
            let session = SESSION.lock().unwrap_or_else(PoisonError::into_inner);
            session.get(&c.name).cloned().map(Some).ok_or(Error::AskPassword)
        }
    }
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

/// Removes both of a connection's keychain secrets.
pub fn forget_secrets(name: &str) -> Result<()> {
    set_session_password(name, None);
    forget(Secret::Database, name)?;
    forget(Secret::Ssh, name)
}

/// `None` keeps the stored secret; an empty one removes it.
fn update(which: Secret, name: &str, secret: Option<&SecretString>) -> Result<()> {
    match secret.map(ExposeSecret::expose_secret) {
        Some("") => forget(which, name),
        Some(s) => Ok(entry(which, name)?.set_password(s)?),
        None => Ok(()),
    }
}

/// Saves or replaces by name. For each secret, `None` keeps the stored one and an empty one removes it; a connection
/// that doesn't keep its password in the keychain has it removed from there.
/// `previous` is the old name when renaming; its record goes and its keychain secrets move over.
pub fn save(
    dir: &Path,
    mut c: Saved,
    password: Option<&SecretString>,
    ssh_password: Option<&SecretString>,
    previous: Option<&str>,
) -> Result<()> {
    let mut file = load_all(dir)?; // first, so an unreadable file stops the save before the keychain changes
    strip_password(&mut c);
    let all = &mut file.connections;
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
    if c.password_mode == PasswordMode::Keychain {
        update(Secret::Database, &c.name, password)?;
    } else {
        forget(Secret::Database, &c.name)?;
    }
    if c.ssh.is_some() {
        update(Secret::Ssh, &c.name, ssh_password)?;
    } else if had_ssh {
        forget(Secret::Ssh, &c.name)?; // SSH turned off: don't leave its password behind
    }
    if !c.group.is_empty() && !file.groups.iter().any(|g| g.name == c.group) {
        file.groups.push(Group { name: c.group.clone(), icon: GroupIcon::default() });
    }
    let all = &mut file.connections;
    all.retain(|x| x.name != c.name);
    all.push(c);
    all.sort_by(|a, b| a.name.cmp(&b.name));
    write(dir, &file)
}

/// Never persist a password typed into the URL.
fn strip_password(c: &mut Saved) {
    if let Ok(mut u) = url::Url::parse(&c.url) {
        let _ = u.set_password(None); // Err means it had none
        c.url = u.to_string();
    }
}

pub fn delete(dir: &Path, name: &str) -> Result<()> {
    let mut file = load_all(dir)?;
    forget_secrets(name)?;
    file.connections.retain(|x| x.name != name);
    write(dir, &file)
}

/// Copies a connection and its keychain secrets under a free name ("name copy"), which it returns.
pub fn duplicate(dir: &Path, name: &str) -> Result<String> {
    let mut file = load_all(dir)?;
    let mut copy = file
        .connections
        .iter()
        .find(|c| c.name == name)
        .cloned()
        .ok_or_else(|| Error::NoSuchConnection(name.into()))?;
    copy.name = file.unique_name(&format!("{name} copy"));
    for which in [Secret::Database, Secret::Ssh] {
        if let Some(s) = stored(which, name)? {
            entry(which, &copy.name)?.set_password(s.expose_secret())?;
        }
    }
    let new = copy.name.clone();
    file.connections.push(copy);
    file.connections.sort_by(|a, b| a.name.cmp(&b.name));
    write(dir, &file)?;
    Ok(new)
}

/// Adds imported connections under free names, with their secrets. `secrets` maps an imported name to its database
/// and SSH secret. Groups and tags that are new here are added; existing ones keep their settings. Returns how many
/// connections were added.
pub fn add_imported(
    dir: &Path,
    incoming: ConnectionsFile,
    secrets: &HashMap<String, (Option<SecretString>, Option<SecretString>)>,
) -> Result<usize> {
    let mut file = load_all(dir)?;
    for t in incoming.tags {
        t.check()?;
        if !file.tags.iter().any(|x| x.name == t.name) {
            file.tags.push(t);
        }
    }
    for g in incoming.groups {
        if !file.groups.iter().any(|x| x.name == g.name) {
            file.groups.push(g);
        }
    }
    let (mut keep, added) = (vec![], incoming.connections.len());
    for mut c in incoming.connections {
        strip_password(&mut c);
        let imported_as = c.name.clone();
        c.name = file.unique_name(&c.name);
        if !c.group.is_empty() && !file.groups.iter().any(|g| g.name == c.group) {
            file.groups.push(Group { name: c.group.clone(), icon: GroupIcon::default() });
        }
        if let Some(s) = secrets.get(&imported_as) {
            keep.push((c.name.clone(), c.password_mode == PasswordMode::Keychain, s));
        }
        file.connections.push(c);
    }
    file.connections.sort_by(|a, b| a.name.cmp(&b.name));
    write(dir, &file)?; // the file first: a keychain failure then loses a password, never a connection
    for (name, keychain, (db, ssh)) in keep {
        if keychain {
            update(Secret::Database, &name, db.as_ref())?;
        }
        update(Secret::Ssh, &name, ssh.as_ref())?;
    }
    Ok(added)
}

/// Puts the named connections in `group` ("" for none).
pub fn move_to_group(dir: &Path, names: &[String], group: &str) -> Result<()> {
    let mut file = load_all(dir)?;
    if !group.is_empty() && !file.groups.iter().any(|g| g.name == group) {
        return Err(Error::Invalid("no such group"));
    }
    for c in file.connections.iter_mut().filter(|c| names.contains(&c.name)) {
        c.group = group.to_string();
    }
    write(dir, &file)
}

/// Adds a group, or replaces `previous` (renaming moves its connections along).
pub fn save_group(dir: &Path, group: Group, previous: Option<&str>) -> Result<()> {
    let mut file = load_all(dir)?;
    if group.name.trim().is_empty() {
        return Err(Error::Invalid("a group needs a name"));
    }
    if previous != Some(group.name.as_str()) && file.groups.iter().any(|g| g.name == group.name) {
        return Err(Error::Invalid("there's already a group with that name"));
    }
    if let Some(old) = previous {
        for c in file.connections.iter_mut().filter(|c| c.group == old) {
            c.group.clone_from(&group.name);
        }
    }
    match file.groups.iter_mut().find(|g| Some(g.name.as_str()) == previous) {
        Some(g) => *g = group,
        None => file.groups.push(group),
    }
    write(dir, &file)
}

/// Removes a group; its connections stay, ungrouped.
pub fn delete_group(dir: &Path, name: &str) -> Result<()> {
    let mut file = load_all(dir)?;
    file.groups.retain(|g| g.name != name);
    for c in file.connections.iter_mut().filter(|c| c.group == name) {
        c.group.clear();
    }
    write(dir, &file)
}

/// Adds a tag, or replaces `previous` (renaming retags its connections).
pub fn save_tag(dir: &Path, tag: Tag, previous: Option<&str>) -> Result<()> {
    tag.check()?;
    let mut file = load_all(dir)?;
    if previous != Some(tag.name.as_str()) && file.tags.iter().any(|t| t.name == tag.name) {
        return Err(Error::Invalid("there's already a tag with that name"));
    }
    if let Some(old) = previous {
        for c in file.connections.iter_mut().filter(|c| c.tag == old) {
            c.tag.clone_from(&tag.name);
        }
    }
    match file.tags.iter_mut().find(|t| Some(t.name.as_str()) == previous) {
        Some(t) => *t = tag,
        None => file.tags.push(tag),
    }
    write(dir, &file)
}

/// Removes a tag; connections that had it lose it.
pub fn delete_tag(dir: &Path, name: &str) -> Result<()> {
    let mut file = load_all(dir)?;
    file.tags.retain(|t| t.name != name);
    for c in file.connections.iter_mut().filter(|c| c.tag == name) {
        c.tag.clear();
    }
    write(dir, &file)
}

#[cfg(test)]
mod tests {
    use super::{Saved, load, save};
    use crate::db::{Engine, Options};
    use crate::safety::Safety;
    use crate::tls::{Tls, TlsMode};
    use std::path::PathBuf;

    /// A fresh folder for one test, removed first in case a previous run left it behind.
    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tableminus-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn conn(name: &str, url: &str) -> Saved {
        Saved {
            name: name.into(),
            kind: Engine::Postgres,
            url: url.into(),
            tag: String::new(),
            safety: Safety::Alert2,
            ssh: None,
            tls: Tls::default(),
            options: Options::default(),
            password_mode: crate::store::PasswordMode::Keychain,
            color: None,
            group: String::new(),
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

    #[test]
    fn load_turns_the_old_confirm_writes_flag_into_a_level() {
        let dir = temp_dir("levels");
        std::fs::create_dir_all(&dir).unwrap();
        let file = r#"[{"name":"a","kind":"sqlite","url":"x","confirm_writes":true},
                       {"name":"b","kind":"sqlite","url":"x","confirm_writes":false},
                       {"name":"c","kind":"sqlite","url":"x"},
                       {"name":"d","kind":"sqlite","url":"x","safety":"safe1"}]"#;
        std::fs::write(dir.join("connections.json"), file).unwrap();
        let levels: Vec<_> = load(&dir).unwrap().into_iter().map(|c| c.safety).collect();
        assert_eq!(levels, [Safety::Alert2, Safety::Silent, Safety::Alert2, Safety::Safe1]);
    }

    #[test]
    fn save_writes_the_level_and_drops_the_old_flag() {
        let dir = temp_dir("level-save");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("connections.json"),
            r#"[{"name":"a","kind":"sqlite","url":"x","confirm_writes":false}]"#,
        )
        .unwrap();
        let mut c = load(&dir).unwrap().remove(0);
        c.url = "y".into();
        save(&dir, c, None, None, None).unwrap();
        let text = std::fs::read_to_string(dir.join("connections.json")).unwrap();
        assert!(text.contains(r#""safety": "silent""#) && !text.contains("confirm_writes"), "{text}");
    }

    #[test]
    fn load_reads_the_old_array_file_with_the_default_tags() {
        let dir = temp_dir("array");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("connections.json"), r#"[{"name":"a","kind":"sqlite","url":"x"}]"#).unwrap();
        let all = super::load_all(&dir).unwrap();
        assert_eq!(all.connections[0].name, "a");
        assert_eq!(all.tags.len(), 4);
    }

    #[test]
    fn password_for_asks_until_a_session_password_is_given() {
        use super::{PasswordMode, password_for, set_session_password};
        use secrecy::ExposeSecret;
        let c = Saved { name: "ask-test".into(), password_mode: PasswordMode::Ask, ..conn("x", "postgres://u@h/db") };
        assert!(matches!(password_for(&c), Err(crate::error::Error::AskPassword)));
        set_session_password("ask-test", Some("pw".into()));
        assert_eq!(password_for(&c).unwrap().unwrap().expose_secret(), "pw");
        set_session_password("ask-test", None);
        assert!(password_for(&c).is_err());
        let none = Saved { password_mode: PasswordMode::NoPassword, ..c };
        assert!(password_for(&none).unwrap().is_none());
    }

    #[test]
    fn renaming_a_group_moves_its_connections_and_deleting_it_ungroups_them() {
        use super::{Group, GroupIcon, delete_group, move_to_group, save_group};
        let dir = temp_dir("groups");
        let c = Saved { password_mode: super::PasswordMode::NoPassword, ..conn("a", "postgres://u@h/db") };
        save(&dir, c, None, None, None).unwrap();
        save_group(&dir, Group { name: "g".into(), icon: GroupIcon::Cloud }, None).unwrap();
        move_to_group(&dir, &["a".into()], "g").unwrap();
        save_group(&dir, Group { name: "h".into(), icon: GroupIcon::Cloud }, Some("g")).unwrap();
        assert_eq!(load(&dir).unwrap()[0].group, "h");
        assert!(move_to_group(&dir, &["a".into()], "nope").is_err());
        delete_group(&dir, "h").unwrap();
        let all = super::load_all(&dir).unwrap();
        assert!(all.groups.is_empty() && all.connections[0].group.is_empty());
    }

    #[test]
    fn save_tag_refuses_a_color_that_is_not_hex() {
        let dir = temp_dir("tags");
        let tag = |color: &str| super::Tag { name: "t".into(), color: color.into() };
        assert!(super::save_tag(&dir, tag("red"), None).is_err());
        assert!(super::save_tag(&dir, tag("#12345g"), None).is_err());
        super::save_tag(&dir, tag("#a0B1c2"), None).unwrap();
        assert!(super::save_tag(&dir, tag("#000000"), None).is_err(), "the name is taken");
    }
}

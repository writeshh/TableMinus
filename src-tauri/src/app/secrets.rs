//! Where secrets live: database and SSH passwords, the passcode's hash and AI keys. Normally that is the system
//! keychain. A Linux desktop without a Secret Service (no D-Bus session, or a minimal window manager) has none, so
//! there they go to a vault file instead: one JSON map of secrets, sealed with AES-256-GCM under a key that argon2id
//! derives from a master password (the same sealing as exported connection files). The user types the master password
//! once per session; until then every secret read or write fails with [`Error::VaultLocked`]. Before the first secret
//! there is no file and nothing to ask for: the first write asks for a new master password.
//!
//! The choice is made once per run, by asking the keychain for an entry that doesn't exist: a platform failure means
//! there is no keychain to use.

use crate::connect::launcher::{Sealed, seal, unseal};
use crate::error::{Error, Result};
use secrecy::{ExposeSecret, SecretSlice, SecretString};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, OnceLock, PoisonError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Keychain,
    Vault,
}

/// Looked up in the keychain to see whether it answers; never written.
const PROBE: (&str, &str) = ("dev.ritesh.tableminus.probe", "probe");
const VAULT_FILE: &str = "secrets.vault";
/// Far more than a vault of passwords; a bigger file isn't one, and isn't read.
const MAX_VAULT: u64 = 16 * 1024 * 1024;
const MIN_MASTER: usize = 8;

static BACKEND: OnceLock<Backend> = OnceLock::new();
static VAULT: LazyLock<Mutex<Option<Vault>>> = LazyLock::new(Mutex::default);

fn backend() -> Backend {
    *BACKEND.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return Backend::Keychain;
        }
        let answer = keyring::Entry::new(PROBE.0, PROBE.1).and_then(|e| e.get_password());
        if matches!(answer, Err(keyring::Error::PlatformFailure(_))) { Backend::Vault } else { Backend::Keychain }
    })
}

/// Tells the vault where its file is: in the config folder. Called once at startup.
pub fn init(config: &Path) {
    let mut vault = VAULT.lock().unwrap_or_else(PoisonError::into_inner);
    if vault.is_none() {
        *vault = Some(Vault::new(config.join(VAULT_FILE)));
    }
}

/// Runs `f` on the vault; an error when [`init`] hasn't run.
fn with_vault<T>(f: impl FnOnce(&mut Vault) -> Result<T>) -> Result<T> {
    let mut vault = VAULT.lock().unwrap_or_else(PoisonError::into_inner);
    f(vault.as_mut().ok_or(Error::Invalid("the secrets vault isn't set up"))?)
}

/// Runs `f` on the vault, or gives `none` when [`init`] hasn't run: without a vault nothing is stored, so there is
/// nothing to read or remove (unit tests run that way).
fn with_vault_or<T>(none: T, f: impl FnOnce(&mut Vault) -> Result<T>) -> Result<T> {
    let mut vault = VAULT.lock().unwrap_or_else(PoisonError::into_inner);
    vault.as_mut().map_or(Ok(none), f)
}

fn vault_key(service: &str, account: &str) -> String {
    format!("{service}\u{1f}{account}")
}

/// The stored secret, or None when there isn't one.
pub fn get(service: &str, account: &str) -> Result<Option<SecretString>> {
    match backend() {
        Backend::Keychain => match keyring::Entry::new(service, account)?.get_password() {
            Ok(s) => Ok(Some(s.into())),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        },
        Backend::Vault => with_vault_or(None, |v| v.get(&vault_key(service, account))),
    }
}

pub fn set(service: &str, account: &str, secret: &SecretString) -> Result<()> {
    match backend() {
        Backend::Keychain => Ok(keyring::Entry::new(service, account)?.set_password(secret.expose_secret())?),
        Backend::Vault => with_vault(|v| v.set(vault_key(service, account), secret.clone())),
    }
}

/// Removes the secret; one that isn't there is fine.
pub fn delete(service: &str, account: &str) -> Result<()> {
    match backend() {
        Backend::Keychain => match keyring::Entry::new(service, account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        },
        Backend::Vault => with_vault_or((), |v| v.delete(&vault_key(service, account))),
    }
}

/// The vault file and, once unlocked, its secrets and the master password that seals them again on each change.
struct Vault {
    path: PathBuf,
    open: Option<Opened>,
}

struct Opened {
    master: SecretString,
    secrets: BTreeMap<String, SecretString>,
}

impl Vault {
    fn new(path: PathBuf) -> Self {
        Vault { path, open: None }
    }

    fn exists(&self) -> bool {
        self.path.exists()
    }

    fn opened(&self) -> Result<&Opened> {
        self.open.as_ref().ok_or(Error::VaultLocked)
    }

    /// Opens the vault with `master`, or creates it with that password when there is no file yet. A file that can't
    /// be read is never written over.
    fn unlock(&mut self, master: SecretString) -> Result<()> {
        let failed = |source| Error::File { path: self.path.display().to_string(), source };
        let text = match std::fs::metadata(&self.path) {
            Ok(m) if m.len() > MAX_VAULT => {
                return Err(Error::Invalid("the secrets vault file is too large to be one"));
            }
            Ok(_) => Some(std::fs::read_to_string(&self.path).map_err(failed)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(failed(e)),
        };
        let Some(text) = text else {
            if master.expose_secret().chars().count() < MIN_MASTER {
                return Err(Error::Invalid("use at least 8 characters for the master password"));
            }
            self.open = Some(Opened { master, secrets: BTreeMap::new() });
            return self.save();
        };
        let sealed: Sealed =
            serde_json::from_str(&text).map_err(|_| Error::Invalid("the secrets vault file is damaged"))?;
        let plain = unseal(&master, &sealed).map_err(|e| match e {
            Error::FileUnsealFailed => Error::WrongMasterPassword,
            e => e,
        })?;
        let map: BTreeMap<String, String> = serde_json::from_slice(plain.expose_secret())
            .map_err(|_| Error::Invalid("the secrets vault file is damaged"))?;
        let secrets = map.into_iter().map(|(k, v)| (k, SecretString::from(v))).collect();
        self.open = Some(Opened { master, secrets });
        Ok(())
    }

    /// Without a file nothing is stored yet, so a read needs no master password; a vault is made on the first write.
    fn get(&self, key: &str) -> Result<Option<SecretString>> {
        match &self.open {
            Some(open) => Ok(open.secrets.get(key).cloned()),
            None if !self.exists() => Ok(None),
            None => Err(Error::VaultLocked),
        }
    }

    fn set(&mut self, key: String, secret: SecretString) -> Result<()> {
        self.open.as_mut().ok_or(Error::VaultLocked)?.secrets.insert(key, secret);
        self.save()
    }

    fn delete(&mut self, key: &str) -> Result<()> {
        if self.open.is_none() && !self.exists() {
            return Ok(());
        }
        if self.open.as_mut().ok_or(Error::VaultLocked)?.secrets.remove(key).is_some() {
            self.save()?;
        }
        Ok(())
    }

    /// Seals the secrets (a fresh salt and nonce each time) and writes them through a temporary file.
    fn save(&self) -> Result<()> {
        let open = self.opened()?;
        let exposed: BTreeMap<&str, &str> = open.secrets.iter().map(|(k, v)| (k.as_str(), v.expose_secret())).collect();
        let plain: SecretSlice<u8> = serde_json::to_vec(&exposed)?.into();
        let text = serde_json::to_string(&seal(&open.master, &plain)?)?;
        crate::assistant::mcp::write_private(&self.path, text.as_bytes())
    }

    /// Moves the file aside (it may yet be opened by someone who remembers the password) and starts over.
    fn reset(&mut self) -> Result<()> {
        if self.exists() {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default();
            let mut aside = self.path.as_os_str().to_owned();
            aside.push(format!(".forgotten-{stamp}"));
            std::fs::rename(&self.path, PathBuf::from(aside))
                .map_err(|source| Error::File { path: self.path.display().to_string(), source })?;
        }
        self.open = None;
        Ok(())
    }
}

#[derive(Debug, Serialize, PartialEq)]
pub struct VaultStatus {
    /// Secrets go to the vault on this computer, as it has no keychain.
    pub needed: bool,
    /// A vault file exists: unlocking opens it rather than creating one.
    pub exists: bool,
    pub unlocked: bool,
}

/// Keychain probing and argon2 block, so the commands run off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f).await?
}

#[tauri::command]
pub async fn vault_status() -> Result<VaultStatus> {
    blocking(|| {
        if backend() == Backend::Keychain {
            return Ok(VaultStatus { needed: false, exists: false, unlocked: false });
        }
        with_vault(|v| Ok(VaultStatus { needed: true, exists: v.exists(), unlocked: v.open.is_some() }))
    })
    .await
}

/// Opens the vault (or creates it). The passcode lock is decided again afterwards: its hash lives in the vault.
#[tauri::command]
pub async fn vault_unlock(open: tauri::State<'_, crate::Open>, master: SecretString) -> Result<()> {
    let r = blocking(move || with_vault(|v| v.unlock(master))).await;
    if matches!(r, Err(Error::WrongMasterPassword)) {
        // Slows down guessing, as the passcode does.
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    r?;
    crate::app::passcode::lock_at_startup(open.inner().clone()).await;
    Ok(())
}

/// Starts over without a vault (a new one is made on the next save); the old file is kept beside it, renamed.
#[tauri::command]
pub async fn vault_reset(open: tauri::State<'_, crate::Open>) -> Result<()> {
    blocking(|| with_vault(Vault::reset)).await?;
    crate::app::passcode::lock_at_startup(open.inner().clone()).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Vault;
    use crate::error::Error;
    use secrecy::{ExposeSecret, SecretString};
    use std::path::PathBuf;

    fn path(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tableminus-vault-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); // a leftover from an earlier run; none is fine
        std::fs::create_dir_all(&d).unwrap();
        d.join("secrets.vault")
    }

    fn master(s: &str) -> SecretString {
        s.into()
    }

    #[test]
    fn vault_should_keep_secrets_across_a_reopen_with_the_master_password() {
        let p = path("reopen");
        let mut v = Vault::new(p.clone());
        v.unlock(master("correct horse")).unwrap();
        v.set("db\u{1f}local".into(), "s3cret".into()).unwrap();
        let mut again = Vault::new(p.clone());
        again.unlock(master("correct horse")).unwrap();
        assert_eq!(again.get("db\u{1f}local").unwrap().unwrap().expose_secret(), "s3cret");
        assert!(!std::fs::read_to_string(&p).unwrap().contains("s3cret"), "sealed on disk");
    }

    #[test]
    fn vault_should_refuse_a_wrong_master_password_and_stay_locked() {
        let p = path("wrong");
        Vault::new(p.clone()).unlock(master("correct horse")).unwrap();
        let mut v = Vault::new(p);
        assert!(matches!(v.unlock(master("wrong horse")), Err(Error::WrongMasterPassword)));
        assert!(v.open.is_none());
    }

    #[test]
    fn vault_should_want_the_master_password_to_write_a_first_secret() {
        let mut v = Vault::new(path("first"));
        assert!(matches!(v.set("x".into(), "y".into()), Err(Error::VaultLocked)));
        assert!(!v.exists(), "nothing written");
    }

    #[test]
    fn vault_without_a_file_should_read_as_empty() {
        let mut v = Vault::new(path("empty"));
        assert!(v.get("x").unwrap().is_none());
        v.delete("x").unwrap();
    }

    #[test]
    fn vault_with_a_file_should_refuse_reads_while_locked() {
        let p = path("locked");
        Vault::new(p.clone()).unlock(master("correct horse")).unwrap();
        let mut v = Vault::new(p);
        assert!(matches!(v.get("x"), Err(Error::VaultLocked)));
        assert!(matches!(v.delete("x"), Err(Error::VaultLocked)));
    }

    #[test]
    fn vault_should_want_a_long_enough_new_master_password() {
        let mut v = Vault::new(path("short"));
        assert!(v.unlock(master("short")).is_err());
        assert!(!v.exists());
    }

    #[test]
    fn vault_should_leave_a_damaged_file_alone() {
        let p = path("damaged");
        std::fs::write(&p, "not a vault").unwrap();
        let mut v = Vault::new(p.clone());
        assert!(v.unlock(master("correct horse")).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "not a vault");
    }

    #[test]
    fn vault_should_delete_a_secret() {
        let mut v = Vault::new(path("delete"));
        v.unlock(master("correct horse")).unwrap();
        v.set("a".into(), "1".into()).unwrap();
        v.delete("a").unwrap();
        v.delete("never there").unwrap();
        assert!(v.get("a").unwrap().is_none());
    }

    #[test]
    fn vault_reset_should_move_the_old_file_aside() {
        let p = path("reset");
        let mut v = Vault::new(p.clone());
        v.unlock(master("correct horse")).unwrap();
        v.reset().unwrap();
        assert!(!p.exists());
        assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1, "the old file, renamed");
        assert!(v.open.is_none());
    }
}

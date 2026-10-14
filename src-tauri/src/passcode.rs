//! The app passcode that locks the window at startup and after time away. Only an argon2 hash of it is kept, in the
//! keychain. The lock itself is the UI's: the backend checks passcodes and keeps the hash.
// ponytail: commands still answer while the UI is locked (and the MCP server keeps serving); gate them on a backend
// lock flag if the passcode has to stop more than someone at the keyboard.

use crate::error::{Error, Result};
use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use secrecy::{ExposeSecret, SecretString};
use std::time::Duration;
use tauri::AppHandle;

const SERVICE: &str = "dev.ritesh.dbmanager.passcode";
const ACCOUNT: &str = "app";
const MIN_LEN: usize = 4;

fn entry() -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, ACCOUNT)?)
}

fn stored_hash() -> Result<Option<String>> {
    match entry()?.get_password() {
        Ok(h) => Ok(Some(h)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn hash(passcode: &SecretString) -> Result<String> {
    let h = Argon2::default()
        .hash_password(passcode.expose_secret().as_bytes())
        .map_err(|e| Error::Passcode(e.to_string()))?;
    Ok(h.to_string())
}

fn matches(passcode: &SecretString, hash: &str) -> bool {
    Argon2::default().verify_password(passcode.expose_secret().as_bytes(), hash).is_ok()
}

/// Checks `passcode` against the stored hash; a wrong one waits a second first, to slow down guessing.
async fn check(passcode: SecretString) -> Result<()> {
    let ok = blocking(move || Ok(stored_hash()?.is_none_or(|h| matches(&passcode, &h)))).await?;
    if ok {
        return Ok(());
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    Err(Error::WrongPasscode)
}

/// Keychain access and argon2 both block, so they run off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f).await?
}

/// Whether a passcode is set (so the UI locks at startup).
#[tauri::command]
pub async fn passcode_set() -> Result<bool> {
    blocking(|| Ok(stored_hash()?.is_some())).await
}

/// Sets or changes the passcode; changing it needs the current one.
#[tauri::command]
pub async fn set_passcode(current: Option<SecretString>, passcode: SecretString) -> Result<()> {
    if passcode.expose_secret().chars().count() < MIN_LEN {
        return Err(Error::Invalid("use at least 4 characters"));
    }
    check(current.unwrap_or_default()).await?;
    blocking(move || Ok(entry()?.set_password(&hash(&passcode)?)?)).await
}

#[tauri::command]
pub async fn remove_passcode(current: SecretString) -> Result<()> {
    check(current).await?;
    blocking(forget_hash).await
}

fn forget_hash() -> Result<()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[tauri::command]
pub async fn unlock(passcode: SecretString) -> Result<()> {
    check(passcode).await
}

#[tauri::command]
pub async fn unlock_with_touch_id() -> Result<()> {
    crate::safety::touch_id("unlock DB Manager").await
}

/// The way out for a forgotten passcode. On macOS the Mac's own login (Touch ID or password) removes it. Elsewhere it
/// goes together with every saved connection's passwords, so removing it gives nothing away.
#[tauri::command]
pub async fn forgot_passcode(app: AppHandle) -> Result<()> {
    if cfg!(target_os = "macos") {
        crate::safety::touch_id("remove the DB Manager passcode").await?;
    } else {
        let dir = crate::dir(&app)?;
        blocking(move || crate::store::load(&dir)?.iter().try_for_each(|c| crate::store::forget_secrets(&c.name)))
            .await?;
    }
    blocking(forget_hash).await
}

#[cfg(test)]
mod tests {
    use super::{hash, matches};
    use secrecy::SecretString;

    #[test]
    fn hash_is_argon2_and_never_the_passcode() {
        let h = hash(&SecretString::from("open sesame")).unwrap();
        assert!(h.starts_with("$argon2id$"), "{h}");
        assert!(!h.contains("open sesame"));
    }

    #[test]
    fn matches_only_the_same_passcode() {
        let h = hash(&SecretString::from("1234")).unwrap();
        assert!(matches(&SecretString::from("1234"), &h));
        assert!(!matches(&SecretString::from("1235"), &h));
        assert!(!matches(&SecretString::from("1234"), "not a hash"));
    }
}

//! The app passcode that locks the window at startup and after time away. Only an argon2 hash of it is kept, in the
//! keychain. While locked, [`crate::Open`] refuses every database request, the MCP server's included, so the lock
//! stops more than the window.

use crate::error::{Error, Result};
use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use secrecy::{ExposeSecret, SecretString};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, State};

// The pre-rename name, kept so an existing passcode still unlocks.
const SERVICE: &str = "dev.ritesh.dbmanager.passcode";
const ACCOUNT: &str = "app";
const MIN_LEN: usize = 4;

fn stored_hash() -> Result<Option<String>> {
    Ok(crate::app::secrets::get(SERVICE, ACCOUNT)?.map(|h| h.expose_secret().to_string()))
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
    blocking(move || crate::app::secrets::set(SERVICE, ACCOUNT, &hash(&passcode)?.into())).await
}

#[tauri::command]
pub async fn remove_passcode(open: State<'_, crate::Open>, current: SecretString) -> Result<()> {
    check(current).await?;
    blocking(forget_hash).await?;
    open.locked.store(false, Ordering::SeqCst);
    Ok(())
}

fn forget_hash() -> Result<()> {
    crate::app::secrets::delete(SERVICE, ACCOUNT)
}

/// The app starts locked; this lets it go when no passcode is set. A keychain that can't be read keeps it locked.
pub async fn lock_at_startup(open: crate::Open) {
    if let Ok(false) = blocking(|| Ok(stored_hash()?.is_some())).await {
        open.locked.store(false, Ordering::SeqCst);
    }
}

/// The UI locked itself (time away); databases lock with it. A keychain that can't be read locks them too, as at
/// startup, rather than leaving them reachable behind a locked window.
#[tauri::command]
pub async fn lock(open: State<'_, crate::Open>) -> Result<()> {
    if !matches!(blocking(|| Ok(stored_hash()?.is_some())).await, Ok(false)) {
        open.locked.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[tauri::command]
pub async fn unlock(open: State<'_, crate::Open>, passcode: SecretString) -> Result<()> {
    check(passcode).await?;
    open.locked.store(false, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
pub async fn unlock_with_touch_id(open: State<'_, crate::Open>) -> Result<()> {
    crate::safety::touch_id("unlock TableMinus").await?;
    open.locked.store(false, Ordering::SeqCst);
    Ok(())
}

/// The way out for a forgotten passcode. On macOS the Mac's own login (Touch ID or password) removes it. Elsewhere it
/// goes together with every saved connection's passwords, so removing it gives nothing away.
#[tauri::command]
pub async fn forgot_passcode(app: AppHandle, open: State<'_, crate::Open>) -> Result<()> {
    if cfg!(target_os = "macos") {
        crate::safety::touch_id("remove the TableMinus passcode").await?;
    } else {
        let dir = crate::dir(&app)?;
        blocking(move || {
            crate::connect::store::load(&dir)?.iter().try_for_each(|c| crate::connect::store::forget_secrets(&c.name))
        })
        .await?;
    }
    blocking(forget_hash).await?;
    open.locked.store(false, Ordering::SeqCst);
    Ok(())
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

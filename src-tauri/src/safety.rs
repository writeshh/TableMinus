//! Safe mode: five levels, from silent to asking for the password, checked by the backend wherever SQL runs.

use crate::error::{Error, Result};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

/// How careful a connection is about running SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Safety {
    /// Runs everything.
    Silent,
    /// Asks before every query.
    Alert1,
    /// Asks before anything that isn't provably read-only. The default, as `confirm_writes: true` was.
    #[default]
    Alert2,
    /// Wants the database password before every query.
    Safe1,
    /// Wants the database password before anything that isn't provably read-only.
    Safe2,
}

impl Safety {
    /// Whether a request needs the user's go-ahead at this level.
    pub fn asks(self, read_only: bool) -> bool {
        match self {
            Safety::Silent => false,
            Safety::Alert1 | Safety::Safe1 => true,
            Safety::Alert2 | Safety::Safe2 => !read_only,
        }
    }

    /// Whether the go-ahead is the database password (or Touch ID) rather than a click.
    pub fn wants_password(self) -> bool {
        matches!(self, Safety::Safe1 | Safety::Safe2)
    }
}

/// Reads a level, or the boolean `confirm_writes` saved before levels existed (true asked before writes).
pub fn level_or_legacy<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Safety, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Level(Safety),
        Legacy(bool),
    }
    Ok(match Stored::deserialize(d)? {
        Stored::Level(s) => s,
        Stored::Legacy(true) => Safety::Alert2,
        Stored::Legacy(false) => Safety::Silent,
    })
}

/// How the user proved it's them at a Safe level.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unlock {
    Password(SecretString),
    TouchId,
    /// A token from [`batch_grant`]: one Touch ID prompt stands for a whole Run All.
    Grant(SecretString),
}

/// How long a batch grant lasts. Long enough for a script's statements, short enough that a forgotten one is gone.
const GRANT_LIFE: Duration = Duration::from_secs(300);

/// Live batch grants, as digests of their tokens and when they expire. In memory only: a restart ends them all.
static GRANTS: Mutex<Vec<([u8; 32], Instant)>> = Mutex::new(Vec::new());

fn token_digest(token: &SecretString) -> [u8; 32] {
    Sha256::digest(token.expose_secret().as_bytes()).into()
}

/// Asks for Touch ID once and returns a token that stands for it for a few minutes, so Run All at a Safe level asks
/// once for the batch instead of once per statement. A password needs no grant: sending it again doesn't ask.
#[tauri::command]
pub async fn batch_grant() -> Result<String> {
    touch_id("run these statements in safe mode").await?;
    let token = crate::mcp::random_hex(32)?;
    let now = Instant::now();
    let mut grants = GRANTS.lock().unwrap_or_else(PoisonError::into_inner);
    grants.retain(|(_, until)| *until > now);
    grants.push((token_digest(&SecretString::from(token.clone())), now + GRANT_LIFE));
    Ok(token)
}

/// Whether `token` is a live grant, compared in constant time.
fn granted(token: &SecretString) -> bool {
    let digest = token_digest(token);
    let now = Instant::now();
    GRANTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|(d, until)| *until > now && bool::from(d.ct_eq(&digest)))
}

/// Lets a request through, or says what the UI must ask for before sending it again: [`Error::ConfirmRequired`]
/// (a click) or [`Error::PasswordRequired`] (the database password, or Touch ID). `stored` is the connection's
/// password from the keychain.
pub async fn check(
    level: Safety,
    read_only: bool,
    confirmed: bool,
    unlock: Option<&Unlock>,
    stored: Option<&SecretString>,
) -> Result<()> {
    if !level.asks(read_only) {
        return Ok(());
    }
    if !level.wants_password() {
        return if confirmed { Ok(()) } else { Err(Error::ConfirmRequired) };
    }
    verify(unlock.ok_or(Error::PasswordRequired)?, stored).await
}

/// Checks the password against the keychain's (in constant time), or asks for Touch ID.
pub async fn verify(unlock: &Unlock, stored: Option<&SecretString>) -> Result<()> {
    match unlock {
        Unlock::Password(given) => {
            let stored = stored.ok_or(Error::SafeModeNoPassword)?;
            // Hashing first makes both sides the same length, so the compare doesn't leak the password's length.
            let digest = |s: &SecretString| Sha256::digest(s.expose_secret().as_bytes());
            if bool::from(digest(given).ct_eq(&digest(stored))) { Ok(()) } else { Err(Error::WrongPassword) }
        }
        Unlock::TouchId => touch_id("run a query in safe mode").await,
        // An expired grant asks again, as if none had been given.
        Unlock::Grant(token) => {
            if granted(token) {
                Ok(())
            } else {
                Err(Error::PasswordRequired)
            }
        }
    }
}

/// Asks for Touch ID (or the Mac's login password) with `reason` shown in the prompt.
#[cfg(target_os = "macos")]
pub async fn touch_id(reason: &str) -> Result<()> {
    use robius_authentication::{AndroidText, BiometricStrength, Context, PolicyBuilder, Text, WindowsText};
    let reason = reason.to_string();
    let answer = tauri::async_runtime::spawn_blocking(move || {
        let policy = PolicyBuilder::new()
            .biometrics(Some(BiometricStrength::Strong))
            .password(true)
            .build()
            .ok_or(Error::TouchId("this Mac can't ask for Touch ID".into()))?;
        let text = Text {
            android: AndroidText { title: "TableMinus", subtitle: None, description: None },
            apple: &reason,
            windows: WindowsText::new_truncated("TableMinus", &reason),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        Context::new(())
            .authenticate(text, &policy, move |r| {
                // The receiver only goes when the wait below gave up; then nobody needs the answer.
                let _ = tx.send(r);
            })
            .map_err(|e| Error::TouchId(format!("{e:?}")))?;
        match rx.recv_timeout(std::time::Duration::from_secs(120)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(Error::TouchId(format!("{e:?}"))),
            Err(_) => Err(Error::TouchId("no answer".into())),
        }
    })
    .await;
    answer.map_err(|e| Error::TouchId(e.to_string()))?
}

#[cfg(not(target_os = "macos"))]
#[expect(clippy::unused_async, reason = "the same signature as the macOS version, which awaits the prompt")]
pub async fn touch_id(_reason: &str) -> Result<()> {
    Err(Error::TouchId("only on macOS".into()))
}

/// Whether Touch ID can be offered here.
#[tauri::command]
pub fn touch_id_available() -> bool {
    cfg!(target_os = "macos")
}

#[cfg(test)]
mod tests {
    use super::{GRANTS, Safety, Unlock, check, token_digest};
    use crate::error::Error;
    use secrecy::SecretString;
    use std::sync::PoisonError;
    use std::time::{Duration, Instant};

    fn run(level: Safety, read_only: bool, confirmed: bool, unlock: Option<&Unlock>) -> Result<(), Error> {
        let stored = SecretString::from("hunter2");
        tauri::async_runtime::block_on(check(level, read_only, confirmed, unlock, Some(&stored)))
    }

    fn password(p: &str) -> Unlock {
        Unlock::Password(p.into())
    }

    #[test]
    fn silent_runs_everything() {
        assert!(run(Safety::Silent, false, false, None).is_ok());
    }

    #[test]
    fn alert1_asks_even_for_reads() {
        assert!(matches!(run(Safety::Alert1, true, false, None), Err(Error::ConfirmRequired)));
        assert!(run(Safety::Alert1, true, true, None).is_ok());
    }

    #[test]
    fn alert2_asks_only_for_writes() {
        assert!(run(Safety::Alert2, true, false, None).is_ok());
        assert!(matches!(run(Safety::Alert2, false, false, None), Err(Error::ConfirmRequired)));
        assert!(run(Safety::Alert2, false, true, None).is_ok());
    }

    #[test]
    fn safe1_wants_the_password_even_for_reads() {
        assert!(matches!(run(Safety::Safe1, true, true, None), Err(Error::PasswordRequired)), "a click isn't enough");
        assert!(run(Safety::Safe1, true, false, Some(&password("hunter2"))).is_ok());
    }

    #[test]
    fn safe2_wants_the_password_only_for_writes() {
        assert!(run(Safety::Safe2, true, false, None).is_ok());
        assert!(matches!(run(Safety::Safe2, false, false, None), Err(Error::PasswordRequired)));
        assert!(run(Safety::Safe2, false, false, Some(&password("hunter2"))).is_ok());
    }

    #[test]
    fn safe_levels_refuse_a_wrong_password() {
        assert!(matches!(run(Safety::Safe2, false, true, Some(&password("hunter3"))), Err(Error::WrongPassword)));
        assert!(matches!(run(Safety::Safe1, true, true, Some(&password(""))), Err(Error::WrongPassword)));
    }

    #[test]
    fn safe_levels_refuse_when_no_password_is_stored() {
        let r = tauri::async_runtime::block_on(check(Safety::Safe1, true, true, Some(&password("")), None));
        assert!(matches!(r, Err(Error::SafeModeNoPassword)));
    }

    /// A grant as `batch_grant` would record it after Touch ID, expiring at `until`; the token to send.
    fn grant(token: &str, until: Instant) -> Unlock {
        GRANTS.lock().unwrap_or_else(PoisonError::into_inner).push((token_digest(&token.into()), until));
        Unlock::Grant(token.into())
    }

    #[test]
    fn safe_levels_take_a_live_batch_grant() {
        let g = grant("live-grant", Instant::now() + Duration::from_secs(60));
        assert!(run(Safety::Safe1, true, false, Some(&g)).is_ok());
        assert!(run(Safety::Safe2, false, false, Some(&g)).is_ok(), "the same grant serves the next statement");
    }

    #[test]
    fn an_expired_or_unknown_grant_asks_again() {
        let expired = grant("old-grant", Instant::now().checked_sub(Duration::from_secs(1)).unwrap());
        assert!(matches!(run(Safety::Safe1, true, false, Some(&expired)), Err(Error::PasswordRequired)));
        let unknown = Unlock::Grant("never-granted".into());
        assert!(matches!(run(Safety::Safe1, true, false, Some(&unknown)), Err(Error::PasswordRequired)));
    }

    #[test]
    fn unlock_should_arrive_from_the_ui_as_json() {
        let g: Unlock = serde_json::from_str(r#"{"grant":"abc"}"#).unwrap();
        assert!(matches!(g, Unlock::Grant(_)));
        assert!(matches!(serde_json::from_str::<Unlock>(r#""touch_id""#).unwrap(), Unlock::TouchId));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn batch_grant_needs_touch_id() {
        assert!(matches!(tauri::async_runtime::block_on(super::batch_grant()), Err(Error::TouchId(_))));
    }
}

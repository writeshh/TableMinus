//! The open sessions of an engine that isn't SQL (Redis, `MongoDB`, `DynamoDB`, Elasticsearch), by session key as
//! `Open` keys the SQL ones: the saved connection's name, then `\u{1f}` and a database tab's own key.

use crate::error::{Error, Result};
use crate::safety::Safety;
use crate::store::Saved;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard};

/// A session that knows the connection it was opened for, so a safe mode change can reach it.
pub trait Session: Send + 'static {
    fn saved_mut(&mut self) -> &mut Saved;
}

pub struct Sessions<S>(Mutex<HashMap<String, Arc<Mutex<S>>>>);

impl<S> Default for Sessions<S> {
    fn default() -> Self {
        Sessions(Mutex::new(HashMap::new()))
    }
}

/// The saved connection's name in a session key.
pub fn connection(key: &str) -> &str {
    key.split_once('\u{1f}').map_or(key, |(n, _)| n)
}

impl<S> Sessions<S> {
    /// The session `name`, locked for one command.
    pub async fn get(&self, name: &str) -> Result<OwnedMutexGuard<S>> {
        let s = self.0.lock().await.get(name).cloned().ok_or(Error::NotConnected)?;
        Ok(s.lock_owned().await)
    }

    /// The session `name`, when it's open.
    pub async fn existing(&self, name: &str) -> Option<Arc<Mutex<S>>> {
        self.0.lock().await.get(name).cloned()
    }

    pub async fn insert(&self, name: String, session: S) {
        self.0.lock().await.insert(name, Arc::new(Mutex::new(session)));
    }

    /// Adds `session` unless another connect opened `name` meanwhile; then that one stays and `session` closes.
    pub async fn insert_new(&self, name: String, session: S) {
        self.0.lock().await.entry(name).or_insert_with(|| Arc::new(Mutex::new(session)));
    }

    pub async fn remove(&self, name: &str) {
        self.0.lock().await.remove(name);
    }

    /// Takes every session of the saved connection `name` out, database tabs included.
    pub async fn remove_all(&self, name: &str) {
        self.0.lock().await.retain(|k, _| connection(k) != name);
    }
}

impl<S: Session> Sessions<S> {
    /// A new safe mode level for every session of the saved connection `name`.
    pub async fn set_safety(&self, name: &str, level: Safety) {
        let sessions: Vec<_> =
            self.0.lock().await.iter().filter(|(k, _)| connection(k) == name).map(|(_, s)| s.clone()).collect();
        for s in sessions {
            s.lock().await.saved_mut().safety = level;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Sessions;

    #[test]
    fn insert_new_should_keep_the_session_that_was_there_first() {
        tauri::async_runtime::block_on(async {
            let sessions = Sessions::default();
            sessions.insert_new("a".into(), "first").await;
            sessions.insert_new("a".into(), "late").await;
            assert_eq!(*sessions.get("a").await.unwrap(), "first");
        });
    }

    #[test]
    fn remove_all_should_drop_every_session_of_the_connection_and_no_other() {
        tauri::async_runtime::block_on(async {
            let sessions = Sessions::default();
            for key in ["prod", "prod\u{1f}2", "prod 2"] {
                sessions.insert(key.into(), ()).await;
            }
            sessions.remove_all("prod").await;
            assert!(sessions.existing("prod").await.is_none());
            assert!(sessions.existing("prod\u{1f}2").await.is_none(), "a database tab goes too");
            assert!(sessions.existing("prod 2").await.is_some(), "another connection's name only looks alike");
        });
    }
}

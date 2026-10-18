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

impl<S: Session> Sessions<S> {
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

    pub async fn remove(&self, name: &str) {
        self.0.lock().await.remove(name);
    }

    /// A new safe mode level for every session of the saved connection `name`.
    pub async fn set_safety(&self, name: &str, level: Safety) {
        let sessions: Vec<_> =
            self.0.lock().await.iter().filter(|(k, _)| connection(k) == name).map(|(_, s)| s.clone()).collect();
        for s in sessions {
            s.lock().await.saved_mut().safety = level;
        }
    }
}

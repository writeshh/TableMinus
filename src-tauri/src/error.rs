//! The one error type every command returns. It reaches the UI as its message text.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Safe mode stopped a write. The UI matches this exact text, asks, then resends with `confirmed: true`.
    #[error("CONFIRM")]
    ConfirmRequired,
    #[error("not connected")]
    NotConnected,
    #[error("no saved connection named {0:?}")]
    NoSuchConnection(String),
    #[error("no answer from the server after {0} seconds")]
    ConnectTimeout(u64),
    /// A request the UI should not have sent, such as an edit with no key.
    #[error("{0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Postgres(#[from] tokio_postgres::Error),
    #[error(transparent)]
    MySql(#[from] mysql_async::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("TLS: {0}")]
    Tls(#[from] native_tls::Error),
    #[error("keychain: {0}")]
    Keychain(#[from] keyring::Error),
    #[error("connections file: {0}")]
    Io(#[from] std::io::Error),
    #[error("connections file is not valid ({0}); fix or move it, then reopen the app")]
    Json(#[from] serde_json::Error),
    #[error("invalid connection URL: {0}")]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    Tauri(#[from] tauri::Error),
}

impl serde::Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

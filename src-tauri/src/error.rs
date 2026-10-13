//! The one error type every command returns. It reaches the UI as its message text.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Safe mode stopped a write. The UI matches this exact text, asks, then resends with `confirmed: true`.
    #[error("CONFIRM")]
    ConfirmRequired,
    #[error("not connected")]
    NotConnected,
    #[error("the query was stopped after the {0}-second query timeout (Settings › General)")]
    QueryTimeout(u64),
    /// The server or network dropped the connection. The UI matches this exact text and offers to reconnect.
    #[error("DISCONNECTED")]
    Disconnected,
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
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("can't read {path}: {source}")]
    File { path: String, source: std::io::Error },
    #[error("connections file is not valid ({0}); fix or move it, then reopen the app")]
    Json(#[from] serde_json::Error),
    #[error("MCP settings file is not valid ({0}); fix or move mcp.json, then reopen the app")]
    McpConfig(serde_json::Error),
    #[error("invalid connection URL: {0}")]
    Url(#[from] url::ParseError),
    #[error(transparent)]
    Tauri(#[from] tauri::Error),
    #[error("SSH: {0}")]
    Ssh(#[from] russh::Error),
    #[error("SSH key: {0}")]
    SshKey(#[from] russh::keys::Error),
    #[error("~/.ssh/config: {0}")]
    SshConfig(#[from] russh_config::Error),
    #[error("SSH sign-in failed: {0}")]
    SshAuth(&'static str),
    #[error(
        "the SSH host key for {host} has changed (~/.ssh/known_hosts line {line}). If the server was reinstalled, \
         remove that line; otherwise someone may be intercepting the connection"
    )]
    SshHostKeyChanged { host: String, line: usize },
    #[error("startup script {0}")]
    StartupScript(String),
    #[error("startup SQL: {0}")]
    StartupSql(Box<Error>),
    #[error("the SSH server could not reach {host}:{port}: {reason}")]
    SshForward { host: String, port: u16, reason: String },
}

impl serde::Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

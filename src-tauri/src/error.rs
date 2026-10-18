//! The one error type every command returns. It reaches the UI as `{ code, message }`: the message to show, and for
//! the errors the UI acts on rather than shows, a stable [`Error::code`].

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Safe mode stopped a write. The UI asks, then resends with `confirmed: true`.
    #[error("safe mode wants a confirmation before running this")]
    ConfirmRequired,
    /// Safe 1 and 2 want the database password (or Touch ID). The UI asks, then resends with `unlock`.
    #[error("safe mode wants the database password before running this")]
    PasswordRequired,
    /// The connection asks for its password every time and none was given yet. The UI asks, then hands it over with
    /// `set_session_password` and connects again.
    #[error("this connection asks for its password every time, and none was given yet")]
    AskPassword,
    /// An exported connections file is password-protected. The UI asks for it.
    #[error("the connections file is password-protected")]
    FilePasswordRequired,
    #[error("wrong password, or the file is damaged")]
    FileUnsealFailed,
    /// Imported connections run shell commands before connecting. The UI shows them and imports again with or without.
    #[error("{0}")]
    ImportHasScripts(String),
    #[error("encryption: {0}")]
    Crypto(String),
    /// The UI asks again.
    #[error("that isn't the connection's saved password")]
    WrongPassword,
    #[error(
        "this safe mode level checks the connection's saved password, and it has none; save one in the connection's \
         settings or choose another level"
    )]
    SafeModeNoPassword,
    #[error("Touch ID: {0}")]
    TouchId(String),
    #[error("wrong passcode")]
    WrongPasscode,
    #[error("passcode: {0}")]
    Passcode(String),
    #[error("{0}")]
    Locations(String),
    /// The passcode lock is on.
    #[error("TableMinus is locked; unlock it first")]
    Locked,
    #[error("not connected")]
    NotConnected,
    #[error("the query was stopped after the {0}-second query timeout (Settings › General)")]
    QueryTimeout(u64),
    /// The server or network dropped the connection. The UI offers to reconnect.
    #[error("the connection to the server was lost")]
    Disconnected,
    #[error("no saved connection named {0:?}")]
    NoSuchConnection(String),
    #[error("no answer from the server after {0} seconds")]
    ConnectTimeout(u64),
    /// A request the UI should not have sent, such as an edit with no key.
    #[error("{0}")]
    Invalid(&'static str),
    /// SQLite's table rebuild couldn't read the table's CREATE statement, or SQL typed for it.
    #[error("can't rebuild the table: {0}")]
    Rebuild(String),
    #[error("git: {0}")]
    Git(String),
    /// The Redis server's error, or a reply TableMinus won't read.
    #[error("{0}")]
    Redis(String),
    /// A plugin that can't be installed or read, or whose action failed.
    #[error("plugin: {0}")]
    Plugin(String),
    /// An assistant agent (acp.rs) failed to start, stopped, or refused a request.
    #[error("agent: {0}")]
    Agent(String),
    /// The AI provider couldn't be reached, or refused or garbled the request.
    #[error("AI: {0}")]
    Ai(String),
    #[error("{}", postgres_message(.0))]
    Postgres(#[from] tokio_postgres::Error),
    #[error(transparent)]
    MySql(#[from] mysql_async::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("DuckDB: {0}")]
    DuckDb(#[from] duckdb::Error),
    #[error("SQL Server: {0}")]
    Mssql(#[from] tiberius::error::Error),
    /// `ClickHouse` answered with an error, in its own words.
    #[error("ClickHouse: {0}")]
    ClickHouse(String),
    /// `MongoDB`'s own error, or a script or value the shell couldn't read.
    #[error("MongoDB: {0}")]
    Mongo(String),
    /// `DynamoDB`'s own error (its type and message), or a value typed in `PartiQL` that couldn't be read.
    #[error("DynamoDB: {0}")]
    Dynamo(String),
    /// Oracle's own error (`ORA-…`), or the driver's.
    #[error("Oracle: {0}")]
    Oracle(String),
    /// Cassandra's own error, or the driver's.
    #[error("Cassandra: {0}")]
    Cassandra(String),
    /// Elasticsearch's own error (its type and reason), or a request typed in the console that couldn't be read.
    #[error("Elasticsearch: {0}")]
    Elastic(String),
    /// Checking for, downloading or installing an update failed.
    #[error("update: {0}")]
    Update(String),
    /// A server reached over HTTP (libSQL, Cloudflare D1) answered with an error, or not in its format.
    #[error("{engine}: {message}")]
    Server { engine: &'static str, message: String },
    /// A `DuckDB` value Arrow couldn't turn into text.
    #[error("DuckDB: {0}")]
    Arrow(String),
    #[error("TLS: {0}")]
    Tls(#[from] native_tls::Error),
    /// A CA, certificate or key file rustls couldn't read.
    #[error("TLS: {0}")]
    TlsFiles(String),
    #[error("keychain: {0}")]
    Keychain(#[from] keyring::Error),
    /// Secrets live in the vault file on this computer (no keychain), and it isn't unlocked yet. The UI asks for the
    /// master password.
    #[error("the saved passwords are locked")]
    VaultLocked,
    #[error("wrong master password")]
    WrongMasterPassword,
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
    #[error("SQLite's ALTER TABLE can't {0}; the table has to be rebuilt")]
    SqliteRebuild(&'static str),
    /// MySQL commits DDL statement by statement, so an error can come after some statements took effect.
    #[error("{source}\n\nMySQL commits each DDL statement, so these had already run:\n{ran}")]
    DdlPartlyApplied { ran: String, source: Box<Error> },
    /// A MySQL routine was dropped to be replaced, the new definition failed, and the old one is back.
    #[error("{0}\n\nNothing changed: the original definition was put back.")]
    DdlRestored(Box<Error>),
    #[error("{source}\n\nPutting the original definition back failed too ({restore}). It was:\n\n{original}")]
    DdlRestoreFailed { source: Box<Error>, restore: Box<Error>, original: String },
    #[error("can't write {path}: {source}")]
    Write { path: String, source: std::io::Error },
    /// A backup or restore tool failed; its own error lines.
    #[error("{0}")]
    Tool(String),
    #[error("no {tool} for {server} found{found}. Install the server's client tools, or choose them with Choose…")]
    NoTools { tool: &'static str, server: String, found: String },
    #[error("cancelled")]
    Cancelled,
    #[error("statement {n}: {source}")]
    Statement { n: u64, source: Box<Error> },
    #[error("not a JSON array of objects: {0}")]
    JsonImport(serde_json::Error),
    /// An update or delete would hit several rows. The UI offers row identity.
    #[error("a key matched more than one row; nothing was saved")]
    SeveralRows,
    #[error("the SSH server could not reach {host}:{port}: {reason}")]
    SshForward { host: String, port: u16, reason: String },
}

impl Error {
    /// What the UI branches on, for the errors it acts on (asks, retries, offers something); the message stays free to
    /// change. `ipc.ts` lists the same codes.
    fn code(&self) -> Option<&'static str> {
        Some(match self {
            Error::ConfirmRequired => "confirm",
            Error::PasswordRequired => "password",
            Error::WrongPassword => "wrong_password",
            Error::AskPassword => "ask_password",
            Error::FilePasswordRequired => "file_password",
            Error::FileUnsealFailed => "file_unseal_failed",
            Error::ImportHasScripts(_) => "import_scripts",
            Error::Disconnected => "disconnected",
            Error::SeveralRows => "several_rows",
            Error::VaultLocked => "vault_locked",
            _ => return None,
        })
    }
}

impl serde::Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut out = s.serialize_struct("Error", 2)?;
        out.serialize_field("code", &self.code())?;
        out.serialize_field("message", &self.to_string())?;
        out.end()
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl From<mongodb::error::Error> for Error {
    fn from(e: mongodb::error::Error) -> Self {
        Error::Mongo(e.to_string())
    }
}

/// tokio-postgres's own message leaves out the cause ("db error", "error connecting to server"). A server error shows
/// as the server wrote it (severity, message, detail, hint); anything else gets its chain of causes.
fn postgres_message(e: &tokio_postgres::Error) -> String {
    if let Some(db) = e.as_db_error() {
        return db.to_string();
    }
    let mut message = e.to_string();
    let mut cause = std::error::Error::source(e);
    while let Some(c) = cause {
        message.push_str(": ");
        message.push_str(&c.to_string());
        cause = c.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn an_error_the_ui_acts_on_should_serialize_with_its_code() {
        let json = serde_json::to_value(Error::ConfirmRequired).unwrap();
        assert_eq!(json["code"], "confirm");
        assert_eq!(json["message"], Error::ConfirmRequired.to_string());
    }

    #[test]
    fn an_error_the_ui_only_shows_should_serialize_without_a_code() {
        let json = serde_json::to_value(Error::NotConnected).unwrap();
        assert!(json["code"].is_null());
        assert_eq!(json["message"], "not connected");
    }

    #[test]
    fn a_postgres_connect_error_should_say_why() {
        tauri::async_runtime::block_on(async {
            // Port 1 is closed, so the connect fails at once with the OS's reason as the cause.
            let e = tokio_postgres::connect("host=127.0.0.1 port=1 user=x", tokio_postgres::NoTls).await.err().unwrap();
            let message = Error::Postgres(e).to_string();
            assert!(message.starts_with("error connecting to server: "), "{message}");
            assert!(message.len() > "error connecting to server: ".len(), "{message}");
        });
    }

    #[test]
    #[ignore = "needs a local Postgres"]
    fn a_postgres_server_error_should_show_the_servers_message() {
        tauri::async_runtime::block_on(async {
            let url =
                std::env::var("DBM_PG_URL").unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5432/postgres".into());
            let (client, conn) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await.unwrap();
            tauri::async_runtime::spawn(conn);
            let e = client.simple_query("select 1/0").await.unwrap_err();
            assert_eq!(Error::Postgres(e).to_string(), "ERROR: division by zero");
        });
    }
}

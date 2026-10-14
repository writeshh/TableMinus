//! One connection type over the supported engines: open, run SQL (results as text cells), browse the catalog.

use crate::error::{Error, Result};
use crate::tls::{Tls, TlsMode};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How long opening a connection may take before giving up, so a dead host can't hang the app.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Postgres,
    Mysql,
    Mariadb,
    Sqlite,
}

/// Per-connection options beyond host, user and TLS. Engine-specific ones are ignored by other engines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
#[expect(clippy::struct_excessive_bools, reason = "independent on/off settings, one per checkbox in the form")]
pub struct Options {
    /// SQL run right after connecting, and again whenever the connection is reopened.
    pub startup_sql: String,
    /// Shell script run before connecting (and before any SSH tunnel), e.g. to bring up a VPN. It must finish
    /// within the connect timeout; a non-zero exit stops the connect.
    pub startup_script: String,
    /// Postgres 17+: start TLS straight away instead of asking the server first (`sslnegotiation=direct`).
    pub direct_tls: bool,
    /// Postgres: never use SCRAM channel binding, for poolers and proxies that break it.
    pub no_channel_binding: bool,
    /// Postgres: also list tables in `pg_catalog` and `information_schema`.
    pub system_schemas: bool,
    /// MySQL: connect through this Unix socket instead of host and port.
    pub socket: String,
    /// MySQL: the TLS server name to send and check, for proxies that route by SNI.
    pub sni: String,
    /// MySQL: allow the cleartext password plugin (PAM/LDAP accounts). Sends the password as is: use with TLS.
    pub cleartext_auth: bool,
}

impl Options {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Runs the connection's startup script with `/bin/sh -c`. The script is the user's own shell code, so a
/// shell is exactly what it needs; nothing from elsewhere is interpolated into it.
pub async fn run_startup_script(script: &str) -> Result<()> {
    let run = tokio::process::Command::new("/bin/sh").arg("-c").arg(script).kill_on_drop(true).output();
    let out = tokio::time::timeout(CONNECT_TIMEOUT, run)
        .await
        .map_err(|_| Error::StartupScript(format!("still running after {} seconds", CONNECT_TIMEOUT.as_secs())))??;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let last: String = stderr.trim().lines().last().unwrap_or_default().chars().take(300).collect();
    Err(Error::StartupScript(match out.status.code() {
        Some(code) if last.is_empty() => format!("exited with status {code}"),
        Some(code) => format!("exited with status {code}: {last}"),
        None => "was stopped by a signal".into(),
    }))
}

#[derive(Debug, Serialize)]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub primary: bool,
}

#[derive(Debug, Serialize, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub affected: u64,
    /// The row limit cut the result short.
    pub truncated: bool,
}

/// A way to stop a running query from outside its connection.
pub enum Stopper {
    /// Sends Postgres's cancel request on a separate socket.
    Pg(tokio_postgres::CancelToken),
    /// The MySQL session id, for `KILL QUERY` from a second connection.
    My(u32),
    /// SQLite runs on the calling thread, so it is interrupted from another one.
    Lite(rusqlite::InterruptHandle),
}

/// How an engine writes SQL, for code that builds statements without needing a live connection (DDL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Pg,
    My,
    Lite,
}

impl Dialect {
    /// A quoted identifier. Identifiers can't be bound as parameters, so this and [`Dialect::literal`] are the
    /// only places SQL is escaped by hand.
    pub fn ident(self, name: &str) -> String {
        match self {
            Dialect::My => format!("`{}`", name.replace('`', "``")),
            _ => format!("\"{}\"", name.replace('"', "\"\"")),
        }
    }

    /// A table name; Postgres tables are `schema.table`.
    pub fn table_ident(self, table: &str) -> String {
        match (self, table.split_once('.')) {
            (Dialect::Pg, Some((schema, name))) => format!("{}.{}", self.ident(schema), self.ident(name)),
            _ => self.ident(table),
        }
    }

    /// A quoted string literal, for the few places a value can't be bound: DDL such as `COMMENT ON` takes no
    /// parameters. Postgres gets an `E''` string, whose meaning doesn't depend on `standard_conforming_strings`.
    /// MySQL gets escaped backslashes too: under `NO_BACKSLASH_ESCAPES` a backslash then reads doubled, but the
    /// quote still can't be closed early.
    pub fn literal(self, value: &str) -> String {
        let quoted = value.replace('\'', "''");
        match self {
            Dialect::Pg => format!("E'{}'", quoted.replace('\\', "\\\\")),
            Dialect::My => format!("'{}'", quoted.replace('\\', "\\\\")),
            Dialect::Lite => format!("'{quoted}'"),
        }
    }
}

/// A quoted identifier for code that only knows whether it's MySQL; [`Dialect::ident`] does the quoting.
pub(crate) fn quote_ident(mysql: bool, name: &str) -> String {
    if mysql { Dialect::My.ident(name) } else { Dialect::Pg.ident(name) }
}

/// Receives a query result as it arrives: the column names once, then each row.
pub(crate) trait RowSink {
    fn columns(&mut self, names: Vec<String>) -> Result<()>;
    fn row(&mut self, row: Vec<Option<String>>) -> Result<()>;
}

/// Feeds a MySQL result to `sink` row by row.
async fn my_stream<P: mysql_async::prelude::Protocol>(
    mut res: mysql_async::QueryResult<'_, '_, P>,
    sink: &mut dyn RowSink,
) -> Result<()> {
    sink.columns(res.columns_ref().iter().map(|c| c.name_str().into_owned()).collect())?;
    while let Some(row) = res.next().await? {
        sink.row(my_row(row))?;
    }
    Ok(())
}

// ponytail: an enum over three engines, not a Driver trait; extract a trait when a 4th engine needs it.
pub enum Conn {
    Pg(tokio_postgres::Client),
    My(mysql_async::Conn),
    Lite(rusqlite::Connection),
}

/// A Postgres parameter sent as text, which the server parses as the target column's type, exactly as it
/// would a quoted literal. Lets one code path bind values for every column type.
#[derive(Debug)]
struct PgText<'a>(Option<&'a str>);

impl tokio_postgres::types::ToSql for PgText<'_> {
    fn to_sql(
        &self,
        _: &tokio_postgres::types::Type,
        out: &mut bytes::BytesMut,
    ) -> std::result::Result<tokio_postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
        use tokio_postgres::types::IsNull;
        Ok(match self.0 {
            Some(s) => {
                out.extend_from_slice(s.as_bytes());
                IsNull::No
            }
            None => IsNull::Yes,
        })
    }

    fn accepts(_: &tokio_postgres::types::Type) -> bool {
        true
    }

    fn encode_format(&self, _: &tokio_postgres::types::Type) -> tokio_postgres::types::Format {
        tokio_postgres::types::Format::Text
    }

    tokio_postgres::types::to_sql_checked!();
}

/// A MySQL row as text cells, the way the text protocol would show them. Prepared statements return typed
/// values, so dates and times are formatted from the column's type and fractional-second digits.
fn my_row(row: mysql_async::Row) -> Vec<Option<String>> {
    let columns = row.columns();
    row.unwrap().into_iter().zip(columns.iter()).map(|(v, col)| my_cell(v, col)).collect()
}

fn my_cell(value: mysql_async::Value, col: &mysql_async::Column) -> Option<String> {
    use mysql_async::Value as V;
    use mysql_async::consts::ColumnType;
    let frac = |micros: u32| match col.decimals() {
        digits @ 1..=6 => format!(".{}", &format!("{micros:06}")[..usize::from(digits)]),
        _ => String::new(),
    };
    let date_only = matches!(col.column_type(), ColumnType::MYSQL_TYPE_DATE | ColumnType::MYSQL_TYPE_NEWDATE);
    Some(match value {
        V::NULL => return None,
        V::Bytes(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        V::Int(n) => n.to_string(),
        V::UInt(n) => n.to_string(),
        V::Float(n) => n.to_string(),
        V::Double(n) => n.to_string(),
        V::Date(year, month, day, ..) if date_only => format!("{year:04}-{month:02}-{day:02}"),
        V::Date(year, month, day, hour, minute, second, micros) => {
            format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}{}", frac(micros))
        }
        V::Time(negative, days, hour, minute, second, micros) => {
            let hours = days * 24 + u32::from(hour);
            format!("{}{hours:02}:{minute:02}:{second:02}{}", if negative { "-" } else { "" }, frac(micros))
        }
    })
}

fn lite_cell(v: rusqlite::types::ValueRef) -> Option<String> {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Some(format!("<{} bytes>", b.len())),
    }
}

async fn open_postgres(
    url: &str,
    password: Option<&SecretString>,
    tls: &Tls,
    options: &Options,
    tunnel_port: Option<u16>,
) -> Result<tokio_postgres::Client> {
    let mut cfg: tokio_postgres::Config = match tunnel_port {
        Some(port) => {
            // The URL's port would be dialled alongside the tunnel's, so drop it before adding the tunnel's.
            let mut u = url::Url::parse(url)?;
            let _ = u.set_port(None); // Err only for URLs that can't have a port, which parse() then reports
            let mut cfg: tokio_postgres::Config = u.as_str().parse()?;
            cfg.hostaddr(std::net::Ipv4Addr::LOCALHOST.into()).port(port);
            cfg
        }
        None => url.parse()?,
    };
    if let Some(password) = password {
        cfg.password(password.expose_secret());
    }
    let (mode, connector) = tls.postgres()?;
    cfg.ssl_mode(mode);
    if options.direct_tls {
        cfg.ssl_negotiation(tokio_postgres::config::SslNegotiation::Direct);
    }
    if options.no_channel_binding {
        cfg.channel_binding(tokio_postgres::config::ChannelBinding::Disable);
    }
    let (client, connection) = cfg.connect(connector).await?;
    tauri::async_runtime::spawn(connection); // drives the socket; ends when the client is dropped
    Ok(client)
}

async fn open_mysql(
    url: &str,
    password: Option<&SecretString>,
    tls: &Tls,
    options: &Options,
    tunnel_port: Option<u16>,
) -> Result<mysql_async::Conn> {
    let opts = mysql_async::Opts::from_url(url).map_err(mysql_async::Error::from)?;
    let host = opts.ip_or_hostname().to_string();
    let mut builder =
        mysql_async::OptsBuilder::from_opts(opts).enable_cleartext_plugin(options.cleartext_auth).prefer_socket(false);
    if let Some(password) = password {
        builder = builder.pass(Some(password.expose_secret()));
    }
    if let Some(port) = tunnel_port {
        builder = builder.ip_or_hostname("127.0.0.1").tcp_port(port);
    } else if !options.socket.is_empty() {
        builder = builder.socket(Some(options.socket.as_str()));
    }
    // The name TLS checks: an explicit SNI, else the real host when the address dialled is a tunnel's.
    let verify_as =
        if options.sni.is_empty() { tunnel_port.map(|_| host.as_str()) } else { Some(options.sni.as_str()) };
    let ssl = tls.mysql(verify_as)?;
    match mysql_async::Conn::new(builder.clone().ssl_opts(ssl)).await {
        // Prefer: a server without TLS gets a plain connection, as libmysqlclient's PREFERRED does.
        Err(mysql_async::Error::Driver(mysql_async::DriverError::NoClientSslFlagFromServer))
            if tls.mode == TlsMode::Prefer =>
        {
            Ok(mysql_async::Conn::new(builder.ssl_opts(None)).await?)
        }
        other => Ok(other?),
    }
}

/// Opens a SQLite file. With `key`, the file is an `SQLCipher` database: the key is a passphrase, or a raw key
/// written `x'…'`.
fn open_sqlite(path: &str, key: Option<&SecretString>) -> Result<rusqlite::Connection> {
    let c = rusqlite::Connection::open(path)?;
    if let Some(key) = key.filter(|k| !k.expose_secret().is_empty()) {
        c.pragma_update(None, "key", key.expose_secret())?;
        // SQLCipher only checks the key on first read; read now so a wrong one is reported here.
        c.query_row("select count(*) from sqlite_master", [], |_| Ok(())).map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::NotADatabase => {
                Error::Invalid("wrong passphrase, or the file isn't an SQLCipher database")
            }
            e => e.into(),
        })?;
    }
    Ok(c)
}

impl Conn {
    /// Opens a connection and runs its startup SQL. `url` is a connection URL without its password, or a file
    /// path for SQLite; `password` is the login password, or the `SQLCipher` key for SQLite. With `tunnel_port`,
    /// the driver dials that port on 127.0.0.1 instead of the URL's host, but still checks TLS against it.
    pub async fn open(
        engine: Engine,
        url: &str,
        password: Option<&SecretString>,
        tls: &Tls,
        options: &Options,
        tunnel_port: Option<u16>,
    ) -> Result<Conn> {
        let open = async {
            Ok::<_, Error>(match engine {
                Engine::Postgres => Conn::Pg(open_postgres(url, password, tls, options, tunnel_port).await?),
                Engine::Mysql | Engine::Mariadb => {
                    Conn::My(open_mysql(url, password, tls, options, tunnel_port).await?)
                }
                Engine::Sqlite => Conn::Lite(open_sqlite(url, password)?),
            })
        };
        let mut conn: Conn = tokio::time::timeout(CONNECT_TIMEOUT, open)
            .await
            .map_err(|_| Error::ConnectTimeout(CONNECT_TIMEOUT.as_secs()))??;
        if !options.startup_sql.trim().is_empty() {
            conn.batch(&options.startup_sql).await.map_err(|e| Error::StartupSql(Box::new(e)))?;
        }
        Ok(conn)
    }

    /// Runs one or more statements, discarding any results.
    async fn batch(&mut self, sql: &str) -> Result<()> {
        match self {
            Conn::Pg(c) => c.batch_execute(sql).await?,
            Conn::My(c) => mysql_async::prelude::Queryable::query_drop(c, sql).await?,
            Conn::Lite(c) => c.execute_batch(sql)?,
        }
        Ok(())
    }

    pub fn stopper(&self) -> Stopper {
        match self {
            Conn::Pg(c) => Stopper::Pg(c.cancel_token()),
            Conn::My(c) => Stopper::My(c.id()),
            Conn::Lite(c) => Stopper::Lite(c.get_interrupt_handle()),
        }
    }

    /// Whether `err` came from SQLite being interrupted by its [`Stopper`].
    pub fn interrupted_by(err: &Error) -> bool {
        matches!(err, Error::Sqlite(rusqlite::Error::SqliteFailure(f, _)) if f.code == rusqlite::ErrorCode::OperationInterrupted)
    }

    /// A round trip that does nothing, to keep idle connections from being closed by the server or the network.
    pub async fn ping(&mut self) -> Result<()> {
        match self {
            Conn::Pg(c) => {
                c.simple_query("").await?;
            }
            Conn::My(c) => mysql_async::prelude::Queryable::ping(c).await?,
            Conn::Lite(_) => {}
        }
        Ok(())
    }

    /// The TLS cipher the session uses; None when it's unencrypted (or SQLite).
    pub async fn tls_cipher(&mut self) -> Result<Option<String>> {
        let cipher = match self {
            Conn::Pg(_) => self.scalar("select cipher from pg_stat_ssl where pid = pg_backend_pid()").await?,
            Conn::My(_) => {
                let r = self.query("show status like 'Ssl_cipher'").await?;
                r.rows.into_iter().next().and_then(|mut row| row.pop().flatten())
            }
            Conn::Lite(_) => None,
        };
        Ok(cipher.filter(|c| !c.is_empty()))
    }

    /// Whether `err` (from this connection) means the connection itself is gone, not just that a statement failed.
    pub fn lost_by(&self, err: &Error) -> bool {
        match (self, err) {
            (Conn::Pg(c), _) => c.is_closed(),
            (Conn::My(_), Error::MySql(mysql_async::Error::Io(_))) => true,
            (Conn::My(_), Error::MySql(mysql_async::Error::Driver(d))) => {
                matches!(d, mysql_async::DriverError::ConnectionClosed)
            }
            _ => false,
        }
    }

    /// Runs SQL typed by the user and returns the last result set as text cells (None = NULL).
    pub async fn query(&mut self, sql: &str) -> Result<QueryResult> {
        self.query_limited(sql, None).await
    }

    /// Like [`Conn::query`], but keeps at most `max_rows` rows of the result; `truncated` says more were there.
    /// Rows past the limit are read and dropped, so memory stays bounded and the connection stays in step.
    pub async fn query_limited(&mut self, sql: &str, max_rows: Option<usize>) -> Result<QueryResult> {
        let max = max_rows.unwrap_or(usize::MAX);
        let mut r = QueryResult::default();
        match self {
            // The simple-query (text) protocol renders every Postgres type as text, so no per-type decoding.
            Conn::Pg(c) => {
                use futures_util::StreamExt;
                use tokio_postgres::SimpleQueryMessage as M;
                let stream = c.simple_query_raw(sql).await?;
                futures_util::pin_mut!(stream);
                while let Some(msg) = stream.next().await {
                    match msg? {
                        // Each statement's result replaces the previous one's: the last result set is shown.
                        M::RowDescription(cols) => {
                            r.columns = cols.iter().map(|c| c.name().to_string()).collect();
                            r.rows.clear();
                            r.truncated = false;
                        }
                        M::Row(row) if r.rows.len() < max => {
                            r.rows.push((0..row.len()).map(|i| row.get(i).map(String::from)).collect());
                        }
                        M::Row(_) => r.truncated = true,
                        M::CommandComplete(n) => r.affected = n,
                        _ => {}
                    }
                }
            }
            Conn::My(c) => {
                use mysql_async::prelude::Queryable;
                let mut res = c.query_iter(sql).await?;
                r.columns = res.columns_ref().iter().map(|c| c.name_str().into_owned()).collect();
                while let Some(row) = res.next().await? {
                    if r.rows.len() < max {
                        r.rows.push(my_row(row));
                    } else {
                        r.truncated = true;
                    }
                }
                r.affected = res.affected_rows();
                res.drop_result().await?; // ponytail: only the first result set of a multi-statement query
            }
            Conn::Lite(c) => {
                // SQLite prepares one statement at a time; the editor sends scripts statement by statement.
                let mut st = c.prepare(sql)?;
                r.columns = st.column_names().into_iter().map(String::from).collect();
                if r.columns.is_empty() {
                    r.affected = st.execute([])? as u64;
                    return Ok(r);
                }
                let n = r.columns.len();
                let mut rows = st.query([])?;
                while let Some(row) = rows.next()? {
                    if r.rows.len() == max {
                        r.truncated = true; // SQLite steps lazily, so there's nothing more to read
                        break;
                    }
                    r.rows.push((0..n).map(|i| row.get_ref(i).map(lite_cell)).collect::<rusqlite::Result<Vec<_>>>()?);
                }
            }
        }
        Ok(r)
    }

    /// Every table's columns in the current database, for autocomplete: (table, column) pairs in column order.
    /// Postgres tables are `schema.table`.
    pub async fn schema(&mut self) -> Result<Vec<(String, String)>> {
        let sql = match self {
            Conn::Pg(_) => {
                "select table_schema || '.' || table_name, column_name from information_schema.columns \
                 where table_schema not in ('pg_catalog', 'information_schema') \
                 order by table_schema, table_name, ordinal_position"
            }
            Conn::My(_) => {
                "select table_name, column_name from information_schema.columns where table_schema = database() \
                 order by table_name, ordinal_position"
            }
            Conn::Lite(_) => {
                "select m.name, p.name from sqlite_master m join pragma_table_info(m.name) p \
                 where m.type in ('table', 'view') order by m.name, p.cid"
            }
        };
        let r = self.query(sql).await?;
        Ok(r.rows
            .into_iter()
            .filter_map(|mut row| {
                let column = row.pop().flatten()?;
                Some((row.pop().flatten()?, column))
            })
            .collect())
    }

    /// The placeholder for the `n`th (1-based) bound parameter.
    pub(crate) fn param(&self, n: usize) -> String {
        match self {
            Conn::My(_) => "?".into(),
            _ => format!("${n}"),
        }
    }

    /// Runs one statement with bound parameters and returns the rows it affected.
    pub(crate) async fn execute_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<u64> {
        match self {
            Conn::Pg(c) => {
                let params: Vec<PgText> = params.iter().copied().map(PgText).collect();
                let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
                    params.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
                Ok(c.execute(sql, &refs).await?)
            }
            Conn::My(c) => {
                use mysql_async::prelude::Queryable;
                c.exec_drop(sql, params.iter().copied().map(mysql_async::Value::from).collect::<Vec<_>>()).await?;
                Ok(c.affected_rows())
            }
            Conn::Lite(c) => Ok(c.execute(sql, rusqlite::params_from_iter(params))? as u64),
        }
    }

    /// Runs a catalog query with bound parameters. Every selected column must be text (cast `::text` on Postgres).
    pub(crate) async fn fetch_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Vec<Vec<Option<String>>>> {
        match self {
            Conn::Pg(c) => {
                let params: Vec<PgText> = params.iter().copied().map(PgText).collect();
                let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
                    params.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
                c.query(sql, &refs)
                    .await?
                    .iter()
                    .map(|row| (0..row.len()).map(|i| row.try_get(i)).collect::<std::result::Result<Vec<_>, _>>())
                    .collect::<std::result::Result<_, _>>()
                    .map_err(Error::from)
            }
            Conn::My(c) => {
                use mysql_async::prelude::Queryable;
                let params: Vec<_> = params.iter().copied().map(mysql_async::Value::from).collect();
                let rows: Vec<mysql_async::Row> = c.exec(sql, params).await?;
                Ok(rows.into_iter().map(my_row).collect())
            }
            Conn::Lite(c) => {
                let mut st = c.prepare(sql)?;
                let n = st.column_count();
                let rows = st
                    .query_map(rusqlite::params_from_iter(params), |r| {
                        (0..n).map(|i| r.get_ref(i).map(lite_cell)).collect::<rusqlite::Result<Vec<_>>>()
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            }
        }
    }

    /// Runs one query and hands its rows to `sink` as they arrive, so a large result never sits in memory. An
    /// error from the sink stops the query. With `params` on Postgres, every selected column must be text (as for
    /// [`Conn::fetch_with`]); without, any query works.
    pub(crate) async fn stream(&mut self, sql: &str, params: &[Option<&str>], sink: &mut dyn RowSink) -> Result<()> {
        use futures_util::StreamExt;
        match self {
            Conn::Pg(c) if params.is_empty() => {
                use tokio_postgres::SimpleQueryMessage as M;
                let stream = c.simple_query_raw(sql).await?;
                futures_util::pin_mut!(stream);
                let mut described = false;
                while let Some(msg) = stream.next().await {
                    match msg? {
                        M::RowDescription(_) if described => {
                            return Err(Error::Invalid("export one statement at a time"));
                        }
                        M::RowDescription(cols) => {
                            described = true;
                            sink.columns(cols.iter().map(|c| c.name().to_string()).collect())?;
                        }
                        M::Row(row) => sink.row((0..row.len()).map(|i| row.get(i).map(String::from)).collect())?,
                        _ => {}
                    }
                }
            }
            Conn::Pg(c) => {
                let st = c.prepare(sql).await?;
                sink.columns(st.columns().iter().map(|c| c.name().to_string()).collect())?;
                let params: Vec<PgText> = params.iter().copied().map(PgText).collect();
                let stream = c.query_raw(&st, params).await?;
                futures_util::pin_mut!(stream);
                while let Some(row) = stream.next().await {
                    let row = row?;
                    sink.row((0..row.len()).map(|i| row.try_get(i)).collect::<std::result::Result<_, _>>()?)?;
                }
            }
            Conn::My(c) if params.is_empty() => {
                // The text protocol: some statements (SHOW …) can't be prepared.
                my_stream(mysql_async::prelude::Queryable::query_iter(c, sql).await?, sink).await?;
            }
            Conn::My(c) => {
                let params: Vec<_> = params.iter().copied().map(mysql_async::Value::from).collect();
                my_stream(mysql_async::prelude::Queryable::exec_iter(c, sql, params).await?, sink).await?;
            }
            Conn::Lite(c) => {
                let mut st = c.prepare(sql)?;
                let n = st.column_count();
                sink.columns(st.column_names().into_iter().map(String::from).collect())?;
                let mut rows = st.query(rusqlite::params_from_iter(params))?;
                while let Some(row) = rows.next()? {
                    sink.row((0..n).map(|i| row.get_ref(i).map(lite_cell)).collect::<rusqlite::Result<_>>()?)?;
                }
            }
        }
        Ok(())
    }

    async fn scalar(&mut self, sql: &str) -> Result<Option<String>> {
        Ok(self.query(sql).await?.rows.into_iter().next().and_then(|mut row| row.swap_remove(0)))
    }

    /// Databases on the server; empty for SQLite (one file, one database).
    pub async fn databases(&mut self) -> Result<Vec<String>> {
        let sql = match self {
            Conn::Pg(_) => "select datname from pg_database where not datistemplate and datallowconn order by 1",
            Conn::My(_) => "select schema_name from information_schema.schemata order by 1",
            Conn::Lite(_) => return Ok(vec![]),
        };
        Ok(self.query(sql).await?.rows.into_iter().filter_map(|mut row| row.swap_remove(0)).collect())
    }

    /// The selected database; None when a MySQL connection has none yet.
    pub async fn current_database(&mut self) -> Result<Option<String>> {
        match self {
            Conn::Pg(_) => self.scalar("select current_database()").await,
            Conn::My(_) => self.scalar("select database()").await,
            Conn::Lite(_) => Ok(None),
        }
    }

    /// The server's name and version, e.g. "PostgreSQL 17.2" or "MariaDB 11.4.2".
    pub async fn server_version(&mut self) -> Result<String> {
        let (label, sql) = match self {
            Conn::Pg(_) => ("PostgreSQL", "show server_version"),
            Conn::My(_) => ("MySQL", "select version()"),
            Conn::Lite(_) => ("SQLite", "select sqlite_version()"),
        };
        let v = self.scalar(sql).await?.unwrap_or_default();
        // MariaDB reports e.g. "11.4.2-MariaDB"; name it after what the server says it is.
        Ok(if v.contains("MariaDB") {
            format!("MariaDB {}", v.split('-').next().unwrap_or(&v))
        } else {
            format!("{label} {v}")
        })
    }

    /// MySQL switches in place; Postgres cannot change database within a session, so the caller reconnects.
    pub async fn use_database(&mut self, db: &str) -> Result<()> {
        match self {
            Conn::My(_) => {
                let sql = format!("use {}", self.ident(db));
                self.query(&sql).await.map(|_| ())
            }
            _ => Err(Error::Invalid("this engine switches database by reconnecting")),
        }
    }

    pub(crate) fn dialect(&self) -> Dialect {
        match self {
            Conn::Pg(_) => Dialect::Pg,
            Conn::My(_) => Dialect::My,
            Conn::Lite(_) => Dialect::Lite,
        }
    }

    /// A quoted identifier; see [`Dialect::ident`].
    pub(crate) fn ident(&self, name: &str) -> String {
        self.dialect().ident(name)
    }

    pub(crate) fn table_ident(&self, table: &str) -> String {
        self.dialect().table_ident(table)
    }

    /// Columns of `table` (`schema.table` for Postgres) in definition order.
    pub async fn columns(&mut self, table: &str) -> Result<Vec<Column>> {
        let (sql, params) = match self {
            Conn::Pg(_) => {
                let (schema, name) = table.split_once('.').unwrap_or(("public", table));
                (
                    "select c.column_name::text, (c.udt_name || coalesce('(' || c.character_maximum_length || ')', ''))::text, \
                     c.is_nullable::text, c.column_default::text, case when exists (select 1 from information_schema.table_constraints t \
                     join information_schema.key_column_usage k on k.constraint_name = t.constraint_name \
                     and k.table_schema = t.table_schema and k.table_name = t.table_name \
                     where t.constraint_type = 'PRIMARY KEY' and t.table_schema = c.table_schema \
                     and t.table_name = c.table_name and k.column_name = c.column_name) then 'PRI' else '' end \
                     from information_schema.columns c where c.table_schema = $1 and c.table_name = $2 order by c.ordinal_position",
                    vec![Some(schema), Some(name)],
                )
            }
            Conn::My(_) => (
                "select column_name, column_type, is_nullable, column_default, column_key from information_schema.columns \
                 where table_schema = database() and table_name = ? order by ordinal_position",
                vec![Some(table)],
            ),
            Conn::Lite(_) => (
                "select name, type, case when \"notnull\" then 'NO' else 'YES' end, dflt_value, \
                 case when pk > 0 then 'PRI' else '' end from pragma_table_info($1)",
                vec![Some(table)],
            ),
        };
        Ok(self
            .fetch_with(sql, &params)
            .await?
            .into_iter()
            .map(|mut r| {
                r.resize(5, None);
                let mut cell = |i: usize| r[i].take();
                Column {
                    name: cell(0).unwrap_or_default(),
                    data_type: cell(1).unwrap_or_default(),
                    nullable: cell(2).as_deref() == Some("YES"),
                    default: cell(3),
                    primary: cell(4).as_deref() == Some("PRI"),
                }
            })
            .collect())
    }

    /// Tables and views in the current database (`schema.table` for Postgres).
    /// `system` adds Postgres's `pg_catalog` and `information_schema` tables.
    pub async fn tables(&mut self, system: bool) -> Result<Vec<String>> {
        let sql = match self {
            Conn::Pg(_) if system => {
                "select table_schema || '.' || table_name from information_schema.tables order by 1"
            }
            Conn::Pg(_) => {
                "select table_schema || '.' || table_name from information_schema.tables \
                 where table_schema not in ('pg_catalog', 'information_schema') order by 1"
            }
            Conn::My(_) => {
                "select table_name from information_schema.tables where table_schema = database() order by 1"
            }
            Conn::Lite(_) => "select name from sqlite_master where type in ('table', 'view') order by 1",
        };
        let r = self.query(sql).await?;
        Ok(r.rows.into_iter().filter_map(|mut row| row.swap_remove(0)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{Conn, Engine};
    use crate::tls::Tls;

    async fn open(engine: Engine, url: &str) -> Conn {
        Conn::open(engine, url, None, &Tls::default(), &super::Options::default(), None).await.unwrap()
    }

    #[test]
    fn sqlite_round_trip() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            assert_eq!(c.query("create table t(id integer, name text)").await.unwrap().affected, 0);
            assert_eq!(c.query("insert into t values (1, 'a'), (2, null)").await.unwrap().affected, 2);
            let r = c.query("select id, name from t order by id").await.unwrap();
            assert_eq!(r.columns, ["id", "name"]);
            assert_eq!(r.rows, [vec![Some("1".into()), Some("a".into())], vec![Some("2".into()), None]]);
            assert_eq!(c.tables(false).await.unwrap(), ["t"]);
            assert_eq!(c.databases().await.unwrap(), Vec::<String>::new());
            assert!(c.server_version().await.unwrap().starts_with("SQLite 3."));
        });
    }

    #[test]
    fn columns_handles_quotes_in_table_names() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            c.query("create table \"it's\"(id integer primary key, v text not null default 'x')").await.unwrap();
            let cols = c.columns("it's").await.unwrap();
            assert_eq!((cols[0].name.as_str(), cols[0].primary, cols[1].nullable), ("id", true, false));
            assert_eq!(cols[1].default.as_deref(), Some("'x'"));
        });
    }

    #[test]
    fn query_limited_should_cap_rows_and_say_so() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            let sql = "with recursive n(x) as (select 1 union all select x + 1 from n where x < 50) select x from n";
            let r = c.query_limited(sql, Some(10)).await.unwrap();
            assert_eq!((r.rows.len(), r.truncated), (10, true));
            let r = c.query_limited(sql, Some(50)).await.unwrap();
            assert_eq!((r.rows.len(), r.truncated), (50, false));
        });
    }

    #[test]
    fn schema_should_list_columns_per_table_in_order() {
        tauri::async_runtime::block_on(async {
            let mut c = open(Engine::Sqlite, ":memory:").await;
            c.query("create table b(y text, x text)").await.unwrap();
            c.query("create table a(id integer)").await.unwrap();
            let pairs = c.schema().await.unwrap();
            let pairs: Vec<(&str, &str)> = pairs.iter().map(|(t, col)| (t.as_str(), col.as_str())).collect();
            assert_eq!(pairs, [("a", "id"), ("b", "y"), ("b", "x")]);
        });
    }

    #[test]
    fn open_runs_the_startup_sql() {
        tauri::async_runtime::block_on(async {
            let options = super::Options {
                startup_sql: "create table t(x); insert into t values (7);".into(),
                ..Default::default()
            };
            let mut c = Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &options, None).await.unwrap();
            assert_eq!(c.query("select x from t").await.unwrap().rows[0][0].as_deref(), Some("7"));
        });
    }

    #[test]
    fn open_reports_failing_startup_sql() {
        tauri::async_runtime::block_on(async {
            let options = super::Options { startup_sql: "not sql".into(), ..Default::default() };
            let r = Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &options, None).await;
            let err = r.err().unwrap().to_string();
            assert!(err.starts_with("startup SQL: "), "{err}");
        });
    }

    #[test]
    fn startup_script_should_pass_when_it_exits_zero() {
        tauri::async_runtime::block_on(async { super::run_startup_script("true").await.unwrap() });
    }

    #[test]
    fn startup_script_should_report_its_status_and_last_error_line() {
        tauri::async_runtime::block_on(async {
            let err = super::run_startup_script("echo first >&2; echo vpn down >&2; exit 3").await.unwrap_err();
            assert_eq!(err.to_string(), "startup script exited with status 3: vpn down");
        });
    }

    #[test]
    fn sqlcipher_key_should_unlock_only_with_the_right_passphrase() {
        tauri::async_runtime::block_on(async {
            let path = std::env::temp_dir().join(format!("tableminus-{}-cipher.db", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let path = path.to_str().unwrap();
            let opts = super::Options::default();
            let key = |k: &str| secrecy::SecretString::from(k);

            let mut c =
                Conn::open(Engine::Sqlite, path, Some(&key("hunter2")), &Tls::default(), &opts, None).await.unwrap();
            c.query("create table secret(v text)").await.unwrap();
            c.query("insert into secret values ('kept')").await.unwrap();
            drop(c);
            assert!(!std::fs::read(path).unwrap().starts_with(b"SQLite format 3"), "the file is encrypted on disk");

            let err = Conn::open(Engine::Sqlite, path, Some(&key("wrong")), &Tls::default(), &opts, None)
                .await
                .err()
                .unwrap();
            assert_eq!(err.to_string(), "wrong passphrase, or the file isn't an SQLCipher database");

            let mut c =
                Conn::open(Engine::Sqlite, path, Some(&key("hunter2")), &Tls::default(), &opts, None).await.unwrap();
            assert_eq!(c.query("select v from secret").await.unwrap().rows[0][0].as_deref(), Some("kept"));
            let _ = std::fs::remove_file(path);
        });
    }

    #[test]
    fn open_reports_bad_sqlite_path() {
        tauri::async_runtime::block_on(async {
            let r = Conn::open(
                Engine::Sqlite,
                "/no/such/dir/x.db",
                None,
                &Tls::default(),
                &super::Options::default(),
                None,
            )
            .await;
            assert!(r.is_err());
        });
    }

    /// The session's TLS cipher on the local MySQL in `mode`; empty when the session is plain.
    async fn local_mysql_cipher(mode: crate::tls::TlsMode) -> crate::error::Result<String> {
        let tls = Tls { mode, ..Tls::default() };
        let options = super::Options::default();
        let mut c = Conn::open(Engine::Mysql, "mysql://root@127.0.0.1:3306", None, &tls, &options, None).await?;
        let r = c.query("show status like 'Ssl_cipher'").await?;
        Ok(r.rows[0][1].clone().unwrap_or_default())
    }

    /// TLS modes against a local MySQL with its auto-generated (self-signed) certificate.
    #[test]
    #[ignore = "needs a local MySQL with TLS on"]
    fn local_mysql_tls_modes() {
        use crate::tls::TlsMode;
        tauri::async_runtime::block_on(async {
            assert_eq!(local_mysql_cipher(TlsMode::Disable).await.unwrap(), "", "Disable: plain");
            assert_ne!(local_mysql_cipher(TlsMode::Prefer).await.unwrap(), "", "Prefer: encrypted");
            assert_ne!(local_mysql_cipher(TlsMode::Require).await.unwrap(), "", "Require: no certificate check");
            let err = local_mysql_cipher(TlsMode::VerifyCa).await.unwrap_err();
            println!("VerifyCa refused as expected: {err}");
        });
    }

    /// A MySQL session killed from another connection reads as lost, not as a failed statement.
    #[test]
    #[ignore = "needs a local MySQL"]
    fn local_mysql_killed_session_is_lost() {
        tauri::async_runtime::block_on(async {
            let mut victim = open(Engine::Mysql, "mysql://root@127.0.0.1:3306").await;
            let id = victim.scalar("select connection_id()").await.unwrap().unwrap();
            let mut killer = open(Engine::Mysql, "mysql://root@127.0.0.1:3306").await;
            killer.query(&format!("kill {id}")).await.unwrap();
            let err = victim.query("select 1").await.unwrap_err();
            assert!(victim.lost_by(&err), "{err:?}");
            let syntax = killer.query("selec 1").await.unwrap_err();
            assert!(!killer.lost_by(&syntax), "a bad statement keeps the connection");
        });
    }

    /// Checks against local servers with default credentials: `cargo test -- --ignored`.
    #[test]
    #[ignore = "needs local MySQL and Postgres servers"]
    fn local_servers_without_database() {
        tauri::async_runtime::block_on(async {
            let mut my = open(Engine::Mysql, "mysql://root@127.0.0.1:3306").await;
            assert_eq!(my.current_database().await.unwrap(), None);
            let r = my.query_limited("select * from information_schema.columns", Some(5)).await.unwrap();
            assert_eq!((r.rows.len(), r.truncated), (5, true));
            assert_eq!(my.query("select 1").await.unwrap().rows[0][0].as_deref(), Some("1"), "still in step");
            assert!(my.databases().await.unwrap().contains(&"mysql".to_string()));
            my.use_database("mysql").await.unwrap();
            assert_eq!(my.current_database().await.unwrap().as_deref(), Some("mysql"));
            assert!(my.tables(false).await.unwrap().contains(&"user".to_string()));
            assert!(my.columns("user").await.unwrap().iter().any(|c| c.name == "Host" && c.primary));
            my.query("create temporary table dbm_edit(id int primary key, note text)").await.unwrap();
            my.query("insert into dbm_edit values (1, 'x')").await.unwrap();
            let edit = crate::rows::Update {
                key: vec![("id".into(), "1".into())],
                id: None,
                set: vec![("note".into(), crate::rows::Value::Text(Some("a\\b'c".into())))],
            };
            let changes = crate::rows::Changes { updates: vec![edit], ..Default::default() };
            assert_eq!(my.save_rows("dbm_edit", &changes).await.unwrap(), 1);
            let r = my.query("select note from dbm_edit").await.unwrap();
            assert_eq!(r.rows[0][0].as_deref(), Some("a\\b'c"), "backslashes and quotes stored as typed");

            let mut pg = open(Engine::Postgres, "postgres://postgres@127.0.0.1:5432?sslmode=prefer").await;
            assert!(pg.databases().await.unwrap().contains(&"postgres".to_string()));
            assert!(pg.columns("information_schema.tables").await.unwrap().iter().any(|c| c.name == "table_name"));

            // Text parameters into typed columns, on a session-only temp table (nothing persists).
            pg.query(
                "create temporary table dbm_edit(id int primary key, ok bool, at timestamp, n numeric(6,2), note text)",
            )
            .await
            .unwrap();
            pg.query("insert into dbm_edit values (1, false, '2026-01-01', 1.5, 'x')").await.unwrap();
            let set = |c: &str, v: Option<&str>| (c.to_string(), crate::rows::Value::Text(v.map(String::from)));
            let edit = crate::rows::Update {
                key: vec![("id".into(), "1".into())],
                id: None,
                set: vec![
                    set("ok", Some("t")),
                    set("at", Some("2026-10-13 09:30:00")),
                    set("n", Some("42.25")),
                    set("note", None),
                ],
            };
            let changes = crate::rows::Changes { updates: vec![edit], ..Default::default() };
            assert_eq!(pg.save_rows("pg_temp.dbm_edit", &changes).await.unwrap(), 1);
            let r = pg.query("select ok, at, n, note from dbm_edit").await.unwrap();
            assert_eq!(
                r.rows[0],
                vec![Some("t".into()), Some("2026-10-13 09:30:00".into()), Some("42.25".into()), None]
            );
        });
    }
}

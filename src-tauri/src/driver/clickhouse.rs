//! `ClickHouse` over its HTTP interface. Each statement is one request, answered in `TabSeparatedWithNamesAndTypes`:
//! a line of names, a line of types, then a line per row with every value as text (`\N` for NULL). A request is a
//! connection of its own, tied to the others by an HTTP session (`session_id`), so `SET` and temporary tables last
//! from one statement to the next. The database goes with each request, and cancelling a query is closing its
//! connection (`cancel_http_readonly_queries_on_client_close`).

use super::{Driver, Results, RowSink, Stopper, only_comments};
use crate::db::{Dialect, Options};
use crate::error::{Error, Result};
use crate::tls::{Tls, TlsMode};
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, header};
use hyper_util::rt::TokioIo;
use secrecy::{ExposeSecret, SecretString};
use std::any::Any;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::net::TcpStream;
use tokio::sync::Notify;

/// The most an error answer is read for.
const MAX_ERROR: usize = 64 * 1024;
/// The widest line (one row) a result may have; a row wider than this is refused rather than held.
const MAX_LINE: usize = 256 * 1024 * 1024;
/// The most a bound value may hold to go as a form field: the server refuses a field over 128 KiB.
const FIELD_LIMIT: usize = 100_000;
/// The statements that turn server-enforced read-only on and off. They aren't sent: `SET readonly = 1` would stay in
/// the session and refuse the `SET` that ends it, so the driver sends `readonly=1` with every request while it is on.
const READONLY_ON: &str = "set readonly = 1";
const READONLY_OFF: &str = "set readonly = 0";

pub struct ClickHouse {
    /// The host name TLS is checked against and the `Host` header names.
    host: String,
    port: u16,
    /// Where the socket goes: the host, or a local tunnel port.
    dial: (String, u16),
    tls: Option<tokio_native_tls::TlsConnector>,
    /// The `Authorization` header value.
    auth: header::HeaderValue,
    database: String,
    /// The HTTP session every statement runs in.
    session: String,
    readonly: bool,
    cancel: Arc<Notify>,
    /// The id of the request in flight, so a cancel can ask the server to stop it as well as hang up.
    running: Mutex<Option<String>>,
}

/// A poisoned lock only means a callback panicked; what it guards is still whole.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("v={}", s.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// A value written the way a TSV field is read: `\N` for NULL, and backslash, tab and line breaks escaped.
fn tsv_escape(value: Option<&str>) -> String {
    let Some(value) = value else { return "\\N".into() };
    value.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r")
}

/// A TSV field as text: `None` for `\N`, else the value with its escapes undone.
fn tsv_field(field: &[u8]) -> Option<String> {
    if field == b"\\N" {
        return None;
    }
    let mut out = Vec::with_capacity(field.len());
    let mut bytes = field.iter().copied();
    while let Some(b) = bytes.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match bytes.next() {
            Some(b'n') => out.push(b'\n'),
            Some(b't') => out.push(b'\t'),
            Some(b'r') => out.push(b'\r'),
            Some(b'0') => out.push(0),
            Some(b'b') => out.push(8),
            Some(b'f') => out.push(12),
            Some(b'a') => out.push(7),
            Some(b'v') => out.push(11),
            Some(other) => out.push(other), // \\ \' \" \` and any other escaped byte stand for themselves
            None => out.push(b'\\'),
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

fn tsv_row(line: &[u8]) -> Vec<Option<String>> {
    line.split(|b| *b == b'\t').map(tsv_field).collect()
}

/// Splits `sql` at the semicolons outside quotes, quoted names and comments; `ClickHouse` takes one statement per
/// request.
pub fn split_statements(sql: &str) -> Vec<&str> {
    let b = sql.as_bytes();
    let (mut out, mut start, mut i) = (vec![], 0, 0);
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += if b[i] == b'\\' { 2 } else { 1 }; // a backslash escapes the next byte; a doubled quote is two strings in a row
                }
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b';' => {
                out.push(&sql[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&sql[start.min(sql.len())..]);
    out.into_iter().filter(|s| !only_comments(s)).collect()
}

/// Whether a line is the server's error text, as it ends a stream that failed partway.
fn is_exception(line: &[u8]) -> bool {
    line.windows(b"DB::Exception".len()).any(|w| w == b"DB::Exception")
}

/// The answer to one request, read line by line.
struct Lines {
    body: Incoming,
    buf: BytesMut,
    done: bool,
}

impl Lines {
    async fn next(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            if let Some(at) = self.buf.iter().position(|b| *b == b'\n') {
                let mut line = self.buf.split_to(at + 1);
                line.truncate(at);
                return Ok(Some(line.to_vec()));
            }
            if self.done {
                return Ok((!self.buf.is_empty()).then(|| self.buf.split().to_vec()));
            }
            match self.body.frame().await {
                Some(frame) => {
                    if let Ok(data) = frame.map_err(|e| Error::ClickHouse(e.to_string()))?.into_data() {
                        self.buf.extend_from_slice(&data);
                        if self.buf.len() > MAX_LINE {
                            return Err(Error::ClickHouse("a row is too wide to read".into()));
                        }
                    }
                }
                None => self.done = true,
            }
        }
    }
}

/// An answer: its rows to read, and the row count `ClickHouse` reports for what the statement wrote.
struct Answer {
    lines: Lines,
    written: u64,
}

impl ClickHouse {
    pub async fn open(
        url: &str,
        password: Option<&SecretString>,
        tls: &Tls,
        _options: &Options,
        tunnel_port: Option<u16>,
    ) -> Result<ClickHouse> {
        let u = url::Url::parse(url)?;
        let host = u.host_str().unwrap_or("localhost").trim_matches(['[', ']']).to_string();
        let port = u.port().unwrap_or(8123);
        // Plain HTTP unless TLS is asked for; Prefer means TLS on the HTTPS ports.
        let secure = match tls.mode {
            TlsMode::Disable => false,
            TlsMode::Prefer => matches!(port, 8443 | 443),
            TlsMode::Require | TlsMode::VerifyCa | TlsMode::VerifyFull => true,
        };
        let connector = secure.then(|| tls.connector()).transpose()?.map(tokio_native_tls::TlsConnector::from);
        let user = decode(u.username());
        let user = if user.is_empty() { "default".to_string() } else { user };
        let credentials = format!("{user}:{}", password.map(ExposeSecret::expose_secret).unwrap_or_default());
        let mut auth = header::HeaderValue::from_str(&format!("Basic {}", base64(credentials.as_bytes())))
            .map_err(|_| Error::Invalid("the user name or password has characters a header can't hold"))?;
        auth.set_sensitive(true);
        let dial = match tunnel_port {
            Some(p) => ("127.0.0.1".to_string(), p),
            None => (host.clone(), port),
        };
        let c = ClickHouse {
            host,
            port,
            dial,
            tls: connector,
            auth,
            database: decode(u.path().trim_start_matches('/')),
            session: random_hex()?,
            readonly: false,
            cancel: Arc::new(Notify::new()),
            running: Mutex::new(None),
        };
        // Nothing connects until a request: say now whether the server, the login and the database are right.
        c.request("SELECT 1", &[]).await?;
        Ok(c)
    }

    /// The URL's query: the answer's format, the session, the database, read-only when it's on, and the bound
    /// parameters.
    fn query_string(
        &self,
        query_id: &str,
        in_session: bool,
        query_size: Option<usize>,
        in_url: &[(usize, Option<&str>)],
    ) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("default_format", "TabSeparatedWithNamesAndTypes");
        query.append_pair("query_id", query_id);
        if in_session {
            query.append_pair("session_id", &self.session);
            // The longest the server allows by default; keep-alive pings keep it from timing out sooner.
            query.append_pair("session_timeout", "3600");
        }
        query.append_pair("cancel_http_readonly_queries_on_client_close", "1");
        if !self.database.is_empty() {
            query.append_pair("database", &self.database);
        }
        if self.readonly {
            query.append_pair("readonly", "1");
        }
        for (n, p) in in_url {
            query.append_pair(&format!("param_p{n}"), &tsv_escape(*p));
        }
        if let Some(size) = query_size {
            // The statement holds a big value, and the parser's default cap on a statement is 256 KiB.
            query.append_pair("max_query_size", &size.to_string());
        }
        query.finish()
    }

    /// Sends one statement in the session and returns the start of its answer. A query the driver hung up on (a
    /// row limit, a cancel) holds the session until the server notices, so a locked session is tried again for a
    /// few seconds.
    async fn request(&self, sql: &str, params: &[Option<&str>]) -> Result<Answer> {
        let mut tries = 0;
        loop {
            match self.send(sql, params, true).await {
                Err(Error::ClickHouse(m)) if m.contains("SESSION_IS_LOCKED") && tries < 50 => {
                    tries += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                r => return r,
            }
        }
    }

    /// Sends one statement and returns the start of its answer. An error status is read in full and returned.
    async fn send(&self, sql: &str, params: &[Option<&str>], in_session: bool) -> Result<Answer> {
        let query_id = random_hex()?;
        *lock(&self.running) = Some(query_id.clone());
        // Bound values go in the body, as form fields next to the statement, so a value as big as a cell can be sent;
        // in the URL they'd be held to its length limit.
        // A value too big for a field is written into the statement as an escaped literal instead.
        let mut sql = sql.to_string();
        let mut sent: Vec<(usize, Option<&str>)> = Vec::new();
        for (i, p) in params.iter().enumerate() {
            match p {
                Some(v) if v.len() > FIELD_LIMIT => {
                    sql = sql.replace(&format!("{{p{}:String}}", i + 1), &Dialect::Click.literal(v));
                }
                _ => sent.push((i + 1, *p)),
            }
        }
        let query_size = (sql.len() > 200_000).then(|| sql.len() + 1024);
        // A statement over the field limit can't go as a form field either: it is the body, and the values go in the URL.
        let in_url: &[(usize, Option<&str>)] = if sql.len() > FIELD_LIMIT { &sent } else { &[] };
        let (content_type, body) = if sent.is_empty() || !in_url.is_empty() {
            ("text/plain; charset=utf-8".to_string(), Bytes::copy_from_slice(sql.as_bytes()))
        } else {
            let mut boundary = [0u8; 12];
            getrandom::fill(&mut boundary).map_err(|e| Error::ClickHouse(e.to_string()))?;
            let boundary = boundary.iter().fold(String::from("tableminus"), |s, b| s + &format!("{b:02x}"));
            let mut body = Vec::with_capacity(sql.len() + params.len() * 64);
            let mut field = |name: &str, value: &str| {
                body.extend_from_slice(
                    format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
                );
                body.extend_from_slice(value.as_bytes());
                body.extend_from_slice(b"\r\n");
            };
            field("query", &sql);
            for (n, p) in &sent {
                field(&format!("param_p{n}"), &tsv_escape(*p));
            }
            body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            (format!("multipart/form-data; boundary={boundary}"), Bytes::from(body))
        };
        let request = Request::post(format!("/?{}", self.query_string(&query_id, in_session, query_size, in_url)))
            .header(header::HOST, format!("{}:{}", self.host, self.port))
            .header(header::AUTHORIZATION, self.auth.clone())
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONNECTION, "close")
            .body(Full::new(body))
            .map_err(|e| Error::ClickHouse(e.to_string()))?;
        let (host, port) = &self.dial;
        let tcp = tokio::time::timeout(crate::db::CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), *port)))
            .await
            .map_err(|_| Error::ConnectTimeout(crate::db::CONNECT_TIMEOUT.as_secs()))??;
        tcp.set_nodelay(true)?;
        let response = match &self.tls {
            Some(tls) => {
                let stream = tokio::time::timeout(crate::db::CONNECT_TIMEOUT, tls.connect(&self.host, tcp))
                    .await
                    .map_err(|_| Error::ConnectTimeout(crate::db::CONNECT_TIMEOUT.as_secs()))??;
                send(stream, request).await?
            }
            None => send(tcp, request).await?,
        };
        let status = response.status();
        let written = response
            .headers()
            .get("x-clickhouse-summary")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok())
            .and_then(|v| v.get("written_rows")?.as_str()?.parse().ok())
            .unwrap_or(0);
        let mut lines = Lines { body: response.into_body(), buf: BytesMut::new(), done: false };
        if !status.is_success() {
            let mut text = Vec::new();
            while let Some(line) = lines.next().await.ok().flatten() {
                text.extend(line);
                text.push(b'\n');
                if text.len() > MAX_ERROR {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&text);
            return Err(Error::ClickHouse(text.trim().chars().take(1000).collect()));
        }
        Ok(Answer { lines, written })
    }

    /// Runs one statement and hands its column names, then each row, to the callbacks; `on_row` says whether to go
    /// on. Returns whether there was a result set (an empty body is a statement that returned none).
    async fn read(
        &self,
        sql: &str,
        params: &[Option<&str>],
        mut on_columns: impl FnMut(Vec<String>) -> Result<()>,
        mut on_row: impl FnMut(Vec<Option<String>>) -> Result<bool>,
    ) -> Result<(bool, u64)> {
        let mut answer = self.request(sql, params).await?;
        let Some(names) = answer.lines.next().await? else { return Ok((false, answer.written)) };
        answer.lines.next().await?; // the types
        on_columns(tsv_row(&names).into_iter().map(Option::unwrap_or_default).collect())?;
        while let Some(line) = answer.lines.next().await? {
            // A query that fails after the first rows are out has no status to say so: the server ends the stream
            // with its error as text.
            if line.starts_with(b"Code: ") && !line.contains(&b'\t') && is_exception(&line) {
                return Err(Error::ClickHouse(String::from_utf8_lossy(&line).chars().take(1000).collect()));
            }
            if !on_row(tsv_row(&line))? {
                break; // dropping the answer closes the connection, which stops a read-only query
            }
        }
        Ok((true, answer.written))
    }
}

async fn send<I>(io: I, request: Request<Full<Bytes>>) -> Result<Response<Incoming>>
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let failed = |e: hyper::Error| Error::ClickHouse(e.to_string());
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await.map_err(failed)?;
    tauri::async_runtime::spawn(conn);
    sender.send_request(request).await.map_err(failed)
}

/// 16 random bytes in hex: a query or session id.
fn random_hex() -> Result<String> {
    let mut id = [0u8; 16];
    getrandom::fill(&mut id).map_err(|e| Error::ClickHouse(e.to_string()))?;
    Ok(id.iter().fold(String::new(), |s, b| s + &format!("{b:02x}")))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Runs `$work`; when the stopper fires first, drops it (which closes its connection, and with it a read-only query)
/// and reports the call cancelled. Returns from the calling function either way.
macro_rules! guarded {
    ($self:ident, $work:expr) => {{
        let cancel = $self.cancel.clone();
        let outcome = {
            let work = $work;
            tokio::pin!(work);
            tokio::select! {
                r = &mut work => Some(r),
                () = cancel.notified() => None,
            }
        };
        match outcome {
            Some(r) => r,
            None => {
                $self.kill_running().await;
                Err(Error::Cancelled)
            }
        }
    }};
}

impl ClickHouse {
    /// Asks the server to stop the request that was cancelled: hanging up stops a read-only query when the server
    /// next looks at the socket, and this doesn't wait for that. Best effort; the request is already dropped.
    async fn kill_running(&self) {
        let Some(id) = lock(&self.running).take() else { return };
        // Outside the session, which the query being killed holds.
        let _ = self.send("KILL QUERY WHERE query_id = {p1:String} ASYNC", &[Some(&id)], false).await;
    }
}

#[async_trait]
impl Driver for ClickHouse {
    fn dialect(&self) -> Dialect {
        Dialect::Click
    }

    async fn batch(&mut self, sql: &str) -> Result<()> {
        self.run(sql, &mut Results::new(Some(0))).await
    }

    async fn run(&mut self, sql: &str, out: &mut Results) -> Result<()> {
        let statements = split_statements(sql);
        let this = &*self;
        let mut readonly = self.readonly;
        let result = guarded!(self, async {
            for statement in statements {
                match statement.trim().to_ascii_lowercase().as_str() {
                    READONLY_ON => readonly = true,
                    READONLY_OFF => readonly = false,
                    _ => {
                        let out = Mutex::new(&mut *out);
                        let (began, written) = this
                            .read(
                                statement,
                                &[],
                                |names| {
                                    lock(&out).begin(names);
                                    Ok(())
                                },
                                |row| {
                                    let mut out = lock(&out);
                                    if out.keep() {
                                        out.push(row);
                                        Ok(true)
                                    } else {
                                        Ok(false)
                                    }
                                },
                            )
                            .await?;
                        lock(&out).end(if began { 0 } else { written });
                        continue;
                    }
                }
                out.begin(vec![]);
                out.end(0);
            }
            Ok(())
        });
        self.readonly = readonly;
        result
    }

    async fn execute_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<u64> {
        let this = &*self;
        guarded!(self, async { Ok(this.read(sql, params, |_| Ok(()), |_| Ok(true)).await?.1) })
    }

    async fn fetch_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Vec<Vec<Option<String>>>> {
        let this = &*self;
        guarded!(self, async {
            let rows = Mutex::new(vec![]);
            this.read(
                sql,
                params,
                |_| Ok(()),
                |row| {
                    lock(&rows).push(row);
                    Ok(true)
                },
            )
            .await?;
            Ok(rows.into_inner().unwrap_or_else(PoisonError::into_inner))
        })
    }

    async fn stream(&mut self, sql: &str, params: &[Option<&str>], sink: &mut dyn RowSink) -> Result<()> {
        let this = &*self;
        guarded!(self, async {
            let sink = Mutex::new(sink);
            this.read(sql, params, |names| lock(&sink).columns(names), |row| lock(&sink).row(row).map(|()| true))
                .await
                .map(|_| ())
        })
    }

    async fn ping(&mut self) -> Result<()> {
        self.request("SELECT 1", &[]).await.map(|_| ())
    }

    async fn use_database(&mut self, db: &str) -> Result<()> {
        let was = std::mem::replace(&mut self.database, db.to_string());
        // The database goes with each request, so asking something proves it exists.
        if let Err(e) = self.request("SELECT 1", &[]).await {
            self.database = was;
            return Err(e);
        }
        Ok(())
    }

    fn stopper(&self) -> Stopper {
        Stopper::Notify(self.cancel.clone())
    }

    // These aren't sent (see READONLY_ON): the driver sends `readonly=1` with each request while on, and the
    // server then refuses every write.
    fn read_only_bounds(&self) -> (&'static str, &'static str) {
        (READONLY_ON, READONLY_OFF)
    }

    fn param(&self, n: usize) -> String {
        format!("{{p{n}:String}}")
    }

    // Small enough that a statement with its placeholders stays well under the form field limit.
    fn max_params(&self) -> usize {
        4_000
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{base64, is_exception, split_statements, tsv_escape, tsv_field, tsv_row};

    #[test]
    fn tsv_fields_should_unescape_and_tell_null_from_text() {
        assert_eq!(tsv_field(b"\\N"), None);
        assert_eq!(tsv_field(b"a\\tb\\\\c\\n\\'x"), Some("a\tb\\c\n'x".into()));
        assert_eq!(tsv_field(b""), Some(String::new()));
        assert_eq!(tsv_row(b"1\t\\N\t\tx\\ty"), [Some("1".into()), None, Some(String::new()), Some("x\ty".into())]);
        assert_eq!(tsv_field("héllo".as_bytes()), Some("héllo".into()));
    }

    #[test]
    fn tsv_escape_should_round_trip_through_tsv_field() {
        for value in ["plain", "a\\%b", "tab\there", "line\nbreak", "\\N", ""] {
            assert_eq!(tsv_field(tsv_escape(Some(value)).as_bytes()).as_deref(), Some(value));
        }
        assert_eq!(tsv_escape(None), "\\N");
    }

    #[test]
    fn split_statements_should_split_at_top_level_semicolons_only() {
        assert_eq!(
            split_statements("select 1; select ';\\';' as a, `b;` ; /* x; */ select 'it''s;'; -- tail;"),
            ["select 1", " select ';\\';' as a, `b;` ", " /* x; */ select 'it''s;'"]
        );
        assert_eq!(split_statements("-- only a comment\n;\n"), Vec::<&str>::new());
    }

    #[test]
    fn is_exception_should_recognise_the_servers_error_line_and_nothing_else() {
        assert!(is_exception(
            b"Code: 394. DB::Exception: Query was cancelled. (QUERY_WAS_CANCELLED) (version 24.8.14.39)"
        ));
        assert!(!is_exception(b"Code: 5, a plain value"));
        assert!(!is_exception(b"DB::Exce"));
    }

    #[test]
    fn base64_should_encode_basic_auth_credentials() {
        assert_eq!(base64(b"tm:testpass"), "dG06dGVzdHBhc3M=");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b""), "");
    }

    use crate::db::{Conn, Engine, Options};
    use crate::driver::Stopper;
    use crate::error::Error;
    use crate::tls::Tls;
    use secrecy::SecretString;

    /// A connection to a local `ClickHouse` (`scripts/test-env.sh up`) in a scratch database that this makes.
    async fn local(database: &str) -> Conn {
        let (tls, options) = (Tls::default(), Options::default());
        let password = SecretString::from("testpass");
        let url = std::env::var("DBM_CLICKHOUSE_URL").unwrap_or_else(|_| "clickhouse://tm@127.0.0.1:8123".into());
        let mut c = Conn::open(Engine::Clickhouse, &url, Some(&password), &tls, &options, None).await.unwrap();
        c.query(&format!("drop database if exists {database} sync; create database {database}")).await.unwrap();
        c.use_database(database).await.unwrap();
        c
    }

    async fn drop_database(mut c: Conn, database: &str) {
        c.use_database("default").await.unwrap();
        c.query(&format!("drop database {database} sync")).await.unwrap();
    }

    #[test]
    #[ignore = "needs a local ClickHouse (scripts/test-env.sh up); creates and drops a scratch database"]
    fn local_clickhouse_should_run_statements_count_rows_and_write_values_as_text() {
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_values").await;
            let script = "create table t(id UInt64, n Nullable(Int64), s String, a Array(UInt8), d Date, dt DateTime64(3), \
                 big UInt128, tup Tuple(UInt8, String), m Map(String, UInt8), u UUID, dec Decimal(10,2)) \
                 engine = MergeTree order by id; \
                 insert into t values (1, 5, 'tab\\there', [1,2], '2024-01-02', '2024-01-02 03:04:05.123', \
                   12345678901234567890123, (1,'a'), {'k':1}, '6f9619ff-8b86-d011-b42d-00c04fc964ff', 12.5), \
                   (2, null, 'x', [], '2024-01-03', '2024-01-03 00:00:00.000', 0, (0,''), {}, \
                   '00000000-0000-0000-0000-000000000000', 0); \
                 select * from t order by id; \
                 select 1 as one; select 'two' as two";
            let r = c.query_results(script, None).await.unwrap();
            assert_eq!(r.len(), 5, "create, insert, select, two selects: {r:?}");
            assert_eq!((r[0].columns.len(), r[0].affected), (0, 0));
            assert_eq!((r[1].columns.len(), r[1].affected), (0, 2), "the insert's count");
            assert_eq!(
                r[2].rows[0],
                [
                    "1",
                    "5",
                    "tab\there",
                    "[1,2]",
                    "2024-01-02",
                    "2024-01-02 03:04:05.123",
                    "12345678901234567890123",
                    "(1,'a')",
                    "{'k':1}",
                    "6f9619ff-8b86-d011-b42d-00c04fc964ff",
                    "12.5"
                ]
                .map(|v| Some(v.to_string()))
            );
            assert_eq!(r[2].rows[1][1], None, "NULL stays NULL");
            assert_eq!(r[3].rows, [[Some("1".into())]]);
            assert!(c.query("select nope").await.is_err());
            assert_eq!(c.query("select 7 as seven").await.unwrap().rows, [[Some("7".into())]], "usable after an error");
            let err = c.query("select * from missing").await.unwrap_err().to_string();
            assert!(err.contains("UNKNOWN_TABLE") || err.contains("doesn't exist"), "{err}");
            let limited = c.query_limited("select number from numbers(1000)", Some(10)).await.unwrap();
            assert_eq!((limited.rows.len(), limited.truncated), (10, true));
            drop_database(c, "tm_values").await;
        });
    }

    const SCRIPT: &str = "create table owners(id UInt32, name String) engine = MergeTree order by id; \
        create table pets(id UInt64, owner UInt32, age Nullable(Int32) comment 'years', tags Array(String), \
          note String default 'x', total UInt64 materialized id * 2) \
          engine = MergeTree partition by intDiv(id, 100) order by (owner, id) comment 'the pets'; \
        alter table pets add index age_idx age type minmax granularity 1; \
        create view adults as select id, age from pets where age > 2; \
        insert into owners values (1, 'ann'), (2, 'bob'); \
        insert into pets (id, owner, age, tags) values (1, 1, 3, ['a']), (2, 2, 1, [])";

    #[test]
    #[ignore = "needs a local ClickHouse (scripts/test-env.sh up); creates and drops a scratch database"]
    fn local_clickhouse_session_should_keep_settings_and_temporary_tables_between_calls() {
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_session").await;
            c.query("SET max_threads = 3").await.unwrap();
            c.query("create temporary table scratch (n UInt8)").await.unwrap();
            c.query("insert into scratch values (7)").await.unwrap();
            let r = c.query("select getSetting('max_threads'), (select n from scratch)").await.unwrap();
            assert_eq!(r.rows[0], [Some("3".into()), Some("7".into())]);
            // A row limit hangs up mid-answer; the next statement waits for the session rather than failing.
            let cut = c.query_limited("select number from system.numbers limit 5000000", Some(10)).await.unwrap();
            assert!(cut.truncated);
            assert_eq!(c.query("select 'after'").await.unwrap().rows[0][0].as_deref(), Some("after"));
            drop_database(c, "tm_session").await;
        });
    }

    #[test]
    #[ignore = "needs a local ClickHouse (scripts/test-env.sh up); creates and drops function tm_twice"]
    fn local_clickhouse_functions_should_list_open_replace_and_drop() {
        use crate::catalog::ItemKind;
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_functions").await;
            c.query("drop function if exists tm_twice").await.unwrap();
            let create = crate::objects::routine_template(c.dialect(), ItemKind::Function, "tm_twice").unwrap();
            c.run_ddl(
                &crate::objects::routine_sql(c.dialect(), ItemKind::Function, "tm_twice", &create, true).unwrap(),
            )
            .await
            .unwrap();
            assert!(c.items(false).await.unwrap().iter().any(|i| i.kind == ItemKind::Function && i.name == "tm_twice"));
            let def = c.routine_definitions(ItemKind::Function, "tm_twice").await.unwrap().remove(0);
            let changed = def.replace("x + 1", "x * 2");
            c.run_ddl(
                &crate::objects::routine_sql(c.dialect(), ItemKind::Function, "tm_twice", &changed, false).unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(c.query("select tm_twice(21)").await.unwrap().rows[0][0].as_deref(), Some("42"));
            let drop = c.drop_item_sql(ItemKind::Function, "tm_twice", false).await.unwrap();
            c.run_ddl(&drop).await.unwrap();
            assert!(!c.items(false).await.unwrap().iter().any(|i| i.name == "tm_twice"));
            drop_database(c, "tm_functions").await;
        });
    }

    #[test]
    #[ignore = "needs a local ClickHouse (scripts/test-env.sh up); creates and drops a scratch database"]
    fn local_clickhouse_catalog_should_list_tables_columns_structure_and_ddl() {
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_catalog").await;
            c.query_results(SCRIPT, None).await.unwrap();
            assert_eq!(c.tables(false).await.unwrap(), ["adults", "owners", "pets"]);
            let cols = c.columns("pets").await.unwrap();
            let seen: Vec<_> =
                cols.iter().map(|c| (c.name.as_str(), c.data_type.as_str(), c.nullable, c.primary)).collect();
            assert_eq!(seen[0], ("id", "UInt64", false, true), "{seen:?}");
            assert_eq!(seen[2], ("age", "Nullable(Int32)", true, false), "{seen:?}");
            assert!(c.schema().await.unwrap().contains(&("pets".into(), "age".into())));
            assert_eq!(c.items(false).await.unwrap().iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["adults"]);
            assert!(c.is_view("adults").await.unwrap() && !c.is_view("pets").await.unwrap());
            assert_eq!(c.current_database().await.unwrap().as_deref(), Some("tm_catalog"));
            assert!(c.databases().await.unwrap().contains(&"tm_catalog".to_string()));
            assert!(!c.databases().await.unwrap().contains(&"system".to_string()));
            assert!(c.server_version().await.unwrap().starts_with("ClickHouse 24."));
            assert_eq!(c.tls_cipher().await.unwrap(), None);

            let s = c.structure("pets").await.unwrap();
            assert_eq!(s.comment.as_deref(), Some("the pets"));
            assert_eq!(s.columns[2].comment.as_deref(), Some("years"));
            assert_eq!(s.columns[4].default.as_deref(), Some("'x'"));
            assert!(s.columns[5].generated, "a MATERIALIZED column is marked");
            let idx: Vec<_> = s.indexes.iter().map(|i| (i.name.as_str(), i.columns.clone(), i.constraint)).collect();
            assert!(idx.contains(&("PRIMARY KEY", vec!["owner".into(), "id".into()], true)), "{idx:?}");
            assert!(idx.contains(&("PARTITION BY", vec!["intDiv(id, 100)".into()], true)), "{idx:?}");
            assert!(idx.contains(&("age_idx", vec!["age".into()], false)), "{idx:?}");
            assert!(c.structure("nope").await.is_err());
            let ddl = c.ddl("pets").await.unwrap();
            assert!(
                ddl.starts_with("CREATE TABLE tm_catalog.pets")
                    && ddl.ends_with(';')
                    && ddl.contains("ORDER BY (owner, id)"),
                "{ddl}"
            );
            let body = c.view_body("adults").await.unwrap().to_lowercase().replace(['`', '\n'], "");
            assert!(body.contains("select id, age"), "{body}");
            let diagram = c.diagram(None).await.unwrap();
            let names: Vec<_> = diagram.tables.iter().map(|t| (t.name.as_str(), t.view)).collect();
            assert_eq!(names, [("adults", true), ("owners", false), ("pets", false)]);
            assert_eq!(diagram.links, []);
            let o = c.overview("pets").await.unwrap();
            assert_eq!(o.rows, Some(2));
            assert!(o.bytes.is_some());
            drop_database(c, "tm_catalog").await;
        });
    }

    struct Collect(Vec<String>, Vec<Vec<Option<String>>>);

    impl crate::driver::RowSink for Collect {
        fn columns(&mut self, names: Vec<String>) -> crate::error::Result<()> {
            self.0 = names;
            Ok(())
        }

        fn row(&mut self, row: Vec<Option<String>>) -> crate::error::Result<()> {
            self.1.push(row);
            Ok(())
        }
    }

    #[test]
    #[ignore = "needs a local ClickHouse (scripts/test-env.sh up); creates and drops a scratch database"]
    fn local_clickhouse_should_browse_stream_and_refuse_writes_when_read_only() {
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_rows").await;
            c.query_results(
                "create table people(id UInt32, name String, age Nullable(UInt8), note String) engine = MergeTree order by id; \
                 insert into people values (1, 'Ada', 36, '50% off'), (2, 'Bob', 25, ''), (3, 'Cy', 41, 'a_b'), \
                 (4, 'O''Hara', 25, 'back\\\\slash'), (5, 'Eve', null, 'x')",
                None,
            )
            .await
            .unwrap();
            let browse = |filters: serde_json::Value, extra: serde_json::Value| {
                let mut v = serde_json::json!({
                    "table": "people", "filters": filters, "sort": [{"column": "id", "desc": false}],
                    "limit": 100, "offset": 0, "count": true
                });
                v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
                serde_json::from_value::<crate::browse::Browse>(v).unwrap()
            };
            let ids = |p: crate::browse::Page| p.rows.into_iter().map(|r| r[0].clone().unwrap()).collect::<Vec<_>>();
            let none = serde_json::json!({});
            let page =
                c.browse(&browse(serde_json::json!([]), serde_json::json!({"limit": 2, "offset": 1}))).await.unwrap();
            assert_eq!(
                (page.columns.clone(), page.total),
                (vec!["id".into(), "name".into(), "age".into(), "note".into()], Some(5))
            );
            assert_eq!(ids(page), ["2", "3"]);
            let f = |column: serde_json::Value, op: &str, value: &str| serde_json::json!([{"column": column, "op": op, "value": value}]);
            for (filters, want) in [
                (f("name".into(), "eq", "O'Hara"), vec!["4"]),
                (f("name".into(), "eq", "x' or '1'='1"), vec![]),
                (f("id".into(), "eq", "3"), vec!["3"]),
                (f("id".into(), "gt", "3"), vec!["4", "5"]),
                (f("note".into(), "contains", "%"), vec!["1"]),
                (f("note".into(), "contains", "_"), vec!["3"]),
                (f("note".into(), "contains", "\\"), vec!["4"]),
                (f("name".into(), "contains", "ADA"), vec!["1"]),
                (f(serde_json::Value::Null, "contains", "41"), vec!["3"]),
                (f("id".into(), "in", "1, 3"), vec!["1", "3"]),
                (f("age".into(), "is_null", ""), vec!["5"]),
                (f("note".into(), "is_empty", ""), vec!["2"]),
                (f("note".into(), "starts_with", "50"), vec!["1"]),
            ] {
                let got = ids(c.browse(&browse(filters.clone(), none.clone())).await.unwrap());
                assert_eq!(got, want, "{filters}");
            }
            let sql =
                c.filter_sql(&browse(f("name".into(), "eq", "Ada"), serde_json::json!({"limit": 10}))).await.unwrap();
            assert_eq!(sql, "select * from \"people\" where \"name\" = 'Ada' order by \"id\" asc limit 10 offset 0");

            let mut sink = Collect(vec![], vec![]);
            c.stream("select id from people where age > {p1:String} order by id", &[Some("28")], &mut sink)
                .await
                .unwrap();
            assert_eq!((sink.0, sink.1.len()), (vec!["id".to_string()], 2));
            let n = c.execute_with("insert into people (id, name, age, note) values ({p1:String}, {p2:String}, {p3:String}, {p4:String})", &[Some("6"), Some("Zed"), None, Some("tab\there")]).await.unwrap();
            assert_eq!(n, 1);
            let r = c.query("select age is null, note from people where id = 6").await.unwrap();
            assert_eq!(r.rows[0], [Some("1".into()), Some("tab\there".into())]);

            // Read-only: the bounds make the server refuse writes, then let them through again.
            let (on, off) = c.read_only_bounds();
            c.query(on).await.unwrap();
            let err =
                c.query("insert into people (id, name, note) values (9, 'No', '')").await.unwrap_err().to_string();
            assert!(err.to_lowercase().contains("readonly"), "{err}");
            assert_eq!(c.query("select count() from people").await.unwrap().rows[0][0].as_deref(), Some("6"));
            c.query(off).await.unwrap();
            c.query("insert into people (id, name, note) values (9, 'Ok', '')").await.unwrap();
            let edit: crate::rows::Changes =
                serde_json::from_value(serde_json::json!({"deletes": [{"key": [["id", "1"]]}]})).unwrap();
            assert!(c.save_rows("people", &edit).await.is_err(), "rows are edited in a SQL tab");
            drop_database(c, "tm_rows").await;
        });
    }

    #[test]
    #[ignore = "needs a local ClickHouse (scripts/test-env.sh up); creates and drops a scratch database"]
    #[expect(clippy::too_many_lines, reason = "one scenario against one server, step by step")]
    fn local_clickhouse_should_alter_tables_and_run_table_operations() {
        use crate::ddl::{Change, ColumnDef, TableOp};
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_ddl").await;
            let col = |name: &str, data_type: &str| ColumnDef {
                name: name.into(),
                data_type: data_type.into(),
                nullable: false,
                default: None,
                comment: None,
                auto_increment: false,
                primary: false,
            };
            let create = [
                Change::AddColumn { column: ColumnDef { primary: true, ..col("id", "UInt64") } },
                Change::AddColumn { column: ColumnDef { comment: Some("who".into()), ..col("name", "String") } },
                Change::Comment { comment: Some("owners".into()) },
            ];
            let sql = c.alter_sql("owners", true, &create).await.unwrap();
            assert!(
                sql[0].contains("ENGINE = MergeTree ORDER BY (\"id\")") && sql[0].ends_with("COMMENT 'owners'"),
                "{sql:?}"
            );
            c.run_ddl(&sql).await.unwrap();
            let s = c.structure("owners").await.unwrap();
            assert_eq!((s.comment.as_deref(), s.columns[1].comment.as_deref()), (Some("owners"), Some("who")));
            let sql = c.alter_sql("keyless", true, &[Change::AddColumn { column: col("a", "UInt8") }]).await.unwrap();
            assert!(sql[0].ends_with("ENGINE = MergeTree ORDER BY tuple()"), "{sql:?}");
            c.run_ddl(&sql).await.unwrap();

            let changes = [
                Change::AddColumn { column: ColumnDef { default: Some("7".into()), ..col("age", "UInt8") } },
                Change::AddColumn { column: col("nick", "Nullable(String)") },
                Change::ModifyColumn {
                    from: "name".into(),
                    column: ColumnDef { comment: Some("full name".into()), ..col("title", "String") },
                },
            ];
            let sql = c.alter_sql("owners", false, &changes).await.unwrap();
            c.run_ddl(&sql).await.unwrap();
            let s = c.structure("owners").await.unwrap();
            let seen: Vec<_> = s
                .columns
                .iter()
                .map(|c| (c.name.as_str(), c.data_type.as_str(), c.default.as_deref(), c.comment.as_deref()))
                .collect();
            assert!(seen.contains(&("age", "UInt8", Some("7"), None)), "{seen:?}");
            assert!(seen.contains(&("title", "String", None, Some("full name"))), "{seen:?}");
            let retype = [
                Change::ModifyColumn { from: "age".into(), column: col("age", "UInt32") },
                Change::DropColumn { name: "nick".into() },
            ];
            let sql = c.alter_sql("owners", false, &retype).await.unwrap();
            c.run_ddl(&sql).await.unwrap();
            let s = c.structure("owners").await.unwrap();
            assert!(
                s.columns.iter().any(|c| c.name == "age" && c.data_type == "UInt32")
                    && !s.columns.iter().any(|c| c.name == "nick")
            );

            for refused in [
                Change::SetPrimaryKey { columns: vec!["id".into()] },
                Change::AddIndex { name: "i".into(), columns: vec!["age".into()], unique: false, method: None },
                Change::AddCheck { name: "c".into(), expression: "age > 0".into() },
                Change::AddTrigger {
                    name: "t".into(),
                    timing: crate::ddl::TriggerTiming::After,
                    events: vec![crate::ddl::TriggerEvent::Insert],
                    body: "x".into(),
                },
                Change::ModifyColumn {
                    from: "age".into(),
                    column: ColumnDef { auto_increment: true, ..col("age", "UInt32") },
                },
            ] {
                assert!(c.alter_sql("owners", false, &[refused]).await.is_err());
            }

            c.query("insert into owners (id, title, age) values (1, 'a', 1), (2, 'b', 2)").await.unwrap();
            for (op, check) in [
                (
                    TableOp::Clone { table: "owners".into(), to: "owners2".into(), data: true },
                    "select count() from owners2",
                ),
                (TableOp::Rename { table: "owners2".into(), to: "owners3".into() }, "select count() from owners3"),
                (
                    TableOp::Truncate {
                        table: "owners3".into(),
                        cascade: false,
                        restart_identity: false,
                        ignore_fk: false,
                    },
                    "select count() from owners3",
                ),
                (
                    TableOp::Drop { table: "owners3".into(), cascade: false, ignore_fk: false },
                    "select count() from owners",
                ),
            ] {
                let sql = c.op_sql(&op).await.unwrap();
                c.run_ddl(&sql).await.unwrap();
                c.query(check).await.unwrap();
            }
            assert!(c.query("select * from owners3").await.is_err(), "dropped");
            assert!(c.op_sql(&TableOp::ResetAutoIncrement { table: "owners".into() }).await.is_err());

            let sql = c
                .database_sql(&crate::ddl::DbOp::Create {
                    name: "tm_ddl_other".into(),
                    encoding: None,
                    collation: None,
                })
                .await
                .unwrap();
            c.run_ddl(&sql).await.unwrap();
            let sql = c
                .database_sql(&crate::ddl::DbOp::Rename { name: "tm_ddl_other".into(), to: "tm_ddl_renamed".into() })
                .await
                .unwrap();
            c.run_ddl(&sql).await.unwrap();
            assert!(c.databases().await.unwrap().contains(&"tm_ddl_renamed".to_string()));
            let sql = c.database_sql(&crate::ddl::DbOp::Drop { name: "tm_ddl_renamed".into() }).await.unwrap();
            c.run_ddl(&sql).await.unwrap();
            assert!(c.use_database("no_such_database").await.is_err());
            assert_eq!(
                c.current_database().await.unwrap().as_deref(),
                Some("tm_ddl"),
                "a failed switch changes nothing"
            );
            drop_database(c, "tm_ddl").await;
        });
    }

    #[test]
    #[ignore = "needs a local ClickHouse (scripts/test-env.sh up)"]
    fn local_clickhouse_stopper_should_cancel_a_long_query_and_the_connection_should_carry_on() {
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_stop").await;
            let stopper = c.stopper();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if let Stopper::Notify(cancel) = stopper {
                    cancel.notify_waiters();
                }
            });
            let started = std::time::Instant::now();
            let err = c.query("select sleepEachRow(1) from numbers(30) settings max_block_size = 1").await.unwrap_err();
            assert!(matches!(err, Error::Cancelled), "{err}");
            assert!(started.elapsed() < std::time::Duration::from_secs(10));
            assert_eq!(c.query("select 1 as one").await.unwrap().rows, [[Some("1".into())]]);
            // The server stopped it too: nothing of ours is left running.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let r = c.query("select count() from system.processes where query like '%sleepEachRow%' and query not like '%system.processes%'").await.unwrap();
            assert_eq!(r.rows[0][0].as_deref(), Some("0"));
            drop_database(c, "tm_stop").await;
        });
    }

    #[test]
    #[ignore = "needs a local ClickHouse (scripts/test-env.sh up)"]
    fn local_clickhouse_process_list_should_show_a_running_query_and_kill_it() {
        tauri::async_runtime::block_on(async {
            let mut me = local("tm_proc").await;
            let mut other = local("tm_proc_other").await;
            let running = tokio::spawn(async move {
                other.query("select sleepEachRow(1) from numbers(20) settings max_block_size = 1").await
            });
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let r = me.query(crate::processes::CLICKHOUSE).await.unwrap();
            assert_eq!(r.columns[0], "Query");
            let row = r
                .rows
                .iter()
                .find(|row| row.last().and_then(Option::as_deref).is_some_and(|q| q.contains("sleepEachRow")))
                .unwrap();
            let id = row[0].clone().unwrap();
            me.fetch_with("KILL QUERY WHERE query_id = {p1:String} SYNC", &[Some(&id)]).await.unwrap();
            let killed = running.await.unwrap();
            assert!(killed.is_err(), "the other query was stopped");
            drop_database(me, "tm_proc").await;
        });
    }

    #[test]
    #[ignore = "needs a local ClickHouse (scripts/test-env.sh up); creates and drops a scratch database"]
    fn local_clickhouse_should_import_a_csv_with_a_big_cell_and_export_it_back() {
        use crate::import::{Conflict, CsvFormat, FileFormat, Import};
        use std::fmt::Write as _;
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_import").await;
            let dir = std::env::temp_dir().join(format!("tableminus-{}-ch-import", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir); // a leftover from an earlier run, or nothing
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("rows.csv");
            let big = "x".repeat(2 * 1024 * 1024);
            let mut csv = String::from("id,name,note\n");
            for i in 0..1200 {
                writeln!(csv, "{i},\"n{i}, \"\"q\"\"\",tab\there").unwrap();
            }
            writeln!(csv, "9999,big,{big}").unwrap();
            std::fs::write(&path, csv).unwrap();
            let import = |create: bool, conflict: Conflict| Import {
                path: path.clone(),
                format: FileFormat::Csv(CsvFormat { delimiter: ',', quote: '"', header: true, empty_is_null: false }),
                table: "people".into(),
                columns: vec![Some("id".into()), Some("name".into()), Some("note".into())],
                create,
                conflict,
            };
            let job = crate::jobs::Job::quiet();
            assert_eq!(crate::import::run(&mut c, &import(true, Conflict::Stop), &job).await.unwrap(), 1201);
            let r = c.query("select count(), max(length(note)), anyIf(name, id = '7') from people").await.unwrap();
            assert_eq!(r.rows[0], [Some("1201".into()), Some("2097152".into()), Some("n7, \"q\"".into())]);
            assert!(
                crate::import::run(&mut c, &import(false, Conflict::Skip), &job).await.is_err(),
                "no keys to skip on"
            );
            drop_database(c, "tm_import").await;
            let _ = std::fs::remove_dir_all(&dir); // best effort tidy-up
        });
    }

    #[test]
    #[ignore = "needs a local ClickHouse (scripts/test-env.sh up); creates and drops a scratch database"]
    fn local_clickhouse_should_quote_names_and_values_so_the_server_reads_them_back() {
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_quote").await;
            for name in [
                "plain",
                "with space",
                "back\\slash",
                "dq\"uote",
                "sq'uote",
                "back`tick",
                "tab\there",
                "ünïcode",
                "a\\\"b",
            ] {
                let d = crate::db::Dialect::Click;
                c.query(&format!(
                    "create table {} (x UInt8, {} UInt8) engine = MergeTree order by x",
                    d.ident(name),
                    d.ident(name)
                ))
                .await
                .unwrap();
                let listed = c.tables(false).await.unwrap();
                assert!(listed.contains(&name.to_string()), "{name:?} in {listed:?}");
                let read = c
                    .query(&format!(
                        "select name from system.columns where table = {} and position = 2",
                        d.literal(name)
                    ))
                    .await
                    .unwrap();
                assert_eq!(read.rows[0][0].as_deref(), Some(name), "{name:?}");
                let value = c.query(&format!("select {} as v", d.literal(name))).await.unwrap();
                assert_eq!(value.rows[0][0].as_deref(), Some(name), "{name:?}");
                c.query(&format!("drop table {}", d.ident(name))).await.unwrap();
            }
            drop_database(c, "tm_quote").await;
        });
    }
}

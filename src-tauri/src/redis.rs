//! Redis connections (`Engine::Redis`): a small RESP2 client over TCP or TLS (and an SSH tunnel when the connection
//! has one), the sessions the workspace uses, and the commands behind its key browser, value view and console.
//!
//! Redis isn't SQL, so its sessions live apart from the SQL ones (`Open`), under the same session keys. Safe mode
//! applies as for SQL: a command that may write asks, by the same levels; the read-only commands are listed in
//! [`is_read_only`], anything else counts as a write. Commands that block the connection (BLPOP, SUBSCRIBE,
//! MONITOR…) aren't run.

use crate::db::{CONNECT_TIMEOUT, Engine};
use crate::error::{Error, Result};
use crate::safety::{self, Unlock};
use crate::store::{self, Saved};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use tauri::{AppHandle, State};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufStream};

/// Longer than any value worth showing; a bigger one is cut (strings) or refused.
const MAX_BULK: usize = 64 * 1024 * 1024;
const MAX_ARRAY: usize = 1_000_000;
const MAX_DEPTH: usize = 16;
/// A status or error line.
const MAX_LINE: usize = 64 * 1024;
/// Members, fields or elements a value view shows.
const SHOWN: usize = 1000;
/// The most items the value view reads at once; "Show more" grows the view towards it.
const MAX_SHOWN: usize = 50_000;
/// A string longer than this shows its start only.
const SHOWN_STRING: usize = 16 * 1024 * 1024;
const MAX_SCAN: u32 = 5000;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// A reply, as RESP2 sends it.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Status(String),
    Error(String),
    Int(i64),
    Bulk(Option<Vec<u8>>),
    Array(Option<Vec<Reply>>),
}

impl Reply {
    fn bytes(self) -> Option<Vec<u8>> {
        match self {
            Reply::Bulk(b) => b,
            Reply::Status(s) => Some(s.into_bytes()),
            Reply::Int(n) => Some(n.to_string().into_bytes()),
            _ => None,
        }
    }

    fn text(self) -> String {
        self.bytes().map(|b| shown(&b)).unwrap_or_default()
    }

    fn int(&self) -> i64 {
        match self {
            Reply::Int(n) => *n,
            Reply::Bulk(Some(b)) => std::str::from_utf8(b).ok().and_then(|s| s.parse().ok()).unwrap_or_default(),
            _ => 0,
        }
    }

    fn items(self) -> Vec<Reply> {
        match self {
            Reply::Array(Some(items)) => items,
            _ => Vec::new(),
        }
    }

    /// The reply, or the server's error as ours.
    fn ok(self) -> Result<Reply> {
        match self {
            Reply::Error(e) => Err(Error::Redis(e)),
            r => Ok(r),
        }
    }
}

/// Text as redis-cli shows it: UTF-8 as is, other bytes as `\xNN`.
fn shown(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().fold(String::new(), |mut s, &c| {
            if c.is_ascii_graphic() || c == b' ' {
                s.push(char::from(c));
            } else {
                let _ = write!(s, "\\x{c:02x}"); // writing to a String can't fail
            }
            s
        }),
    }
}

/// A key as the UI holds it: its text, and its bytes in hex when they aren't UTF-8, so it round-trips exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub text: String,
    #[serde(default)]
    pub hex: Option<String>,
}

impl Key {
    fn of(b: Vec<u8>) -> Key {
        match String::from_utf8(b) {
            Ok(text) => Key { text, hex: None },
            Err(e) => {
                let b = e.into_bytes();
                let hex = b.iter().fold(String::with_capacity(b.len() * 2), |mut h, c| {
                    let _ = write!(h, "{c:02x}"); // writing to a String can't fail
                    h
                });
                Key { text: shown(&b), hex: Some(hex) }
            }
        }
    }

    fn bytes(&self) -> Result<Vec<u8>> {
        let Some(hex) = &self.hex else { return Ok(self.text.as_bytes().to_vec()) };
        if !hex.len().is_multiple_of(2) {
            return Err(Error::Invalid("a key's hex isn't whole bytes"));
        }
        (0..hex.len())
            .step_by(2)
            .map(|i| {
                hex.get(i..i + 2)
                    .and_then(|p| u8::from_str_radix(p, 16).ok())
                    .ok_or(Error::Invalid("a key's hex isn't hex"))
            })
            .collect()
    }
}

/// One connection to a Redis server.
pub struct Client {
    io: BufStream<Box<dyn Io>>,
}

impl Client {
    /// Connects to `host:port` (plain, or TLS checked against `tls_name`), signs in and selects `db`.
    async fn connect(
        dial: (&str, u16),
        tls: Option<(native_tls::TlsConnector, &str)>,
        user: Option<&str>,
        password: Option<&SecretString>,
        db: u32,
    ) -> Result<Client> {
        let tcp = tokio::net::TcpStream::connect(dial).await?;
        let io: Box<dyn Io> = match tls {
            Some((connector, name)) => {
                Box::new(tokio_native_tls::TlsConnector::from(connector).connect(name, tcp).await?)
            }
            None => Box::new(tcp),
        };
        let mut c = Client { io: BufStream::new(io) };
        if let Some(p) = password {
            let mut args: Vec<&[u8]> = vec![b"AUTH"];
            if let Some(u) = user.filter(|u| !u.is_empty()) {
                args.push(u.as_bytes());
            }
            args.push(p.expose_secret().as_bytes());
            c.call(&args).await?.ok()?;
        }
        if db != 0 {
            c.call(&[b"SELECT", db.to_string().as_bytes()]).await?.ok()?;
        }
        c.call(&[b"PING"]).await?.ok()?;
        Ok(c)
    }

    async fn send(&mut self, args: &[&[u8]]) -> Result<()> {
        let mut out = format!("*{}\r\n", args.len()).into_bytes();
        for a in args {
            out.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
            out.extend_from_slice(a);
            out.extend_from_slice(b"\r\n");
        }
        self.io.write_all(&out).await?;
        Ok(self.io.flush().await?)
    }

    /// Sends one command and reads its reply.
    async fn call(&mut self, args: &[&[u8]]) -> Result<Reply> {
        self.send(args).await?;
        self.read(0).await
    }

    /// Sends several commands at once and reads their replies, in order.
    async fn pipeline(&mut self, commands: &[Vec<Vec<u8>>]) -> Result<Vec<Reply>> {
        let mut out = Vec::new();
        for args in commands {
            out.extend_from_slice(format!("*{}\r\n", args.len()).as_bytes());
            for a in args {
                out.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
                out.extend_from_slice(a);
                out.extend_from_slice(b"\r\n");
            }
        }
        self.io.write_all(&out).await?;
        self.io.flush().await?;
        let mut replies = Vec::with_capacity(commands.len());
        for _ in commands {
            replies.push(self.read(0).await?);
        }
        Ok(replies)
    }

    async fn line(&mut self) -> Result<String> {
        let mut line = Vec::new();
        (&mut self.io).take(MAX_LINE as u64).read_until(b'\n', &mut line).await?;
        if !line.ends_with(b"\r\n") {
            return Err(Error::Disconnected);
        }
        line.truncate(line.len() - 2);
        Ok(String::from_utf8_lossy(&line).into_owned())
    }

    fn read(&mut self, depth: usize) -> Pin<Box<dyn Future<Output = Result<Reply>> + Send + '_>> {
        Box::pin(async move {
            if depth > MAX_DEPTH {
                return Err(Error::Redis("the reply is nested too deeply to read".into()));
            }
            let kind = self.io.read_u8().await?;
            let line = self.line().await?;
            let number = || line.parse::<i64>().map_err(|_| Error::Redis("the server sent a malformed reply".into()));
            Ok(match kind {
                b'+' => Reply::Status(line.clone()),
                b'-' => Reply::Error(line.clone()),
                b':' => Reply::Int(number()?),
                b'$' => match usize::try_from(number()?) {
                    Err(_) => Reply::Bulk(None),
                    Ok(n) if n > MAX_BULK => {
                        return Err(Error::Redis("a value is larger than TableMinus reads (64 MB)".into()));
                    }
                    Ok(n) => {
                        let mut b = vec![0u8; n + 2];
                        self.io.read_exact(&mut b).await?;
                        b.truncate(n);
                        Reply::Bulk(Some(b))
                    }
                },
                b'*' => match usize::try_from(number()?) {
                    Err(_) => Reply::Array(None),
                    Ok(n) if n > MAX_ARRAY => {
                        return Err(Error::Redis("a reply has more items than TableMinus reads".into()));
                    }
                    Ok(n) => {
                        let mut items = Vec::with_capacity(n.min(1024));
                        for _ in 0..n {
                            items.push(self.read(depth + 1).await?);
                        }
                        Reply::Array(Some(items))
                    }
                },
                _ => return Err(Error::Redis("the server sent something that isn't RESP".into())),
            })
        })
    }
}

/// Whether a command only reads, so safe mode needn't ask. Anything not listed counts as a write.
pub fn is_read_only(args: &[Vec<u8>]) -> bool {
    let name = args.first().map(|a| String::from_utf8_lossy(a).to_ascii_uppercase()).unwrap_or_default();
    let sub = args.get(1).map(|a| String::from_utf8_lossy(a).to_ascii_uppercase()).unwrap_or_default();
    match name.as_str() {
        "CLIENT" => ["LIST", "INFO", "GETNAME", "ID"].contains(&sub.as_str()),
        "CONFIG" => sub == "GET",
        "MEMORY" => ["USAGE", "STATS", "DOCTOR"].contains(&sub.as_str()),
        "SLOWLOG" => sub == "GET" || sub == "LEN",
        "OBJECT" | "COMMAND" | "LATENCY" | "XINFO" => true,
        _ => READ_ONLY.contains(&name.as_str()),
    }
}

const READ_ONLY: &[&str] = &[
    "GET",
    "MGET",
    "STRLEN",
    "GETRANGE",
    "SUBSTR",
    "EXISTS",
    "TYPE",
    "TTL",
    "PTTL",
    "EXPIRETIME",
    "PEXPIRETIME",
    "KEYS",
    "SCAN",
    "RANDOMKEY",
    "DBSIZE",
    "DUMP",
    "HGET",
    "HMGET",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HEXISTS",
    "HSCAN",
    "HSTRLEN",
    "HRANDFIELD",
    "LRANGE",
    "LLEN",
    "LINDEX",
    "LPOS",
    "SMEMBERS",
    "SISMEMBER",
    "SMISMEMBER",
    "SCARD",
    "SSCAN",
    "SRANDMEMBER",
    "SINTER",
    "SUNION",
    "SDIFF",
    "SINTERCARD",
    "ZRANGE",
    "ZRANGEBYSCORE",
    "ZREVRANGE",
    "ZREVRANGEBYSCORE",
    "ZRANGEBYLEX",
    "ZREVRANGEBYLEX",
    "ZSCORE",
    "ZMSCORE",
    "ZCARD",
    "ZCOUNT",
    "ZLEXCOUNT",
    "ZRANK",
    "ZREVRANK",
    "ZSCAN",
    "ZRANDMEMBER",
    "ZINTER",
    "ZUNION",
    "ZDIFF",
    "ZINTERCARD",
    "XRANGE",
    "XREVRANGE",
    "XLEN",
    "XPENDING",
    "BITCOUNT",
    "BITPOS",
    "GETBIT",
    "BITFIELD_RO",
    "PFCOUNT",
    "GEOPOS",
    "GEODIST",
    "GEOHASH",
    "GEORADIUS_RO",
    "GEORADIUSBYMEMBER_RO",
    "GEOSEARCH",
    "INFO",
    "PING",
    "ECHO",
    "TIME",
    "LASTSAVE",
    "ROLE",
    "LOLWUT",
    "LCS",
];

/// Commands that hold the connection until something happens elsewhere; the console doesn't run them.
fn blocks(args: &[Vec<u8>]) -> bool {
    let name = args.first().map(|a| String::from_utf8_lossy(a).to_ascii_uppercase()).unwrap_or_default();
    let blocking_read = name == "XREAD" || name == "XREADGROUP";
    let has_block = args.iter().any(|a| a.eq_ignore_ascii_case(b"BLOCK"));
    [
        "BLPOP",
        "BRPOP",
        "BRPOPLPUSH",
        "BLMOVE",
        "BLMPOP",
        "BZPOPMIN",
        "BZPOPMAX",
        "BZMPOP",
        "SUBSCRIBE",
        "PSUBSCRIBE",
        "SSUBSCRIBE",
        "MONITOR",
        "SYNC",
        "PSYNC",
        "WAIT",
        "WAITAOF",
    ]
    .contains(&name.as_str())
        || (blocking_read && has_block)
}

/// Splits a console line into arguments as redis-cli does: spaces separate, "double quotes" take `\n`, `\t`, `\\`,
/// `\"` and `\xNN`, 'single quotes' take the text as it is.
pub fn split_line(line: &str) -> Result<Vec<Vec<u8>>> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else { break };
        let mut arg = Vec::new();
        if first == '"' || first == '\'' {
            chars.next();
            let mut closed = false;
            while let Some(c) = chars.next() {
                if c == first {
                    closed = true;
                    break;
                }
                if c == '\\' && first == '"' {
                    match chars.next() {
                        Some('n') => arg.push(b'\n'),
                        Some('r') => arg.push(b'\r'),
                        Some('t') => arg.push(b'\t'),
                        Some('x') => {
                            let hex: String = chars.by_ref().take(2).collect();
                            arg.push(
                                u8::from_str_radix(&hex, 16).map_err(|_| Error::Invalid("\\x wants two hex digits"))?,
                            );
                        }
                        Some(other) => arg.extend_from_slice(other.to_string().as_bytes()),
                        None => break,
                    }
                } else if c == '\\' && chars.peek() == Some(&'\'') {
                    chars.next();
                    arg.push(b'\'');
                } else {
                    arg.extend_from_slice(c.to_string().as_bytes());
                }
            }
            if !closed {
                return Err(Error::Invalid("a quote isn't closed"));
            }
            if chars.peek().is_some_and(|c| !c.is_whitespace()) {
                return Err(Error::Invalid("a closing quote must be followed by a space"));
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
                arg.extend_from_slice(c.to_string().as_bytes());
            }
        }
        args.push(arg);
    }
    Ok(args)
}

// ---------- Sessions ----------

pub(crate) struct Session {
    client: Client,
    saved: Saved,
    password: Option<SecretString>,
    db: u32,
    _tunnel: Option<crate::ssh::Tunnel>,
}

/// The open sessions, by session key.
pub(crate) type Sessions = crate::sessions::Sessions<Session>;

impl crate::sessions::Session for Session {
    fn saved_mut(&mut self) -> &mut Saved {
        &mut self.saved
    }
}

/// `redis://[user@]host[:port][/db]`: where to dial, who to sign in as, and which database.
fn parse_url(url: &str) -> Result<(String, u16, Option<String>, u32)> {
    let u = url::Url::parse(url)?;
    if !matches!(u.scheme(), "redis" | "rediss") {
        return Err(Error::Invalid("a Redis URL starts with redis://"));
    }
    let host = u.host_str().unwrap_or("127.0.0.1").trim_start_matches('[').trim_end_matches(']').to_string();
    let user = Some(u.username().to_string()).filter(|s| !s.is_empty());
    let db = u.path().trim_matches('/').parse().unwrap_or(0);
    Ok((host, u.port().unwrap_or(6379), user, db))
}

/// Opens a session for a saved connection, through its SSH tunnel when it has one.
async fn open_session(
    saved: Saved,
    password: Option<SecretString>,
    ssh_secret: Option<SecretString>,
    db: Option<u32>,
) -> Result<Session> {
    if !saved.options.startup_script.trim().is_empty() {
        crate::db::run_startup_script(&saved.options.startup_script).await?;
    }
    let (host, port, user, url_db) = parse_url(&saved.url)?;
    let db = db.unwrap_or(url_db);
    let tunnel = match &saved.ssh {
        Some(ssh) => Some(crate::ssh::open_for(Engine::Redis, &saved.url, ssh, ssh_secret.as_ref()).await?),
        None => None,
    };
    let dial = tunnel.as_ref().map_or((host.as_str(), port), |t| ("127.0.0.1", t.port));
    let tls = saved.tls.redis()?.map(|c| (c, host.as_str()));
    let client =
        tokio::time::timeout(CONNECT_TIMEOUT, Client::connect(dial, tls, user.as_deref(), password.as_ref(), db))
            .await
            .map_err(|_| Error::ConnectTimeout(CONNECT_TIMEOUT.as_secs()))??;
    Ok(Session { client, saved, password, db, _tunnel: tunnel })
}

/// Runs `f` on the session within the query timeout; a dropped connection reads as DISCONNECTED.
async fn with<T>(
    prefs: &crate::Prefs,
    s: &mut Session,
    f: impl for<'a> FnOnce(&'a mut Client) -> Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>,
) -> Result<T> {
    let limit = crate::query_limit(prefs);
    let run = f(&mut s.client);
    let r = match limit {
        Some(d) => tokio::time::timeout(d, run).await.map_err(|_| Error::QueryTimeout(d.as_secs()))?,
        None => run.await,
    };
    r.map_err(|e| match e {
        Error::Io(_) => Error::Disconnected,
        e => e,
    })
}

/// What the workspace header shows, in the shape `connect` gives for SQL.
#[derive(Debug, Serialize)]
pub struct Info {
    server: String,
    database: Option<String>,
    can_switch: bool,
    tables: Vec<String>,
}

async fn info(prefs: &crate::Prefs, s: &mut Session) -> Result<Info> {
    let text =
        with(prefs, s, |c| Box::pin(async move { Ok(c.call(&[b"INFO", b"server"]).await?.ok()?.text()) })).await?;
    let version = text.lines().find_map(|l| l.strip_prefix("redis_version:")).unwrap_or("").trim().to_string();
    let product = if text.contains("valkey_version") { "Valkey" } else { "Redis" };
    Ok(Info {
        server: format!("{product} {version}").trim().to_string(),
        database: Some(s.db.to_string()),
        can_switch: true,
        tables: Vec::new(),
    })
}

/// Test in the connection form: opens a session, says what answered, and closes it.
pub async fn test(saved: Saved, password: Option<SecretString>, ssh_secret: Option<SecretString>) -> Result<String> {
    let mut s = open_session(saved, password, ssh_secret, None).await?;
    let server = info(&crate::Prefs::default(), &mut s).await?.server;
    let keys = s.client.call(&[b"DBSIZE"]).await?.ok()?.int();
    Ok(format!("Connected · {server} · {keys} keys in database {}", s.db))
}

/// Opens (or reuses) the session `name` of a saved Redis connection, on `database` when given.
#[tauri::command]
pub async fn redis_connect(
    app: AppHandle,
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
    database: Option<String>,
) -> Result<Info> {
    open.unlocked()?;
    let db = database
        .as_deref()
        .map(|d| d.parse::<u32>().map_err(|_| Error::Invalid("a Redis database is a number")))
        .transpose()?;
    if let Some(existing) = sessions.existing(&name).await {
        let mut s = existing.lock().await;
        if db.is_none_or(|d| d == s.db) {
            return info(&prefs, &mut s).await;
        }
    }
    let saved_name = name.split_once('\u{1f}').map_or(name.as_str(), |(n, _)| n);
    let saved = store::load(&crate::dir(&app)?)?
        .into_iter()
        .find(|c| c.name == saved_name)
        .ok_or_else(|| Error::NoSuchConnection(saved_name.to_string()))?;
    if saved.kind != Engine::Redis {
        return Err(Error::Invalid("not a Redis connection"));
    }
    let password = store::password_for(&saved)?;
    let ssh_secret = if saved.ssh.is_some() { store::stored(store::Secret::Ssh, &saved.name)? } else { None };
    let badge = saved.needs_credentials.then(|| saved.name.clone());
    let mut s = open_session(saved, password, ssh_secret, db).await?;
    if let Some(badge) = badge {
        // The badge is only a hint: failing to take it off must not fail a connect that worked.
        let _ = store::clear_needs_credentials(&crate::dir(&app)?, &badge);
    }
    let r = info(&prefs, &mut s).await;
    sessions.insert(name, s).await;
    r
}

/// The databases to choose from, with their key counts where the server says.
#[derive(Debug, Serialize)]
pub struct Database {
    index: u32,
    keys: u64,
}

#[tauri::command]
pub async fn redis_databases(
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
) -> Result<Vec<Database>> {
    open.unlocked()?;
    let mut s = sessions.get(&name).await?;
    let (count, keyspace) = with(&prefs, &mut s, |c| {
        Box::pin(async move {
            let replies = c
                .pipeline(&[
                    vec![b"CONFIG".to_vec(), b"GET".to_vec(), b"databases".to_vec()],
                    vec![b"INFO".to_vec(), b"keyspace".to_vec()],
                ])
                .await?;
            let mut it = replies.into_iter();
            // Managed servers may refuse CONFIG; 16 is Redis's default.
            let count =
                it.next().map(Reply::items).and_then(|i| i.into_iter().nth(1)).map_or(16, |r| r.int()).clamp(1, 1024);
            Ok((count, it.next().map(Reply::text).unwrap_or_default()))
        })
    })
    .await?;
    let keys = |i: i64| {
        keyspace
            .lines()
            .find_map(|l| l.strip_prefix(&format!("db{i}:")))
            .and_then(|rest| rest.split(',').find_map(|kv| kv.strip_prefix("keys=")))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0)
    };
    Ok((0..count).map(|i| Database { index: u32::try_from(i).unwrap_or_default(), keys: keys(i) }).collect())
}

#[derive(Debug, Serialize)]
pub struct KeyEntry {
    key: Key,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Serialize)]
pub struct Scanned {
    /// "0" when the scan is complete.
    cursor: String,
    keys: Vec<KeyEntry>,
}

/// One SCAN step: keys matching `pattern` from `cursor`, each with its type.
#[tauri::command]
pub async fn redis_scan(
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
    pattern: String,
    cursor: String,
    count: u32,
) -> Result<Scanned> {
    open.unlocked()?;
    let pattern = if pattern.trim().is_empty() { "*".to_string() } else { pattern };
    let count = count.clamp(1, MAX_SCAN).to_string();
    let mut s = sessions.get(&name).await?;
    with(&prefs, &mut s, |c| {
        Box::pin(async move {
            let reply = c
                .call(&[b"SCAN", cursor.as_bytes(), b"MATCH", pattern.as_bytes(), b"COUNT", count.as_bytes()])
                .await?
                .ok()?;
            let mut parts = reply.items().into_iter();
            let cursor = parts.next().map_or_else(|| "0".into(), Reply::text);
            let keys: Vec<Vec<u8>> =
                parts.next().map(Reply::items).unwrap_or_default().into_iter().filter_map(Reply::bytes).collect();
            let types = c.pipeline(&keys.iter().map(|k| vec![b"TYPE".to_vec(), k.clone()]).collect::<Vec<_>>()).await?;
            let keys = keys.into_iter().zip(types).map(|(k, t)| KeyEntry { key: Key::of(k), kind: t.text() }).collect();
            Ok(Scanned { cursor, keys })
        })
    })
    .await
}

/// A key's value as the value view shows it: at most [`SHOWN`] items (more when asked, up to [`MAX_SHOWN`]), each text.
#[derive(Debug, Serialize, PartialEq)]
pub struct Value {
    #[serde(rename = "type")]
    kind: String,
    /// Milliseconds left; -1 without an expiry, -2 when the key is gone.
    ttl_ms: i64,
    /// Length, members, fields or entries in all.
    size: u64,
    /// string: one item; list: index, value; set: member; zset: member, score; hash: field, value; stream: id, then
    /// field and value pairs.
    items: Vec<Vec<String>>,
}

async fn read_value(c: &mut Client, key: &[u8], limit: usize) -> Result<Value> {
    let head = c.pipeline(&[vec![b"TYPE".to_vec(), key.to_vec()], vec![b"PTTL".to_vec(), key.to_vec()]]).await?;
    let mut head = head.into_iter();
    let kind = head.next().map(Reply::text).unwrap_or_default();
    let ttl_ms = head.next().map_or(-2, |r| r.int());
    let shown_n = (limit - 1).to_string();
    let (size, items): (u64, Vec<Vec<String>>) = match kind.as_str() {
        "string" => {
            let len = c.call(&[b"STRLEN", key]).await?.ok()?.int();
            let end = (SHOWN_STRING - 1).to_string();
            let v = c.call(&[b"GETRANGE", key, b"0", end.as_bytes()]).await?.ok()?.text();
            (u64::try_from(len).unwrap_or_default(), vec![vec![v]])
        }
        "list" => {
            let len = c.call(&[b"LLEN", key]).await?.ok()?.int();
            let items = c.call(&[b"LRANGE", key, b"0", shown_n.as_bytes()]).await?.ok()?.items();
            (
                u64::try_from(len).unwrap_or_default(),
                items.into_iter().enumerate().map(|(i, v)| vec![i.to_string(), v.text()]).collect(),
            )
        }
        "zset" => {
            let len = c.call(&[b"ZCARD", key]).await?.ok()?.int();
            let flat = c.call(&[b"ZRANGE", key, b"0", shown_n.as_bytes(), b"WITHSCORES"]).await?.ok()?.items();
            let items = flat.chunks(2).map(|p| p.iter().cloned().map(Reply::text).collect()).collect();
            (u64::try_from(len).unwrap_or_default(), items)
        }
        "set" | "hash" => {
            let (len_cmd, scan_cmd, pairs): (&[u8], &[u8], bool) =
                if kind == "set" { (b"SCARD", b"SSCAN", false) } else { (b"HLEN", b"HSCAN", true) };
            let len = c.call(&[len_cmd, key]).await?.ok()?.int();
            let mut cursor = b"0".to_vec();
            let mut flat = Vec::new();
            loop {
                let reply = c.call(&[scan_cmd, key, &cursor, b"COUNT", b"500"]).await?.ok()?;
                let mut parts = reply.items().into_iter();
                cursor = parts.next().and_then(Reply::bytes).unwrap_or_else(|| b"0".to_vec());
                flat.extend(parts.next().map(Reply::items).unwrap_or_default());
                if cursor == b"0" || flat.len() >= limit * if pairs { 2 } else { 1 } {
                    break;
                }
            }
            let items: Vec<Vec<String>> = if pairs {
                flat.chunks(2).take(limit).map(|p| p.iter().cloned().map(Reply::text).collect()).collect()
            } else {
                flat.into_iter().take(limit).map(|m| vec![m.text()]).collect()
            };
            (u64::try_from(len).unwrap_or_default(), items)
        }
        "stream" => {
            let len = c.call(&[b"XLEN", key]).await?.ok()?.int();
            let n = limit.to_string();
            let entries = c.call(&[b"XRANGE", key, b"-", b"+", b"COUNT", n.as_bytes()]).await?.ok()?.items();
            let items = entries
                .into_iter()
                .map(|e| {
                    let mut parts = e.items().into_iter();
                    let id = parts.next().map(Reply::text).unwrap_or_default();
                    let fields = parts.next().map(Reply::items).unwrap_or_default();
                    std::iter::once(id).chain(fields.into_iter().map(Reply::text)).collect()
                })
                .collect();
            (u64::try_from(len).unwrap_or_default(), items)
        }
        _ => (0, Vec::new()),
    };
    Ok(Value { kind, ttl_ms, size, items })
}

#[tauri::command]
pub async fn redis_value(
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
    key: Key,
    limit: Option<usize>,
) -> Result<Value> {
    open.unlocked()?;
    let key = key.bytes()?;
    let limit = limit.unwrap_or(SHOWN).clamp(SHOWN, MAX_SHOWN);
    let mut s = sessions.get(&name).await?;
    with(&prefs, &mut s, |c| Box::pin(async move { read_value(c, &key, limit).await })).await
}

/// A change made from the value view. Each is one command (two for removing a list element by position).
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    /// A new string, or a string's new value, keeping its expiry.
    SetString {
        key: Key,
        value: String,
    },
    HashSet {
        key: Key,
        field: String,
        value: String,
    },
    HashDelete {
        key: Key,
        field: String,
    },
    ListSet {
        key: Key,
        index: i64,
        value: String,
    },
    ListPush {
        key: Key,
        value: String,
        head: bool,
    },
    ListRemove {
        key: Key,
        index: i64,
    },
    SetAdd {
        key: Key,
        member: String,
    },
    SetRemove {
        key: Key,
        member: String,
    },
    ZsetAdd {
        key: Key,
        member: String,
        score: f64,
    },
    ZsetRemove {
        key: Key,
        member: String,
    },
    StreamAdd {
        key: Key,
        fields: Vec<(String, String)>,
    },
    /// None removes the expiry.
    Expire {
        key: Key,
        seconds: Option<i64>,
    },
    Rename {
        key: Key,
        to: String,
    },
    Delete {
        keys: Vec<Key>,
    },
}

impl Edit {
    /// The commands the edit runs.
    fn commands(&self) -> Result<Vec<Vec<Vec<u8>>>> {
        let b = |s: &str| s.as_bytes().to_vec();
        Ok(match self {
            Edit::SetString { key, value } => vec![vec![b("SET"), key.bytes()?, b(value), b("KEEPTTL")]],
            Edit::HashSet { key, field, value } => vec![vec![b("HSET"), key.bytes()?, b(field), b(value)]],
            Edit::HashDelete { key, field } => vec![vec![b("HDEL"), key.bytes()?, b(field)]],
            Edit::ListSet { key, index, value } => vec![vec![b("LSET"), key.bytes()?, b(&index.to_string()), b(value)]],
            Edit::ListPush { key, value, head } => {
                vec![vec![b(if *head { "LPUSH" } else { "RPUSH" }), key.bytes()?, b(value)]]
            }
            Edit::ListRemove { key, index } => {
                // Lists remove by value, so the element becomes a value nothing else holds, which then goes.
                let mark = format!("__tableminus_removed_{}", crate::mcp::random_hex(16)?);
                vec![
                    vec![b("LSET"), key.bytes()?, b(&index.to_string()), b(&mark)],
                    vec![b("LREM"), key.bytes()?, b("1"), b(&mark)],
                ]
            }
            Edit::SetAdd { key, member } => vec![vec![b("SADD"), key.bytes()?, b(member)]],
            Edit::SetRemove { key, member } => vec![vec![b("SREM"), key.bytes()?, b(member)]],
            Edit::ZsetAdd { key, member, score } => {
                if !score.is_finite() {
                    return Err(Error::Invalid("a score is a finite number"));
                }
                vec![vec![b("ZADD"), key.bytes()?, b(&score.to_string()), b(member)]]
            }
            Edit::ZsetRemove { key, member } => vec![vec![b("ZREM"), key.bytes()?, b(member)]],
            Edit::StreamAdd { key, fields } => {
                if fields.is_empty() {
                    return Err(Error::Invalid("a stream entry needs a field"));
                }
                let mut args = vec![b("XADD"), key.bytes()?, b("*")];
                for (f, v) in fields {
                    args.extend([b(f), b(v)]);
                }
                vec![args]
            }
            Edit::Expire { key, seconds: Some(s) } if *s > 0 => {
                vec![vec![b("EXPIRE"), key.bytes()?, b(&s.to_string())]]
            }
            Edit::Expire { seconds: Some(_), .. } => {
                return Err(Error::Invalid("an expiry is a number of seconds above 0"));
            }
            Edit::Expire { key, seconds: None } => vec![vec![b("PERSIST"), key.bytes()?]],
            // RENAMENX: a key of that name is never overwritten.
            Edit::Rename { key, to } => vec![vec![b("RENAMENX"), key.bytes()?, b(to)]],
            Edit::Delete { keys } => {
                let mut args = vec![b("DEL")];
                for k in keys {
                    args.push(k.bytes()?);
                }
                vec![args]
            }
        })
    }
}

/// Runs an edit, after safe mode's question (`confirmed`, `unlock`) when the level asks before writes.
#[tauri::command]
pub async fn redis_edit(
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
    edit: Edit,
    confirmed: bool,
    unlock: Option<Unlock>,
) -> Result<()> {
    open.unlocked()?;
    let commands = edit.commands()?;
    let mut s = sessions.get(&name).await?;
    safety::check(s.saved.safety, false, confirmed, unlock.as_ref(), s.password.as_ref()).await?;
    for c in &commands {
        crate::console::record(&c.iter().map(|a| shown(a)).collect::<Vec<_>>().join(" "));
    }
    let renamed_onto_existing = matches!(edit, Edit::Rename { .. });
    let replies = with(&prefs, &mut s, |c| Box::pin(async move { c.pipeline(&commands).await })).await?;
    for r in replies {
        let r = r.ok()?;
        if renamed_onto_existing && r == Reply::Int(0) {
            return Err(Error::Invalid("a key of that name exists already"));
        }
    }
    Ok(())
}

/// A reply as the console shows it.
#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Shown {
    Status { text: String },
    Error { text: String },
    Int { value: i64 },
    Bulk { text: Option<String> },
    Array { items: Option<Vec<Shown>> },
}

impl From<Reply> for Shown {
    fn from(r: Reply) -> Self {
        match r {
            Reply::Status(text) => Shown::Status { text },
            Reply::Error(text) => Shown::Error { text },
            Reply::Int(value) => Shown::Int { value },
            Reply::Bulk(b) => Shown::Bulk { text: b.map(|b| shown(&b)) },
            Reply::Array(items) => Shown::Array { items: items.map(|i| i.into_iter().map(Shown::from).collect()) },
        }
    }
}

/// Runs a console line. Safe mode asks before anything not known to only read.
#[tauri::command]
pub async fn redis_command(
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
    line: String,
    confirmed: bool,
    unlock: Option<Unlock>,
) -> Result<Shown> {
    open.unlocked()?;
    let args = split_line(&line)?;
    if args.is_empty() {
        return Err(Error::Invalid("type a command"));
    }
    if blocks(&args) {
        return Err(Error::Invalid("commands that wait (BLPOP, SUBSCRIBE, MONITOR…) don't run in the console"));
    }
    let first = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
    if first == "SELECT" || first == "QUIT" || first == "RESET" {
        return Err(Error::Invalid("choose the database with the database button; this console stays on it"));
    }
    let mut s = sessions.get(&name).await?;
    safety::check(s.saved.safety, is_read_only(&args), confirmed, unlock.as_ref(), s.password.as_ref()).await?;
    crate::console::record(&line);
    let reply = with(&prefs, &mut s, |c| {
        Box::pin(async move {
            let refs: Vec<&[u8]> = args.iter().map(Vec::as_slice).collect();
            c.call(&refs).await
        })
    })
    .await?;
    Ok(reply.into())
}

/// Every Redis session's connection, for Settings › Safety's level change and connection info.
#[derive(Debug, Serialize)]
pub struct ConnectionInfo {
    server: String,
    driver: String,
    cipher: Option<String>,
    ssh: Option<String>,
}

#[tauri::command]
pub async fn redis_connection_info(
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
) -> Result<ConnectionInfo> {
    let mut s = sessions.get(&name).await?;
    let server = info(&prefs, &mut s).await?.server;
    let tls = s.saved.tls.redis()?.is_some();
    let ssh = s.saved.ssh.as_ref().map(|t| format!("{}@{}", t.user, t.host));
    Ok(ConnectionInfo { server, driver: "TableMinus RESP2 client".into(), cipher: tls.then(|| "TLS".to_string()), ssh })
}

#[cfg(test)]
mod tests {
    use super::{Client, Edit, Key, Reply, SHOWN, blocks, is_read_only, parse_url, read_value, split_line};

    fn args(line: &str) -> Vec<Vec<u8>> {
        split_line(line).unwrap()
    }

    #[test]
    fn split_line_should_read_quotes_and_escapes_like_redis_cli() {
        assert_eq!(
            args(r#"SET "a key" 'it\'s' "x\ny\x41""#),
            [b"SET".to_vec(), b"a key".to_vec(), b"it's".to_vec(), b"x\nyA".to_vec()]
        );
        assert_eq!(args("  GET   k  "), [b"GET".to_vec(), b"k".to_vec()]);
        assert!(split_line(r#"GET "open"#).is_err());
        assert!(split_line(r#"GET "a"b"#).is_err());
    }

    #[test]
    fn is_read_only_should_know_reads_and_count_the_rest_as_writes() {
        assert!(is_read_only(&args("get k")));
        assert!(is_read_only(&args("HGETALL h")));
        assert!(is_read_only(&args("config get maxmemory")));
        assert!(!is_read_only(&args("config set maxmemory 1")));
        assert!(!is_read_only(&args("SET k v")));
        assert!(!is_read_only(&args("FLUSHALL")));
        assert!(!is_read_only(&args("EVAL 'return 1' 0")), "scripts may write");
    }

    #[test]
    fn blocks_should_catch_commands_that_wait() {
        assert!(blocks(&args("BLPOP q 0")));
        assert!(blocks(&args("xread block 0 streams s $")));
        assert!(!blocks(&args("XREAD COUNT 1 STREAMS s 0")));
        assert!(blocks(&args("subscribe ch")));
    }

    #[test]
    fn key_should_round_trip_bytes_that_arent_utf8() {
        let k = Key::of(vec![b'a', 0xff, b'b']);
        assert_eq!(k.text, "a\\xffb");
        assert_eq!(k.bytes().unwrap(), vec![b'a', 0xff, b'b']);
        assert_eq!(Key::of(b"plain".to_vec()).hex, None);
    }

    #[test]
    fn parse_url_should_read_host_port_user_and_database() {
        assert_eq!(
            parse_url("redis://app@cache.local:6380/3").unwrap(),
            ("cache.local".into(), 6380, Some("app".into()), 3)
        );
        assert_eq!(parse_url("redis://[::1]").unwrap(), ("::1".into(), 6379, None, 0));
        assert!(parse_url("postgres://x").is_err());
    }

    #[test]
    fn edit_should_refuse_a_score_that_isnt_a_number() {
        let e = Edit::ZsetAdd { key: Key::of(b"z".to_vec()), member: "m".into(), score: f64::NAN };
        assert!(e.commands().is_err());
    }

    /// A local Redis on the default port, database `db`, emptied first. Each test has its own database, as they run
    /// at once.
    async fn local(db: u32) -> Client {
        let c = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            Client::connect(("127.0.0.1", 6379), None, None, None, db),
        )
        .await;
        let mut c = c.unwrap().unwrap();
        c.call(&[b"FLUSHDB"]).await.unwrap();
        c
    }

    #[test]
    #[ignore = "needs a local Redis on 6379; empties database 15"]
    fn local_redis_values_of_every_type() {
        tauri::async_runtime::block_on(async {
            let mut c = local(15).await;
            c.call(&[b"SET", b"s", b"hello"]).await.unwrap();
            c.call(&[b"RPUSH", b"l", b"a", b"b"]).await.unwrap();
            c.call(&[b"SADD", b"set", b"m"]).await.unwrap();
            c.call(&[b"ZADD", b"z", b"1.5", b"m"]).await.unwrap();
            c.call(&[b"HSET", b"h", b"f", b"v"]).await.unwrap();
            c.call(&[b"XADD", b"x", b"1-1", b"f", b"v"]).await.unwrap();
            c.call(&[b"EXPIRE", b"s", b"100"]).await.unwrap();
            let s = read_value(&mut c, b"s", SHOWN).await.unwrap();
            assert_eq!((s.kind.as_str(), s.size, s.items.clone()), ("string", 5, vec![vec!["hello".to_string()]]));
            assert!(s.ttl_ms > 90_000);
            assert_eq!(read_value(&mut c, b"l", SHOWN).await.unwrap().items, [["0", "a"], ["1", "b"]]);
            assert_eq!(read_value(&mut c, b"set", SHOWN).await.unwrap().items, [["m"]]);
            assert_eq!(read_value(&mut c, b"z", SHOWN).await.unwrap().items, [["m", "1.5"]]);
            assert_eq!(read_value(&mut c, b"h", SHOWN).await.unwrap().items, [["f", "v"]]);
            assert_eq!(read_value(&mut c, b"x", SHOWN).await.unwrap().items, [["1-1", "f", "v"]]);
            assert_eq!(read_value(&mut c, b"gone", SHOWN).await.unwrap().ttl_ms, -2);
        });
    }

    #[test]
    #[ignore = "needs a local Redis on 6379; empties database 14"]
    fn local_redis_edits_and_errors() {
        tauri::async_runtime::block_on(async {
            let mut c = local(14).await;
            c.call(&[b"RPUSH", b"l", b"a", b"b", b"c"]).await.unwrap();
            let remove = Edit::ListRemove { key: Key::of(b"l".to_vec()), index: 1 }.commands().unwrap();
            c.pipeline(&remove).await.unwrap();
            assert_eq!(read_value(&mut c, b"l", SHOWN).await.unwrap().items, [["0", "a"], ["1", "c"]]);
            let wrong = c.call(&[b"HGET", b"l", b"f"]).await.unwrap();
            assert!(matches!(wrong, Reply::Error(e) if e.starts_with("WRONGTYPE")));
            c.call(&[b"SET", b"other", b"x"]).await.unwrap();
            let rename = Edit::Rename { key: Key::of(b"l".to_vec()), to: "other".into() }.commands().unwrap();
            assert_eq!(c.pipeline(&rename).await.unwrap(), [Reply::Int(0)], "RENAMENX leaves an existing key alone");
            let big = vec![b'x'; 3 * 1024 * 1024];
            c.call(&[b"SET", b"big", &big]).await.unwrap();
            assert_eq!(read_value(&mut c, b"big", SHOWN).await.unwrap().size, 3 * 1024 * 1024);
        });
    }

    #[test]
    #[ignore = "needs a local Redis on 6379; empties database 12"]
    fn local_redis_values_should_show_more_items_when_asked() {
        tauri::async_runtime::block_on(async {
            let mut c = local(12).await;
            let mut commands = Vec::new();
            for i in 0..2500 {
                let n = i.to_string().into_bytes();
                commands.push(vec![b"RPUSH".to_vec(), b"l".to_vec(), n.clone()]);
                commands.push(vec![b"HSET".to_vec(), b"h".to_vec(), n.clone(), n.clone()]);
                commands.push(vec![b"SADD".to_vec(), b"set".to_vec(), n.clone()]);
                commands.push(vec![b"ZADD".to_vec(), b"z".to_vec(), n.clone(), n]);
            }
            c.pipeline(&commands).await.unwrap();
            for key in [&b"l"[..], b"h", b"set", b"z"] {
                let first = read_value(&mut c, key, SHOWN).await.unwrap();
                assert_eq!((first.items.len(), first.size), (SHOWN, 2500), "{}", String::from_utf8_lossy(key));
                let all = read_value(&mut c, key, 3000).await.unwrap();
                assert_eq!(all.items.len(), 2500, "{}", String::from_utf8_lossy(key));
            }
        });
    }

    #[test]
    #[ignore = "needs a local Redis on 6379; uses database 13"]
    fn local_redis_session_opens_from_a_saved_connection_and_tests() {
        tauri::async_runtime::block_on(async {
            let saved = crate::store::Saved {
                name: "cache".into(),
                kind: crate::db::Engine::Redis,
                url: "redis://127.0.0.1:6379/13".into(),
                tag: String::new(),
                safety: crate::safety::Safety::Silent,
                ssh: None,
                tls: crate::tls::Tls::default(),
                options: crate::db::Options::default(),
                password_mode: crate::store::PasswordMode::NoPassword,
                color: None,
                group: String::new(),
                flavor: None,
                needs_credentials: false,
            };
            let mut s = super::open_session(saved.clone(), None, None, None).await.unwrap();
            assert_eq!(s.db, 13);
            let info = super::info(&crate::Prefs::default(), &mut s).await.unwrap();
            assert!(info.server.starts_with("Redis ") || info.server.starts_with("Valkey "), "{}", info.server);
            let said = super::test(saved, None, None).await.unwrap();
            assert!(said.contains("database 13"), "{said}");
        });
    }
}

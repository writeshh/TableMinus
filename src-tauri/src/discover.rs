//! Finds database servers running on this computer, so the launcher can save a connection for each one. Nothing is
//! saved by [`discover_local`]: it only looks, and the UI asks before [`save_discovered`] writes anything.
//!
//! A port is identified by what the server says, not by its number (Docker publishes a Postgres on any port): MySQL
//! and MariaDB greet first, Redis answers `PING`, `MongoDB` a `hello`, Postgres an SSL request. Besides the usual
//! ports, it looks at the ports Docker publishes and those database servers (by process name) listen on. Each server found is then tried
//! with the usual default sign-ins, and one that refuses them is kept anyway, flagged [`Saved::needs_credentials`].

use crate::db::{Engine, Options};
use crate::error::{Error, Result};
use crate::safety::Safety;
use crate::store::{self, ConnectionsFile, Flavor, PasswordMode, Saved};
use crate::tls::Tls;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::time::Duration;
use tauri::{AppHandle, State};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::task::JoinSet;
use url::Url;

const HOST: &str = "127.0.0.1";
/// How long a local port gets to connect and answer; a server on this computer does both at once.
const PROBE_TIMEOUT: Duration = Duration::from_millis(400);
const DOCKER_TIMEOUT: Duration = Duration::from_secs(5);
/// Inclusive ranges: the usual ports, and the few above them that `DBngin` and a second Homebrew install move to.
const USUAL_PORTS: [(u16, u16); 7] =
    [(5432, 5440), (3306, 3310), (6379, 6382), (26257, 26258), (1433, 1434), (8123, 8124), (27017, 27019)];
/// Processes whose listening ports are worth a look whatever their number (`lsof`'s command names).
const SERVERS: [&str; 11] = [
    "postgres",
    "mysqld",
    "mariadbd",
    "redis-server",
    "valkey-server",
    "mongod",
    "clickhouse",
    "clickhouse-server",
    "sqlservr",
    "cockroach",
    "sqld",
];
const LSOF_TIMEOUT: Duration = Duration::from_secs(5);
const COCKROACH_PORT: u16 = 26257;
/// A busy Docker host publishes a lot; this many are plenty to look at.
const MAX_DOCKER_PORTS: usize = 100;
const MAX_SAVED_AT_ONCE: usize = 200;
/// `docker` isn't on the PATH of an app started from Finder, so the usual install places are tried too.
const DOCKER: [&str; 4] = [
    "docker",
    "/usr/local/bin/docker",
    "/opt/homebrew/bin/docker",
    "/Applications/Docker.app/Contents/Resources/bin/docker",
];
/// Postgres's request to start TLS (length 8, code 80877103); any Postgres server answers `S` or `N`.
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 4, 210, 22, 47];

/// A TDS pre-login message (the start of every `SQL Server` connection), which a server answers with a type 4 packet.
const TDS_PRELOGIN: [u8; 47] = [
    0x12, 0x01, 0x00, 0x2f, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x1a, 0x00, 0x06, 0x01, 0x00, 0x20, 0x00, 0x01, 0x02,
    0x00, 0x21, 0x00, 0x01, 0x03, 0x00, 0x22, 0x00, 0x04, 0x04, 0x00, 0x26, 0x00, 0x01, 0xff, 0x09, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// What speaks on a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
    Postgres,
    /// Whether the greeting says MariaDB.
    Mysql(bool),
    /// Whether it wants a password before answering.
    Redis(bool),
    SqlServer,
    /// Answered `GET /ping` as `ClickHouse` does.
    ClickHouse,
    /// Answered a `hello` in `MongoDB`'s wire protocol.
    Mongo,
}

/// A server found, ready to save under a free name.
#[derive(Debug, Serialize)]
pub struct Found {
    conn: Saved,
    /// What answered, or why the defaults didn't get in, for the question the UI asks.
    detail: String,
}

/// MySQL and MariaDB speak first: a packet (3 length bytes, sequence 0) of protocol 10 and the version as text, or
/// an error packet (`0xff`) from a server that refuses this host. Returns whether it says MariaDB.
fn mysql_greeting(bytes: &[u8]) -> Option<bool> {
    if *bytes.get(3)? != 0 {
        return None;
    }
    match *bytes.get(4)? {
        10 => {
            let version = bytes.get(5..)?.split(|b| *b == 0).next()?;
            Some(String::from_utf8_lossy(version).contains("MariaDB"))
        }
        0xff => Some(false),
        _ => None,
    }
}

/// Redis's reply to `PING`: `+PONG`, or an error that asks for a password. Returns whether it wants one.
fn redis_reply(bytes: &[u8]) -> Option<bool> {
    let text = String::from_utf8_lossy(bytes);
    if text.starts_with("+PONG") {
        return Some(false);
    }
    let wants = text.contains("NOAUTH") || text.contains("DENIED") || text.contains("Authentication");
    (text.starts_with('-') && wants).then_some(true)
}

/// A `MongoDB` `hello` as an `OP_MSG` (opcode 2013): a 16-byte header, no flags, and the command as one BSON body.
fn mongo_hello() -> Vec<u8> {
    let mut body = Vec::new();
    // Writing a document to a Vec can't fail; an empty message is only a probe that finds nothing.
    let _ = mongodb::bson::doc! { "hello": 1, "$db": "admin" }.to_writer(&mut body);
    let len = u32::try_from(16 + 4 + 1 + body.len()).unwrap_or(0);
    let mut msg = Vec::with_capacity(len as usize);
    for n in [len, 1, 0, 2013] {
        msg.extend_from_slice(&n.to_le_bytes());
    }
    msg.extend_from_slice(&0u32.to_le_bytes());
    msg.push(0);
    msg.extend(body);
    msg
}

/// Whether a reply is a `MongoDB` `OP_MSG` answering request 1.
fn mongo_reply(bytes: &[u8]) -> bool {
    let word = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    word(8) == Some(1) && word(12) == Some(2013)
}

/// Connects, sends `request` when there is one, and reads what arrives within the timeout. `None` when nothing is
/// listening; zero bytes when it listens and stays silent.
async fn exchange(port: u16, request: Option<&[u8]>, buf: &mut [u8]) -> Option<usize> {
    let mut stream = tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect((HOST, port))).await.ok()?.ok()?;
    if let Some(request) = request {
        tokio::time::timeout(PROBE_TIMEOUT, stream.write_all(request)).await.ok()?.ok()?;
    }
    Some(
        tokio::time::timeout(PROBE_TIMEOUT, stream.read(buf)).await.ok().and_then(std::result::Result::ok).unwrap_or(0),
    )
}

/// What database speaks on `port`, if any. One connection per question, since Postgres drops one that talks nonsense.
async fn probe(port: u16) -> Option<Wire> {
    let mut buf = [0u8; 512]; // ClickHouse's headers are long enough that its "Ok." comes after 128 bytes
    let n = exchange(port, None, &mut buf).await?;
    if n > 0 {
        return mysql_greeting(&buf[..n]).map(Wire::Mysql);
    }
    let n = exchange(port, Some(b"PING\r\n"), &mut buf).await?;
    if let Some(auth) = redis_reply(&buf[..n]) {
        return Some(Wire::Redis(auth));
    }
    let n = exchange(port, Some(b"GET /ping HTTP/1.0\r\n\r\n"), &mut buf).await?;
    let reply = String::from_utf8_lossy(&buf[..n]);
    if reply.starts_with("HTTP/")
        && reply.contains(" 200")
        && (reply.contains("X-ClickHouse") || reply.ends_with("Ok.\n"))
    {
        return Some(Wire::ClickHouse);
    }
    let n = exchange(port, Some(&mongo_hello()), &mut buf).await?;
    if mongo_reply(&buf[..n]) {
        return Some(Wire::Mongo);
    }
    let n = exchange(port, Some(&TDS_PRELOGIN), &mut buf).await?;
    if buf[..n].first() == Some(&0x04) {
        return Some(Wire::SqlServer);
    }
    let n = exchange(port, Some(&SSL_REQUEST), &mut buf).await?;
    matches!(buf[..n].first(), Some(b'S' | b'N')).then_some(Wire::Postgres)
}

/// `(host port, container name)` for each TCP port Docker publishes, from `docker ps`'s `Names` and `Ports`
/// columns, e.g. `0.0.0.0:5433->5432/tcp, :::5433->5432/tcp`.
fn parse_docker(out: &str) -> Vec<(u16, String)> {
    let mut found: Vec<(u16, String)> = vec![];
    for line in out.lines() {
        let Some((name, ports)) = line.split_once('\t') else { continue };
        for field in ports.split(", ") {
            let Some((host, _)) = field.strip_suffix("/tcp").and_then(|f| f.split_once("->")) else { continue };
            let Some(port) = host.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) else { continue };
            if !found.iter().any(|(p, _)| *p == port) {
                found.push((port, name.to_string()));
            }
        }
    }
    found.truncate(MAX_DOCKER_PORTS);
    found
}

/// The ports `lsof -F cn` lists for the known database servers: each process's `c` (command) line, then an `n`
/// line per address it listens on (`127.0.0.1:5433`, `*:3306`, `[::1]:27017`).
fn parse_lsof(out: &str) -> Vec<u16> {
    let mut ports = vec![];
    let mut server = false;
    for line in out.lines() {
        if let Some(command) = line.strip_prefix('c') {
            server = SERVERS.contains(&command);
        } else if let Some(address) = line.strip_prefix('n').filter(|_| server)
            && let Some(port) = address.rsplit(':').next().and_then(|p| p.parse().ok())
            && !ports.contains(&port)
        {
            ports.push(port);
        }
    }
    ports.truncate(MAX_DOCKER_PORTS);
    ports
}

/// The TCP ports database servers on this computer listen on, by `lsof`; none where it's missing (Windows).
async fn server_ports() -> Vec<u16> {
    let mut cmd = Command::new("lsof");
    cmd.args(["-nP", "-iTCP", "-sTCP:LISTEN", "+c", "0", "-F", "cn"]).stdin(Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(LSOF_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => parse_lsof(&String::from_utf8_lossy(&out.stdout)),
        _ => vec![],
    }
}

/// The ports Docker publishes; none when Docker isn't installed or its daemon isn't running.
async fn docker_ports() -> Vec<(u16, String)> {
    for bin in DOCKER {
        let mut cmd = Command::new(bin);
        cmd.args(["ps", "--format", "{{.Names}}\t{{.Ports}}"]).stdin(Stdio::null()).kill_on_drop(true);
        let Ok(run) = tokio::time::timeout(DOCKER_TIMEOUT, cmd.output()).await else { return vec![] };
        match run {
            Ok(out) if out.status.success() => return parse_docker(&String::from_utf8_lossy(&out.stdout)),
            Ok(_) => return vec![], // installed, but the daemon isn't running
            Err(_) => {}            // not at this path; try the next
        }
    }
    vec![]
}

fn label(kind: Engine, flavor: Option<Flavor>) -> &'static str {
    match (kind, flavor) {
        (_, Some(Flavor::Cockroach)) => "CockroachDB",
        (Engine::Postgres, None) => "PostgreSQL",
        (Engine::Mysql, _) => "MySQL",
        (Engine::Mariadb, _) => "MariaDB",
        (Engine::Redis, _) => "Redis",
        (Engine::Mongodb, _) => "MongoDB",
        (Engine::Dynamodb, _) => "DynamoDB",
        (Engine::Elasticsearch, _) => "Elasticsearch",
        (Engine::Cassandra, _) => "Cassandra",
        (Engine::Oracle, _) => "Oracle",
        (Engine::Sqlite, _) => "SQLite",
        (Engine::Duckdb, _) => "DuckDB",
        (Engine::Mssql, _) => "SQL Server",
        (Engine::Clickhouse, _) => "ClickHouse",
        (Engine::Libsql, _) => "libSQL",
        (Engine::D1, _) => "Cloudflare D1",
    }
}

fn url_for(scheme: &str, user: &str, port: u16) -> Result<String> {
    let mut url = Url::parse(&format!("{scheme}://{HOST}:{port}"))?;
    url.set_username(user).map_err(|()| Error::Invalid("that user name can't go in a URL"))?;
    Ok(url.to_string())
}

fn new_saved(kind: Engine, url: String) -> Saved {
    Saved {
        name: String::new(),
        kind,
        url,
        tag: "local".into(),
        safety: Safety::default(),
        ssh: None,
        tls: Tls::default(),
        options: Options::default(),
        password_mode: PasswordMode::Keychain,
        color: None,
        group: String::new(),
        flavor: None,
        needs_credentials: false,
    }
}

/// Names it for the server and where it was found: the Docker container's name, or the engine and port.
fn named(mut conn: Saved, port: u16, container: Option<&str>) -> Saved {
    conn.name = match container {
        Some(c) => format!("{c} (Docker)"),
        None => format!("{} · localhost:{port}", label(conn.kind, conn.flavor)),
    };
    conn
}

/// Users to try on a Postgres-protocol server: its usual superuser, then the OS user (Homebrew and Postgres.app make
/// that the superuser), then `root` (`CockroachDB`'s, tried first on its own port).
fn postgres_users(port: u16, os_user: Option<&str>) -> Vec<String> {
    let mut users: Vec<String> = if port == COCKROACH_PORT { vec!["root".into()] } else { vec![] };
    for u in ["postgres"].into_iter().chain(os_user).chain(["root"]) {
        if !u.is_empty() && !users.iter().any(|x| x == u) {
            users.push(u.to_string());
        }
    }
    users
}

/// Opens each of `users` without a password until one gets in. The server found is saved as the first user that did,
/// else as the first user, flagged as needing credentials, with the first refusal as the detail.
async fn try_sql(port: u16, kind: Engine, scheme: &str, users: &[String]) -> Result<(Saved, String)> {
    let mut refused = None;
    for user in users {
        let conn = new_saved(kind, url_for(scheme, user, port)?);
        match crate::open_db(&conn, None, None).await {
            Ok((mut db, _tunnel)) => {
                let version = db.server_version().await.unwrap_or_default();
                let mut conn = conn;
                if version.starts_with("CockroachDB") {
                    conn.flavor = Some(Flavor::Cockroach);
                } else if version.starts_with("MariaDB") {
                    conn.kind = Engine::Mariadb;
                }
                return Ok((conn, format!("{version}, signed in as {user}")));
            }
            Err(e) => {
                refused.get_or_insert(e.to_string());
            }
        }
    }
    let first = users.first().map_or("", String::as_str);
    let mut conn = new_saved(kind, url_for(scheme, first, port)?);
    conn.needs_credentials = true;
    if port == COCKROACH_PORT {
        conn.flavor = Some(Flavor::Cockroach);
    }
    Ok((conn, refused.unwrap_or_default()))
}

/// `MongoDB` without a user: one that wants a login refuses to list the collections, and is kept needing credentials.
async fn try_mongo(port: u16) -> Result<(Saved, String)> {
    let mut conn = new_saved(Engine::Mongodb, format!("mongodb://{HOST}:{port}/"));
    match crate::mongo::test(conn.clone(), None, None).await {
        Ok(said) => Ok((conn, said.trim_start_matches("Connected · ").to_string())),
        Err(e) => {
            conn.needs_credentials = true;
            Ok((conn, e.to_string()))
        }
    }
}

/// Redis with no user: it answered `PING` unasked, so the connection should work; one that wants a password isn't tried.
async fn try_redis(port: u16, wants_password: bool) -> Result<(Saved, String)> {
    let mut conn = new_saved(Engine::Redis, url_for("redis", "", port)?);
    let detail = if wants_password {
        "asks for a password".to_string()
    } else {
        match crate::redis::test(conn.clone(), None, None).await {
            Ok(said) => return Ok((conn, said.trim_start_matches("Connected · ").to_string())),
            Err(e) => e.to_string(),
        }
    };
    conn.needs_credentials = true;
    Ok((conn, detail))
}

async fn found_on(port: u16, container: Option<String>) -> Option<Found> {
    let wire = probe(port).await?;
    let os_user = std::env::var("USER").ok();
    let tried = match wire {
        Wire::Postgres => try_sql(port, Engine::Postgres, "postgres", &postgres_users(port, os_user.as_deref())).await,
        Wire::Mysql(maria) => {
            let kind = if maria { Engine::Mariadb } else { Engine::Mysql };
            try_sql(port, kind, "mysql", &["root".to_string()]).await
        }
        Wire::Redis(auth) => try_redis(port, auth).await,
        Wire::SqlServer => try_sql(port, Engine::Mssql, "mssql", &["sa".to_string()]).await,
        Wire::ClickHouse => try_sql(port, Engine::Clickhouse, "clickhouse", &["default".to_string()]).await,
        Wire::Mongo => try_mongo(port).await,
    };
    let (conn, detail) = tried.ok()?;
    Some(Found { conn: named(conn, port, container.as_deref()), detail })
}

/// The port a saved connection reaches on this computer, when it does; SQLite files, sockets and tunnelled servers
/// don't.
fn local_port(c: &Saved) -> Option<u16> {
    if c.ssh.is_some() || !c.options.socket.is_empty() {
        return None;
    }
    let url = Url::parse(&c.url).ok()?;
    if !matches!(url.host_str()?, "127.0.0.1" | "localhost" | "[::1]" | "0.0.0.0") {
        return None;
    }
    url.port().or(match c.kind {
        Engine::Postgres => Some(5432),
        Engine::Mysql | Engine::Mariadb => Some(3306),
        Engine::Redis => Some(6379),
        Engine::Mongodb => Some(27017),
        Engine::Dynamodb => Some(8000),
        Engine::Elasticsearch => Some(9200),
        Engine::Cassandra => Some(9042),
        Engine::Oracle => Some(1521),
        Engine::Mssql => Some(1433),
        Engine::Clickhouse => Some(8123),
        Engine::Libsql => Some(8080),
        Engine::Sqlite | Engine::Duckdb | Engine::D1 => None,
    })
}

/// Whether `existing` already has a connection to the same kind of server on the same local port, or the same socket.
fn already_saved(existing: &[Saved], found: &Saved) -> bool {
    if !found.options.socket.is_empty() {
        return existing.iter().any(|c| c.options.socket == found.options.socket);
    }
    let same_kind = |a: Engine, b: Engine| {
        a == b || matches!((a, b), (Engine::Mysql, Engine::Mariadb) | (Engine::Mariadb, Engine::Mysql))
    };
    existing
        .iter()
        .any(|c| same_kind(c.kind, found.kind) && local_port(c).is_some() && local_port(c) == local_port(found))
}

/// Where MySQL and MariaDB put their socket: Homebrew, the distributions' packages, and a bare `mysqld`.
const MYSQL_SOCKETS: [&str; 6] = [
    "/tmp/mysql.sock",
    "/var/run/mysqld/mysqld.sock",
    "/run/mysqld/mysqld.sock",
    "/var/lib/mysql/mysql.sock",
    "/opt/homebrew/var/mysql/mysql.sock",
    "/usr/local/var/mysql/mysql.sock",
];
const MAX_SOCKETS: usize = 20;

/// The MySQL sockets that exist: the usual places, and `DBngin`'s `/tmp/mysql_<port>.sock`.
#[cfg(unix)]
fn mysql_sockets() -> Vec<String> {
    use std::os::unix::fs::FileTypeExt;
    let is_socket = |p: &str| std::fs::metadata(p).is_ok_and(|m| m.file_type().is_socket());
    let mut found: Vec<String> = MYSQL_SOCKETS.iter().copied().filter(|p| is_socket(p)).map(String::from).collect();
    if let Ok(dir) = std::fs::read_dir("/tmp") {
        for entry in dir.flatten().take(10_000) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = format!("/tmp/{name}");
            if name.starts_with("mysql_")
                && std::path::Path::new(&name).extension().is_some_and(|e| e.eq_ignore_ascii_case("sock"))
                && !found.contains(&path)
                && is_socket(&path)
            {
                found.push(path);
            }
        }
    }
    found.truncate(MAX_SOCKETS);
    found
}

#[cfg(not(unix))]
fn mysql_sockets() -> Vec<String> {
    vec![]
}

/// Whether a MySQL server answers on the socket (`Some(true)`: MariaDB), by its greeting.
#[cfg(unix)]
async fn socket_greeting(path: &str) -> Option<bool> {
    let mut stream = tokio::time::timeout(PROBE_TIMEOUT, tokio::net::UnixStream::connect(path)).await.ok()?.ok()?;
    let mut buf = [0u8; 128];
    let n = tokio::time::timeout(PROBE_TIMEOUT, stream.read(&mut buf)).await.ok()?.ok()?;
    mysql_greeting(&buf[..n])
}

#[cfg(not(unix))]
async fn socket_greeting(_path: &str) -> Option<bool> {
    None
}

/// A MySQL server on a socket, tried as `root` with no password, and the TCP port it also listens on (`Some(0)`
/// when it has none; `None` when it wouldn't let `root` in, so it can't say).
async fn found_on_socket(path: String) -> Option<(Found, Option<u16>)> {
    let kind = if socket_greeting(&path).await? { Engine::Mariadb } else { Engine::Mysql };
    let mut conn = new_saved(kind, "mysql://root@localhost".into());
    conn.options.socket.clone_from(&path);
    let (conn, detail, port) = match crate::open_db(&conn, None, None).await {
        Ok((mut db, _tunnel)) => {
            let version = db.server_version().await.unwrap_or_default();
            let port =
                db.query("select @@port").await.ok().and_then(|r| r.rows.first()?.first()?.as_ref()?.parse().ok());
            if version.starts_with("MariaDB") {
                conn.kind = Engine::Mariadb;
            }
            (conn, format!("{version}, signed in as root"), port)
        }
        Err(e) => {
            conn.needs_credentials = true;
            (conn, e.to_string(), None)
        }
    };
    let name = format!("{} · {path}", label(conn.kind, conn.flavor));
    Some((Found { conn: Saved { name, ..conn }, detail }, port))
}

/// Postgres's sockets (`.s.PGSQL.<port>`) in the folders servers put them: Homebrew, Postgres.app and `DBngin` use
/// `/tmp`, the distributions' packages `/var/run/postgresql`.
#[cfg(unix)]
fn postgres_sockets() -> Vec<(String, u16)> {
    use std::os::unix::fs::FileTypeExt;
    let mut found = vec![];
    for dir in ["/tmp", "/var/run/postgresql", "/run/postgresql"] {
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for entry in entries.flatten().take(10_000) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(port) = name.strip_prefix(".s.PGSQL.").and_then(|p| p.parse::<u16>().ok()) else { continue };
            let path = format!("{dir}/{name}");
            if std::fs::metadata(&path).is_ok_and(|m| m.file_type().is_socket())
                && !found.iter().any(|(_, p)| *p == port)
            {
                found.push((path, port));
            }
        }
    }
    found.truncate(MAX_SOCKETS);
    found
}

#[cfg(not(unix))]
fn postgres_sockets() -> Vec<(String, u16)> {
    vec![]
}

/// A Postgres server on a socket, tried with the usual users and no password.
async fn found_on_pg_socket(path: String, port: u16) -> Option<Found> {
    let os_user = std::env::var("USER").ok();
    let mut refused = None;
    let users = postgres_users(port, os_user.as_deref());
    for user in &users {
        let mut conn = new_saved(Engine::Postgres, url_for("postgres", user, port).ok()?);
        conn.options.socket.clone_from(&path);
        match crate::open_db(&conn, None, None).await {
            Ok((mut db, _tunnel)) => {
                let version = db.server_version().await.unwrap_or_default();
                let name = format!("PostgreSQL · {path}");
                return Some(Found { conn: Saved { name, ..conn }, detail: format!("{version}, signed in as {user}") });
            }
            Err(e) => {
                refused.get_or_insert(e.to_string());
            }
        }
    }
    let mut conn = new_saved(Engine::Postgres, url_for("postgres", users.first()?, port).ok()?);
    conn.options.socket.clone_from(&path);
    conn.needs_credentials = true;
    Some(Found { conn: Saved { name: format!("PostgreSQL · {path}"), ..conn }, detail: refused.unwrap_or_default() })
}

/// Whether a server found on a socket is one the port probes found already. The server says which TCP port it also
/// listens on; one that wouldn't say is taken for a duplicate when any MySQL was found on a port, since the sockets
/// of a server that only listens on its socket are the ones worth adding.
fn socket_duplicates_a_port(found: &[Found], port: Option<u16>) -> bool {
    let mysql = |f: &&Found| matches!(f.conn.kind, Engine::Mysql | Engine::Mariadb);
    match port {
        Some(0) => false,
        Some(p) => found.iter().filter(mysql).any(|f| local_port(&f.conn) == Some(p)),
        None => found.iter().any(|f| mysql(&f)),
    }
}

/// Looks for database servers on this computer that aren't saved yet: the usual ports, the ports Docker publishes,
/// and MySQL's sockets. Doesn't save anything.
#[tauri::command]
pub async fn discover_local(app: AppHandle, open: State<'_, crate::Open>) -> Result<Vec<Found>> {
    open.unlocked()?;
    let existing = store::load(&crate::dir(&app)?)?;
    let mut ports: BTreeMap<u16, Option<String>> =
        USUAL_PORTS.iter().flat_map(|&(a, b)| a..=b).map(|p| (p, None)).collect();
    for port in server_ports().await {
        ports.entry(port).or_insert(None);
    }
    for (port, container) in docker_ports().await {
        ports.insert(port, Some(container));
    }
    let mut tasks = JoinSet::new();
    for (port, container) in ports {
        tasks.spawn(found_on(port, container));
    }
    let mut found = vec![];
    while let Some(done) = tasks.join_next().await {
        // A probe that panicked found nothing; the rest still count.
        if let Ok(Some(f)) = done {
            found.push(f);
        }
    }
    // Judged against everything the ports found, saved already or not, so a saved server's socket isn't offered.
    let mut sockets = JoinSet::new();
    for path in mysql_sockets() {
        sockets.spawn(found_on_socket(path));
    }
    let mut on_sockets = vec![];
    while let Some(done) = sockets.join_next().await {
        if let Ok(Some((f, port))) = done
            && !socket_duplicates_a_port(&found, port)
        {
            on_sockets.push(f);
        }
    }
    // A Postgres socket's name gives its port: one a port probe found Postgres on is the same server.
    let mut pg = JoinSet::new();
    for (path, port) in postgres_sockets() {
        let on_tcp = found.iter().any(|f| f.conn.kind == Engine::Postgres && local_port(&f.conn) == Some(port));
        if !on_tcp {
            pg.spawn(found_on_pg_socket(path, port));
        }
    }
    while let Some(done) = pg.join_next().await {
        if let Ok(Some(f)) = done {
            on_sockets.push(f);
        }
    }
    found.extend(on_sockets);
    found.retain(|f| !already_saved(&existing, &f.conn));
    found.sort_by_key(|f| (local_port(&f.conn).is_none(), local_port(&f.conn)));
    Ok(found)
}

/// Saves what [`discover_local`] found and the user agreed to, each under a name no connection has (an existing one
/// is never replaced). Returns how many were saved.
#[tauri::command]
pub async fn save_discovered(app: AppHandle, found: Vec<Saved>) -> Result<usize> {
    if found.len() > MAX_SAVED_AT_ONCE {
        return Err(Error::Invalid("too many connections to save at once"));
    }
    let dir = crate::dir(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        store::add_imported(&dir, ConnectionsFile { connections: found, ..ConnectionsFile::default() }, &HashMap::new())
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::{
        Found, SSL_REQUEST, Wire, already_saved, local_port, mongo_hello, mongo_reply, mysql_greeting, new_saved,
        parse_docker, parse_lsof, postgres_users, probe, redis_reply,
    };
    use crate::db::Engine;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn greeting(version: &str) -> Vec<u8> {
        let mut p = vec![0, 0, 0, 0, 10];
        p.extend(version.as_bytes());
        p.push(0);
        p
    }

    /// A server on a free local port that sends `first` on connect, then answers one request with `reply(request)`.
    fn fake_server(first: Vec<u8>, reply: fn(&[u8]) -> Vec<u8>) -> u16 {
        let listener = tauri::async_runtime::block_on(TcpListener::bind(("127.0.0.1", 0))).unwrap();
        let port = listener.local_addr().unwrap().port();
        tauri::async_runtime::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let first = first.clone();
                tauri::async_runtime::spawn(async move {
                    if !first.is_empty() {
                        let _ = s.write_all(&first).await; // the probe may have hung up already
                        return;
                    }
                    let mut buf = [0u8; 16];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let _ = s.write_all(&reply(&buf[..n])).await; // as above
                });
            }
        });
        port
    }

    #[test]
    fn mysql_greeting_tells_mysql_from_mariadb_and_rejects_the_rest() {
        assert_eq!(mysql_greeting(&greeting("8.0.36")), Some(false));
        assert_eq!(mysql_greeting(&greeting("5.5.5-10.11.6-MariaDB-1:10.11.6")), Some(true));
        assert_eq!(mysql_greeting(&[0, 0, 0, 0, 0xff, 1, 2]), Some(false), "an error packet is still MySQL");
        assert_eq!(mysql_greeting(&[0, 0, 0, 1, 10, b'8', 0]), None, "not the first packet");
        assert_eq!(mysql_greeting(b"HTTP/1.1 400 Bad Request"), None);
        assert_eq!(mysql_greeting(&[0, 0]), None);
    }

    #[test]
    fn redis_reply_tells_open_from_password_protected_and_rejects_the_rest() {
        assert_eq!(redis_reply(b"+PONG\r\n"), Some(false));
        assert_eq!(redis_reply(b"-NOAUTH Authentication required.\r\n"), Some(true));
        assert_eq!(redis_reply(b"-DENIED Redis is running in protected mode\r\n"), Some(true));
        assert_eq!(redis_reply(b"-ERR unknown command\r\n"), None);
        assert_eq!(redis_reply(b""), None);
    }

    #[test]
    fn probe_should_name_each_protocol_by_what_it_says() {
        let mysql = fake_server(greeting("11.4.2-MariaDB"), |_| vec![]);
        let redis =
            fake_server(vec![], |req| if req == b"PING\r\n" { b"+PONG\r\n".to_vec() } else { b"-ERR\r\n".to_vec() });
        let pg = fake_server(vec![], |req| if req == SSL_REQUEST { b"N".to_vec() } else { vec![] });
        let sql_server =
            fake_server(vec![], |req| if req.first() == Some(&0x12) { vec![0x04, 0x01, 0x00, 0x20] } else { vec![] });
        let clickhouse = fake_server(vec![], |req| {
            if req.starts_with(b"GET /ping") { b"HTTP/1.0 200 OK\r\n\r\nOk.\n".to_vec() } else { vec![] }
        });
        let other = fake_server(vec![], |_| b"HTTP/1.1 400 Bad Request\r\n".to_vec());
        let results = tauri::async_runtime::block_on(async {
            (
                probe(mysql).await,
                probe(redis).await,
                probe(pg).await,
                probe(sql_server).await,
                probe(clickhouse).await,
                probe(other).await,
            )
        });
        assert_eq!(
            results,
            (
                Some(Wire::Mysql(true)),
                Some(Wire::Redis(false)),
                Some(Wire::Postgres),
                Some(Wire::SqlServer),
                Some(Wire::ClickHouse),
                None
            )
        );
    }

    #[test]
    #[ignore = "needs servers on the usual local ports, such as a Postgres on 5432"]
    fn local_servers_should_be_found_and_tried_with_the_defaults() {
        let found = tauri::async_runtime::block_on(async {
            let mut found = vec![];
            for port in [5432, 3306, 6379, 26257, 1433, 8123] {
                found.extend(super::found_on(port, None).await);
            }
            found
        });
        assert!(!found.is_empty(), "no server answered on 5432, 3306, 6379, 26257 or 1433");
        for f in &found {
            println!(
                "{} · {} · needs credentials: {} · {}",
                f.conn.name, f.conn.url, f.conn.needs_credentials, f.detail
            );
        }
    }

    #[test]
    fn probe_should_find_nothing_on_a_closed_port() {
        let port = {
            let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            l.local_addr().unwrap().port()
        };
        assert_eq!(tauri::async_runtime::block_on(probe(port)), None);
    }

    #[test]
    fn parse_docker_should_keep_each_published_tcp_port_once_with_its_container() {
        let out = "pg\t0.0.0.0:5433->5432/tcp, :::5433->5432/tcp\n\
                   web\t0.0.0.0:8000-8002->8000-8002/tcp, 80/tcp\n\
                   cache\t127.0.0.1:6380->6379/tcp\n\
                   dns\t0.0.0.0:5353->53/udp\n\
                   junk without a tab\n";
        assert_eq!(parse_docker(out), [(5433, "pg".to_string()), (6380, "cache".to_string())]);
    }

    #[test]
    fn postgres_users_should_try_the_usual_superuser_the_os_user_and_root_once_each() {
        assert_eq!(postgres_users(5432, Some("ritesh")), ["postgres", "ritesh", "root"]);
        assert_eq!(postgres_users(5432, Some("postgres")), ["postgres", "root"]);
        assert_eq!(postgres_users(26257, None), ["root", "postgres"]);
        assert_eq!(postgres_users(5432, Some("")), ["postgres", "root"]);
    }

    #[test]
    fn already_saved_should_match_the_same_kind_of_server_on_the_same_local_port() {
        let at = |kind, url: &str| new_saved(kind, url.to_string());
        let existing = [
            at(Engine::Postgres, "postgres://u@localhost/db"),
            at(Engine::Mysql, "mysql://root@127.0.0.1:3307"),
            at(Engine::Redis, "redis://cache.example.com:6379"),
            at(Engine::Sqlite, "/tmp/x.db"),
        ];
        assert!(already_saved(&existing, &at(Engine::Postgres, "postgres://postgres@127.0.0.1:5432/")), "default port");
        assert!(
            already_saved(&existing, &at(Engine::Mariadb, "mysql://root@127.0.0.1:3307/")),
            "MariaDB is MySQL's kind"
        );
        assert!(!already_saved(&existing, &at(Engine::Postgres, "postgres://postgres@127.0.0.1:5433/")));
        assert!(
            !already_saved(&existing, &at(Engine::Redis, "redis://127.0.0.1:6379/")),
            "a remote host isn't this one"
        );
        assert!(!already_saved(&existing, &at(Engine::Mysql, "mysql://root@127.0.0.1:3306/")));
    }

    #[test]
    fn already_saved_should_match_a_socket_by_its_path_only() {
        let mut saved = new_saved(Engine::Mysql, "mysql://root@localhost".into());
        saved.options.socket = "/tmp/mysql.sock".into();
        let mut found = saved.clone();
        assert!(already_saved(&[saved.clone()], &found));
        found.options.socket = "/tmp/mysql_3307.sock".into();
        assert!(!already_saved(&[saved], &found));
        let on_port = new_saved(Engine::Mysql, "mysql://root@127.0.0.1:3306".into());
        assert!(!already_saved(&[on_port], &found), "a port isn't a socket");
    }

    #[test]
    fn a_socket_should_be_dropped_when_its_server_was_found_on_a_port() {
        let found = |port: u16| Found {
            conn: new_saved(Engine::Mysql, format!("mysql://root@127.0.0.1:{port}")),
            detail: String::new(),
        };
        let ports = [found(3306)];
        assert!(super::socket_duplicates_a_port(&ports, Some(3306)), "it listens on 3306 too");
        assert!(!super::socket_duplicates_a_port(&ports, Some(3307)), "another server");
        assert!(!super::socket_duplicates_a_port(&ports, Some(0)), "a socket-only server is new");
        assert!(super::socket_duplicates_a_port(&ports, None), "can't say, and a MySQL has been found");
        assert!(!super::socket_duplicates_a_port(&[], None), "nothing else found: it may be the only one");
    }

    #[cfg(unix)]
    #[test]
    fn socket_greeting_should_read_a_mysql_greeting_from_a_socket() {
        let path = std::env::temp_dir().join(format!("tableminus-{}-greeting.sock", std::process::id()));
        let _ = std::fs::remove_file(&path); // a leftover from an earlier run, or nothing
        let greeting = tauri::async_runtime::block_on(async {
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            tauri::async_runtime::spawn(async move {
                if let Ok((mut s, _)) = listener.accept().await {
                    let _ = s.write_all(&greeting("10.11.6-MariaDB")).await; // the probe may have hung up already
                }
            });
            super::socket_greeting(path.to_str().unwrap()).await
        });
        let _ = std::fs::remove_file(&path); // best effort tidy-up
        assert_eq!(greeting, Some(true));
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "needs a local MySQL with a socket at /tmp/mysql_3306.sock (DBngin) or /tmp/mysql.sock"]
    fn local_mysql_socket_should_be_found_and_tried_as_root() {
        let path = super::mysql_sockets().into_iter().next().expect("no MySQL socket found");
        let (found, port) = tauri::async_runtime::block_on(super::found_on_socket(path)).unwrap();
        println!(
            "{} · needs credentials: {} · {} · server port {port:?}",
            found.conn.name, found.conn.needs_credentials, found.detail
        );
        assert!(!found.conn.needs_credentials);
    }

    #[test]
    fn local_port_ignores_tunnelled_connections() {
        let mut c = new_saved(Engine::Postgres, "postgres://u@127.0.0.1:5432".into());
        assert_eq!(local_port(&c), Some(5432));
        c.ssh = Some(crate::ssh::Ssh { host: "h".into(), port: None, user: String::new(), key: String::new() });
        assert_eq!(local_port(&c), None);
    }

    #[test]
    fn parse_lsof_should_keep_the_ports_of_database_servers_only() {
        let out = "p1\ncControlCenter\nf9\nn*:7000\np2\ncpostgres\nf5\nn127.0.0.1:5499\nf6\nn[::1]:5499\n\
                   p3\ncmongod\nf9\nn127.0.0.1:27030\n";
        assert_eq!(parse_lsof(out), [5499, 27030]);
    }

    #[test]
    fn mongo_reply_should_take_an_op_msg_answering_the_hello() {
        let hello = mongo_hello();
        assert_eq!(u32::from_le_bytes(hello[0..4].try_into().unwrap()) as usize, hello.len());
        let mut reply = vec![0u8; 16];
        reply[8..12].copy_from_slice(&1u32.to_le_bytes());
        reply[12..16].copy_from_slice(&2013u32.to_le_bytes());
        assert!(mongo_reply(&reply));
        assert!(!mongo_reply(b"+PONG\r\n"));
    }

    #[test]
    #[ignore = "needs a MongoDB on 127.0.0.1:27018 (scripts/test-env.sh up)"]
    fn local_probe_should_recognise_mongodb() {
        tauri::async_runtime::block_on(async {
            assert_eq!(probe(27018).await, Some(Wire::Mongo));
        });
    }

    #[test]
    #[ignore = "needs a local Postgres listening on /tmp/.s.PGSQL.5432 that takes postgres or the OS user"]
    fn local_postgres_should_connect_through_its_socket() {
        tauri::async_runtime::block_on(async {
            let f = super::found_on_pg_socket("/tmp/.s.PGSQL.5432".into(), 5432).await.unwrap();
            assert!(!f.conn.needs_credentials, "{}", f.detail);
            assert!(f.detail.starts_with("PostgreSQL"), "{}", f.detail);
        });
    }
}

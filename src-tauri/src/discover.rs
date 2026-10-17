//! Finds database servers running on this computer, so the launcher can save a connection for each one. Nothing is
//! saved by [`discover_local`]: it only looks, and the UI asks before [`save_discovered`] writes anything.
//!
//! A port is identified by what the server says, not by its number (Docker publishes a Postgres on any port): MySQL
//! and MariaDB greet first, Redis answers `PING`, Postgres answers an SSL request. Each server found is then tried
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
const USUAL_PORTS: [(u16, u16); 4] = [(5432, 5440), (3306, 3310), (6379, 6382), (26257, 26258)];
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

/// What speaks on a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
    Postgres,
    /// Whether the greeting says MariaDB.
    Mysql(bool),
    /// Whether it wants a password before answering.
    Redis(bool),
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
    let mut buf = [0u8; 128];
    let n = exchange(port, None, &mut buf).await?;
    if n > 0 {
        return mysql_greeting(&buf[..n]).map(Wire::Mysql);
    }
    let n = exchange(port, Some(b"PING\r\n"), &mut buf).await?;
    if let Some(auth) = redis_reply(&buf[..n]) {
        return Some(Wire::Redis(auth));
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
        (Engine::Sqlite, _) => "SQLite",
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
    };
    let (conn, detail) = tried.ok()?;
    Some(Found { conn: named(conn, port, container.as_deref()), detail })
}

/// The port a saved connection reaches on this computer, when it does; SQLite files and tunnelled servers don't.
fn local_port(c: &Saved) -> Option<u16> {
    if c.ssh.is_some() {
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
        Engine::Sqlite => None,
    })
}

/// Whether `existing` already has a connection to the same kind of server on the same local port.
fn already_saved(existing: &[Saved], found: &Saved) -> bool {
    let same_kind = |a: Engine, b: Engine| {
        a == b || matches!((a, b), (Engine::Mysql, Engine::Mariadb) | (Engine::Mariadb, Engine::Mysql))
    };
    existing
        .iter()
        .any(|c| same_kind(c.kind, found.kind) && local_port(c).is_some() && local_port(c) == local_port(found))
}

/// Looks for database servers on this computer that aren't saved yet: the usual ports and the ports Docker publishes.
/// Doesn't save anything.
#[tauri::command]
pub async fn discover_local(app: AppHandle, open: State<'_, crate::Open>) -> Result<Vec<Found>> {
    open.unlocked()?;
    let existing = store::load(&crate::dir(&app)?)?;
    let mut ports: BTreeMap<u16, Option<String>> =
        USUAL_PORTS.iter().flat_map(|&(a, b)| a..=b).map(|p| (p, None)).collect();
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
        if let Ok(Some(f)) = done
            && !already_saved(&existing, &f.conn)
        {
            found.push(f);
        }
    }
    found.sort_by_key(|f| local_port(&f.conn));
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
        SSL_REQUEST, Wire, already_saved, local_port, mysql_greeting, new_saved, parse_docker, postgres_users, probe,
        redis_reply,
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
        let other = fake_server(vec![], |_| b"HTTP/1.1 400 Bad Request\r\n".to_vec());
        let results = tauri::async_runtime::block_on(async {
            (probe(mysql).await, probe(redis).await, probe(pg).await, probe(other).await)
        });
        assert_eq!(results, (Some(Wire::Mysql(true)), Some(Wire::Redis(false)), Some(Wire::Postgres), None));
    }

    #[test]
    #[ignore = "needs servers on the usual local ports, such as a Postgres on 5432"]
    fn local_servers_should_be_found_and_tried_with_the_defaults() {
        let found = tauri::async_runtime::block_on(async {
            let mut found = vec![];
            for port in [5432, 3306, 6379, 26257] {
                found.extend(super::found_on(port, None).await);
            }
            found
        });
        assert!(!found.is_empty(), "no server answered on 5432, 3306, 6379 or 26257");
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
    fn local_port_ignores_tunnelled_connections() {
        let mut c = new_saved(Engine::Postgres, "postgres://u@127.0.0.1:5432".into());
        assert_eq!(local_port(&c), Some(5432));
        c.ssh = Some(crate::ssh::Ssh { host: "h".into(), port: None, user: String::new(), key: String::new() });
        assert_eq!(local_port(&c), None);
    }
}

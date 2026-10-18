//! Backup and restore. Postgres and MySQL/MariaDB run the server's own client tools (`pg_dump`, `pg_restore`,
//! `psql`, `mysqldump`, `mysql`) as child processes; SQLite copies pages with its online backup API. Each job
//! runs on its own thread, reports `backup-progress` events and ends with one `backup-done` event.

use crate::db::{CONNECT_TIMEOUT, Engine};
use crate::error::{Error, Result};
use crate::ssh::expand_home;
use crate::store::Saved;
use crate::tls::TlsMode;
use flate2::{Compression, bufread::MultiGzDecoder, write::GzEncoder};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

/// The longest custom-flags string accepted.
const MAX_FLAGS: usize = 4096;
/// Error lines kept from a tool's stderr for the final message.
const KEPT_LINES: usize = 12;

// ---------- Finding the client tools ----------

#[derive(Debug, Clone, Copy)]
enum Role {
    Dump,
    /// Restores `pg_dump`'s archive formats.
    Archive,
    /// Runs a SQL script.
    Script,
}

fn names(engine: Engine, role: Role) -> &'static [&'static str] {
    match (engine, role) {
        (Engine::Postgres, Role::Dump) => &["pg_dump"],
        (Engine::Postgres, Role::Archive) => &["pg_restore"],
        (Engine::Postgres, Role::Script) => &["psql"],
        (_, Role::Dump | Role::Archive) => &["mariadb-dump", "mysqldump"],
        (_, Role::Script) => &["mariadb", "mysql"],
    }
}

/// A folder holding one version of a server's client tools.
#[derive(Debug, Clone, Serialize)]
pub struct Tools {
    pub dir: PathBuf,
    /// The dump tool's version, e.g. "17.0" or "8.0.33"; blank for a folder the user chose.
    pub version: String,
    pub mariadb: bool,
}

impl Tools {
    /// A folder the user chose, or the folder of a tool they chose.
    fn chosen(path: &Path) -> Tools {
        let dir = if path.is_file() { path.parent().unwrap_or(path) } else { path };
        let mariadb = ["mariadb", "mariadb-dump"].iter().any(|n| dir.join(n).is_file());
        Tools { dir: dir.to_path_buf(), version: String::new(), mariadb }
    }

    fn exe(&self, engine: Engine, role: Role) -> Result<PathBuf> {
        let names = names(engine, role);
        names
            .iter()
            .map(|n| self.dir.join(n))
            .find(|p| p.is_file())
            .ok_or_else(|| Error::Tool(format!("{} isn't in {}", names.join(" or "), self.dir.display())))
    }
}

/// Where client tools usually live: PATH (short for an app started from the Finder), Homebrew, Postgres.app,
/// `DBngin`, and the system and MySQL installer folders. Folders named per version are listed one level deep.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/local/mysql/bin", "/usr/bin"].map(PathBuf::from));
    for parent in [
        "/opt/homebrew/opt",
        "/usr/local/opt",
        "/Applications/Postgres.app/Contents/Versions",
        "/Users/Shared/DBngin/postgresql",
        "/Users/Shared/DBngin/mysql",
        "/Users/Shared/DBngin/mariadb",
        "/usr/lib/postgresql",
    ] {
        if let Ok(entries) = std::fs::read_dir(parent) {
            dirs.extend(entries.flatten().map(|e| e.path().join("bin")));
        }
    }
    dirs
}

/// Every installed version of the engine's dump tool, oldest first.
async fn discover(engine: Engine) -> Vec<Tools> {
    let mut seen = HashSet::new();
    let mut found = vec![];
    for dir in search_dirs() {
        let Some(exe) = names(engine, Role::Dump).iter().map(|n| dir.join(n)).find(|p| p.is_file()) else {
            continue;
        };
        // The same binary is often linked from several folders (Homebrew's bin and opt, PATH and the list).
        if !seen.insert(std::fs::canonicalize(&exe).unwrap_or_else(|_| exe.clone())) {
            continue;
        }
        if let Some((version, mariadb)) = version_of(&exe).await {
            found.push(Tools { dir, version, mariadb });
        }
    }
    found.sort_by_key(|t| version_key(engine, &t.version));
    found
}

async fn version_of(exe: &Path) -> Option<(String, bool)> {
    let run = tokio::process::Command::new(exe).arg("--version").kill_on_drop(true).output();
    let out = tokio::time::timeout(Duration::from_secs(5), run).await.ok()?.ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    Some((parse_version(&text)?, text.contains("MariaDB")))
}

/// The version in a tool's `--version` text or a server's version string: `pg_dump (PostgreSQL) 17.0`,
/// `mysqldump  Ver 10.19 Distrib 10.11.6-MariaDB` (the Distrib one counts), `MariaDB 11.4.2`.
fn parse_version(text: &str) -> Option<String> {
    let after = text.split_once("Distrib ").map_or(text, |(_, rest)| rest);
    let word = after.split_whitespace().find(|w| w.starts_with(|c: char| c.is_ascii_digit()))?;
    let v: String = word.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    Some(v.trim_end_matches('.').to_string())
}

/// The part of a version that has to match: the major version, plus the minor one for Postgres before 10.
fn version_key(engine: Engine, version: &str) -> (u32, u32) {
    let mut parts = version.split('.').map(|p| p.parse().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = if engine == Engine::Postgres && major < 10 { parts.next().unwrap_or(0) } else { 0 };
    (major, minor)
}

/// Which of `found` (oldest first) to use with `server`, as [`Conn::server_version`] names it: the same flavour
/// (MySQL or MariaDB) and the oldest version that isn't older than the server. `pg_dump` refuses servers newer
/// than itself, and newer tools write SQL an older server may reject.
fn pick(engine: Engine, server: &str, found: &[Tools]) -> Result<usize> {
    let mariadb = server.starts_with("MariaDB");
    let want = parse_version(server).map(|v| version_key(engine, &v)).unwrap_or_default();
    found.iter().position(|t| t.mariadb == mariadb && version_key(engine, &t.version) >= want).ok_or_else(|| {
        let versions: Vec<&str> = found.iter().map(|t| t.version.as_str()).collect();
        Error::NoTools {
            tool: names(engine, Role::Dump).last().copied().unwrap_or_default(),
            server: server.to_string(),
            found: if versions.is_empty() { String::new() } else { format!(" (found {})", versions.join(", ")) },
        }
    })
}

// ---------- Command lines ----------

/// libpq's quoting for a connection-string value.
fn conninfo_value(v: &str) -> String {
    format!("'{}'", v.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The libpq connection string for the client tools, mirroring how the app connects: through a tunnel it dials
/// 127.0.0.1 and the tunnel's port (`hostaddr`) but keeps `host`, so verify-full still checks the real name.
fn pg_conninfo(saved: &Saved, tunnel_port: Option<u16>, database: &str) -> Result<String> {
    use tokio_postgres::config::Host;
    let cfg: tokio_postgres::Config = saved.url.parse()?;
    let mut kv: Vec<(&str, String)> = vec![];
    match cfg.get_hosts().first() {
        Some(Host::Tcp(h)) => kv.push(("host", h.clone())),
        #[cfg(unix)]
        Some(Host::Unix(p)) => kv.push(("host", p.display().to_string())),
        None => {}
    }
    match tunnel_port {
        Some(port) => kv.extend([("hostaddr", "127.0.0.1".into()), ("port", port.to_string())]),
        None => kv.extend(cfg.get_ports().first().map(|p| ("port", p.to_string()))),
    }
    kv.extend(cfg.get_user().map(|u| ("user", u.to_string())));
    kv.push(("dbname", database.to_string()));
    kv.push(("connect_timeout", CONNECT_TIMEOUT.as_secs().to_string()));
    let tls = &saved.tls;
    let mode = match tls.mode {
        TlsMode::Disable => "disable",
        TlsMode::Prefer => "prefer",
        TlsMode::Require => "require",
        TlsMode::VerifyCa => "verify-ca",
        TlsMode::VerifyFull => "verify-full",
    };
    kv.push(("sslmode", mode.into()));
    if !tls.ca.is_empty() {
        kv.push(("sslrootcert", expand_home(&tls.ca)?.display().to_string()));
    } else if tls.mode == TlsMode::VerifyFull {
        kv.push(("sslrootcert", "system".into())); // libpq 16+: the system's CAs, which the app trusts too
    }
    if !tls.cert.is_empty() {
        kv.push(("sslcert", expand_home(&tls.cert)?.display().to_string()));
    }
    if !tls.key.is_empty() {
        kv.push(("sslkey", expand_home(&tls.key)?.display().to_string()));
    }
    if saved.options.direct_tls {
        kv.push(("sslnegotiation", "direct".into()));
    }
    if saved.options.no_channel_binding {
        kv.push(("channel_binding", "disable".into()));
    }
    Ok(kv.iter().map(|(k, v)| format!("{k}={}", conninfo_value(v))).collect::<Vec<_>>().join(" "))
}

/// Connection and TLS options for `mysqldump`/`mysql`. MariaDB's tools spell TLS differently from MySQL's.
fn my_args(saved: &Saved, tunnel_port: Option<u16>, mariadb_tool: bool) -> Result<Vec<String>> {
    let opts = mysql_async::Opts::from_url(&saved.url).map_err(mysql_async::Error::from)?;
    let mut a: Vec<String> = match tunnel_port {
        Some(port) => vec!["--host=127.0.0.1".into(), format!("--port={port}"), "--protocol=TCP".into()],
        None if !saved.options.socket.is_empty() => vec![format!("--socket={}", saved.options.socket)],
        // TCP even for "localhost", which the tools would otherwise take to mean the default socket.
        None => vec![
            format!("--host={}", opts.ip_or_hostname()),
            format!("--port={}", opts.tcp_port()),
            "--protocol=TCP".into(),
        ],
    };
    a.extend(opts.user().map(|u| format!("--user={u}")));
    let tls = &saved.tls;
    if mariadb_tool {
        // Prefer and Require only encrypt, as in the app; MariaDB 11.4+ tools verify unless told not to.
        let verify = matches!(tls.mode, TlsMode::VerifyCa | TlsMode::VerifyFull) || !tls.ca.is_empty();
        match tls.mode {
            TlsMode::Disable => a.push("--skip-ssl".into()),
            TlsMode::Prefer => {}
            _ => a.push("--ssl".into()),
        }
        if tls.mode != TlsMode::Disable {
            a.push(if verify { "--ssl-verify-server-cert" } else { "--skip-ssl-verify-server-cert" }.into());
        }
    } else {
        let mode = match tls.mode {
            TlsMode::Disable => "DISABLED",
            TlsMode::Prefer => "PREFERRED",
            TlsMode::Require => "REQUIRED",
            TlsMode::VerifyCa => "VERIFY_CA",
            TlsMode::VerifyFull => "VERIFY_IDENTITY",
        };
        a.push(format!("--ssl-mode={mode}"));
    }
    for (flag, path) in [("--ssl-ca", &tls.ca), ("--ssl-cert", &tls.cert), ("--ssl-key", &tls.key)] {
        if !path.is_empty() {
            a.push(format!("{flag}={}", expand_home(path)?.display()));
        }
    }
    if saved.options.cleartext_auth {
        a.push("--enable-cleartext-plugin".into());
    }
    Ok(a)
}

/// The user's extra flags, one argument per whitespace-separated word.
fn custom_flags(flags: &str) -> Result<Vec<&str>> {
    if flags.len() > MAX_FLAGS {
        return Err(Error::Invalid("custom flags are limited to 4,096 characters"));
    }
    Ok(flags.split_whitespace().collect())
}

/// The password goes in the environment, never the command line, where other users' `ps` could see it.
/// `DuckDB` is one file and has no dump tool of its own to run.
fn no_duckdb_backup() -> Error {
    Error::Invalid("DuckDB has no backup tool here: copy the database file, or run EXPORT DATABASE in a SQL tab")
}

fn password_env(cmd: &mut Command, engine: Engine, password: Option<&SecretString>) {
    let var = if engine == Engine::Postgres { "PGPASSWORD" } else { "MYSQL_PWD" };
    match password {
        Some(p) => cmd.env(var, p.expose_secret()),
        None => cmd.env_remove(var),
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Contents {
    All,
    Schema,
    Data,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Sql,
    /// `pg_dump`'s custom archive, restored with `pg_restore`.
    Custom,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupRequest {
    /// Ignored for SQLite.
    pub database: String,
    /// The tools' folder, or one of the tools in it; None picks the installed version matching the server.
    pub tools: Option<PathBuf>,
    pub contents: Contents,
    pub format: Format,
    pub gzip: bool,
    pub flags: String,
    pub path: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreRequest {
    /// Ignored for SQLite.
    pub database: String,
    /// Create `database` first.
    pub create: bool,
    pub tools: Option<PathBuf>,
    pub flags: String,
    pub path: PathBuf,
}

/// The command that writes a dump of `r.database` to its stdout.
fn dump_command(
    saved: &Saved,
    password: Option<&SecretString>,
    tunnel_port: Option<u16>,
    tools: &Tools,
    r: &BackupRequest,
) -> Result<Command> {
    let flags = custom_flags(&r.flags)?;
    let mut cmd = Command::new(tools.exe(saved.kind, Role::Dump)?);
    let only = |schema, data| match r.contents {
        Contents::All => None,
        Contents::Schema => Some(schema),
        Contents::Data => Some(data),
    };
    match saved.kind {
        Engine::Postgres => {
            cmd.args(["--verbose", "--no-password"]);
            cmd.arg(if r.format == Format::Custom { "--format=custom" } else { "--format=plain" });
            cmd.args(only("--schema-only", "--data-only"));
            cmd.args(flags).arg(format!("--dbname={}", pg_conninfo(saved, tunnel_port, &r.database)?));
        }
        Engine::Mysql | Engine::Mariadb => {
            cmd.args(my_args(saved, tunnel_port, tools.mariadb)?).args(["--verbose", "--single-transaction"]);
            cmd.args(only("--no-data", "--no-create-info"));
            cmd.args(flags).arg("--").arg(&r.database);
        }
        Engine::Sqlite => return Err(Error::Invalid("SQLite is backed up without client tools")),
        Engine::Duckdb => return Err(no_duckdb_backup()),
        Engine::Redis => return Err(Error::Invalid("Redis backups are the server's own (BGSAVE)")),
    }
    password_env(&mut cmd, saved.kind, password);
    Ok(cmd)
}

/// The command that reads a dump on its stdin into `database`: `pg_restore` for an archive, else psql or mysql.
fn restore_command(
    saved: &Saved,
    password: Option<&SecretString>,
    tunnel_port: Option<u16>,
    tools: &Tools,
    database: &str,
    archive: bool,
    flags: &str,
) -> Result<Command> {
    let flags = custom_flags(flags)?;
    let mut cmd;
    match saved.kind {
        Engine::Postgres => {
            if archive {
                cmd = Command::new(tools.exe(saved.kind, Role::Archive)?);
                cmd.args(["--verbose", "--no-password"]);
            } else {
                cmd = Command::new(tools.exe(saved.kind, Role::Script)?);
                cmd.args(["--no-psqlrc", "--no-password", "--set=ON_ERROR_STOP=1"]);
            }
            cmd.args(flags).arg(format!("--dbname={}", pg_conninfo(saved, tunnel_port, database)?));
        }
        Engine::Mysql | Engine::Mariadb => {
            cmd = Command::new(tools.exe(saved.kind, Role::Script)?);
            cmd.args(my_args(saved, tunnel_port, tools.mariadb)?).args(flags).arg(format!("--database={database}"));
        }
        Engine::Sqlite => return Err(Error::Invalid("SQLite is restored without client tools")),
        Engine::Duckdb => return Err(no_duckdb_backup()),
        Engine::Redis => return Err(Error::Invalid("Redis backups are the server's own (BGSAVE)")),
    }
    password_env(&mut cmd, saved.kind, password);
    Ok(cmd)
}

// ---------- Running a job ----------

/// How far a job got: bytes done, out of how many when known, and the tool's latest message.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub bytes: Option<u64>,
    pub total: Option<u64>,
    pub line: Option<String>,
}

type Report<'a> = &'a (dyn Fn(Progress) + Sync);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Stops a running job: sets the flag, and kills the tool's process once there is one.
#[derive(Clone, Default)]
pub struct Stop {
    cancelled: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Stop {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Some(child) = lock(&self.child).as_mut() {
            // Fails only when it already exited, and then there's nothing to stop.
            let _ = child.kill();
        }
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// Counts the bytes read through it, for a restore's progress.
struct Counted<R> {
    inner: R,
    read: Arc<AtomicU64>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// A dump file opened for restoring. Gzip is undone on the fly; `read` counts the file's bytes used so far,
/// out of `size`.
pub struct Dump {
    reader: Box<dyn Read + Send>,
    read: Arc<AtomicU64>,
    size: u64,
    /// `pg_dump`'s custom or tar archive rather than a SQL script.
    archive: bool,
}

fn open_dump(path: &Path) -> Result<Dump> {
    let failed = |source| Error::File { path: path.display().to_string(), source };
    let file = File::open(path).map_err(failed)?;
    let size = file.metadata().map_err(failed)?.len();
    let read = Arc::new(AtomicU64::new(0));
    let mut raw = BufReader::new(Counted { inner: file, read: read.clone() });
    let gzip = raw.fill_buf().map_err(failed)?.starts_with(&[0x1f, 0x8b]);
    let mut reader: Box<dyn Read + Send> = if gzip { Box::new(MultiGzDecoder::new(raw)) } else { Box::new(raw) };
    let mut head = Vec::with_capacity(5);
    (&mut reader).take(5).read_to_end(&mut head).map_err(failed)?;
    let archive = head == b"PGDMP"; // how pg_dump's archives start; tar archives are left to psql to reject
    Ok(Dump { reader: Box::new(std::io::Cursor::new(head).chain(reader)), read, size, archive })
}

/// Copies `from` into `to`, calling `tick` with the bytes copied so far at most five times a second.
fn pump(from: &mut dyn Read, to: &mut dyn Write, tick: &dyn Fn(u64)) -> std::io::Result<u64> {
    let mut buf = vec![0; 64 * 1024];
    let (mut total, mut last) = (0u64, Instant::now());
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        to.write_all(&buf[..n])?;
        total += n as u64;
        if last.elapsed() >= Duration::from_millis(200) {
            tick(total);
            last = Instant::now();
        }
    }
    to.flush()?;
    tick(total);
    Ok(total)
}

/// Reports each stderr line and returns what the error message needs: the lines that mention an error, or
/// else the last few.
fn read_lines(stderr: impl Read, report: Report) -> Vec<String> {
    let (mut errors, mut tail) = (vec![], VecDeque::new());
    for line in BufReader::new(stderr).split(b'\n').map_while(std::io::Result::ok) {
        let line: String = String::from_utf8_lossy(&line).trim_end().chars().take(500).collect();
        if line.is_empty() {
            continue;
        }
        report(Progress { line: Some(line.clone()), ..Progress::default() });
        if errors.len() < KEPT_LINES && line.to_ascii_lowercase().contains("error") {
            errors.push(line.clone());
        }
        tail.push_back(line);
        if tail.len() > 3 {
            tail.pop_front();
        }
    }
    if errors.is_empty() { tail.into() } else { errors }
}

/// Waits for the tool to exit without holding the lock, so [`Stop::cancel`] can still kill it.
fn wait(stop: &Stop) -> std::io::Result<ExitStatus> {
    loop {
        match lock(&stop.child).as_mut() {
            Some(child) => {
                if let Some(status) = child.try_wait()? {
                    return Ok(status);
                }
            }
            None => return Err(std::io::Error::other("the tool's process is gone")),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Where a tool's data goes: its output into a writer (backup), or a dump into its input (restore).
enum Feed<'a> {
    Out(&'a mut dyn Write),
    In(Dump),
}

/// Runs a client tool to the end, reporting its stderr lines as they come. Fails with its error lines when it
/// exits unsuccessfully, and with [`Error::Cancelled`] when stopped.
fn run_tool(mut cmd: Command, feed: Feed, stop: &Stop, report: Report) -> Result<()> {
    let backup = matches!(feed, Feed::Out(_));
    cmd.stdin(if backup { Stdio::null() } else { Stdio::piped() })
        .stdout(if backup { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::piped());
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd.spawn().map_err(|e| Error::Tool(format!("can't run {program}: {e}")))?;
    let (stdout, stdin, stderr) = (child.stdout.take(), child.stdin.take(), child.stderr.take());
    *lock(&stop.child) = Some(child);
    if stop.cancelled() {
        stop.cancel(); // cancelled before there was a process to kill
    }
    let missing = || std::io::Error::other("no pipe to the tool");
    let (copied, status, lines) = std::thread::scope(|s| {
        let lines = s.spawn(|| stderr.map(|e| read_lines(e, report)).unwrap_or_default());
        let copied = match feed {
            Feed::Out(out) => stdout
                .ok_or_else(missing)
                .and_then(|mut o| pump(&mut o, out, &|n| report(Progress { bytes: Some(n), ..Progress::default() }))),
            Feed::In(mut dump) => stdin.ok_or_else(missing).and_then(|mut input| {
                let (read, total) = (dump.read.clone(), dump.size);
                let tick =
                    |_| report(Progress { bytes: Some(read.load(Ordering::Relaxed)), total: Some(total), line: None });
                pump(&mut dump.reader, &mut input, &tick) // dropping `input` then closes the tool's stdin
            }),
        };
        let status = wait(stop);
        (copied, status, lines.join().unwrap_or_default())
    });
    lock(&stop.child).take();
    if stop.cancelled() {
        return Err(Error::Cancelled);
    }
    let status = status?;
    if !status.success() {
        let name = Path::new(&program).file_name().map_or(program.clone(), |n| n.to_string_lossy().into_owned());
        return Err(Error::Tool(if lines.is_empty() { format!("{name} failed ({status})") } else { lines.join("\n") }));
    }
    copied?;
    Ok(())
}

fn part_path(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".part");
    p.into()
}

/// Writes `path` by way of `path.part`, renamed once `write` succeeds, so a failed or cancelled backup never
/// leaves a partial file under the real name, nor replaces an older backup there.
fn write_via_part(path: &Path, write: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let part = part_path(path);
    let r = write(&part).and_then(|()| {
        std::fs::rename(&part, path).map_err(|source| Error::Write { path: path.display().to_string(), source })
    });
    if r.is_err() {
        // An incomplete dump isn't worth keeping; it may also not exist yet.
        let _ = std::fs::remove_file(&part);
    }
    r
}

/// Runs a dump tool and saves its output at `path`, gzipped when asked.
fn backup_with_tool(cmd: Command, path: &Path, gzip: bool, stop: &Stop, report: Report) -> Result<()> {
    write_via_part(path, |part| {
        let failed = |source| Error::Write { path: part.display().to_string(), source };
        let file = BufWriter::new(File::create(part).map_err(failed)?);
        let file = if gzip {
            let mut gz = GzEncoder::new(file, Compression::default());
            run_tool(cmd, Feed::Out(&mut gz), stop, report)?;
            gz.finish().map_err(failed)?
        } else {
            let mut file = file;
            run_tool(cmd, Feed::Out(&mut file), stop, report)?;
            file
        };
        file.into_inner().map_err(|e| failed(e.into_error()))?.sync_all().map_err(failed)
    })
}

fn apply_key(c: &rusqlite::Connection, key: Option<&SecretString>) -> Result<()> {
    if let Some(key) = key.filter(|k| !k.expose_secret().is_empty()) {
        c.pragma_update(None, "key", key.expose_secret())?;
    }
    Ok(())
}

/// Copies every page of `from` into `to` with SQLite's online backup API, a few hundred pages a step so the
/// job can report progress and be cancelled. Stopping before the end leaves `to` as it was.
fn copy_pages(from: &rusqlite::Connection, to: &mut rusqlite::Connection, stop: &Stop, report: Report) -> Result<()> {
    use rusqlite::backup::{Backup, StepResult};
    use rusqlite::types::Value;
    // SQLCipher answers page_size as text (it's its cipher_page_size), plain SQLite as a number.
    let page = match from.pragma_query_value(None, "page_size", |r| r.get::<_, Value>(0))? {
        Value::Integer(n) => u64::try_from(n).unwrap_or_default(),
        Value::Text(t) => t.parse().unwrap_or_default(),
        _ => 0,
    };
    let backup = Backup::new(from, to)?;
    loop {
        if stop.cancelled() {
            return Err(Error::Cancelled);
        }
        let result = backup.step(256)?;
        let p = backup.progress();
        let pages = |n: i32| u64::try_from(n).unwrap_or_default() * page;
        report(Progress { bytes: Some(pages(p.pagecount - p.remaining)), total: Some(pages(p.pagecount)), line: None });
        match result {
            StepResult::Done => return Ok(()),
            StepResult::More => {}
            _ => std::thread::sleep(Duration::from_millis(50)), // busy or locked by another connection
        }
    }
}

/// Backs up an open SQLite database to `path`. An `SQLCipher` database stays encrypted with the same key.
fn backup_sqlite(
    from: &rusqlite::Connection,
    key: Option<&SecretString>,
    path: &Path,
    stop: &Stop,
    report: Report,
) -> Result<()> {
    write_via_part(path, |part| {
        // A leftover from an earlier failed run would be opened instead of a fresh file; missing is fine.
        let _ = std::fs::remove_file(part);
        let mut to = rusqlite::Connection::open(part)?;
        apply_key(&to, key)?;
        copy_pages(from, &mut to, stop, report)
    })
}

/// Copies a SQLite backup at `path` back over the database `to`.
fn restore_sqlite(
    path: &Path,
    key: Option<&SecretString>,
    to: &mut rusqlite::Connection,
    stop: &Stop,
    report: Report,
) -> Result<()> {
    if !path.is_file() {
        return Err(Error::Invalid("the backup file doesn't exist"));
    }
    let from = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    apply_key(&from, key)?;
    // Refuse anything that isn't a readable SQLite database before a page is overwritten.
    from.query_row("select count(*) from sqlite_master", [], |_| Ok(())).map_err(|e| match e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::NotADatabase => {
            Error::Invalid("the file isn't a SQLite database, or has a different passphrase")
        }
        e => e.into(),
    })?;
    copy_pages(&from, to, stop, report)
}

// ---------- Commands ----------

/// Running jobs, by id, so they can be cancelled.
#[derive(Default)]
pub struct Jobs {
    next: AtomicU64,
    running: Mutex<HashMap<u64, Stop>>,
}

#[derive(Serialize, Clone)]
struct ProgressEvent {
    id: u64,
    #[serde(flatten)]
    progress: Progress,
}

#[derive(Serialize, Clone)]
struct DoneEvent {
    id: u64,
    /// None when it worked; "cancelled" when it was stopped.
    error: Option<String>,
}

/// Runs `job` on its own thread, emitting its progress and its outcome under the returned id.
fn start(app: &AppHandle, job: impl FnOnce(&Stop, Report) -> Result<()> + Send + 'static) -> u64 {
    let jobs = app.state::<Jobs>();
    let id = jobs.next.fetch_add(1, Ordering::Relaxed) + 1;
    let stop = Stop::default();
    lock(&jobs.running).insert(id, stop.clone());
    let app = app.clone();
    std::thread::spawn(move || {
        // Nobody listening (the window closed) is fine: the outcome is in the file or the database.
        let report = |progress| {
            let _ = app.emit("backup-progress", ProgressEvent { id, progress });
        };
        let outcome = job(&stop, &report);
        lock(&app.state::<Jobs>().running).remove(&id);
        let _ = app.emit("backup-done", DoneEvent { id, error: outcome.err().map(|e| e.to_string()) });
    });
    id
}

/// What the backup and restore windows offer for a connection.
#[derive(Debug, Serialize)]
pub struct Setup {
    server: String,
    databases: Vec<String>,
    database: Option<String>,
    /// Installed client tools, oldest first; empty for SQLite.
    tools: Vec<Tools>,
    /// The index in `tools` that matches the server, else why none does.
    matched: Option<usize>,
    problem: Option<String>,
}

#[tauri::command]
pub async fn backup_setup(app: AppHandle, name: String) -> Result<Setup> {
    let mut live = crate::open_saved(&app.state::<crate::Open>(), &crate::dir(&app)?, &name).await?;
    if live.saved.kind == Engine::Duckdb {
        return Err(no_duckdb_backup());
    }
    let server = live.conn.server_version().await?;
    let (tools, matched, problem) = if live.saved.kind == Engine::Sqlite {
        (vec![], None, None)
    } else {
        let tools = discover(live.saved.kind).await;
        match pick(live.saved.kind, &server, &tools) {
            Ok(i) => (tools, Some(i), None),
            Err(e) => (tools, None, Some(e.to_string())),
        }
    };
    Ok(Setup {
        databases: live.conn.databases().await?,
        database: live.conn.current_database().await?,
        server,
        tools,
        matched,
        problem,
    })
}

/// The tools a job uses: the folder the user chose, else the installed version that matches the server.
async fn tools_for(live: &mut crate::Live, chosen: Option<&Path>) -> Result<Tools> {
    if let Some(path) = chosen {
        return Ok(Tools::chosen(path));
    }
    let server = live.conn.server_version().await?;
    let mut found = discover(live.saved.kind).await;
    let i = pick(live.saved.kind, &server, &found)?;
    Ok(found.swap_remove(i))
}

/// Starts a backup; progress and the outcome arrive as events under the returned job id. The job has its own
/// connection (and SSH tunnel), so it carries on whatever the workspace does.
#[tauri::command]
pub async fn start_backup(app: AppHandle, name: String, request: BackupRequest) -> Result<u64> {
    let mut live = crate::open_saved(&app.state::<crate::Open>(), &crate::dir(&app)?, &name).await?;
    if live.saved.kind == Engine::Duckdb {
        return Err(no_duckdb_backup());
    }
    if live.saved.kind == Engine::Sqlite {
        let crate::Live { conn, password, .. } = live;
        let Some(from) = conn.into_sqlite() else {
            return Err(Error::Invalid("not a SQLite connection"));
        };
        return Ok(start(&app, move |stop, report| {
            backup_sqlite(&from, password.as_ref(), &request.path, stop, report)
        }));
    }
    let tools = tools_for(&mut live, request.tools.as_deref()).await?;
    let port = live.tunnel.as_ref().map(|t| t.port);
    let cmd = dump_command(&live.saved, live.password.as_ref(), port, &tools, &request)?;
    let tunnel = live.tunnel.take(); // the tool connects through it; the connection itself isn't needed
    Ok(start(&app, move |stop, report| {
        let _tunnel = tunnel;
        backup_with_tool(cmd, &request.path, request.gzip, stop, report)
    }))
}

/// Starts a restore. Safe mode applies; see [`crate::safety::check`].
#[tauri::command]
pub async fn start_restore(
    app: AppHandle,
    name: String,
    request: RestoreRequest,
    confirmed: bool,
    unlock: Option<crate::safety::Unlock>,
) -> Result<u64> {
    let mut live = crate::open_saved(&app.state::<crate::Open>(), &crate::dir(&app)?, &name).await?;
    let (level, password) = (live.saved.safety, live.password.as_ref());
    crate::safety::check(level, false, confirmed, unlock.as_ref(), password).await?;
    if live.saved.kind == Engine::Duckdb {
        return Err(no_duckdb_backup());
    }
    if live.saved.kind == Engine::Sqlite {
        let crate::Live { conn, password, .. } = live;
        let Some(mut to) = conn.into_sqlite() else {
            return Err(Error::Invalid("not a SQLite connection"));
        };
        return Ok(start(&app, move |stop, report| {
            restore_sqlite(&request.path, password.as_ref(), &mut to, stop, report)
        }));
    }
    if request.database.trim().is_empty() {
        return Err(Error::Invalid("choose the database to restore into"));
    }
    let dump = open_dump(&request.path)?;
    let tools = tools_for(&mut live, request.tools.as_deref()).await?;
    let port = live.tunnel.as_ref().map(|t| t.port);
    // Built before the database is created, so bad flags don't leave an empty database behind.
    let cmd = restore_command(
        &live.saved,
        live.password.as_ref(),
        port,
        &tools,
        &request.database,
        dump.archive,
        &request.flags,
    )?;
    if request.create {
        let sql = format!("create database {}", live.conn.ident(&request.database));
        live.conn.query(&sql).await?;
    }
    let tunnel = live.tunnel.take();
    Ok(start(&app, move |stop, report| {
        let _tunnel = tunnel;
        run_tool(cmd, Feed::In(dump), stop, report)
    }))
}

#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
pub fn cancel_backup(jobs: State<'_, Jobs>, id: u64) {
    if let Some(stop) = lock(&jobs.running).get(&id) {
        stop.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Conn;
    use crate::db::Options;
    use crate::tls::Tls;

    fn saved(kind: Engine, url: &str) -> Saved {
        Saved {
            name: "t".into(),
            kind,
            url: url.into(),
            tag: String::new(),
            safety: crate::safety::Safety::Silent,
            ssh: None,
            tls: Tls::default(),
            options: Options::default(),
            password_mode: crate::store::PasswordMode::Keychain,
            color: None,
            group: String::new(),
            flavor: None,
            needs_credentials: false,
        }
    }

    fn tools(version: &str, mariadb: bool) -> Tools {
        Tools { dir: PathBuf::from(format!("/t/{version}")), version: version.into(), mariadb }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tableminus-{}-backup-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn quiet(_: Progress) {}

    #[test]
    fn parse_version_should_read_tool_and_server_versions() {
        assert_eq!(parse_version("pg_dump (PostgreSQL) 17.0 (DBngin.app)").as_deref(), Some("17.0"));
        assert_eq!(parse_version("mysqldump  Ver 8.0.33 for macos13 on arm64").as_deref(), Some("8.0.33"));
        assert_eq!(
            parse_version("mysqldump  Ver 10.19 Distrib 10.11.6-MariaDB, for osx10.19").as_deref(),
            Some("10.11.6")
        );
        assert_eq!(parse_version("mariadb-dump from 11.4.2-MariaDB, client 10.19").as_deref(), Some("11.4.2"));
        assert_eq!(parse_version("PostgreSQL 16.4 (Debian 16.4-1)").as_deref(), Some("16.4"));
    }

    #[test]
    fn pick_should_take_the_oldest_tools_not_older_than_the_server() {
        let found = [tools("14.9", false), tools("16.1", false), tools("17.0", false)];
        assert_eq!(pick(Engine::Postgres, "PostgreSQL 15.4", &found).unwrap(), 1);
        assert_eq!(pick(Engine::Postgres, "PostgreSQL 16.4", &found).unwrap(), 1, "minor versions don't matter");
    }

    #[test]
    fn pick_should_refuse_tools_older_than_the_server() {
        let found = [tools("14.9", false)];
        let err = pick(Engine::Postgres, "PostgreSQL 17.2", &found).unwrap_err().to_string();
        assert!(err.starts_with("no pg_dump for PostgreSQL 17.2 found (found 14.9)"), "{err}");
    }

    #[test]
    fn pick_should_match_mariadb_servers_with_mariadb_tools() {
        let found = [tools("8.0.33", false), tools("11.4.2", true)];
        assert_eq!(pick(Engine::Mysql, "MariaDB 11.4.2", &found).unwrap(), 1);
        assert_eq!(pick(Engine::Mysql, "MySQL 8.0.36", &found).unwrap(), 0);
    }

    #[test]
    fn pg_conninfo_should_dial_the_tunnel_but_keep_the_host_for_tls() {
        let mut s = saved(Engine::Postgres, "postgres://ada@db.example.com:5433/app");
        s.tls.mode = TlsMode::VerifyFull;
        let info = pg_conninfo(&s, Some(40001), "it's").unwrap();
        assert_eq!(
            info,
            "host='db.example.com' hostaddr='127.0.0.1' port='40001' user='ada' dbname='it\\'s' connect_timeout='20' \
             sslmode='verify-full' sslrootcert='system'"
        );
    }

    #[test]
    fn my_args_should_spell_tls_for_mysql_and_mariadb_tools() {
        let mut s = saved(Engine::Mysql, "mysql://root@db.example.com:3307/app");
        s.tls.mode = TlsMode::Require;
        let mysql = my_args(&s, None, false).unwrap();
        assert_eq!(
            mysql,
            ["--host=db.example.com", "--port=3307", "--protocol=TCP", "--user=root", "--ssl-mode=REQUIRED"]
        );
        let mariadb = my_args(&s, Some(40001), true).unwrap();
        assert_eq!(
            mariadb,
            [
                "--host=127.0.0.1",
                "--port=40001",
                "--protocol=TCP",
                "--user=root",
                "--ssl",
                "--skip-ssl-verify-server-cert"
            ]
        );
    }

    #[test]
    fn dump_command_should_pass_the_password_in_the_environment_only() {
        let s = saved(Engine::Postgres, "postgres://ada@localhost/app");
        let request = BackupRequest {
            database: "app".into(),
            tools: None,
            contents: Contents::Schema,
            format: Format::Custom,
            gzip: false,
            flags: "--no-owner  --exclude-table=logs".into(),
            path: PathBuf::from("/tmp/x"),
        };
        let dir = scratch("cmd");
        std::fs::write(dir.join("pg_dump"), "").unwrap();
        let t = Tools { dir, version: "17.0".into(), mariadb: false };
        let cmd = dump_command(&s, Some(&SecretString::from("s3cret")), None, &t, &request).unwrap();
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(
            &args[..6],
            ["--verbose", "--no-password", "--format=custom", "--schema-only", "--no-owner", "--exclude-table=logs"]
        );
        assert!(args.iter().all(|a| !a.contains("s3cret")));
        let env: Vec<_> = cmd.get_envs().collect();
        assert_eq!(env, [(std::ffi::OsStr::new("PGPASSWORD"), Some(std::ffi::OsStr::new("s3cret")))]);
    }

    #[test]
    fn backup_with_tool_should_gzip_the_output_and_restore_should_unzip_it() {
        let dir = scratch("gzip");
        let path = dir.join("out.sql.gz");
        let mut cmd = Command::new("echo");
        cmd.arg("select 1;");
        backup_with_tool(cmd, &path, true, &Stop::default(), &quiet).unwrap();
        assert!(!part_path(&path).exists());
        let mut dump = open_dump(&path).unwrap();
        assert!(!dump.archive);
        let mut text = String::new();
        dump.reader.read_to_string(&mut text).unwrap();
        assert_eq!(text, "select 1;\n");
    }

    #[test]
    fn backup_with_tool_should_report_the_tools_errors_and_leave_no_file() {
        let dir = scratch("fail");
        let path = dir.join("out.sql");
        let mut cmd = Command::new("ls");
        cmd.arg("/no/such/dir");
        let err = backup_with_tool(cmd, &path, false, &Stop::default(), &quiet).unwrap_err().to_string();
        assert!(err.contains("/no/such/dir"), "{err}");
        assert!(!path.exists() && !part_path(&path).exists());
    }

    #[test]
    fn cancel_should_kill_the_tool_and_keep_an_older_backup() {
        let dir = scratch("cancel");
        let path = dir.join("out.sql");
        std::fs::write(&path, "older").unwrap();
        let stop = Stop::default();
        let canceller = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            canceller.cancel();
        });
        let started = Instant::now();
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        assert!(matches!(backup_with_tool(cmd, &path, false, &stop, &quiet), Err(Error::Cancelled)));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "older");
    }

    #[test]
    fn restore_should_feed_the_dump_to_the_tool() {
        let dir = scratch("feed");
        let (dump, copy) = (dir.join("in.sql"), dir.join("copy.sql"));
        std::fs::write(&dump, "insert into t values (1);\n").unwrap();
        let mut cmd = Command::new("tee");
        cmd.arg(&copy);
        let lines = Mutex::new(vec![]);
        let report = |p: Progress| lock(&lines).push((p.bytes, p.total));
        run_tool(cmd, Feed::In(open_dump(&dump).unwrap()), &Stop::default(), &report).unwrap();
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "insert into t values (1);\n");
        assert_eq!(lock(&lines).last(), Some(&(Some(26), Some(26))));
    }

    #[test]
    fn sqlite_backup_and_restore_should_round_trip() {
        let dir = scratch("sqlite");
        let (db, backup) = (dir.join("app.db"), dir.join("app.backup.db"));
        let mut live = rusqlite::Connection::open(&db).unwrap();
        live.execute_batch("create table t (x); insert into t values (1), (2);").unwrap();
        backup_sqlite(&live, None, &backup, &Stop::default(), &quiet).unwrap();
        live.execute("delete from t", []).unwrap();
        restore_sqlite(&backup, None, &mut live, &Stop::default(), &quiet).unwrap();
        let n: i64 = live.query_row("select count(*) from t", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn sqlite_backup_should_keep_an_encrypted_database_encrypted() {
        let dir = scratch("cipher");
        let (db, backup) = (dir.join("app.db"), dir.join("app.backup.db"));
        let key = SecretString::from("correct horse");
        let live = rusqlite::Connection::open(&db).unwrap();
        apply_key(&live, Some(&key)).unwrap();
        live.execute_batch("create table t (x); insert into t values (1);").unwrap();
        backup_sqlite(&live, Some(&key), &backup, &Stop::default(), &quiet).unwrap();
        assert!(!std::fs::read(&backup).unwrap().starts_with(b"SQLite format 3"));
        let mut other = rusqlite::Connection::open(dir.join("other.db")).unwrap();
        apply_key(&other, Some(&key)).unwrap();
        restore_sqlite(&backup, Some(&key), &mut other, &Stop::default(), &quiet).unwrap();
        let n: i64 = other.query_row("select count(*) from t", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn restore_sqlite_should_refuse_a_file_that_isnt_a_database() {
        let dir = scratch("notdb");
        let bogus = dir.join("notes.txt");
        std::fs::write(&bogus, "x".repeat(4096)).unwrap();
        let mut live = rusqlite::Connection::open(dir.join("app.db")).unwrap();
        live.execute_batch("create table t (x)").unwrap();
        let err = restore_sqlite(&bogus, None, &mut live, &Stop::default(), &quiet).unwrap_err();
        assert_eq!(err.to_string(), "the file isn't a SQLite database, or has a different passphrase");
        live.execute("insert into t values (1)", []).unwrap();
    }

    /// Backs up a scratch database with the installed mysqldump, restores it into a second one, then drops both.
    #[test]
    #[ignore = "needs a local MySQL (root, no password) and mysqldump"]
    fn mysql_backup_and_restore_should_round_trip() {
        tauri::async_runtime::block_on(async {
            let (source, target) =
                (format!("dbm_backup_{}", std::process::id()), format!("dbm_restore_{}", std::process::id()));
            let s = saved(Engine::Mysql, "mysql://root@127.0.0.1:3306");
            let mut conn = Conn::open(Engine::Mysql, &s.url, None, &s.tls, &s.options, None).await.unwrap();
            conn.query(&format!("create database {source}")).await.unwrap();
            conn.query(&format!("create table {source}.t (id int primary key, name text)")).await.unwrap();
            conn.query(&format!("insert into {source}.t values (1, 'Ada'), (2, 'Grace')")).await.unwrap();
            let server = conn.server_version().await.unwrap();
            let found = discover(Engine::Mysql).await;
            let t = &found[pick(Engine::Mysql, &server, &found).unwrap()];

            let path = scratch("mysql").join("dump.sql.gz");
            let request = BackupRequest {
                database: source.clone(),
                tools: None,
                contents: Contents::All,
                format: Format::Sql,
                gzip: true,
                flags: String::new(),
                path: path.clone(),
            };
            let lines = Mutex::new(vec![]);
            let report = |p: Progress| lock(&lines).extend(p.line);
            let r = backup_with_tool(
                dump_command(&s, None, None, t, &request).unwrap(),
                &path,
                true,
                &Stop::default(),
                &report,
            );
            conn.query(&format!("create database {target}")).await.unwrap();
            let dump = open_dump(&path).unwrap();
            let cmd = restore_command(&s, None, None, t, &target, dump.archive, "").unwrap();
            let restored = r.and_then(|()| run_tool(cmd, Feed::In(dump), &Stop::default(), &quiet));
            let rows = conn.query(&format!("select name from {target}.t order by id")).await;
            conn.query(&format!("drop database {source}")).await.unwrap();
            conn.query(&format!("drop database {target}")).await.unwrap();
            restored.unwrap();
            assert!(lock(&lines).iter().any(|l| l.contains("Retrieving table structure")), "{:?}", lock(&lines));
            let names: Vec<_> = rows.unwrap().rows.into_iter().map(|r| r[0].clone().unwrap()).collect();
            assert_eq!(names, ["Ada", "Grace"]);
        });
    }
}

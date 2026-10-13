//! Imports and exports run in the background on a connection of their own, so the workspace stays usable while
//! they do. Each job sends `job-progress` events as `[id, Progress]` and ends with one `job-done`; `cancel_job`
//! stops it at the next row or statement.

use crate::error::{Error, Result};
use crate::{Open, db, export, import, readonly, store};
use secrecy::SecretString;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, PoisonError};
use tauri::{AppHandle, Emitter, Manager, State};

/// How far a job has got. `done` and `total` are rows for exports and JSON imports, bytes for CSV and SQL files.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Progress {
    /// The file being written (exports of several tables), of `items`.
    pub item: usize,
    pub items: usize,
    pub done: u64,
    pub total: Option<u64>,
    /// Rows written or imported so far, or statements run for SQL files.
    pub rows: u64,
}

/// What a running job sees: whether it was cancelled, and where to report progress.
pub struct Job {
    cancel: Arc<AtomicBool>,
    report: Box<dyn Fn(Progress) + Send + Sync>,
}

impl Job {
    /// [`Error::Cancelled`] once the user has cancelled.
    pub fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) { Err(Error::Cancelled) } else { Ok(()) }
    }

    pub fn report(&self, p: Progress) {
        (self.report)(p);
    }

    #[cfg(test)]
    pub fn quiet() -> Job {
        Job { cancel: Arc::default(), report: Box::new(|_| {}) }
    }

    #[cfg(test)]
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The running jobs' cancel flags.
#[derive(Default)]
pub struct Jobs {
    next: AtomicU64,
    running: std::sync::Mutex<HashMap<u64, Arc<AtomicBool>>>,
}

#[derive(Debug, Clone, Serialize)]
struct Done {
    id: u64,
    rows: u64,
    error: Option<String>,
}

/// What a job needs to open its own connection like the live one, including the database chosen since.
struct Target {
    saved: store::Saved,
    url: String,
    password: Option<SecretString>,
    port: Option<u16>,
    database: Option<String>,
}

impl Target {
    /// Copies the live connection's settings. With `writes`, safe mode applies: [`Error::ConfirmRequired`] until
    /// `confirmed`.
    async fn of(open: &Open, name: &str, writes: bool, confirmed: bool) -> Result<Target> {
        let mut open = open.0.lock().await;
        let live = open.get_mut(name).ok_or(Error::NotConnected)?;
        if writes && live.saved.confirm_writes && !confirmed {
            return Err(Error::ConfirmRequired);
        }
        // MySQL switches database in place (Postgres's URL already names it), so the job's connection follows.
        let database = if matches!(live.conn, db::Conn::My(_)) {
            let r = live.conn.current_database().await;
            live.check(r)?
        } else {
            None
        };
        Ok(Target {
            saved: live.saved.clone(),
            url: live.url.clone(),
            password: live.password.clone(),
            port: live.tunnel.as_ref().map(|t| t.port),
            database,
        })
    }

    /// The startup script already ran when the live connection opened; the startup SQL runs again here.
    async fn open(&self) -> Result<db::Conn> {
        let s = &self.saved;
        let mut conn = db::Conn::open(s.kind, &self.url, self.password.as_ref(), &s.tls, &s.options, self.port).await?;
        if let Some(database) = &self.database {
            conn.use_database(database).await?;
        }
        Ok(conn)
    }
}

/// Runs `work` on a thread of its own and returns the job's id. The thread drives the job's database calls itself,
/// so file I/O and SQLite's blocking steps never hold up the async runtime.
fn spawn<W, F>(app: AppHandle, jobs: &Jobs, work: W) -> u64
where
    W: FnOnce(Job) -> F + Send + 'static,
    F: Future<Output = Result<u64>>,
{
    let id = jobs.next.fetch_add(1, Ordering::Relaxed) + 1;
    let cancel = Arc::new(AtomicBool::new(false));
    jobs.running.lock().unwrap_or_else(PoisonError::into_inner).insert(id, cancel.clone());
    let emitter = app.clone();
    let job = Job {
        cancel,
        report: Box::new(move |p| {
            // Nobody listening (the window is closing) is fine; the job goes on regardless.
            let _ = emitter.emit("job-progress", (id, p));
        }),
    };
    tauri::async_runtime::spawn_blocking(move || {
        let result = tauri::async_runtime::block_on(work(job));
        app.state::<Jobs>().running.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
        let done = match result {
            Ok(rows) => Done { id, rows, error: None },
            Err(e) => Done { id, rows: 0, error: Some(e.to_string()) },
        };
        let _ = app.emit("job-done", done); // as above
    });
    id
}

/// Starts an export; see [`export::Export`]. An editor query that may write goes through safe mode.
#[tauri::command]
pub async fn start_export(
    app: AppHandle,
    open: State<'_, Open>,
    jobs: State<'_, Jobs>,
    name: String,
    export: export::Export,
    confirmed: bool,
) -> Result<u64> {
    let kind = open.0.lock().await.get(&name).ok_or(Error::NotConnected)?.saved.kind;
    let writes = export
        .items
        .iter()
        .any(|i| matches!(&i.source, export::Source::Query { sql, .. } if !readonly::is_read_only(kind, sql)));
    let target = Target::of(&open, &name, writes, confirmed).await?;
    Ok(spawn(app, &jobs, move |job| async move {
        let mut conn = target.open().await?;
        export::run(&mut conn, &export, &job).await
    }))
}

/// Starts a CSV or JSON import; see [`import::Import`]. Safe mode applies.
#[tauri::command]
pub async fn start_import(
    app: AppHandle,
    open: State<'_, Open>,
    jobs: State<'_, Jobs>,
    name: String,
    import: import::Import,
    confirmed: bool,
) -> Result<u64> {
    let target = Target::of(&open, &name, true, confirmed).await?;
    Ok(spawn(app, &jobs, move |job| async move {
        let mut conn = target.open().await?;
        import::run(&mut conn, &import, &job).await
    }))
}

/// Starts running a `.sql` file statement by statement. Safe mode applies.
#[tauri::command]
pub async fn start_sql_import(
    app: AppHandle,
    open: State<'_, Open>,
    jobs: State<'_, Jobs>,
    name: String,
    path: PathBuf,
    confirmed: bool,
) -> Result<u64> {
    let target = Target::of(&open, &name, true, confirmed).await?;
    Ok(spawn(app, &jobs, move |job| async move {
        let mut conn = target.open().await?;
        import::run_sql(&mut conn, target.saved.kind, &path, &job).await
    }))
}

/// The first rows of a file to import, as the import would read them.
#[tauri::command]
pub async fn preview_import(path: PathBuf, format: import::FileFormat) -> Result<import::Preview> {
    tauri::async_runtime::spawn_blocking(move || import::preview(&path, &format)).await?
}

/// Stops a job at its next row or statement. An unknown id (the job just ended) is fine.
#[tauri::command]
#[expect(clippy::needless_pass_by_value, reason = "Tauri hands command arguments over by value")]
pub fn cancel_job(jobs: State<'_, Jobs>, id: u64) {
    if let Some(flag) = jobs.running.lock().unwrap_or_else(PoisonError::into_inner).get(&id) {
        flag.store(true, Ordering::Relaxed);
    }
}

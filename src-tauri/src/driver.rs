//! What an engine's client library has to do. [`crate::db::Conn`] holds one [`Driver`] and builds everything else
//! (catalog queries, DDL) on top of it by the engine's [`Dialect`]. A new SQL engine is a new `Driver`, a `Dialect`
//! variant, and the arms the compiler then points out where SQL differs.

pub mod duckdb;
pub mod mssql;
pub mod mysql;
pub mod postgres;
pub mod sqlite;

use crate::db::{Dialect, QueryResult};
use crate::error::{Error, Result};
use async_trait::async_trait;
use std::any::Any;

/// Runs blocking work without stalling the async worker it's on: the pool takes over the worker's other tasks
/// meanwhile. `block_in_place` rather than `spawn_blocking`, which would need to own the borrowed connection. Every
/// runtime here (the app's, the tests', the MCP bridge's) is multi-threaded, which it requires. For the embedded
/// engines (SQLite, `DuckDB`), whose calls run on the calling thread.
pub(crate) fn blocking<T>(work: impl FnOnce() -> T) -> T {
    tokio::task::block_in_place(work)
}

/// Whether `sql` is blank or only comments, which `DuckDB` refuses to prepare.
pub(crate) fn only_comments(sql: &str) -> bool {
    let mut rest = sql.trim_start();
    loop {
        if let Some(r) = rest.strip_prefix("--") {
            rest = r.split_once('\n').map_or("", |(_, after)| after).trim_start();
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map_or("", |(_, after)| after).trim_start();
        } else {
            return rest.is_empty();
        }
    }
}

/// The statement's first word, lower case.
pub(crate) fn first_word(sql: &str) -> String {
    let mut rest = sql.trim_start();
    while rest.starts_with("--") || rest.starts_with("/*") {
        rest = if let Some(r) = rest.strip_prefix("--") {
            r.split_once('\n').map_or("", |(_, a)| a)
        } else {
            rest[2..].split_once("*/").map_or("", |(_, a)| a)
        }
        .trim_start();
    }
    rest.split(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or_default().to_ascii_lowercase()
}

/// The most results one call keeps; statements after that still run, but their results are dropped.
pub const MAX_RESULTS: usize = 100;

/// Gathers the results of one call, keeping at most `left` rows across them and [`MAX_RESULTS`] results.
#[derive(Default)]
pub struct Results {
    done: Vec<QueryResult>,
    current: Option<QueryResult>,
    left: usize,
}

impl Results {
    /// Keeps at most `max_rows` rows across the results (all of them when `None`).
    pub fn new(max_rows: Option<usize>) -> Self {
        Results { left: max_rows.unwrap_or(usize::MAX), ..Results::default() }
    }

    /// A statement's result starts.
    pub fn begin(&mut self, columns: Vec<String>) {
        self.current = Some(QueryResult { columns, ..QueryResult::default() });
    }

    /// Whether to keep the row that just arrived; when not, its result is marked cut short.
    pub fn keep(&mut self) -> bool {
        let full = self.done.len() >= MAX_RESULTS;
        let Some(r) = self.current.as_mut().filter(|_| !full) else { return false };
        if self.left == 0 {
            r.truncated = true;
            return false;
        }
        self.left -= 1;
        true
    }

    pub fn push(&mut self, row: Vec<Option<String>>) {
        if let Some(r) = &mut self.current {
            r.rows.push(row);
        }
    }

    /// A statement finished; one that returned no rows has no `begin`.
    pub fn end(&mut self, affected: u64) {
        let mut r = self.current.take().unwrap_or_default();
        r.affected = affected;
        if self.done.len() < MAX_RESULTS {
            self.done.push(r);
        }
    }

    pub fn finish(mut self) -> Vec<QueryResult> {
        if self.current.is_some() {
            self.end(0);
        }
        if self.done.is_empty() {
            self.done.push(QueryResult::default()); // e.g. only a comment
        }
        self.done
    }
}

/// A way to stop a running query from outside its connection.
pub enum Stopper {
    /// Sends Postgres's cancel request on a separate socket.
    Pg(tokio_postgres::CancelToken),
    /// The MySQL session id, for `KILL QUERY` from a second connection.
    My(u32),
    /// SQLite runs on the calling thread, so it is interrupted from another one.
    Lite(rusqlite::InterruptHandle),
    /// Likewise `DuckDB`.
    Duck(std::sync::Arc<::duckdb::InterruptHandle>),
    /// Wakes the running call, which cancels itself on the server (the connection stays usable).
    Mssql(std::sync::Arc<tokio::sync::Notify>),
}

/// Receives a query result as it arrives: the column names once, then each row.
pub trait RowSink: Send {
    fn columns(&mut self, names: Vec<String>) -> Result<()>;
    fn row(&mut self, row: Vec<Option<String>>) -> Result<()>;
}

/// One open connection to a server, at the level of its client library: run SQL, read cells as text. Cells are
/// `None` for NULL; every driver renders its types as text, so nothing above it decodes values.
#[async_trait]
pub trait Driver: Send {
    fn dialect(&self) -> Dialect;

    /// Runs one or more statements, discarding any results.
    async fn batch(&mut self, sql: &str) -> Result<()>;

    /// Runs one or more statements, handing each one's columns and rows to `out`.
    async fn run(&mut self, sql: &str, out: &mut Results) -> Result<()>;

    /// Runs one statement with bound parameters and returns the rows it affected.
    async fn execute_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<u64>;

    /// Runs a catalog query with bound parameters. Every selected column must be text (cast `::text` on Postgres).
    async fn fetch_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Vec<Vec<Option<String>>>>;

    /// Runs one query and hands its rows to `sink` as they arrive, so a large result never sits in memory. An error
    /// from the sink stops the query. With `params` on Postgres, every selected column must be text (as for
    /// [`Driver::fetch_with`]); without, any query works.
    async fn stream(&mut self, sql: &str, params: &[Option<&str>], sink: &mut dyn RowSink) -> Result<()>;

    /// A round trip that does nothing, to keep idle connections from being closed by the server or the network.
    async fn ping(&mut self) -> Result<()>;

    fn stopper(&self) -> Stopper;

    /// Whether `err` (from this connection) means the connection itself is gone, not just that a statement failed.
    fn lost_by(&self, _err: &Error) -> bool {
        false
    }

    /// Statements that make the database itself refuse writes, then undo that.
    fn read_only_bounds(&self) -> (&'static str, &'static str);

    /// The placeholder for the `n`th (1-based) bound parameter.
    fn param(&self, n: usize) -> String {
        format!("${n}")
    }

    /// For code that needs the engine's own connection (SQLite's backup API).
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

//! SQLite (and `SQLCipher`) through rusqlite. SQLite runs on the calling thread, so every call goes through
//! [`blocking`].

use super::{Driver, Results, RowSink, Stopper};
use crate::db::Dialect;
use crate::error::{Error, Result};
use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use std::any::Any;

/// Runs blocking SQLite work without stalling the async worker it's on: the pool takes over the worker's other tasks
/// meanwhile. `block_in_place` rather than `spawn_blocking`, which would need to own the borrowed connection. Every
/// runtime here (the app's, the tests', the MCP bridge's) is multi-threaded, which it requires.
fn blocking<T>(work: impl FnOnce() -> T) -> T {
    tokio::task::block_in_place(work)
}

pub struct Sqlite(pub rusqlite::Connection);

impl Sqlite {
    /// Opens a SQLite file. With `key`, the file is an `SQLCipher` database: the key is a passphrase, or a raw key
    /// written `x'…'`. Opening can take a while: `SQLCipher` derives the key from the passphrase.
    pub fn open(path: &str, key: Option<&SecretString>) -> Result<Sqlite> {
        blocking(|| {
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
            Ok(Sqlite(c))
        })
    }
}

fn cell(v: rusqlite::types::ValueRef) -> Option<String> {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Some(format!("<{} bytes>", b.len())),
    }
}

#[async_trait]
impl Driver for Sqlite {
    fn dialect(&self) -> Dialect {
        Dialect::Lite
    }

    async fn batch(&mut self, sql: &str) -> Result<()> {
        Ok(blocking(|| self.0.execute_batch(sql))?)
    }

    async fn run(&mut self, sql: &str, out: &mut Results) -> Result<()> {
        blocking(|| -> Result<()> {
            use rusqlite::fallible_iterator::FallibleIterator;
            // SQLite prepares one statement at a time; Batch steps through the rest of the text.
            let mut batch = rusqlite::Batch::new(&self.0, sql);
            while let Some(mut st) = batch.next()? {
                let columns: Vec<String> = st.column_names().into_iter().map(String::from).collect();
                if columns.is_empty() {
                    out.begin(columns);
                    out.end(st.execute([])? as u64);
                    continue;
                }
                let n = columns.len();
                out.begin(columns);
                let mut rows = st.query([])?;
                while let Some(row) = rows.next()? {
                    if !out.keep() {
                        break; // SQLite steps lazily, so there's nothing more to read
                    }
                    out.push((0..n).map(|i| row.get_ref(i).map(cell)).collect::<rusqlite::Result<Vec<_>>>()?);
                }
                out.end(0);
            }
            Ok(())
        })
    }

    async fn execute_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<u64> {
        Ok(blocking(|| self.0.execute(sql, rusqlite::params_from_iter(params)))? as u64)
    }

    async fn fetch_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Vec<Vec<Option<String>>>> {
        blocking(|| {
            let mut st = self.0.prepare(sql)?;
            let n = st.column_count();
            let rows = st
                .query_map(rusqlite::params_from_iter(params), |r| {
                    (0..n).map(|i| r.get_ref(i).map(cell)).collect::<rusqlite::Result<Vec<_>>>()
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    async fn stream(&mut self, sql: &str, params: &[Option<&str>], sink: &mut dyn RowSink) -> Result<()> {
        blocking(|| -> Result<()> {
            let mut st = self.0.prepare(sql)?;
            let n = st.column_count();
            sink.columns(st.column_names().into_iter().map(String::from).collect())?;
            let mut rows = st.query(rusqlite::params_from_iter(params))?;
            while let Some(row) = rows.next()? {
                sink.row((0..n).map(|i| row.get_ref(i).map(cell)).collect::<rusqlite::Result<_>>()?)?;
            }
            Ok(())
        })
    }

    async fn ping(&mut self) -> Result<()> {
        Ok(())
    }

    fn stopper(&self) -> Stopper {
        Stopper::Lite(self.0.get_interrupt_handle())
    }

    fn read_only_bounds(&self) -> (&'static str, &'static str) {
        ("pragma query_only = on", "pragma query_only = off")
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

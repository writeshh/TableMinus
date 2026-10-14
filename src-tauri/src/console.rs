//! The console log: every statement the app sends to a database (editor queries, browsing, catalog reads, saves,
//! DDL, imports), oldest first, in a bounded ring buffer. The UI reads what's new while its console is open.

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

/// Entries kept; older ones fall off.
const KEEP: usize = 1000;
/// Characters kept of one statement, so a huge INSERT doesn't fill the buffer.
const MAX_SQL: usize = 10_000;

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    /// Rises by one per statement, so the UI can ask for what came after the last one it has.
    pub id: u64,
    /// Milliseconds since the Unix epoch.
    pub at: u64,
    pub sql: String,
}

struct Log {
    next: u64,
    entries: VecDeque<Entry>,
}

static LOG: Mutex<Log> = Mutex::new(Log { next: 1, entries: VecDeque::new() });

fn log() -> std::sync::MutexGuard<'static, Log> {
    // A panic elsewhere while holding the lock can't leave the log half-written in a way that matters.
    LOG.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Notes a statement about to be sent.
pub fn record(sql: &str) {
    let sql = sql.trim();
    let sql = match sql.char_indices().nth(MAX_SQL) {
        Some((cut, _)) => format!("{}…", &sql[..cut]),
        None => sql.to_string(),
    };
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default();
    let mut log = log();
    let id = log.next;
    log.next += 1;
    if log.entries.len() == KEEP {
        log.entries.pop_front();
    }
    log.entries.push_back(Entry { id, at, sql });
}

/// The entries after `after` (0 for all of them).
#[tauri::command]
pub fn console_log(after: u64) -> Vec<Entry> {
    log().entries.iter().filter(|e| e.id > after).cloned().collect()
}

#[tauri::command]
pub fn console_clear() {
    log().entries.clear();
}

#[cfg(test)]
mod tests {
    use super::{KEEP, MAX_SQL, console_log, record};

    // One test, matching entries by content: the log is global, and other tests' queries land in it too.
    #[test]
    fn record_should_keep_the_newest_entries_trimmed_and_numbered() {
        let before = console_log(0).last().map_or(0, |e| e.id);
        record("  select 'console test'  ");
        assert!(console_log(before).iter().any(|e| e.sql == "select 'console test'"));
        for _ in 0..KEEP + 5 {
            record("x");
        }
        let all = console_log(0);
        assert!(all.len() <= KEEP);
        assert!(all.windows(2).all(|w| w[0].id < w[1].id));
        record(&"é".repeat(MAX_SQL + 3));
        let cut = |e: &super::Entry| e.sql.starts_with('é') && e.sql.ends_with('…');
        let long = console_log(0).into_iter().find(cut).unwrap();
        assert_eq!(long.sql.chars().count(), MAX_SQL + 1, "cut, with an ellipsis");
    }
}

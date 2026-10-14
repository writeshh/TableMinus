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

/// Notes a statement about to be sent, with any password in it masked (see [`redact`]).
pub fn record(sql: &str) {
    let sql = redact(sql.trim());
    let sql = sql.as_ref();
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

/// What a masked password literal reads as.
const MASK: &str = "'********'";

/// `sql` with password literals masked: the string after `PASSWORD` (Postgres `PASSWORD 'x'`, MariaDB
/// `PASSWORD('x')`), after `BY`, `AS` or `REPLACE` in a statement that says `IDENTIFIED` (MySQL), and after the `=`
/// of `SET PASSWORD`. Quoted identifiers and comments are skipped, so their quotes don't throw the scan off; a literal
/// left open masks the rest of the text.
pub fn redact(sql: &str) -> std::borrow::Cow<'_, str> {
    let b = sql.as_bytes();
    let (mut out, mut copied) = (String::new(), 0);
    let mut statement = 0; // where the current statement starts
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b';' => statement = i + 1,
            b'-' if b.get(i + 1) == Some(&b'-') => i = find(b, i, b"\n").unwrap_or(b.len()),
            b'/' if b.get(i + 1) == Some(&b'*') => i = find(b, i + 2, b"*/").map_or(b.len(), |e| e + 1),
            q @ (b'"' | b'`') => i = find(b, i + 1, &[q]).unwrap_or(b.len()),
            b'\'' => {
                let end = literal_end(b, i);
                if is_password(&sql[statement..i]) {
                    out.push_str(&sql[copied..i]);
                    out.push_str(MASK);
                    copied = end;
                }
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    if copied == 0 {
        return sql.into();
    }
    out.push_str(&sql[copied..]);
    out.into()
}

/// Where `needle` next starts at or after `from`.
fn find(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    b.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// Just past the literal opening at `start`: `''` and a backslash escape a quote (MySQL, Postgres `E''`).
fn literal_end(b: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 1,
            b'\'' if b.get(i + 1) == Some(&b'\'') => i += 1,
            b'\'' => return i + 1,
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// Whether a literal right after `before` (the statement so far) is a password.
fn is_password(before: &str) -> bool {
    let head = before.strip_suffix(['E', 'e']).filter(|h| !h.ends_with(|c: char| c.is_alphanumeric() || c == '_'));
    let head = head.unwrap_or(before).trim_end();
    let (head, assigned) = match head.strip_suffix('=') {
        Some(h) => (h, true),
        None => (head.strip_suffix('(').unwrap_or(head), false),
    };
    let head = head.trim_end();
    let word: String = head.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    let word: String = word.chars().rev().collect::<String>().to_ascii_uppercase();
    let upper = before.to_ascii_uppercase();
    word == "PASSWORD"
        || (matches!(word.as_str(), "BY" | "AS" | "REPLACE") && upper.contains("IDENTIFIED"))
        || (assigned && upper.trim_start().starts_with("SET PASSWORD"))
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
    use super::{KEEP, MAX_SQL, console_log, record, redact};

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

    #[test]
    fn redact_should_mask_password_literals_and_nothing_else() {
        let cases = [
            ("ALTER ROLE \"a\" PASSWORD E'it''s\\\\' LOGIN", "ALTER ROLE \"a\" PASSWORD E'********' LOGIN"),
            (
                "create role a with login encrypted password 'x' valid until '2030-01-01'",
                "create role a with login encrypted password '********' valid until '2030-01-01'",
            ),
            (
                "CREATE USER 'u'@'%' IDENTIFIED WITH caching_sha2_password BY 'p''w'",
                "CREATE USER 'u'@'%' IDENTIFIED WITH caching_sha2_password BY '********'",
            ),
            (
                "alter user 'u'@'h' identified by 'new' replace 'old'",
                "alter user 'u'@'h' identified by '********' replace '********'",
            ),
            (
                "CREATE USER 'u'@'h' IDENTIFIED VIA ed25519 USING PASSWORD('pw')",
                "CREATE USER 'u'@'h' IDENTIFIED VIA ed25519 USING PASSWORD('********')",
            ),
            ("SET PASSWORD FOR 'u'@'h' = 'pw'", "SET PASSWORD FOR 'u'@'h' = '********'"),
            ("select 'by' as x order by 'a'", "select 'by' as x order by 'a'"),
            (
                "select \"password\" from t where note = 'password'",
                "select \"password\" from t where note = 'password'",
            ),
            ("-- it's\nalter role a password 'pw'", "-- it's\nalter role a password '********'"),
            ("alter role a password 'never closed", "alter role a password '********'"),
            (
                "CREATE USER 'u'@'h' IDENTIFIED BY 'a'; select 'b' as c",
                "CREATE USER 'u'@'h' IDENTIFIED BY '********'; select 'b' as c",
            ),
        ];
        for (sql, want) in cases {
            assert_eq!(redact(sql), want, "{sql}");
        }
    }

    #[test]
    fn record_should_not_keep_a_password() {
        record("ALTER USER 'console-redact'@'%' IDENTIFIED BY 'hunter2'");
        let all = console_log(0);
        assert!(all.iter().any(|e| e.sql == "ALTER USER 'console-redact'@'%' IDENTIFIED BY '********'"));
        assert!(!all.iter().any(|e| e.sql.contains("hunter2")));
    }
}

//! Safe mode's read-only check.

use crate::db::Engine;
use sqlparser::ast::{Expr, ObjectName, ObjectNamePart, Select, Statement, TableFactor, Visit, Visitor};
use sqlparser::dialect::{Dialect, DuckDbDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;
use std::ops::ControlFlow;

/// Built-in functions that change something even when called from a SELECT: sequences, sessions, locks, server
/// state, large objects, files, settings and other databases (`dblink`). Matched by name, lowercase, any schema.
const SIDE_EFFECTS: &[&str] = &[
    // Postgres
    "nextval",
    "setval",
    "pg_terminate_backend",
    "pg_cancel_backend",
    "pg_reload_conf",
    "pg_rotate_logfile",
    "pg_switch_wal",
    "pg_create_restore_point",
    "pg_promote",
    "pg_wal_replay_pause",
    "pg_wal_replay_resume",
    "pg_create_physical_replication_slot",
    "pg_create_logical_replication_slot",
    "pg_drop_replication_slot",
    "pg_logical_emit_message",
    "pg_stat_reset",
    "pg_stat_reset_shared",
    "pg_stat_reset_single_table_counters",
    "pg_stat_statements_reset",
    "pg_notify",
    "set_config",
    "lo_creat",
    "lo_create",
    "lo_import",
    "lo_export",
    "lo_unlink",
    "lo_put",
    "lo_truncate",
    "lo_from_bytea",
    "pg_file_write",
    "pg_file_rename",
    "pg_file_unlink",
    "dblink",
    "dblink_exec",
    // Advisory locks outlive the query that takes them (the session-level ones) or block other sessions.
    "pg_advisory_lock",
    "pg_advisory_lock_shared",
    "pg_advisory_xact_lock",
    "pg_advisory_xact_lock_shared",
    "pg_try_advisory_lock",
    "pg_try_advisory_lock_shared",
    "pg_try_advisory_xact_lock",
    "pg_try_advisory_xact_lock_shared",
    "pg_advisory_unlock",
    "pg_advisory_unlock_shared",
    "pg_advisory_unlock_all",
    // MySQL / MariaDB (MariaDB's sequences use nextval and setval, as above)
    "get_lock",
    "release_lock",
    "release_all_locks",
    // SQLite
    "load_extension",
    "writefile",
];

/// True only when every statement, including nested ones (e.g. a DELETE inside a CTE), is a plain query that calls
/// no [`SIDE_EFFECTS`] function and writes no `SELECT … INTO` target. Anything unparseable counts as a write, so it
/// gets the safe-mode prompt.
// ponytail: a user-defined function can write too, and only a read-only transaction would catch that.
pub fn is_read_only(engine: Engine, sql: &str) -> bool {
    let dialect: &dyn Dialect = match engine {
        Engine::Postgres => &PostgreSqlDialect {},
        Engine::Mysql | Engine::Mariadb => &MySqlDialect {},
        Engine::Sqlite => &SQLiteDialect {},
        Engine::Duckdb => &DuckDbDialect {},
        // Redis commands aren't SQL; redis.rs has its own list.
        Engine::Redis => return false,
    };
    let Ok(stmts) = Parser::parse_sql(dialect, sql) else { return false };
    !stmts.is_empty() && stmts.visit(&mut Writes).is_continue()
}

/// Stops at the first sign of a write.
struct Writes;

fn side_effect(name: &ObjectName) -> bool {
    name.0
        .last()
        .and_then(ObjectNamePart::as_ident)
        .is_some_and(|i| SIDE_EFFECTS.contains(&i.value.to_lowercase().as_str()))
}

fn stop_if(write: bool) -> ControlFlow<()> {
    if write { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
}

impl Visitor for Writes {
    type Break = ();

    fn pre_visit_statement(&mut self, s: &Statement) -> ControlFlow<()> {
        // EXPLAIN is as safe as what it explains: the visit goes on into the inner statement, which must be a query
        // too (so EXPLAIN ANALYZE of a write, which runs it, still asks).
        stop_if(!matches!(s, Statement::Query(_) | Statement::Explain { .. }))
    }

    /// `SELECT … INTO new_table` (Postgres) and `INTO OUTFILE` (MySQL) write.
    fn pre_visit_select(&mut self, s: &Select) -> ControlFlow<()> {
        stop_if(s.into.is_some())
    }

    fn pre_visit_expr(&mut self, e: &Expr) -> ControlFlow<()> {
        stop_if(matches!(e, Expr::Function(f) if side_effect(&f.name)))
    }

    /// Set-returning calls in FROM: `select * from dblink(…)`.
    fn pre_visit_table_factor(&mut self, t: &TableFactor) -> ControlFlow<()> {
        stop_if(
            matches!(t, TableFactor::Table { name, args: Some(_), .. } | TableFactor::Function { name, .. } if side_effect(name)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::is_read_only;
    use crate::db::Engine::{Mysql, Postgres, Sqlite};

    #[test]
    fn plain_queries_are_read_only() {
        assert!(is_read_only(Postgres, "select 1; select * from t where id = 2"));
        assert!(is_read_only(Mysql, "with x as (select 1) select * from x"));
    }

    #[test]
    fn explaining_a_query_is_read_only() {
        assert!(is_read_only(Postgres, "explain select * from t"));
        assert!(is_read_only(Mysql, "explain select * from t"));
        assert!(is_read_only(Sqlite, "explain query plan select * from t"));
        assert!(!is_read_only(Postgres, "explain analyze delete from t"), "ANALYZE runs the delete");
    }

    #[test]
    fn any_write_needs_confirmation() {
        assert!(!is_read_only(Postgres, "update t set a = 1"));
        assert!(!is_read_only(Postgres, "select 1; drop table t"));
        assert!(!is_read_only(Postgres, "with d as (delete from t returning *) select * from d"));
        assert!(!is_read_only(Sqlite, "explain analyze delete from t"), "EXPLAIN may execute");
    }

    #[test]
    fn side_effect_functions_need_confirmation() {
        assert!(!is_read_only(Postgres, "select nextval('s')"));
        assert!(!is_read_only(Postgres, "select pg_catalog.pg_terminate_backend(42)"));
        assert!(!is_read_only(Postgres, "select * from t where id = (select setval('s', 1))"));
        assert!(!is_read_only(Postgres, "select * from dblink('db', 'delete from t') as x(n int)"));
        assert!(!is_read_only(Sqlite, "select load_extension('x')"));
        assert!(!is_read_only(Postgres, "select pg_advisory_lock(1)"));
        assert!(!is_read_only(Mysql, "select get_lock('job', 10)"));
        assert!(!is_read_only(Mysql, "select release_all_locks()"));
        assert!(is_read_only(Postgres, "select currval('s'), lower(name) from t"), "reading a sequence is fine");
    }

    #[test]
    fn select_into_needs_confirmation() {
        assert!(!is_read_only(Postgres, "select * into copy from t"));
        assert!(!is_read_only(Mysql, "select * from t into outfile '/tmp/x'"));
    }

    #[test]
    fn unparseable_or_empty_sql_needs_confirmation() {
        assert!(!is_read_only(Postgres, "not valid sql at all"));
        assert!(!is_read_only(Postgres, ""));
    }

    /// Properties over generated input: the check never panics, sees a write after any read, and passes plain
    /// SELECTs of any quoted names.
    mod props {
        use super::is_read_only;
        use crate::db::{Dialect, Engine};
        use proptest::prelude::*;

        const ENGINES: [(Engine, Dialect); 4] = [
            (Engine::Postgres, Dialect::Pg),
            (Engine::Mysql, Dialect::My),
            (Engine::Sqlite, Dialect::Lite),
            (Engine::Duckdb, Dialect::Duck),
        ];
        const WRITES: [&str; 5] = [
            "DELETE FROM {}",
            "UPDATE {} SET a = 1",
            "INSERT INTO {} VALUES (1)",
            "DROP TABLE {}",
            "TRUNCATE TABLE {}",
        ];

        proptest! {
            #[test]
            fn is_read_only_never_panics(sql in any::<String>()) {
                for (engine, _) in ENGINES {
                    is_read_only(engine, &sql);
                }
            }

            #[test]
            fn a_write_after_a_read_is_never_read_only(table in "\\PC{1,20}", write in prop::sample::select(&WRITES[..])) {
                for (engine, d) in ENGINES {
                    let sql = format!("SELECT 1; {}", write.replace("{}", &d.ident(&table)));
                    prop_assert!(!is_read_only(engine, &sql), "{}", sql);
                }
            }

            #[test]
            fn a_select_of_any_quoted_names_is_read_only(table in "\\PC{1,20}", column in "\\PC{1,20}") {
                for (engine, d) in ENGINES {
                    let sql = format!("SELECT {} FROM {}", d.ident(&column), d.ident(&table));
                    prop_assert!(is_read_only(engine, &sql), "{}", sql);
                }
            }
        }
    }
}

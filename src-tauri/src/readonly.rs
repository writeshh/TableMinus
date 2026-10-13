//! Safe mode's read-only check.

use crate::db::Engine;
use sqlparser::ast::{Statement, visit_statements};
use sqlparser::dialect::{Dialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;
use std::ops::ControlFlow;

/// True only when every statement, including nested ones (e.g. a DELETE inside a CTE), is a plain query.
/// Anything unparseable counts as a write, so it gets the safe-mode prompt.
// ponytail: SELECT calling side-effect functions (nextval, pg_terminate_backend) or SELECT ... INTO OUTFILE
// still passes; add a function/INTO deny-list if that matters.
pub fn is_read_only(engine: Engine, sql: &str) -> bool {
    let dialect: &dyn Dialect = match engine {
        Engine::Postgres => &PostgreSqlDialect {},
        Engine::Mysql | Engine::Mariadb => &MySqlDialect {},
        Engine::Sqlite => &SQLiteDialect {},
    };
    let Ok(stmts) = Parser::parse_sql(dialect, sql) else { return false };
    !stmts.is_empty()
        && visit_statements(&stmts, |s| match s {
            Statement::Query(_) => ControlFlow::Continue(()),
            _ => ControlFlow::Break(()),
        })
        .is_continue()
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
    fn any_write_needs_confirmation() {
        assert!(!is_read_only(Postgres, "update t set a = 1"));
        assert!(!is_read_only(Postgres, "select 1; drop table t"));
        assert!(!is_read_only(Postgres, "with d as (delete from t returning *) select * from d"));
        assert!(!is_read_only(Sqlite, "explain analyze delete from t"), "EXPLAIN may execute");
    }

    #[test]
    fn unparseable_or_empty_sql_needs_confirmation() {
        assert!(!is_read_only(Postgres, "not valid sql at all"));
        assert!(!is_read_only(Postgres, ""));
    }
}

use sqlparser::ast::{visit_statements, Statement};
use sqlparser::dialect::{Dialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;
use std::ops::ControlFlow;

/// True only when every statement, including nested ones (e.g. a DELETE inside a CTE), is a plain query.
/// Anything unparseable counts as a write, so it gets the safe-mode prompt.
// ponytail: SELECT calling side-effect functions (nextval, pg_terminate_backend) or SELECT ... INTO OUTFILE
// still passes; add a function/INTO deny-list if that matters.
pub fn is_read_only(kind: &str, sql: &str) -> bool {
    let dialect: &dyn Dialect = match kind {
        "postgres" => &PostgreSqlDialect {},
        "mysql" => &MySqlDialect {},
        _ => &SQLiteDialect {},
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

    #[test]
    fn classifies_statements() {
        assert!(is_read_only("postgres", "select 1; select * from t where id = 2"));
        assert!(is_read_only("mysql", "with x as (select 1) select * from x"));
        assert!(!is_read_only("postgres", "update t set a = 1"));
        assert!(!is_read_only("postgres", "select 1; drop table t"));
        assert!(!is_read_only("postgres", "with d as (delete from t returning *) select * from d"));
        assert!(!is_read_only("sqlite", "explain analyze delete from t")); // EXPLAIN may execute; prompt
        assert!(!is_read_only("postgres", "not valid sql at all"));
        assert!(!is_read_only("postgres", ""));
    }
}

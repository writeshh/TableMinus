//! Table browsing: one page of rows with filters and sorting, built with quoted identifiers and bound values.

use crate::db::Conn;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// The most rows one page may ask for.
pub(crate) const MAX_PAGE: u32 = 100_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Browse {
    pub table: String,
    #[serde(default)]
    pub filters: Vec<Filter>,
    #[serde(default)]
    pub combine: Combine,
    #[serde(default)]
    pub sort: Vec<Sort>,
    pub limit: u32,
    pub offset: u64,
    /// Also count the rows that match, for "1–300 of N".
    #[serde(default)]
    pub count: bool,
    /// Also read each row's identity (ctid on Postgres, rowid on SQLite) where the table has one, so a change can
    /// be saved to a row its key doesn't single out.
    #[serde(default)]
    pub identity: bool,
}

/// How several filters combine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Combine {
    #[default]
    And,
    Or,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    /// None: the condition holds for any column.
    pub column: Option<String>,
    pub op: Op,
    #[serde(default)]
    pub value: String,
    /// The upper bound for Between.
    #[serde(default)]
    pub value2: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    /// `value` is a comma-separated list.
    In,
    NotIn,
    Between,
    NotBetween,
    IsNull,
    NotNull,
    IsEmpty,
    NotEmpty,
    /// `value` is a SQL condition typed by the user, used as is.
    Raw,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sort {
    pub column: String,
    #[serde(default)]
    pub desc: bool,
}

#[derive(Debug, Serialize)]
pub struct Page {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub total: Option<u64>,
    /// Each row's identity, when asked for and the table has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
}

/// A LIKE pattern matching `value` literally, with `!` as the escape character. `!` rather than backslash
/// because MySQL reads backslashes in string literals differently depending on `sql_mode`.
fn like_pattern(value: &str, prefix: bool, suffix: bool) -> String {
    let escaped = value.replace('!', "!!").replace('%', "!%").replace('_', "!_");
    format!("{}{escaped}{}", if prefix { "%" } else { "" }, if suffix { "%" } else { "" })
}

impl Conn {
    /// The column as text, for pattern matching and for conditions applied to every column.
    fn text_expr(&self, column: &str) -> String {
        let c = self.ident(column);
        match self {
            Conn::Pg(_) => format!("{c}::text"),
            Conn::My(_) => format!("cast({c} as char)"),
            Conn::Lite(_) => format!("cast({c} as text)"),
        }
    }

    fn like(&self) -> &'static str {
        if matches!(self, Conn::Pg(_)) { "ilike" } else { "like" }
    }

    /// One filter on one column. `as_text` compares the column's text form, for "any column" filters where
    /// a value typed for one column would be invalid for another's type.
    fn compare(&self, column: &str, as_text: bool, f: &Filter, params: &mut Vec<String>) -> Result<String> {
        let typed = self.ident(column);
        let text = self.text_expr(column);
        let e = if as_text { &text } else { &typed };
        let like = self.like();
        let mut bind = |v: String| {
            params.push(v);
            self.param(params.len())
        };
        let pattern = |prefix, suffix| like_pattern(&f.value, prefix, suffix);
        Ok(match f.op {
            Op::Eq => format!("{e} = {}", bind(f.value.clone())),
            Op::Ne => format!("{e} <> {}", bind(f.value.clone())),
            Op::Lt => format!("{e} < {}", bind(f.value.clone())),
            Op::Le => format!("{e} <= {}", bind(f.value.clone())),
            Op::Gt => format!("{e} > {}", bind(f.value.clone())),
            Op::Ge => format!("{e} >= {}", bind(f.value.clone())),
            Op::Contains => format!("{text} {like} {} escape '!'", bind(pattern(true, true))),
            Op::NotContains => format!("{text} not {like} {} escape '!'", bind(pattern(true, true))),
            Op::StartsWith => format!("{text} {like} {} escape '!'", bind(pattern(false, true))),
            Op::EndsWith => format!("{text} {like} {} escape '!'", bind(pattern(true, false))),
            Op::In | Op::NotIn => {
                let items: Vec<String> =
                    f.value.split(',').map(str::trim).filter(|v| !v.is_empty()).map(String::from).collect();
                if items.is_empty() {
                    return Err(Error::Invalid("IN needs at least one value, separated by commas"));
                }
                let list: Vec<String> = items.into_iter().map(&mut bind).collect();
                let not = if f.op == Op::NotIn { "not " } else { "" };
                format!("{e} {not}in ({})", list.join(", "))
            }
            Op::Between | Op::NotBetween => {
                let not = if f.op == Op::NotBetween { "not " } else { "" };
                format!("{e} {not}between {} and {}", bind(f.value.clone()), bind(f.value2.clone()))
            }
            Op::IsNull => format!("{typed} is null"),
            Op::NotNull => format!("{typed} is not null"),
            Op::IsEmpty => format!("{text} = ''"),
            Op::NotEmpty => format!("{text} <> ''"),
            Op::Raw => format!("({})", f.value),
        })
    }

    fn condition(&self, f: &Filter, columns: &[String], params: &mut Vec<String>) -> Result<String> {
        if f.op == Op::Raw {
            if f.value.trim().is_empty() {
                return Err(Error::Invalid("a raw SQL filter needs a condition"));
            }
            // Runs inside a single prepared SELECT, so it can't add statements; it can call functions, the same as
            // anything typed in the SQL editor.
            return Ok(format!("({})", f.value));
        }
        match &f.column {
            Some(c) if columns.contains(c) => self.compare(c, false, f, params),
            Some(_) => Err(Error::Invalid("a filter names a column the table doesn't have")),
            None => {
                let parts = columns.iter().map(|c| self.compare(c, true, f, params)).collect::<Result<Vec<_>>>()?;
                Ok(format!("({})", parts.join(" or ")))
            }
        }
    }

    /// The queries behind a browse request, without paging: its rows (filtered and sorted) and their count.
    pub(crate) async fn select(&mut self, req: &Browse) -> Result<Select> {
        let columns: Vec<String> = self.columns(&req.table).await?.into_iter().map(|c| c.name).collect();
        if columns.is_empty() {
            return Err(Error::Invalid("no such table, or it has no columns"));
        }
        let mut params = Vec::new();
        let conditions =
            req.filters.iter().map(|f| self.condition(f, &columns, &mut params)).collect::<Result<Vec<_>>>()?;
        let filter = if conditions.is_empty() {
            String::new()
        } else {
            let join = if req.combine == Combine::Or { " or " } else { " and " };
            format!(" where {}", conditions.join(join))
        };
        let mut order = Vec::with_capacity(req.sort.len());
        for s in &req.sort {
            if !columns.contains(&s.column) {
                return Err(Error::Invalid("a sort names a column the table doesn't have"));
            }
            order.push(format!("{} {}", self.ident(&s.column), if s.desc { "desc" } else { "asc" }));
        }
        let order = if order.is_empty() { String::new() } else { format!(" order by {}", order.join(", ")) };
        // Postgres's binary protocol would need a decoder per type; every type can be cast to text instead.
        let select: Vec<String> = columns
            .iter()
            .map(|c| match self {
                Conn::Pg(_) => format!("{} as {}", self.text_expr(c), self.ident(c)),
                _ => self.ident(c),
            })
            .collect();
        let from = self.table_ident(&req.table);
        let identity = if req.identity { self.identity(&from).await } else { None };
        let select: Vec<String> = select.into_iter().chain(identity.map(String::from)).collect();
        let count = if matches!(self, Conn::Pg(_)) { "count(*)::text" } else { "count(*)" };
        Ok(Select {
            rows: format!("select {} from {from}{filter}{order}", select.join(", ")),
            count: format!("select {count} from {from}{filter}"),
            columns,
            params,
            identity: identity.is_some(),
        })
    }

    /// The expression for a row's identity in `from`, if it has one: views and SQLite's WITHOUT ROWID tables
    /// don't, and asking is the simplest way to tell.
    async fn identity(&mut self, from: &str) -> Option<&'static str> {
        let expr = match self {
            Conn::Pg(_) => "ctid::text",
            Conn::Lite(_) => "rowid",
            Conn::My(_) => return None,
        };
        self.fetch_with(&format!("select {expr} from {from} limit 0"), &[]).await.ok().map(|_| expr)
    }

    /// The total from a [`Select::count`] query.
    pub(crate) async fn count(&mut self, s: &Select) -> Result<Option<u64>> {
        let r = self.fetch_with(&s.count, &s.bound()).await?;
        Ok(r.first().and_then(|row| row.first().cloned().flatten()).and_then(|n| n.parse().ok()))
    }

    /// One page of `req.table`. Column names in filters and sorts must belong to the table; values are bound.
    pub async fn browse(&mut self, req: &Browse) -> Result<Page> {
        let s = self.select(req).await?;
        // Limit and offset are numbers from the request type, so they're written in, not bound: MySQL refuses
        // string parameters there.
        let sql = format!("{} limit {} offset {}", s.rows, req.limit.min(MAX_PAGE), req.offset);
        let mut rows = self.fetch_with(&sql, &s.bound()).await?;
        let ids = s.identity.then(|| rows.iter_mut().map(|r| r.pop().flatten().unwrap_or_default()).collect());
        let total = if req.count { self.count(&s).await? } else { None };
        Ok(Page { columns: s.columns, rows, total, ids })
    }
}

/// The SQL for a browse request: see [`Conn::select`].
pub(crate) struct Select {
    pub columns: Vec<String>,
    /// Every matching row, sorted; add `limit … offset …` for a page.
    pub rows: String,
    pub count: String,
    pub params: Vec<String>,
    /// `rows` selects each row's identity after the columns.
    pub identity: bool,
}

impl Select {
    pub fn bound(&self) -> Vec<Option<&str>> {
        self.params.iter().map(|p| Some(p.as_str())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Browse, Combine, Filter, Op, Sort};
    use crate::db::{Conn, Engine, Options};
    use crate::tls::Tls;

    async fn people() -> Conn {
        let mut c =
            Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &Options::default(), None).await.unwrap();
        c.query("create table people(id integer primary key, name text, age integer, note text)").await.unwrap();
        c.query(
            "insert into people values (1, 'Ada', 36, '50% off'), (2, 'Bob', 25, null), (3, 'Cy', 41, 'a_b'), \
             (4, 'O''Hara', 25, '')",
        )
        .await
        .unwrap();
        c
    }

    fn request(filters: Vec<Filter>) -> Browse {
        Browse {
            table: "people".into(),
            filters,
            combine: Combine::And,
            sort: vec![Sort { column: "id".into(), desc: false }],
            limit: 100,
            offset: 0,
            count: true,
            identity: false,
        }
    }

    fn filter(column: Option<&str>, op: Op, value: &str) -> Filter {
        Filter { column: column.map(String::from), op, value: value.into(), value2: String::new() }
    }

    /// The ids of the rows `req` returns.
    async fn ids(c: &mut Conn, req: &Browse) -> Vec<String> {
        c.browse(req).await.unwrap().rows.into_iter().map(|r| r[0].clone().unwrap()).collect()
    }

    #[test]
    fn browse_should_page_and_count_all_rows_without_filters() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let page = c.browse(&Browse { limit: 2, offset: 1, ..request(vec![]) }).await.unwrap();
            assert_eq!(page.columns, ["id", "name", "age", "note"]);
            assert_eq!(page.rows.iter().map(|r| r[0].clone().unwrap()).collect::<Vec<_>>(), ["2", "3"]);
            assert_eq!(page.total, Some(4));
        });
    }

    #[test]
    fn browse_should_bind_values_so_quotes_are_just_text() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            assert_eq!(ids(&mut c, &request(vec![filter(Some("name"), Op::Eq, "O'Hara")])).await, ["4"]);
            let injection = request(vec![filter(Some("name"), Op::Eq, "x' or '1'='1")]);
            assert_eq!(ids(&mut c, &injection).await, Vec::<String>::new());
        });
    }

    #[test]
    fn browse_should_match_percent_and_underscore_literally_in_contains() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            assert_eq!(ids(&mut c, &request(vec![filter(Some("note"), Op::Contains, "%")])).await, ["1"]);
            assert_eq!(ids(&mut c, &request(vec![filter(Some("note"), Op::Contains, "_")])).await, ["3"]);
        });
    }

    #[test]
    fn browse_should_support_in_between_and_null_tests() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            assert_eq!(ids(&mut c, &request(vec![filter(Some("id"), Op::In, "1, 3")])).await, ["1", "3"]);
            let between = Filter { value2: "36".into(), ..filter(Some("age"), Op::Between, "26") };
            assert_eq!(ids(&mut c, &request(vec![between])).await, ["1"]);
            assert_eq!(ids(&mut c, &request(vec![filter(Some("note"), Op::IsNull, "")])).await, ["2"]);
            assert_eq!(ids(&mut c, &request(vec![filter(Some("note"), Op::IsEmpty, "")])).await, ["4"]);
        });
    }

    #[test]
    fn browse_should_combine_filters_with_and_or_or() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let filters = vec![filter(Some("age"), Op::Eq, "25"), filter(Some("name"), Op::StartsWith, "B")];
            assert_eq!(ids(&mut c, &request(filters.clone())).await, ["2"]);
            assert_eq!(ids(&mut c, &Browse { combine: Combine::Or, ..request(filters) }).await, ["2", "4"]);
        });
    }

    #[test]
    fn browse_should_search_every_column_when_none_is_named() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            assert_eq!(ids(&mut c, &request(vec![filter(None, Op::Contains, "41")])).await, ["3"]);
        });
    }

    #[test]
    fn browse_should_sort_by_several_columns() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let sort = vec![Sort { column: "age".into(), desc: true }, Sort { column: "name".into(), desc: false }];
            assert_eq!(ids(&mut c, &Browse { sort, ..request(vec![]) }).await, ["3", "1", "2", "4"]);
        });
    }

    #[test]
    fn browse_should_return_row_identities_only_when_asked_and_available() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let page = c.browse(&Browse { identity: true, ..request(vec![]) }).await.unwrap();
            assert_eq!(page.ids.as_deref(), Some(&["1", "2", "3", "4"].map(String::from)[..]));
            assert_eq!(page.rows[0].len(), 4, "the identity isn't one of the columns");
            assert!(c.browse(&request(vec![])).await.unwrap().ids.is_none());
            c.query("create view v as select * from people").await.unwrap();
            let view = c.browse(&Browse { table: "v".into(), identity: true, ..request(vec![]) }).await.unwrap();
            assert!(view.ids.is_none(), "a view has no rowid");
        });
    }

    #[test]
    fn browse_should_refuse_columns_the_table_does_not_have() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let err = c.browse(&request(vec![filter(Some("nope"), Op::Eq, "1")])).await.unwrap_err();
            assert_eq!(err.to_string(), "a filter names a column the table doesn't have");
            let sort = vec![Sort { column: "id; drop table people".into(), desc: false }];
            let err = c.browse(&Browse { sort, ..request(vec![]) }).await.unwrap_err();
            assert_eq!(err.to_string(), "a sort names a column the table doesn't have");
        });
    }

    #[test]
    fn browse_should_use_a_raw_condition_as_typed() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            assert_eq!(
                ids(&mut c, &request(vec![filter(None, Op::Raw, "age > 30 and note is not null")])).await,
                ["1", "3"]
            );
        });
    }

    #[test]
    fn browse_should_not_let_a_raw_condition_add_a_statement() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            assert!(c.browse(&request(vec![filter(None, Op::Raw, "1=1; drop table people")])).await.is_err());
            assert_eq!(ids(&mut c, &request(vec![])).await.len(), 4, "the table is still there");
        });
    }

    /// Typed MySQL values through prepared statements read like the text protocol's. Creates and drops a
    /// scratch database, because `information_schema` doesn't list temporary tables.
    #[test]
    #[ignore = "needs a local MySQL; creates and drops a scratch database"]
    fn local_mysql_browse_formats_typed_values() {
        tauri::async_runtime::block_on(async {
            let url = "mysql://root@127.0.0.1:3306";
            let mut c = Conn::open(Engine::Mysql, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let db = format!("dbm_scratch_{}", std::process::id());
            c.query(&format!("create database {db}")).await.unwrap();
            c.use_database(&db).await.unwrap();
            c.query(
                "create table t(id int primary key, d date, dt datetime(3), tm time, n decimal(6,2), f double, \
                 s varchar(20), b blob)",
            )
            .await
            .unwrap();
            c.query(
                "insert into t values (1, '2026-01-02', '2026-01-02 03:04:05.678', '-27:15:00', 12.50, 1.5, \
                     'it''s', 'raw'), (2, null, null, null, null, null, 'x', null)",
            )
            .await
            .unwrap();
            let req = Browse {
                table: "t".into(),
                filters: vec![filter(Some("s"), Op::Eq, "it's")],
                combine: Combine::And,
                sort: vec![],
                limit: 10,
                offset: 0,
                count: true,
                identity: false,
            };
            let page = c.browse(&req).await;
            c.query(&format!("drop database {db}")).await.unwrap();
            let page = page.unwrap();
            let row: Vec<Option<&str>> = page.rows[0].iter().map(|v| v.as_deref()).collect();
            assert_eq!(
                row,
                [
                    Some("1"),
                    Some("2026-01-02"),
                    Some("2026-01-02 03:04:05.678"),
                    Some("-27:15:00"),
                    Some("12.50"),
                    Some("1.5"),
                    Some("it's"),
                    Some("raw")
                ]
            );
            assert_eq!(page.total, Some(1));
        });
    }
}

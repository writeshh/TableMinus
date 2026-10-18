//! Table browsing: one page of rows with filters and sorting, built with quoted identifiers and bound values.

use crate::db::{Column, Conn, Dialect};
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
    /// With `count` and no filters: when the catalog estimates more rows than this, the estimate is the total
    /// and nothing is counted (an exact count reads the whole table).
    #[serde(default)]
    pub estimate_over: Option<u64>,
    /// Also read each row's identity (ctid on Postgres, rowid on SQLite) where the table has one, so a change can
    /// be saved to a row its key doesn't single out.
    #[serde(default)]
    pub identity: bool,
    /// `PostGIS` geometry and geography as extended WKT (`SRID=4326;POINT(1 2)`, which also reads back in when
    /// edited) rather than hex WKB.
    #[serde(default)]
    pub wkt: bool,
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
    /// `total` is the catalog's estimate (see [`Browse::estimate_over`]).
    pub estimated: bool,
    /// Each row's identity, when asked for and the table has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
}

/// `rows` (a SELECT) cut to `limit` rows from `offset`. SQL Server pages with OFFSET … FETCH, which needs an ORDER BY,
/// so an unordered one gets the no-op `order by (select null)`.
pub(crate) fn page_clause(d: Dialect, rows: &str, ordered: bool, limit: u32, offset: u64) -> String {
    if d == Dialect::Mssql {
        let order = if ordered { "" } else { " order by (select null)" };
        format!("{rows}{order} offset {offset} rows fetch next {limit} rows only")
    } else {
        format!("{rows} limit {limit} offset {offset}")
    }
}

/// A LIKE pattern matching `value` literally, with `!` as the escape character. `!` rather than backslash
/// because MySQL reads backslashes in string literals differently depending on `sql_mode`.
fn like_pattern(d: Dialect, value: &str, prefix: bool, suffix: bool) -> String {
    // `ClickHouse` has no ESCAPE clause: a backslash is its fixed escape character.
    let esc = if d == Dialect::Click { '\\' } else { '!' };
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '%' | '_') || c == esc {
            escaped.push(esc);
        }
        escaped.push(c);
    }
    // `[` too: SQL Server reads `[a-z]` in a pattern as a range.
    if d == Dialect::Mssql {
        escaped = escaped.replace('[', "![");
    }
    format!("{}{escaped}{}", if prefix { "%" } else { "" }, if suffix { "%" } else { "" })
}

impl Conn {
    /// The column as text, for pattern matching and for conditions applied to every column.
    fn text_expr(&self, column: &str) -> String {
        let c = self.ident(column);
        match self.dialect() {
            Dialect::Pg => format!("{c}::text"),
            Dialect::My => format!("cast({c} as char)"),
            Dialect::Lite => format!("cast({c} as text)"),
            Dialect::Duck => format!("cast({c} as varchar)"),
            Dialect::Mssql => format!("cast({c} as nvarchar(max))"),
            Dialect::Click => format!("toString({c})"),
        }
    }

    fn like(&self) -> &'static str {
        if matches!(self.dialect(), Dialect::Pg | Dialect::Duck | Dialect::Click) { "ilike" } else { "like" }
    }

    /// One filter on one column. `as_text` compares the column's text form, for "any column" filters where
    /// a value typed for one column would be invalid for another's type.
    fn compare(&self, column: &str, as_text: bool, f: &Filter, params: &mut Values<'_>) -> Result<String> {
        let typed = self.ident(column);
        let text = self.text_expr(column);
        let e = if as_text { &text } else { &typed };
        let like = self.like();
        let mut bind = |v: String| match params {
            Values::Bound(list) => {
                list.push(v);
                self.param(list.len())
            }
            Values::Shown => self.dialect().literal(&v),
        };
        let d = self.dialect();
        let pattern = |prefix, suffix| like_pattern(d, &f.value, prefix, suffix);
        let escape = if d == Dialect::Click { "" } else { " escape '!'" };
        Ok(match f.op {
            Op::Eq => format!("{e} = {}", bind(f.value.clone())),
            Op::Ne => format!("{e} <> {}", bind(f.value.clone())),
            Op::Lt => format!("{e} < {}", bind(f.value.clone())),
            Op::Le => format!("{e} <= {}", bind(f.value.clone())),
            Op::Gt => format!("{e} > {}", bind(f.value.clone())),
            Op::Ge => format!("{e} >= {}", bind(f.value.clone())),
            Op::Contains => format!("{text} {like} {}{escape}", bind(pattern(true, true))),
            Op::NotContains => format!("{text} not {like} {}{escape}", bind(pattern(true, true))),
            Op::StartsWith => format!("{text} {like} {}{escape}", bind(pattern(false, true))),
            Op::EndsWith => format!("{text} {like} {}{escape}", bind(pattern(true, false))),
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

    fn condition(&self, f: &Filter, columns: &[String], params: &mut Values<'_>) -> Result<String> {
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

    /// The table's columns, and the ` where …` and ` order by …` of a browse request; values go into `params`.
    async fn clauses(&mut self, req: &Browse, params: &mut Values<'_>) -> Result<(Vec<Column>, String, String)> {
        let table_columns = self.columns(&req.table).await?;
        let columns: Vec<String> = table_columns.iter().map(|c| c.name.clone()).collect();
        if columns.is_empty() {
            return Err(Error::Invalid("no such table, or it has no columns"));
        }
        let conditions = req.filters.iter().map(|f| self.condition(f, &columns, params)).collect::<Result<Vec<_>>>()?;
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
        Ok((table_columns, filter, order))
    }

    /// The queries behind a browse request, without paging: its rows (filtered and sorted) and their count.
    pub(crate) async fn select(&mut self, req: &Browse) -> Result<Select> {
        let mut params = Vec::new();
        let (table_columns, filter, order) = self.clauses(req, &mut Values::Bound(&mut params)).await?;
        let spatial: Vec<bool> =
            table_columns.iter().map(|c| req.wkt && matches!(c.data_type.as_str(), "geometry" | "geography")).collect();
        let columns: Vec<String> = table_columns.into_iter().map(|c| c.name).collect();
        // Postgres's binary protocol would need a decoder per type; every type can be cast to text instead.
        let select: Vec<String> = columns
            .iter()
            .zip(&spatial)
            .map(|(c, &spatial)| match self.dialect() {
                Dialect::Pg if spatial => format!("st_asewkt({0}) as {0}", self.ident(c)),
                Dialect::Pg => format!("{} as {}", self.text_expr(c), self.ident(c)),
                _ => self.ident(c),
            })
            .collect();
        let from = self.table_ident(&req.table);
        let identity = if req.identity { self.identity(&from).await } else { None };
        let select: Vec<String> = select.into_iter().chain(identity.map(String::from)).collect();
        let count = if self.dialect() == Dialect::Pg { "count(*)::text" } else { "count(*)" };
        Ok(Select {
            ordered: !order.is_empty(),
            rows: format!("select {} from {from}{filter}{order}", select.join(", ")),
            count: format!("select {count} from {from}{filter}"),
            columns,
            params,
            identity: identity.is_some(),
        })
    }

    /// The page a browse request reads, as SQL to show the user (the filter bar's "SQL"): values are written in
    /// as quoted literals. Only for display; running uses [`Conn::select`], which binds them.
    pub async fn filter_sql(&mut self, req: &Browse) -> Result<String> {
        let (_, filter, order) = self.clauses(req, &mut Values::Shown).await?;
        let from = self.table_ident(&req.table);
        let rows = format!("select * from {from}{filter}{order}");
        Ok(page_clause(self.dialect(), &rows, !order.is_empty(), req.limit.min(MAX_PAGE), req.offset))
    }

    /// The expression for a row's identity in `from`, if it has one: views and SQLite's WITHOUT ROWID tables
    /// don't, and asking is the simplest way to tell.
    async fn identity(&mut self, from: &str) -> Option<&'static str> {
        let expr = match self.dialect() {
            Dialect::Pg => "ctid::text",
            Dialect::Lite => "rowid",
            // DuckDB's rowid moves when rows are updated, so it can't name a row to edit later.
            Dialect::My | Dialect::Duck | Dialect::Mssql | Dialect::Click => return None,
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
        let sql = s.page(self.dialect(), req.limit.min(MAX_PAGE), req.offset);
        let mut rows = self.fetch_with(&sql, &s.bound()).await?;
        let ids = s.identity.then(|| rows.iter_mut().map(|r| r.pop().flatten().unwrap_or_default()).collect());
        let estimate = match req.estimate_over {
            Some(over) if req.count && req.filters.is_empty() => self.estimate(&req.table, over).await,
            _ => None,
        };
        let total = match estimate {
            Some(n) => Some(n),
            None if req.count => self.count(&s).await?,
            None => None,
        };
        Ok(Page { columns: s.columns, rows, total, estimated: estimate.is_some(), ids })
    }

    /// The catalog's row estimate for `table`, when it is one and above `over`. SQLite keeps none.
    async fn estimate(&mut self, table: &str, over: u64) -> Option<u64> {
        if self.dialect() == Dialect::Lite {
            return None;
        }
        // No estimate (never analyzed, or the catalog can't say) just means counting.
        let o = self.overview(table).await.ok()?;
        o.rows.filter(|&n| o.estimated && n > over)
    }
}

/// Where filter values go: bound as parameters to run the query, or written in as literals to show it.
enum Values<'p> {
    Bound(&'p mut Vec<String>),
    Shown,
}

/// The SQL for a browse request: see [`Conn::select`].
pub(crate) struct Select {
    pub columns: Vec<String>,
    /// Every matching row, sorted; [`Select::page`] makes a page of it.
    pub rows: String,
    /// `rows` has an ORDER BY, which SQL Server's paging needs.
    pub ordered: bool,
    pub count: String,
    pub params: Vec<String>,
    /// `rows` selects each row's identity after the columns.
    pub identity: bool,
}

impl Select {
    /// `rows` cut to one page. Limit and offset are numbers from the request type, so they're written in, not
    /// bound (MySQL refuses string parameters there). SQL Server pages with OFFSET … FETCH, which needs an ORDER BY.
    pub fn page(&self, d: Dialect, limit: u32, offset: u64) -> String {
        page_clause(d, &self.rows, self.ordered, limit, offset)
    }

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
            estimate_over: None,
            identity: false,
            wkt: false,
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

    #[test]
    fn browse_should_count_sqlite_rows_exactly_whatever_the_estimate_threshold() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let page = c.browse(&Browse { estimate_over: Some(0), ..request(vec![]) }).await.unwrap();
            assert_eq!((page.total, page.estimated), (Some(4), false));
        });
    }

    /// A scratch database on the local Postgres (`DBM_PG_URL` points elsewhere): the admin connection, one to the
    /// scratch database, and its name, for dropping it.
    async fn pg_scratch(prefix: &str) -> (Conn, Conn, String) {
        let url = std::env::var("DBM_PG_URL").unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5432/postgres".into());
        let open = |url: String| async move {
            Conn::open(Engine::Postgres, &url, None, &Tls::default(), &Options::default(), None).await.unwrap()
        };
        let mut admin = open(url.clone()).await;
        let db = format!("{prefix}_{}", std::process::id());
        admin.query(&format!("create database {db}")).await.unwrap();
        let mut u = url::Url::parse(&url).unwrap();
        u.set_path(&db);
        (admin, open(u.to_string()).await, db)
    }

    /// The catalog estimate stands in for the count above the threshold, and only without filters.
    #[test]
    #[ignore = "needs a local Postgres; creates and drops a scratch database"]
    fn local_postgres_browse_should_estimate_large_tables() {
        tauri::async_runtime::block_on(async {
            let (mut admin, mut c, db) = pg_scratch("dbm_estimate").await;
            c.query("create table people(id int primary key, name text)").await.unwrap();
            c.query("insert into people select n, 'p' || n from generate_series(1, 500) n").await.unwrap();
            c.query("analyze people").await.unwrap();
            let estimated = c.browse(&Browse { estimate_over: Some(100), ..request(vec![]) }).await.unwrap();
            let small = c.browse(&Browse { estimate_over: Some(1000), ..request(vec![]) }).await.unwrap();
            let filtered = Browse { estimate_over: Some(100), ..request(vec![filter(Some("id"), Op::Le, "10")]) };
            let filtered = c.browse(&filtered).await.unwrap();
            drop(c);
            admin.query(&format!("drop database {db}")).await.unwrap();
            assert_eq!((estimated.total, estimated.estimated), (Some(500), true));
            assert_eq!((small.total, small.estimated), (Some(500), false));
            assert_eq!((filtered.total, filtered.estimated), (Some(10), false));
        });
    }

    /// Spatial values read as extended WKT when asked, as hex WKB otherwise. Needs `PostGIS` on the server.
    #[test]
    #[ignore = "needs a local Postgres with PostGIS; creates and drops a scratch database"]
    fn local_postgres_browse_should_show_geometry_as_wkt_when_asked() {
        tauri::async_runtime::block_on(async {
            let (mut admin, mut c, db) = pg_scratch("dbm_wkt").await;
            let rows = async {
                c.query("create extension postgis").await?;
                c.query("create table people(id int primary key, at geometry(Point, 4326))").await?;
                c.query("insert into people values (1, 'SRID=4326;POINT(1 2)')").await?;
                let wkt = c.browse(&Browse { wkt: true, ..request(vec![]) }).await?.rows;
                let hex = c.browse(&request(vec![])).await?.rows;
                Ok::<_, crate::error::Error>((wkt, hex))
            }
            .await;
            drop(c);
            admin.query(&format!("drop database {db}")).await.unwrap();
            let (wkt, hex) = rows.unwrap();
            assert_eq!(wkt[0][1].as_deref(), Some("SRID=4326;POINT(1 2)"));
            assert!(hex[0][1].as_deref().is_some_and(|h| h.starts_with("0101000020E6100000")), "{hex:?}");
        });
    }

    #[test]
    fn filter_sql_should_show_values_as_escaped_literals() {
        tauri::async_runtime::block_on(async {
            let mut c = people().await;
            let req = Browse {
                limit: 300,
                ..request(vec![filter(Some("name"), Op::Eq, "O'Hara"), filter(None, Op::IsNull, "")])
            };
            let sql = c.filter_sql(&req).await.unwrap();
            assert_eq!(
                sql,
                "select * from \"people\" where \"name\" = 'O''Hara' and (\"id\" is null or \"name\" is null or \
                 \"age\" is null or \"note\" is null) order by \"id\" asc limit 300 offset 0"
            );
            assert_eq!(c.fetch_with(&sql, &[]).await.unwrap().len(), 0, "the shown SQL runs as is");
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
                estimate_over: None,
                identity: false,
                wkt: false,
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

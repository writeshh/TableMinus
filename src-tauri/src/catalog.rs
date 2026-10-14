//! What the left sidebar lists besides tables: schemas, views, materialized views, functions and procedures,
//! sequences and user-defined types; plus "search in database" and a table's overview (rows, size).

use crate::browse::{Browse, Combine, Filter, Op};
use crate::db::Conn;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// The most matching rows "search in database" counts in one table; past this it reports "1,000+".
pub const SEARCH_CAP: u32 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Schema,
    View,
    Matview,
    Function,
    Procedure,
    Sequence,
    Type,
}

impl ItemKind {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "schema" => Self::Schema,
            "view" => Self::View,
            "matview" => Self::Matview,
            "function" => Self::Function,
            "procedure" => Self::Procedure,
            "sequence" => Self::Sequence,
            "type" => Self::Type,
            _ => return None,
        })
    }
}

/// A sidebar item other than a table. Postgres names are `schema.name`.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Item {
    pub kind: ItemKind,
    pub name: String,
}

/// How "search in database" compares the text with each column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
}

/// A table's size at a glance. Postgres's row count is the planner's estimate (`estimated`).
#[derive(Debug, Serialize)]
pub struct Overview {
    pub rows: Option<u64>,
    pub estimated: bool,
    pub bytes: Option<u64>,
    pub created: Option<String>,
}

/// Leaves out Postgres's own schemas unless the connection shows system schemas.
fn pg_user_schemas(system: bool) -> &'static str {
    if system { "true" } else { "n.nspname !~ '^pg_' and n.nspname <> 'information_schema'" }
}

impl Conn {
    /// Everything but tables, sorted by name within each kind. Functions and types that belong to an extension
    /// are left out (pgcrypto alone adds dozens). `system` adds Postgres's catalog schemas.
    pub async fn items(&mut self, system: bool) -> Result<Vec<Item>> {
        let sql = match self {
            Conn::Pg(_) => {
                let user = pg_user_schemas(system);
                let not_extension = "not exists (select 1 from pg_depend d where d.objid = {oid} and d.deptype = 'e')";
                format!(
                    "select distinct * from (\
                     select 'schema', n.nspname::text from pg_namespace n where {user} \
                     union all select case c.relkind when 'v' then 'view' when 'm' then 'matview' else 'sequence' end, \
                       n.nspname || '.' || c.relname from pg_class c join pg_namespace n on n.oid = c.relnamespace \
                       where c.relkind in ('v', 'm', 'S') and {user} \
                     union all select case p.prokind when 'p' then 'procedure' else 'function' end, n.nspname || '.' || p.proname \
                       from pg_proc p join pg_namespace n on n.oid = p.pronamespace \
                       where p.prokind in ('f', 'p') and {user} and {proc_own} \
                     union all select 'type', n.nspname || '.' || t.typname from pg_type t join pg_namespace n on n.oid = t.typnamespace \
                       where (t.typtype in ('e', 'd', 'r', 'm') or (t.typtype = 'c' and exists \
                         (select 1 from pg_class c where c.oid = t.typrelid and c.relkind = 'c'))) and {user} and {type_own}\
                     ) i order by 1, 2",
                    proc_own = not_extension.replace("{oid}", "p.oid"),
                    type_own = not_extension.replace("{oid}", "t.oid"),
                )
            }
            Conn::My(_) => "select 'view', table_name from information_schema.tables \
                 where table_schema = database() and table_type = 'VIEW' \
                 union all select 'sequence', table_name from information_schema.tables \
                 where table_schema = database() and table_type = 'SEQUENCE' \
                 union all select lower(routine_type), routine_name from information_schema.routines \
                 where routine_schema = database() order by 1, 2"
                .into(),
            Conn::Lite(_) => "select 'view', name from sqlite_master where type = 'view' order by 2".into(),
        };
        let rows = self.fetch_with(&sql, &[]).await?;
        Ok(rows
            .into_iter()
            .filter_map(|mut r| {
                let name = r.pop().flatten()?;
                Some(Item { kind: ItemKind::parse(&r.pop().flatten()?)?, name })
            })
            .collect())
    }

    /// How many rows of `table` have `text` in some column (as the filter bar's "any column" does), counting no
    /// further than [`SEARCH_CAP`]. The text is bound as a LIKE pattern with its wildcards escaped.
    pub async fn search_count(&mut self, table: &str, mode: SearchMode, text: &str) -> Result<u64> {
        if text.is_empty() {
            return Err(Error::Invalid("enter some text to search for"));
        }
        let op = match mode {
            SearchMode::Contains => Op::Contains,
            SearchMode::NotContains => Op::NotContains,
            SearchMode::StartsWith => Op::StartsWith,
            SearchMode::EndsWith => Op::EndsWith,
        };
        let req = Browse {
            table: table.into(),
            filters: vec![Filter { column: None, op, value: text.into(), value2: String::new() }],
            combine: Combine::And,
            sort: vec![],
            limit: SEARCH_CAP,
            offset: 0,
            count: false,
            estimate_over: None,
            identity: false,
        };
        let s = self.select(&req).await?;
        let count = if matches!(self, Conn::Pg(_)) { "count(*)::text" } else { "count(*)" };
        let sql = format!("select {count} from ({} limit {SEARCH_CAP}) found", s.rows);
        let r = self.fetch_with(&sql, &s.bound()).await?;
        Ok(r.first().and_then(|row| row.first().cloned().flatten()).and_then(|n| n.parse().ok()).unwrap_or(0))
    }

    /// Rows, size on disk and creation time of a table or view, as far as the engine keeps them.
    pub async fn overview(&mut self, table: &str) -> Result<Overview> {
        match self {
            Conn::Pg(_) => {
                let (schema, name) = table.split_once('.').unwrap_or(("public", table));
                let r = self
                    .fetch_with(
                        "select c.reltuples::bigint::text, pg_total_relation_size(c.oid)::text from pg_class c \
                         join pg_namespace n on n.oid = c.relnamespace where n.nspname = $1 and c.relname = $2",
                        &[Some(schema), Some(name)],
                    )
                    .await?;
                let row = r.into_iter().next().ok_or(Error::Invalid("no such table"))?;
                let num = |i: usize| row.get(i).cloned().flatten().and_then(|n| n.parse::<i64>().ok());
                // -1: never analyzed (Postgres 14+), so there's no estimate yet.
                let rows = num(0).and_then(|n| u64::try_from(n).ok());
                Ok(Overview { rows, estimated: true, bytes: num(1).and_then(|n| u64::try_from(n).ok()), created: None })
            }
            Conn::My(_) => {
                let r = self
                    .fetch_with(
                        "select table_rows, data_length + index_length, create_time, table_type \
                         from information_schema.tables where table_schema = database() and table_name = ?",
                        &[Some(table)],
                    )
                    .await?;
                let row = r.into_iter().next().ok_or(Error::Invalid("no such table"))?;
                let num = |i: usize| row.get(i).cloned().flatten().and_then(|n| n.parse::<u64>().ok());
                let created = row.get(2).cloned().flatten();
                // InnoDB's table_rows is an estimate too.
                let innodb = row.get(3).cloned().flatten().is_some_and(|t| t == "BASE TABLE");
                Ok(Overview { rows: num(0), estimated: innodb, bytes: num(1), created })
            }
            Conn::Lite(_) => {
                let count = format!("select count(*) from {}", self.table_ident(table));
                let rows = self.fetch_with(&count, &[]).await?;
                let rows = rows.first().and_then(|r| r.first().cloned().flatten()).and_then(|n| n.parse().ok());
                // dbstat is only there when SQLite was built with it; without it the size stays unknown.
                let bytes =
                    match self.fetch_with("select sum(pgsize) from dbstat where name = $1", &[Some(table)]).await {
                        Ok(r) => r.first().and_then(|r| r.first().cloned().flatten()).and_then(|n| n.parse().ok()),
                        Err(_) => None,
                    };
                Ok(Overview { rows, estimated: false, bytes, created: None })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Item, ItemKind, SEARCH_CAP, SearchMode};
    use crate::db::{Conn, Engine, Options};
    use crate::tls::Tls;

    async fn shop() -> Conn {
        let mut c =
            Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &Options::default(), None).await.unwrap();
        c.query("create table people(id integer primary key, name text, note text)").await.unwrap();
        c.query("insert into people values (1, 'Ada', '50% off'), (2, 'Bob', null), (3, 'Cyd', 'a_b')").await.unwrap();
        c.query("create view adults as select * from people").await.unwrap();
        c
    }

    #[test]
    fn items_should_list_sqlite_views_but_not_tables() {
        tauri::async_runtime::block_on(async {
            let mut c = shop().await;
            assert_eq!(c.items(false).await.unwrap(), [Item { kind: ItemKind::View, name: "adults".into() }]);
        });
    }

    #[test]
    fn search_count_should_match_any_column_and_treat_wildcards_literally() {
        tauri::async_runtime::block_on(async {
            let mut c = shop().await;
            assert_eq!(c.search_count("people", SearchMode::Contains, "b").await.unwrap(), 2); // Bob, a_b
            assert_eq!(c.search_count("people", SearchMode::Contains, "%").await.unwrap(), 1);
            assert_eq!(c.search_count("people", SearchMode::Contains, "_").await.unwrap(), 1);
            assert_eq!(c.search_count("people", SearchMode::StartsWith, "cy").await.unwrap(), 1);
            assert_eq!(c.search_count("people", SearchMode::EndsWith, "off").await.unwrap(), 1);
        });
    }

    #[test]
    fn search_count_should_stop_counting_at_the_cap() {
        tauri::async_runtime::block_on(async {
            let mut c = shop().await;
            c.query("create table many(v text)").await.unwrap();
            c.query(
                "insert into many with recursive n(i) as (select 1 union all select i + 1 from n where i < 1500) \
                     select 'x' || i from n",
            )
            .await
            .unwrap();
            assert_eq!(c.search_count("many", SearchMode::Contains, "x").await.unwrap(), u64::from(SEARCH_CAP));
        });
    }

    #[test]
    fn search_count_should_refuse_empty_text() {
        tauri::async_runtime::block_on(async {
            let mut c = shop().await;
            assert!(c.search_count("people", SearchMode::Contains, "").await.is_err());
        });
    }

    #[test]
    fn overview_should_count_sqlite_rows_exactly() {
        tauri::async_runtime::block_on(async {
            let mut c = shop().await;
            let o = c.overview("people").await.unwrap();
            assert_eq!((o.rows, o.estimated), (Some(3), false));
        });
    }

    /// Runs in a scratch database, dropped afterwards whatever happens.
    #[test]
    #[ignore = "needs a local MySQL; creates and drops a scratch database"]
    fn local_mysql_items_search_and_overview() {
        tauri::async_runtime::block_on(async {
            let url = "mysql://root@127.0.0.1:3306";
            let mut c = Conn::open(Engine::Mysql, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let db = format!("dbm_catalog_{}", std::process::id());
            c.query(&format!("create database {db}")).await.unwrap();
            c.use_database(&db).await.unwrap();
            let outcome = mysql_scenario(&mut c).await;
            c.query(&format!("drop database {db}")).await.unwrap();
            outcome.unwrap();
        });
    }

    /// Runs in a scratch database, dropped afterwards. `DBM_PG_URL` points elsewhere than the default server.
    #[test]
    #[ignore = "needs a local Postgres; creates and drops a scratch database"]
    fn local_postgres_items_search_and_overview() {
        tauri::async_runtime::block_on(async {
            let url =
                std::env::var("DBM_PG_URL").unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5432/postgres".into());
            let open = |url: String| async move {
                Conn::open(Engine::Postgres, &url, None, &Tls::default(), &Options::default(), None).await.unwrap()
            };
            let mut admin = open(url.clone()).await;
            let db = format!("dbm_catalog_{}", std::process::id());
            admin.query(&format!("create database {db}")).await.unwrap();
            let mut u = url::Url::parse(&url).unwrap();
            u.set_path(&db);
            let mut c = open(u.to_string()).await;
            let outcome = postgres_scenario(&mut c).await;
            drop(c);
            admin.query(&format!("drop database {db}")).await.unwrap();
            outcome.unwrap();
        });
    }

    async fn postgres_scenario(c: &mut Conn) -> crate::error::Result<()> {
        c.query(
            "create schema shop; create table shop.people(id serial primary key, name text); \
             insert into shop.people(name) values ('Ada'), ('Bob'); analyze shop.people; \
             create view shop.v_people as select name from shop.people; \
             create materialized view shop.mv_people as select count(*) from shop.people; \
             create function shop.twice(x int) returns int language sql as 'select x * 2'; \
             create function shop.twice(x text) returns text language sql as 'select x || x'; \
             create procedure shop.noop() language sql as ''; \
             create type shop.mood as enum ('ok', 'sad'); create type shop.pair as (a int, b int); \
             create extension if not exists pg_trgm",
        )
        .await?;
        let items = c.items(false).await?;
        let got: Vec<(ItemKind, &str)> = items.iter().map(|i| (i.kind, i.name.as_str())).collect();
        assert_eq!(
            got,
            [
                (ItemKind::Function, "shop.twice"), // overloads listed once
                (ItemKind::Matview, "shop.mv_people"),
                (ItemKind::Procedure, "shop.noop"),
                (ItemKind::Schema, "public"),
                (ItemKind::Schema, "shop"),
                (ItemKind::Sequence, "shop.people_id_seq"),
                (ItemKind::Type, "shop.mood"),
                (ItemKind::Type, "shop.pair"),
                (ItemKind::View, "shop.v_people"),
            ],
            "pg_trgm's functions and types stay out"
        );
        assert_eq!(c.search_count("shop.people", SearchMode::Contains, "b").await?, 1);
        let o = c.overview("shop.people").await?;
        assert_eq!((o.rows, o.estimated), (Some(2), true));
        assert!(o.bytes.is_some_and(|b| b > 0));
        Ok(())
    }

    async fn mysql_scenario(c: &mut Conn) -> crate::error::Result<()> {
        c.query("create table people(id int primary key, name varchar(20))").await?;
        c.query("insert into people values (1, 'Ada'), (2, 'Bob')").await?;
        c.query("create view v_people as select name from people").await?;
        c.query("create function twice(x int) returns int deterministic return x * 2").await?;
        c.query("create procedure noop() begin end").await?;
        let items = c.items(false).await?;
        let kinds: Vec<(ItemKind, &str)> = items.iter().map(|i| (i.kind, i.name.as_str())).collect();
        assert_eq!(kinds, [(ItemKind::Function, "twice"), (ItemKind::Procedure, "noop"), (ItemKind::View, "v_people")]);
        assert_eq!(c.search_count("people", SearchMode::Contains, "b").await?, 1);
        assert_eq!(c.search_count("people", SearchMode::NotContains, "a").await?, 2); // the id column never has an "a"
        assert!(c.overview("people").await?.rows.is_some());
        Ok(())
    }
}

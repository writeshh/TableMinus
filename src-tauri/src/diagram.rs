//! The ER diagram's data: every table and view of a schema with its columns, and the foreign keys between them,
//! read in one query per engine. Also writes the diagram's PNG export.

use crate::db::Conn;
use crate::error::{Error, Result};
use serde::Serialize;
use std::path::PathBuf;

/// Separates list items inside one catalog cell (the ASCII unit separator, which no identifier contains).
const SEP: char = '\u{1f}';

/// The largest PNG the export writes; a diagram of thousands of tables at 2x stays well under it.
const MAX_IMAGE: usize = 256 * 1024 * 1024;

#[derive(Debug, Serialize, PartialEq)]
pub struct Diagram {
    pub tables: Vec<Table>,
    pub links: Vec<Link>,
}

/// A table or view. Postgres names are `schema.table`, as the sidebar lists them.
#[derive(Debug, Serialize, PartialEq)]
pub struct Table {
    pub name: String,
    pub view: bool,
    pub columns: Vec<Column>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub primary: bool,
    pub nullable: bool,
}

/// A foreign key: `table`'s `columns` reference `ref_table`'s `ref_columns`, position by position. A SQLite key
/// that references the primary key implicitly has an empty name in `ref_columns`.
#[derive(Debug, Serialize, PartialEq)]
pub struct Link {
    pub name: String,
    pub table: String,
    pub columns: Vec<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

// Each query returns one row per column ('c': table, column, type, primary, nullable, view, position) and one per
// foreign key ('f': table, name, columns, referenced table, referenced columns, null, 0), so the whole diagram
// comes in a single round trip. Flags are 'true'/'false' text.

/// `$1` is the schema; NULL means the connection's current one. Partitions are left out (their parent shows).
const PG: &str = "select 'c', n.nspname || '.' || c.relname, a.attname::text, format_type(a.atttypid, a.atttypmod), \
     coalesce(a.attnum = any(p.conkey), false)::text, (not a.attnotnull)::text, (c.relkind in ('v', 'm'))::text, \
     a.attnum::text \
     from pg_class c join pg_namespace n on n.oid = c.relnamespace \
     join pg_attribute a on a.attrelid = c.oid and a.attnum > 0 and not a.attisdropped \
     left join pg_constraint p on p.conrelid = c.oid and p.contype = 'p' \
     where c.relkind in ('r', 'p', 'v', 'm', 'f') and not c.relispartition \
       and n.nspname = coalesce($1, current_schema()) \
     union all select 'f', n.nspname || '.' || c.relname, k.conname::text, \
     array_to_string(array(select a.attname from unnest(k.conkey) with ordinality u(n, i) \
       join pg_attribute a on a.attrelid = k.conrelid and a.attnum = u.n order by u.i), chr(31)), \
     (select rn.nspname || '.' || r.relname from pg_class r join pg_namespace rn on rn.oid = r.relnamespace \
       where r.oid = k.confrelid), \
     array_to_string(array(select a.attname from unnest(k.confkey) with ordinality u(n, i) \
       join pg_attribute a on a.attrelid = k.confrelid and a.attnum = u.n order by u.i), chr(31)), \
     null, '0' \
     from pg_constraint k join pg_class c on c.oid = k.conrelid join pg_namespace n on n.oid = c.relnamespace \
     where k.contype = 'f' and not c.relispartition and n.nspname = coalesce($1, current_schema())";

/// The current database. A foreign key into another database names its table `database.table`.
const MY: &str = "select 'c', c.table_name, c.column_name, c.column_type, if(c.column_key = 'PRI', 'true', 'false'), \
     if(c.is_nullable = 'YES', 'true', 'false'), if(t.table_type = 'VIEW', 'true', 'false'), \
     cast(c.ordinal_position as char) \
     from information_schema.columns c join information_schema.tables t \
       on t.table_schema = c.table_schema and t.table_name = c.table_name \
     where c.table_schema = database() \
     union all select 'f', k.table_name, k.constraint_name, \
     group_concat(k.column_name order by k.ordinal_position separator 0x1f), \
     min(if(k.referenced_table_schema = database(), k.referenced_table_name, \
       concat(k.referenced_table_schema, '.', k.referenced_table_name))), \
     group_concat(k.referenced_column_name order by k.ordinal_position separator 0x1f), null, '0' \
     from information_schema.key_column_usage k \
     where k.table_schema = database() and k.referenced_table_name is not null \
     group by k.table_name, k.constraint_name";

/// SQLite's own tables (`sqlite_sequence`…) are left out. Foreign keys have no names; their id numbers them.
// ponytail: group_concat follows pragma_foreign_key_list's seq order in practice, though SQLite doesn't promise it.
const LITE: &str = "select 'c', m.name, p.name, p.type, case when p.pk > 0 then 'true' else 'false' end, \
     case when p.\"notnull\" = 0 then 'true' else 'false' end, case when m.type = 'view' then 'true' else 'false' end, \
     p.cid \
     from sqlite_master m join pragma_table_info(m.name) p \
     where m.type in ('table', 'view') and m.name not like 'sqlite\\_%' escape '\\' \
     union all select 'f', m.name, 'fk_' || f.id, group_concat(f.\"from\", char(31)), min(f.\"table\"), \
     group_concat(coalesce(f.\"to\", ''), char(31)), null, 0 \
     from sqlite_master m join pragma_foreign_key_list(m.name) f \
     where m.type = 'table' and m.name not like 'sqlite\\_%' escape '\\' \
     group by m.name, f.id";

fn list(cell: Option<String>) -> Vec<String> {
    cell.map(|c| c.split(SEP).map(String::from).collect()).unwrap_or_default()
}

/// Turns the query's rows into tables (by name, columns in table order) and foreign keys (by table and name).
fn assemble(rows: Vec<Vec<Option<String>>>) -> Diagram {
    let mut columns: Vec<(String, i64, bool, Column)> = Vec::new();
    let mut links = Vec::new();
    for r in rows {
        let mut cells = r.into_iter();
        let mut next = || cells.next().flatten();
        let (kind, table, name, a, b, c, d, position) =
            (next(), next(), next(), next(), next(), next(), next(), next());
        let (Some(table), Some(name)) = (table, name) else { continue };
        if kind.as_deref() == Some("f") {
            let ref_table = b.unwrap_or_default();
            links.push(Link { name, table, columns: list(a), ref_table, ref_columns: list(c) });
        } else {
            let flag = |v: Option<String>| v.as_deref() == Some("true");
            let position = position.and_then(|p| p.parse().ok()).unwrap_or_default();
            let column = Column { name, data_type: a.unwrap_or_default(), primary: flag(b), nullable: flag(c) };
            columns.push((table, position, flag(d), column));
        }
    }
    columns.sort_by(|x, y| (&x.0, x.1).cmp(&(&y.0, y.1)));
    let mut tables: Vec<Table> = Vec::new();
    for (table, _, view, column) in columns {
        match tables.last_mut() {
            Some(t) if t.name == table => t.columns.push(column),
            _ => tables.push(Table { name: table, view, columns: vec![column] }),
        }
    }
    links.sort_by(|x, y| (&x.table, &x.name).cmp(&(&y.table, &y.name)));
    Diagram { tables, links }
}

impl Conn {
    /// Every table and view of `schema` (Postgres; None is the current schema) or of the current database
    /// (MySQL, SQLite), with the foreign keys between them.
    pub async fn diagram(&mut self, schema: Option<&str>) -> Result<Diagram> {
        let rows = match self {
            Conn::Pg(_) => self.fetch_with(PG, &[schema]).await?,
            Conn::My(_) => self.fetch_with(MY, &[]).await?,
            Conn::Lite(_) => self.fetch_with(LITE, &[]).await?,
        };
        Ok(assemble(rows))
    }
}

/// Saves the diagram's PNG export: written to a temporary file, then renamed over.
#[tauri::command]
pub async fn write_image(path: PathBuf, bytes: Vec<u8>) -> Result<()> {
    if bytes.len() > MAX_IMAGE {
        return Err(Error::Invalid("the image is too large; zoom out or take fewer tables"));
    }
    if !bytes.starts_with(b"\x89PNG") {
        return Err(Error::Invalid("not a PNG image"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let failed = |source| Error::File { path: path.display().to_string(), source };
        let mut tmp = path.clone().into_os_string();
        tmp.push(".tableminus-tmp");
        std::fs::write(&tmp, bytes).map_err(failed)?;
        std::fs::rename(&tmp, &path).map_err(failed)
    })
    .await
    .map_err(|_| Error::Invalid("the image export stopped unexpectedly"))?
}

#[cfg(test)]
mod tests {
    use super::{Column, Link};
    use crate::db::{Conn, Engine, Options};
    use crate::tls::Tls;

    async fn sqlite() -> Conn {
        Conn::open(Engine::Sqlite, ":memory:", None, &Tls::default(), &Options::default(), None).await.unwrap()
    }

    fn col(name: &str, data_type: &str, primary: bool, nullable: bool) -> Column {
        Column { name: name.into(), data_type: data_type.into(), primary, nullable }
    }

    #[test]
    fn diagram_should_list_sqlite_tables_views_columns_and_keys() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite().await;
            c.query(
                "create table people(id integer primary key autoincrement, name text not null); \
                 create table pets(id integer primary key, owner integer references people(id), \
                   a int, b int, foreign key (a, b) references pairs(x, y)); \
                 create table pairs(x int, y int, primary key (x, y)); \
                 create view names as select name from people",
            )
            .await
            .unwrap();
            let d = c.diagram(None).await.unwrap();
            let names: Vec<(&str, bool)> = d.tables.iter().map(|t| (t.name.as_str(), t.view)).collect();
            assert_eq!(
                names,
                [("names", true), ("pairs", false), ("people", false), ("pets", false)],
                "no sqlite_sequence"
            );
            assert_eq!(d.tables[2].columns, [col("id", "INTEGER", true, true), col("name", "TEXT", false, false)]);
            assert_eq!(d.tables[1].columns.iter().filter(|c| c.primary).count(), 2);
            let keys: Vec<(&str, &[String], &str, &[String])> = d
                .links
                .iter()
                .map(|l| (l.table.as_str(), l.columns.as_slice(), l.ref_table.as_str(), l.ref_columns.as_slice()))
                .collect();
            assert_eq!(
                keys,
                [
                    ("pets", &["a".to_string(), "b".into()][..], "pairs", &["x".to_string(), "y".into()][..]),
                    ("pets", &["owner".to_string()][..], "people", &["id".to_string()][..]),
                ]
            );
        });
    }

    #[test]
    fn diagram_should_keep_an_implicit_sqlite_reference_as_an_empty_column() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite().await;
            c.query("create table a(id integer primary key); create table b(a_id int references a)").await.unwrap();
            let d = c.diagram(None).await.unwrap();
            assert_eq!(d.links[0].ref_columns, [String::new()]);
        });
    }

    #[test]
    fn diagram_should_order_columns_by_position_past_nine() {
        tauri::async_runtime::block_on(async {
            let mut c = sqlite().await;
            let cols: Vec<String> = (0..12).map(|i| format!("c{i} int")).collect();
            c.query(&format!("create table wide({})", cols.join(", "))).await.unwrap();
            let d = c.diagram(None).await.unwrap();
            let got: Vec<&str> = d.tables[0].columns.iter().map(|c| c.name.as_str()).collect();
            let want: Vec<String> = (0..12).map(|i| format!("c{i}")).collect();
            assert_eq!(got, want);
        });
    }

    #[test]
    fn write_image_should_refuse_bytes_that_are_not_a_png() {
        tauri::async_runtime::block_on(async {
            let path = std::env::temp_dir().join(format!("dbm_not_png_{}.png", std::process::id()));
            assert!(super::write_image(path.clone(), b"hello".to_vec()).await.is_err());
            assert!(!path.exists());
        });
    }

    /// Runs in a scratch database, dropped afterwards whatever happens.
    #[test]
    #[ignore = "needs a local MySQL; creates and drops a scratch database"]
    fn local_mysql_diagram() {
        tauri::async_runtime::block_on(async {
            let url = "mysql://root@127.0.0.1:3306";
            let mut c = Conn::open(Engine::Mysql, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let db = format!("dbm_diagram_{}", std::process::id());
            c.query(&format!("create database {db}")).await.unwrap();
            c.query(&format!("create database {db}_x")).await.unwrap();
            c.use_database(&db).await.unwrap();
            let outcome = mysql_scenario(&mut c, &format!("{db}_x")).await;
            c.query(&format!("drop database {db}")).await.unwrap();
            c.query(&format!("drop database {db}_x")).await.unwrap();
            outcome.unwrap();
        });
    }

    async fn mysql_scenario(c: &mut Conn, other: &str) -> crate::error::Result<()> {
        c.query("create table people(id int primary key, name varchar(20) not null)").await?;
        c.query(&format!("create table {other}.kinds(id int primary key)")).await?;
        c.query(&format!(
            "create table pets(id int primary key, owner int, kind int, constraint pets_owner foreign key (owner) \
                 references people(id), constraint pets_kind foreign key (kind) references {other}.kinds(id))"
        ))
        .await?;
        c.query("create view names as select name from people").await?;
        let d = c.diagram(None).await?;
        let names: Vec<(&str, bool)> = d.tables.iter().map(|t| (t.name.as_str(), t.view)).collect();
        assert_eq!(names, [("names", true), ("people", false), ("pets", false)]);
        assert_eq!(d.tables[1].columns, [col("id", "int", true, false), col("name", "varchar(20)", false, false)]);
        assert_eq!(
            d.links,
            [
                Link {
                    name: "pets_kind".into(),
                    table: "pets".into(),
                    columns: vec!["kind".into()],
                    ref_table: format!("{other}.kinds"),
                    ref_columns: vec!["id".into()],
                },
                Link {
                    name: "pets_owner".into(),
                    table: "pets".into(),
                    columns: vec!["owner".into()],
                    ref_table: "people".into(),
                    ref_columns: vec!["id".into()],
                }
            ]
        );
        Ok(())
    }

    /// Runs in a scratch database, dropped afterwards. `DBM_PG_URL` points elsewhere than the default server.
    #[test]
    #[ignore = "needs a local Postgres; creates and drops a scratch database"]
    fn local_postgres_diagram() {
        tauri::async_runtime::block_on(async {
            let url =
                std::env::var("DBM_PG_URL").unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5432/postgres".into());
            let open = |url: String| async move {
                Conn::open(Engine::Postgres, &url, None, &Tls::default(), &Options::default(), None).await.unwrap()
            };
            let mut admin = open(url.clone()).await;
            let db = format!("dbm_diagram_{}", std::process::id());
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
            "create schema shop; create table shop.people(id serial primary key, name text not null); \
             create table shop.pets(id int primary key, owner int constraint pets_owner references shop.people); \
             create view shop.names as select name from shop.people; \
             create table shop.log(at date) partition by range (at); \
             create table shop.log_2026 partition of shop.log for values from ('2026-01-01') to ('2027-01-01'); \
             create table public.elsewhere(id int)",
        )
        .await?;
        let d = c.diagram(Some("shop")).await?;
        let names: Vec<(&str, bool)> = d.tables.iter().map(|t| (t.name.as_str(), t.view)).collect();
        assert_eq!(
            names,
            [("shop.log", false), ("shop.names", true), ("shop.people", false), ("shop.pets", false)],
            "no partitions, no other schemas"
        );
        assert_eq!(d.tables[2].columns, [col("id", "integer", true, false), col("name", "text", false, false)]);
        assert_eq!(
            d.links,
            [Link {
                name: "pets_owner".into(),
                table: "shop.pets".into(),
                columns: vec!["owner".into()],
                ref_table: "shop.people".into(),
                ref_columns: vec!["id".into()],
            }]
        );
        assert_eq!(c.diagram(None).await?.tables.len(), 1, "the current schema is public");
        Ok(())
    }
}

use serde::{Deserialize, Serialize};

/// One row's edits: the primary-key values that find it, and the new values (None = NULL).
#[derive(Deserialize)]
pub struct RowEdit {
    pub key: Vec<(String, String)>,
    pub set: Vec<(String, Option<String>)>,
}

#[derive(Serialize)]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub primary: bool,
}

#[derive(Serialize, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub affected: u64,
}

// ponytail: an enum over three engines, not a Driver trait; extract a trait when a 4th engine needs it.
pub enum Conn {
    Pg(tokio_postgres::Client),
    My(mysql_async::Conn),
    Lite(rusqlite::Connection),
}

fn err(e: impl ToString) -> String {
    e.to_string()
}

impl Conn {
    /// `kind` is "postgres", "mysql"/"mariadb" or "sqlite"; `url` is a connection URL (a file path for SQLite).
    pub async fn open(kind: &str, url: &str) -> Result<Conn, String> {
        match kind {
            "postgres" => {
                // sslmode in the URL decides; tokio-postgres defaults to `prefer`.
                let tls = postgres_native_tls::MakeTlsConnector::new(native_tls::TlsConnector::new().map_err(err)?);
                let (client, connection) = tokio_postgres::connect(url, tls).await.map_err(err)?;
                tauri::async_runtime::spawn(connection); // drives the socket; ends when the client is dropped
                Ok(Conn::Pg(client))
            }
            "mysql" | "mariadb" => Ok(Conn::My(mysql_async::Conn::from_url(url).await.map_err(err)?)),
            "sqlite" => Ok(Conn::Lite(rusqlite::Connection::open(url).map_err(err)?)),
            other => Err(format!("unknown engine: {other}")),
        }
    }

    /// Runs SQL and returns the last result set as text cells (None = NULL).
    pub async fn query(&mut self, sql: &str) -> Result<QueryResult, String> {
        match self {
            // The simple-query (text) protocol renders every Postgres type as text, so no per-type decoding.
            Conn::Pg(c) => {
                use tokio_postgres::SimpleQueryMessage as M;
                let mut r = QueryResult::default();
                for msg in c.simple_query(sql).await.map_err(err)? {
                    match msg {
                        M::RowDescription(cols) => {
                            r.columns = cols.iter().map(|c| c.name().to_string()).collect();
                            r.rows.clear();
                        }
                        M::Row(row) => r.rows.push((0..row.len()).map(|i| row.get(i).map(String::from)).collect()),
                        M::CommandComplete(n) => r.affected = n,
                        _ => {}
                    }
                }
                Ok(r)
            }
            Conn::My(c) => {
                use mysql_async::{prelude::Queryable, Value};
                let mut res = c.query_iter(sql).await.map_err(err)?;
                let columns = res.columns_ref().iter().map(|c| c.name_str().into_owned()).collect();
                let rows: Vec<mysql_async::Row> = res.collect().await.map_err(err)?;
                let affected = res.affected_rows();
                res.drop_result().await.map_err(err)?; // ponytail: only the first result set of a multi-statement query
                let rows = rows
                    .into_iter()
                    .map(|row| {
                        row.unwrap()
                            .into_iter()
                            .map(|v| match v {
                                Value::NULL => None,
                                Value::Bytes(b) => Some(String::from_utf8_lossy(&b).into_owned()),
                                v => Some(v.as_sql(true)),
                            })
                            .collect()
                    })
                    .collect();
                Ok(QueryResult { columns, rows, affected })
            }
            Conn::Lite(c) => {
                use rusqlite::types::ValueRef;
                // ponytail: SQLite runs only the first statement; use execute_batch if scripts are needed.
                let mut st = c.prepare(sql).map_err(err)?;
                let columns: Vec<String> = st.column_names().into_iter().map(String::from).collect();
                if columns.is_empty() {
                    let n = st.execute([]).map_err(err)?;
                    return Ok(QueryResult { affected: n as u64, ..Default::default() });
                }
                let n = columns.len();
                let text = |v: ValueRef| match v {
                    ValueRef::Null => None,
                    ValueRef::Integer(i) => Some(i.to_string()),
                    ValueRef::Real(f) => Some(f.to_string()),
                    ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
                    ValueRef::Blob(b) => Some(format!("<{} bytes>", b.len())),
                };
                let rows = st
                    .query_map([], |r| (0..n).map(|i| r.get_ref(i).map(text)).collect::<Result<Vec<_>, _>>())
                    .map_err(err)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(err)?;
                Ok(QueryResult { columns, rows, affected: 0 })
            }
        }
    }

    async fn scalar(&mut self, sql: &str) -> Result<Option<String>, String> {
        Ok(self.query(sql).await?.rows.into_iter().next().and_then(|mut row| row.swap_remove(0)))
    }

    /// Databases on the server; empty for SQLite (one file, one database).
    pub async fn databases(&mut self) -> Result<Vec<String>, String> {
        let sql = match self {
            Conn::Pg(_) => "select datname from pg_database where not datistemplate and datallowconn order by 1",
            Conn::My(_) => "select schema_name from information_schema.schemata order by 1",
            Conn::Lite(_) => return Ok(vec![]),
        };
        Ok(self.query(sql).await?.rows.into_iter().filter_map(|mut row| row.swap_remove(0)).collect())
    }

    /// The selected database; None when a MySQL connection has none yet.
    pub async fn current_database(&mut self) -> Result<Option<String>, String> {
        match self {
            Conn::Pg(_) => self.scalar("select current_database()").await,
            Conn::My(_) => self.scalar("select database()").await,
            Conn::Lite(_) => Ok(None),
        }
    }

    pub async fn server_version(&mut self) -> Result<String, String> {
        let (label, sql) = match self {
            Conn::Pg(_) => ("PostgreSQL", "show server_version"),
            Conn::My(_) => ("MySQL", "select version()"),
            Conn::Lite(_) => ("SQLite", "select sqlite_version()"),
        };
        let v = self.scalar(sql).await?.unwrap_or_default();
        // MariaDB reports e.g. "11.4.2-MariaDB"; name it after what the server says it is.
        Ok(if v.contains("MariaDB") { format!("MariaDB {}", v.split('-').next().unwrap_or(&v)) } else { format!("{label} {v}") })
    }

    /// MySQL switches in place; Postgres cannot change database within a session, so the caller reconnects.
    pub async fn use_database(&mut self, db: &str) -> Result<(), String> {
        match self {
            Conn::My(_) => self.query(&format!("use `{}`", db.replace('`', "``"))).await.map(|_| ()),
            _ => Err("this engine switches database by reconnecting".into()),
        }
    }

    /// A SQL string literal. MySQL also treats backslash as an escape character by default.
    fn lit(&self, s: &str) -> String {
        let s = s.replace('\'', "''");
        format!("'{}'", if matches!(self, Conn::My(_)) { s.replace('\\', "\\\\") } else { s })
    }

    fn ident(&self, name: &str) -> String {
        match self {
            Conn::My(_) => format!("`{}`", name.replace('`', "``")),
            _ => format!("\"{}\"", name.replace('"', "\"\"")),
        }
    }

    fn table_ident(&self, table: &str) -> String {
        match (self, table.split_once('.')) {
            (Conn::Pg(_), Some((schema, name))) => format!("{}.{}", self.ident(schema), self.ident(name)),
            _ => self.ident(table),
        }
    }

    /// Applies cell edits in one transaction, each row found by its primary key.
    /// Values go in as string literals; the server casts them to the column type.
    /// Rolls back everything if any statement fails or one key matches more than one row.
    // ponytail: MySQL reports rows changed, not matched, so a vanished row is not detected there;
    // set CLIENT_FOUND_ROWS on the connection if that matters.
    pub async fn update_rows(&mut self, table: &str, edits: &[RowEdit]) -> Result<u64, String> {
        let statements: Vec<String> = edits
            .iter()
            .map(|e| {
                let set = e.set.iter().map(|(c, v)| format!("{} = {}", self.ident(c), v.as_deref().map_or("NULL".into(), |v| self.lit(v))));
                let key = e.key.iter().map(|(c, v)| format!("{} = {}", self.ident(c), self.lit(v)));
                format!("update {} set {} where {}", self.table_ident(table), set.collect::<Vec<_>>().join(", "), key.collect::<Vec<_>>().join(" and "))
            })
            .collect();
        if edits.iter().any(|e| e.key.is_empty() || e.set.is_empty()) {
            return Err("every edit needs a primary key and at least one value".into());
        }
        self.query(if matches!(self, Conn::My(_)) { "start transaction" } else { "begin" }).await?;
        let mut total = 0;
        for sql in &statements {
            let outcome = match self.query(sql).await {
                Ok(r) if r.affected > 1 => Err(format!("the key matched {} rows; nothing was saved", r.affected)),
                Ok(r) => Ok(r.affected),
                Err(e) => Err(e),
            };
            match outcome {
                Ok(n) => total += n,
                Err(e) => {
                    let _ = self.query("rollback").await;
                    return Err(e);
                }
            }
        }
        self.query("commit").await?;
        Ok(total)
    }

    /// Columns of `table` (`schema.table` for Postgres) in definition order.
    pub async fn columns(&mut self, table: &str) -> Result<Vec<Column>, String> {
        let sql = match self {
            Conn::Pg(_) => {
                let (schema, name) = table.split_once('.').unwrap_or(("public", table));
                format!(
                    "select c.column_name, c.udt_name || coalesce('(' || c.character_maximum_length || ')', ''), c.is_nullable, \
                     c.column_default, case when exists (select 1 from information_schema.table_constraints t \
                     join information_schema.key_column_usage k on k.constraint_name = t.constraint_name \
                     and k.table_schema = t.table_schema and k.table_name = t.table_name \
                     where t.constraint_type = 'PRIMARY KEY' and t.table_schema = c.table_schema \
                     and t.table_name = c.table_name and k.column_name = c.column_name) then 'PRI' else '' end \
                     from information_schema.columns c where c.table_schema = {} and c.table_name = {} order by c.ordinal_position",
                    self.lit(schema),
                    self.lit(name)
                )
            }
            Conn::My(_) => format!(
                "select column_name, column_type, is_nullable, column_default, column_key from information_schema.columns \
                 where table_schema = database() and table_name = {} order by ordinal_position",
                self.lit(table)
            ),
            Conn::Lite(_) => format!(
                "select name, type, case when \"notnull\" then 'NO' else 'YES' end, dflt_value, \
                 case when pk > 0 then 'PRI' else '' end from pragma_table_info({})",
                self.lit(table)
            ),
        };
        Ok(self
            .query(&sql)
            .await?
            .rows
            .into_iter()
            .map(|r| Column {
                name: r[0].clone().unwrap_or_default(),
                data_type: r[1].clone().unwrap_or_default(),
                nullable: r[2].as_deref() == Some("YES"),
                default: r[3].clone(),
                primary: r[4].as_deref() == Some("PRI"),
            })
            .collect())
    }

    pub async fn tables(&mut self) -> Result<Vec<String>, String> {
        let sql = match self {
            Conn::Pg(_) => "select table_schema || '.' || table_name from information_schema.tables \
                            where table_schema not in ('pg_catalog', 'information_schema') order by 1",
            Conn::My(_) => "select table_name from information_schema.tables where table_schema = database() order by 1",
            Conn::Lite(_) => "select name from sqlite_master where type in ('table', 'view') order by 1",
        };
        let r = self.query(sql).await?;
        Ok(r.rows.into_iter().filter_map(|mut row| row.swap_remove(0)).collect())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sqlite_round_trip() {
        tauri::async_runtime::block_on(async {
            let mut c = super::Conn::open("sqlite", ":memory:").await.unwrap();
            assert_eq!(c.query("create table t(id integer, name text)").await.unwrap().affected, 0);
            assert_eq!(c.query("insert into t values (1, 'a'), (2, null)").await.unwrap().affected, 2);
            let r = c.query("select id, name from t order by id").await.unwrap();
            assert_eq!(r.columns, ["id", "name"]);
            assert_eq!(r.rows, [vec![Some("1".into()), Some("a".into())], vec![Some("2".into()), None]]);
            assert_eq!(c.tables().await.unwrap(), ["t"]);
            assert!(c.databases().await.unwrap().is_empty());
            c.query("create table \"it's\"(id integer primary key, v text not null default 'x')").await.unwrap();
            let cols = c.columns("it's").await.unwrap(); // quote in the name must not break the lookup
            assert_eq!((cols[0].name.as_str(), cols[0].primary, cols[1].nullable), ("id", true, false));
            assert_eq!(cols[1].default.as_deref(), Some("'x'"));

            // Inline edits: quotes survive, NULL works, and all-or-nothing on a bad key.
            c.query("insert into \"it's\"(id, v) values (1, 'a'), (2, 'b')").await.unwrap();
            let edit = |id: &str, v: Option<&str>| super::RowEdit { key: vec![("id".into(), id.into())], set: vec![("v".into(), v.map(String::from))] };
            assert_eq!(c.update_rows("it's", &[edit("1", Some("O'Brien")), edit("2", Some("z"))]).await.unwrap(), 2);
            let r = c.query("select v from \"it's\" order by id").await.unwrap();
            assert_eq!(r.rows, [vec![Some("O'Brien".into())], vec![Some("z".into())]]);
            assert!(c.update_rows("it's", &[edit("1", Some("kept?")), edit("2", None)]).await.is_err()); // v is NOT NULL
            let r = c.query("select v from \"it's\" order by id").await.unwrap();
            assert_eq!(r.rows[0], vec![Some("O'Brien".into())], "the first update must be rolled back");

            c.query("create table dup(k integer, v text)").await.unwrap(); // no unique key: two rows share k
            c.query("insert into dup values (1, 'a'), (1, 'b')").await.unwrap();
            let e = super::RowEdit { key: vec![("k".into(), "1".into())], set: vec![("v".into(), None)] };
            assert!(c.update_rows("dup", &[e]).await.unwrap_err().contains("matched 2 rows"));
            assert_eq!(c.query("select count(*) from dup where v is null").await.unwrap().rows[0][0].as_deref(), Some("0"));
            assert!(c.server_version().await.unwrap().starts_with("SQLite 3."));
        });
    }

    /// Read-only check against local servers with default credentials: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn local_servers_without_database() {
        tauri::async_runtime::block_on(async {
            let mut my = super::Conn::open("mysql", "mysql://root@127.0.0.1:3306").await.unwrap();
            assert_eq!(my.current_database().await.unwrap(), None);
            assert!(my.databases().await.unwrap().contains(&"mysql".to_string()));
            my.use_database("mysql").await.unwrap();
            assert_eq!(my.current_database().await.unwrap().as_deref(), Some("mysql"));
            assert!(!my.tables().await.unwrap().is_empty());
            assert!(my.columns("user").await.unwrap().iter().any(|c| c.name == "Host" && c.primary));
            println!("{}", my.server_version().await.unwrap());

            let mut pg = super::Conn::open("postgres", "postgres://postgres@127.0.0.1:5432?sslmode=prefer").await.unwrap();
            assert!(!pg.databases().await.unwrap().is_empty());
            assert!(pg.columns("information_schema.tables").await.unwrap().iter().any(|c| c.name == "table_name"));

            // Text literals into typed columns, on a session-only temp table (nothing persists).
            pg.query("create temporary table dbm_edit(id int primary key, ok bool, at timestamp, n numeric(6,2), note text)").await.unwrap();
            pg.query("insert into dbm_edit values (1, false, '2026-01-01', 1.5, 'x')").await.unwrap();
            let set = |c: &str, v: Option<&str>| (c.to_string(), v.map(String::from));
            let edit = super::RowEdit {
                key: vec![("id".into(), "1".into())],
                set: vec![set("ok", Some("t")), set("at", Some("2026-10-13 09:30:00")), set("n", Some("42.25")), set("note", None)],
            };
            assert_eq!(pg.update_rows("pg_temp.dbm_edit", &[edit]).await.unwrap(), 1);
            let r = pg.query("select ok, at, n, note from dbm_edit").await.unwrap();
            assert_eq!(r.rows[0], vec![Some("t".into()), Some("2026-10-13 09:30:00".into()), Some("42.25".into()), None]);
            println!("{} · current {:?}", pg.server_version().await.unwrap(), pg.current_database().await.unwrap());
        });
    }
}

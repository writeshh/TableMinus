use serde::Serialize;

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
    /// `kind` is "postgres", "mysql" or "sqlite"; `url` is a connection URL (a file path for SQLite).
    pub async fn open(kind: &str, url: &str) -> Result<Conn, String> {
        match kind {
            "postgres" => {
                // sslmode in the URL decides; tokio-postgres defaults to `prefer`.
                let tls = postgres_native_tls::MakeTlsConnector::new(native_tls::TlsConnector::new().map_err(err)?);
                let (client, connection) = tokio_postgres::connect(url, tls).await.map_err(err)?;
                tauri::async_runtime::spawn(connection); // drives the socket; ends when the client is dropped
                Ok(Conn::Pg(client))
            }
            "mysql" => Ok(Conn::My(mysql_async::Conn::from_url(url).await.map_err(err)?)),
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
        });
    }
}

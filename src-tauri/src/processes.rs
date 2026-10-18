//! The process list: what a Postgres (or `CockroachDB`) or MySQL/MariaDB server is running, with cancel and kill. It has its own
//! connection, so it still answers while the workspace's connection is busy with the query to be stopped.

use crate::db::{Engine, QueryResult};
use crate::error::{Error, Result};
use crate::store::Flavor;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use tauri::{AppHandle, Manager, State, async_runtime::Mutex};

/// The process list's own connection per saved connection, kept while its window is open.
#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, crate::Live>>);

/// Client sessions other than this one; the first column is what [`kill_process`] takes.
const POSTGRES: &str = "select pid as \"PID\", usename as \"User\", datname as \"Database\", client_addr as \"Client\", \
    application_name as \"Application\", state as \"State\", date_trunc('second', now() - query_start) as \"Running\", \
    wait_event as \"Waiting on\", query as \"Query\" from pg_stat_activity \
    where pid <> pg_backend_pid() and backend_type = 'client backend' order by query_start nulls last";
/// `CockroachDB`'s sessions, as hex ids, minus this one. Its `pg_stat_activity` is empty.
const COCKROACH: &str = "select session_id as \"Session\", user_name as \"User\", client_address as \"Client\", \
    application_name as \"Application\", status as \"State\", date_trunc('second', now() - active_query_start) as \"Running\", \
    coalesce(nullif(active_queries, ''), last_active_query) as \"Query\" from crdb_internal.cluster_sessions \
    where session_id <> (select * from [show session_id]) order by active_query_start nulls last";
/// User sessions with the request each is running, minus this one. Needs `VIEW SERVER STATE`.
pub(crate) const MSSQL: &str = "select s.session_id as [Session], s.login_name as [User], s.host_name as [Host], \
    db_name(s.database_id) as [Database], s.status as [Status], r.command as [Command], \
    r.total_elapsed_time / 1000 as [Running (s)], r.wait_type as [Waiting on], t.text as [Query] \
    from sys.dm_exec_sessions s left join sys.dm_exec_requests r on r.session_id = s.session_id \
    outer apply sys.dm_exec_sql_text(r.sql_handle) t \
    where s.is_user_process = 1 and s.session_id <> @@spid order by r.total_elapsed_time desc";
/// Running queries (`ClickHouse` has no idle sessions to list), minus this one, by their ids.
pub(crate) const CLICKHOUSE: &str = "select query_id as `Query`, user as `User`, address as `Client`, \
    current_database as `Database`, round(elapsed, 1) as `Running (s)`, formatReadableSize(memory_usage) as `Memory`, \
    query as `Text` from system.processes where query_id != queryID() order by elapsed desc";
/// `SHOW FULL PROCESSLIST`, minus this connection.
const MYSQL: &str = "select id as `Id`, user as `User`, host as `Host`, db as `Database`, command as `Command`, \
    time as `Time`, state as `State`, info as `Info` from information_schema.processlist \
    where id <> connection_id() order by time desc";

#[tauri::command]
pub async fn processes(app: AppHandle, sessions: State<'_, Sessions>, name: String) -> Result<QueryResult> {
    let mut sessions = sessions.0.lock().await;
    let live = match sessions.entry(name.clone()) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(e) => e.insert(crate::open_saved(&app.state::<crate::Open>(), &crate::dir(&app)?, &name).await?),
    };
    let sql = match live.saved.kind {
        Engine::Postgres if live.saved.flavor == Some(Flavor::Cockroach) => COCKROACH,
        Engine::Postgres => POSTGRES,
        Engine::Mysql | Engine::Mariadb => MYSQL,
        Engine::Sqlite | Engine::Duckdb | Engine::Libsql | Engine::D1 => {
            return Err(Error::Invalid("SQLite-based engines and DuckDB have no process list"));
        }
        Engine::Mssql => MSSQL,
        Engine::Clickhouse => CLICKHOUSE,
        Engine::Redis => return Err(Error::Invalid("Redis clients show in the console: CLIENT LIST")),
    };
    let r = live.conn.query(sql).await;
    if r.as_ref().is_err_and(|e| live.conn.lost_by(e)) {
        sessions.remove(&name); // the next refresh opens a fresh one
    }
    r
}

/// Stops process `id`: its running query, or with `connection` the whole session.
#[tauri::command]
pub async fn kill_process(
    open: State<'_, crate::Open>,
    sessions: State<'_, Sessions>,
    name: String,
    id: String,
    connection: bool,
) -> Result<()> {
    open.unlocked()?;
    let mut sessions = sessions.0.lock().await;
    let live = sessions.get_mut(&name).ok_or(Error::NotConnected)?;
    let number = || id.parse::<u64>().map_err(|_| Error::Invalid("a process id is a number"));
    match live.saved.kind {
        Engine::Postgres if live.saved.flavor == Some(Flavor::Cockroach) => {
            if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::Invalid("a CockroachDB session id is hexadecimal"));
            }
            let sql = if connection {
                "cancel session $1"
            } else {
                "cancel queries select query_id from crdb_internal.cluster_queries where session_id = $1"
            };
            live.conn.fetch_with(sql, &[Some(&id)]).await?;
        }
        Engine::Postgres => {
            let f = if connection { "pg_terminate_backend" } else { "pg_cancel_backend" };
            let id = number()?.to_string();
            let rows = live.conn.fetch_with(&format!("select {f}($1::int)::text"), &[Some(&id)]).await?;
            if rows.first().and_then(|r| r.first()).and_then(Option::as_deref) != Some("true") {
                return Err(Error::Invalid("the process has ended, or this user may not stop it"));
            }
        }
        Engine::Mysql | Engine::Mariadb => {
            // KILL takes no parameters; `id` is a number, so formatting it in can't inject anything.
            let id = number()?;
            let sql = if connection { format!("kill {id}") } else { format!("kill query {id}") };
            live.conn.query(&sql).await?;
        }
        Engine::Clickhouse => {
            // A query id is a UUID, or whatever the client named it; refuse anything odd rather than quote it.
            if id.is_empty()
                || id.len() > 100
                || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err(Error::Invalid("a ClickHouse query id is letters, digits, - and _"));
            }
            live.conn.fetch_with("KILL QUERY WHERE query_id = {p1:String} SYNC", &[Some(&id)]).await?;
        }
        Engine::Mssql => {
            if !connection {
                return Err(Error::Invalid("SQL Server can only end a whole session; use Kill"));
            }
            // KILL takes no parameters; `id` is a number, so formatting it in can't inject anything.
            live.conn.query(&format!("kill {}", number()?)).await?;
        }
        Engine::Sqlite | Engine::Duckdb | Engine::Libsql | Engine::D1 => {
            return Err(Error::Invalid("SQLite-based engines and DuckDB have no process list"));
        }
        Engine::Redis => return Err(Error::Invalid("Redis clients show in the console: CLIENT LIST")),
    }
    Ok(())
}

/// Closes the process list's connection when its window closes.
#[tauri::command]
pub async fn close_processes(sessions: State<'_, Sessions>, name: String) -> Result<()> {
    sessions.0.lock().await.remove(&name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{COCKROACH, MYSQL};
    use crate::db::{Conn, Engine, Options};
    use crate::tls::Tls;

    #[test]
    #[ignore = "needs a local MySQL"]
    fn mysql_process_list_should_show_other_sessions_but_not_its_own() {
        tauri::async_runtime::block_on(async {
            let url = "mysql://root@127.0.0.1:3306";
            let mut me =
                Conn::open(Engine::Mysql, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let mut other =
                Conn::open(Engine::Mysql, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            let other_id = other.query("select connection_id()").await.unwrap().rows[0][0].clone().unwrap();
            let my_id = me.query("select connection_id()").await.unwrap().rows[0][0].clone().unwrap();
            let r = me.query(MYSQL).await.unwrap();
            assert_eq!(r.columns[0], "Id");
            let ids: Vec<_> = r.rows.iter().map(|row| row[0].clone().unwrap()).collect();
            assert!(ids.contains(&other_id) && !ids.contains(&my_id), "{ids:?}");
            me.query(&format!("kill {other_id}")).await.unwrap();
            assert!(other.query("select 1").await.is_err(), "the killed session is gone");
        });
    }

    #[test]
    #[ignore = "needs a local CockroachDB on 26257 (scripts/test-env.sh up)"]
    fn cockroach_process_list_should_show_other_sessions_and_cancel_one() {
        tauri::async_runtime::block_on(async {
            let url = "postgres://root@127.0.0.1:26257/defaultdb";
            let (tls, options) = (Tls::default(), Options::default());
            let open = || Conn::open(Engine::Postgres, url, None, &tls, &options, None);
            let (mut me, mut other) = (open().await.unwrap(), open().await.unwrap());
            let other_id = other.query("show session_id").await.unwrap().rows[0][0].clone().unwrap();
            let my_id = me.query("show session_id").await.unwrap().rows[0][0].clone().unwrap();
            let r = me.query(COCKROACH).await.unwrap();
            assert_eq!(r.columns[0], "Session");
            let ids: Vec<_> = r.rows.iter().map(|row| row[0].clone().unwrap()).collect();
            assert!(ids.contains(&other_id) && !ids.contains(&my_id), "{ids:?}");
            me.fetch_with("cancel session $1", &[Some(&other_id)]).await.unwrap();
            assert!(other.query("select 1").await.is_err(), "the cancelled session is gone");
        });
    }
}

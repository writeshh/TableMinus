//! The process list: what a Postgres or MySQL/MariaDB server is running, with cancel and kill. It has its own
//! connection, so it still answers while the workspace's connection is busy with the query to be stopped.

use crate::db::{Engine, QueryResult};
use crate::error::{Error, Result};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use tauri::{AppHandle, State, async_runtime::Mutex};

/// The process list's own connection per saved connection, kept while its window is open.
#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, crate::Live>>);

/// Client sessions other than this one; the first column is what [`kill_process`] takes.
const POSTGRES: &str = "select pid as \"PID\", usename as \"User\", datname as \"Database\", client_addr as \"Client\", \
    application_name as \"Application\", state as \"State\", date_trunc('second', now() - query_start) as \"Running\", \
    wait_event as \"Waiting on\", query as \"Query\" from pg_stat_activity \
    where pid <> pg_backend_pid() and backend_type = 'client backend' order by query_start nulls last";
/// `SHOW FULL PROCESSLIST`, minus this connection.
const MYSQL: &str = "select id as `Id`, user as `User`, host as `Host`, db as `Database`, command as `Command`, \
    time as `Time`, state as `State`, info as `Info` from information_schema.processlist \
    where id <> connection_id() order by time desc";

#[tauri::command]
pub async fn processes(app: AppHandle, sessions: State<'_, Sessions>, name: String) -> Result<QueryResult> {
    let mut sessions = sessions.0.lock().await;
    let live = match sessions.entry(name.clone()) {
        Entry::Occupied(e) => e.into_mut(),
        Entry::Vacant(e) => e.insert(crate::open_saved(&crate::dir(&app)?, &name).await?),
    };
    let sql = match live.saved.kind {
        Engine::Postgres => POSTGRES,
        Engine::Mysql | Engine::Mariadb => MYSQL,
        Engine::Sqlite => return Err(Error::Invalid("SQLite has no process list")),
    };
    let r = live.conn.query(sql).await;
    if r.as_ref().is_err_and(|e| live.conn.lost_by(e)) {
        sessions.remove(&name); // the next refresh opens a fresh one
    }
    r
}

/// Stops process `id`: its running query, or with `connection` the whole session.
#[tauri::command]
pub async fn kill_process(sessions: State<'_, Sessions>, name: String, id: u64, connection: bool) -> Result<()> {
    let mut sessions = sessions.0.lock().await;
    let live = sessions.get_mut(&name).ok_or(Error::NotConnected)?;
    match live.saved.kind {
        Engine::Postgres => {
            let f = if connection { "pg_terminate_backend" } else { "pg_cancel_backend" };
            let id = id.to_string();
            let rows = live.conn.fetch_with(&format!("select {f}($1::int)::text"), &[Some(&id)]).await?;
            if rows.first().and_then(|r| r.first()).and_then(Option::as_deref) != Some("true") {
                return Err(Error::Invalid("the process has ended, or this user may not stop it"));
            }
        }
        Engine::Mysql | Engine::Mariadb => {
            // KILL takes no parameters; `id` is a number, so formatting it in can't inject anything.
            let sql = if connection { format!("kill {id}") } else { format!("kill query {id}") };
            live.conn.query(&sql).await?;
        }
        Engine::Sqlite => return Err(Error::Invalid("SQLite has no process list")),
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
    use super::MYSQL;
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
}

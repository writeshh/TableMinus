//! Apache Cassandra (and `ScyllaDB`) over CQL's native protocol, through the `scylla` driver. Rows go to the document
//! view as JSON (`SELECT JSON`), which Cassandra writes for every type, and values come back through `fromJson()`,
//! so nothing here decodes CQL types. A row is found again by its primary key; edits that must not create a row
//! say `IF EXISTS`.

use crate::connect::store::{self, Saved};
use crate::error::{Error, Result};
use crate::safety::{self, Unlock};
use crate::sql::db::{CONNECT_TIMEOUT, Engine};
use base64::Engine as _;
use scylla::client::session::Session as Cql;
use scylla::client::session_builder::SessionBuilder;
use scylla::response::PagingState;
use scylla::statement::Statement;
use scylla::value::{CqlValue, Row};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::ops::ControlFlow;
use tauri::{AppHandle, State};

pub(crate) struct Session {
    cql: Cql,
    keyspace: Option<String>,
    server: String,
    encrypted: bool,
    saved: Saved,
    password: Option<SecretString>,
    _tunnel: Option<crate::connect::ssh::Tunnel>,
}

pub(crate) type Sessions = crate::nosql::sessions::Sessions<Session>;

/// Sends every node's traffic to one address: the one dialled.
struct Pinned(std::net::SocketAddr);

#[async_trait::async_trait]
impl scylla::policies::address_translator::AddressTranslator for Pinned {
    async fn translate_address(
        &self,
        _peer: &scylla::policies::address_translator::UntranslatedPeer,
    ) -> std::result::Result<std::net::SocketAddr, scylla::errors::TranslationError> {
        Ok(self.0)
    }
}

impl crate::nosql::sessions::Session for Session {
    fn saved_mut(&mut self) -> &mut Saved {
        &mut self.saved
    }
}

fn failed(e: impl std::fmt::Display) -> Error {
    Error::Cassandra(e.to_string())
}

/// A name as CQL quotes it.
pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A value Cassandra didn't send as JSON (a system table's, an LWT's `[applied]`) as JSON.
fn cql_json(v: Option<&CqlValue>) -> Value {
    match v {
        None => Value::Null,
        Some(CqlValue::Text(s) | CqlValue::Ascii(s)) => json!(s),
        Some(CqlValue::Boolean(b)) => json!(b),
        Some(CqlValue::Int(n)) => json!(n),
        Some(CqlValue::BigInt(n)) => json!(n),
        Some(CqlValue::SmallInt(n)) => json!(n),
        Some(CqlValue::TinyInt(n)) => json!(n),
        Some(CqlValue::Float(n)) => json!(n),
        Some(CqlValue::Double(n)) => json!(n),
        Some(CqlValue::List(items) | CqlValue::Set(items)) => {
            Value::Array(items.iter().map(|i| cql_json(Some(i))).collect())
        }
        Some(other) => json!(format!("{other:?}")),
    }
}

/// A result's rows as JSON objects: a `SELECT JSON` row is its JSON; any other is its columns by name.
fn rows_of(result: scylla::response::query_result::QueryResult) -> Result<Vec<Value>> {
    if !result.is_rows() {
        return Ok(vec![]);
    }
    let rows = result.into_rows_result().map_err(failed)?;
    let names: Vec<String> = rows.column_specs().iter().map(|c| c.name().to_string()).collect();
    let json = names.len() == 1 && names[0] == "[json]";
    rows.rows::<Row>()
        .map_err(failed)?
        .map(|r| {
            let r = r.map_err(failed)?;
            if json {
                return match r.columns.first() {
                    Some(Some(CqlValue::Text(s))) => serde_json::from_str(s).map_err(failed),
                    _ => Ok(Value::Null),
                };
            }
            Ok(Value::Object(names.iter().cloned().zip(r.columns.iter().map(Option::as_ref).map(cql_json)).collect()))
        })
        .collect()
}

impl Session {
    async fn query(&self, cql: &str, values: &[String]) -> Result<Vec<Value>> {
        rows_of(self.cql.query_unpaged(cql, values).await.map_err(failed)?)
    }

    /// Runs a statement that says whether it applied (`IF EXISTS`, `IF NOT EXISTS`).
    async fn applied(&self, cql: &str, values: &[String]) -> Result<bool> {
        let rows = self.query(cql, values).await?;
        Ok(rows.first().and_then(|r| r["[applied]"].as_bool()).unwrap_or(true))
    }

    fn table(&self, table: &str) -> Result<String> {
        let ks = self.keyspace.as_deref().ok_or(Error::Invalid("choose a keyspace first (⌘K)"))?;
        Ok(format!("{}.{}", quote(ks), quote(table)))
    }

    async fn tables(&self) -> Result<Vec<String>> {
        let Some(ks) = &self.keyspace else { return Ok(vec![]) };
        let rows = self
            .query("SELECT table_name FROM system_schema.tables WHERE keyspace_name = ?", std::slice::from_ref(ks))
            .await?;
        let mut names: Vec<String> = rows.iter().filter_map(|r| r["table_name"].as_str().map(String::from)).collect();
        names.sort();
        Ok(names)
    }
}

async fn open_session(
    saved: Saved,
    password: Option<SecretString>,
    ssh_secret: Option<SecretString>,
    keyspace: Option<String>,
) -> Result<Session> {
    if !saved.options.startup_script.trim().is_empty() {
        crate::sql::db::run_startup_script(&saved.options.startup_script).await?;
    }
    let u = url::Url::parse(&saved.url)?;
    let host = u.host_str().ok_or(Error::Invalid("enter the server's host"))?.trim_matches(['[', ']']).to_string();
    let port = u.port().unwrap_or(9042);
    let tunnel = match &saved.ssh {
        Some(ssh) => {
            Some(crate::connect::ssh::open_for(Engine::Cassandra, &saved.url, ssh, ssh_secret.as_ref()).await?)
        }
        None => None,
    };
    let node = match &tunnel {
        Some(t) => format!("127.0.0.1:{}", t.port),
        None => format!("{host}:{port}"),
    };
    let tls = saved.tls.rustls()?;
    let encrypted = tls.is_some();
    let mut b = SessionBuilder::new().known_node(&node).connection_timeout(CONNECT_TIMEOUT).tls_context(tls);
    // Through a tunnel, or to a port Docker maps to this computer, the addresses the server gives for itself
    // (system.local, system.peers) aren't reachable: every node is the one dialled.
    let loopback = tunnel.is_some() || matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1");
    if loopback {
        let at = if host == "::1" && tunnel.is_none() {
            format!("[::1]:{port}")
        } else {
            node.replace("localhost", "127.0.0.1")
        };
        let at: std::net::SocketAddr = at.parse().map_err(|_| Error::Invalid("can't read the server's address"))?;
        b = b.address_translator(std::sync::Arc::new(Pinned(at)));
    }
    let user = url::form_urlencoded::parse(format!("u={}", u.username()).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    if !user.is_empty() {
        b = b.user(user, password.as_ref().map(|p| p.expose_secret().to_string()).unwrap_or_default());
    }
    let cql = tokio::time::timeout(CONNECT_TIMEOUT * 2, b.build())
        .await
        .map_err(|_| Error::ConnectTimeout(CONNECT_TIMEOUT.as_secs()))?
        .map_err(failed)?;
    let keyspace = keyspace.or_else(|| Some(u.path().trim_matches('/').to_string()).filter(|k| !k.is_empty()));
    let mut s = Session { cql, keyspace, server: String::new(), encrypted, saved, password, _tunnel: tunnel };
    let local = s.query("SELECT release_version FROM system.local", &[]).await?;
    let version = local.first().and_then(|r| r["release_version"].as_str()).unwrap_or_default().to_string();
    s.server = format!("Cassandra {version}").trim().to_string();
    Ok(s)
}

#[derive(Debug, Serialize)]
pub struct Info {
    server: String,
    database: Option<String>,
    can_switch: bool,
    tables: Vec<String>,
}

async fn info(s: &Session) -> Result<Info> {
    Ok(Info { server: s.server.clone(), database: s.keyspace.clone(), can_switch: true, tables: s.tables().await? })
}

pub async fn test(saved: Saved, password: Option<SecretString>, ssh_secret: Option<SecretString>) -> Result<String> {
    let s = open_session(saved, password, ssh_secret, None).await?;
    let i = info(&s).await?;
    Ok(format!("Connected · {} · {} tables", i.server, i.tables.len()))
}

/// Opens (or reuses) the session; with `database`, it moves to that keyspace.
#[tauri::command]
pub async fn cassandra_connect(
    app: AppHandle,
    open: State<'_, crate::Open>,
    sessions: State<'_, Sessions>,
    name: String,
    database: Option<String>,
) -> Result<Info> {
    open.unlocked()?;
    if let Some(existing) = sessions.existing(&name).await {
        let mut s = existing.lock().await;
        if let Some(ks) = database {
            s.keyspace = Some(ks);
        }
        return info(&s).await;
    }
    let saved_name = crate::nosql::sessions::connection(&name);
    let saved = store::load(&crate::dir(&app)?)?
        .into_iter()
        .find(|c| c.name == saved_name)
        .ok_or_else(|| Error::NoSuchConnection(saved_name.to_string()))?;
    if saved.kind != Engine::Cassandra {
        return Err(Error::Invalid("not a Cassandra connection"));
    }
    let password = store::password_for(&saved)?;
    let ssh_secret = if saved.ssh.is_some() { store::stored(store::Secret::Ssh, &saved.name)? } else { None };
    let s = open_session(saved, password, ssh_secret, database).await?;
    let r = info(&s).await;
    sessions.insert_new(name, s).await;
    r
}

#[tauri::command]
pub async fn cassandra_keyspaces(
    open: State<'_, crate::Open>,
    sessions: State<'_, Sessions>,
    name: String,
) -> Result<Vec<String>> {
    open.unlocked()?;
    let s = sessions.get(&name).await?;
    let rows = s.query("SELECT keyspace_name FROM system_schema.keyspaces", &[]).await?;
    let mut names: Vec<String> = rows
        .iter()
        .filter_map(|r| r["keyspace_name"].as_str())
        .filter(|k| !k.starts_with("system"))
        .map(String::from)
        .collect();
    names.sort();
    Ok(names)
}

/// A table's primary key: partition key columns, then clustering columns, each with its CQL type.
#[derive(Debug, Serialize, PartialEq)]
pub struct Described {
    keys: Vec<(String, String)>,
}

async fn describe(s: &Session, table: &str) -> Result<Described> {
    let ks = s.keyspace.clone().ok_or(Error::Invalid("choose a keyspace first (⌘K)"))?;
    let rows = s
        .query("SELECT column_name, kind, position, type FROM system_schema.columns WHERE keyspace_name = ? AND table_name = ?", &[ks, table.to_string()])
        .await?;
    let mut keys: Vec<(u8, i64, String, String)> = rows
        .iter()
        .filter_map(|r| {
            let rank = match r["kind"].as_str()? {
                "partition_key" => 0,
                "clustering" => 1,
                _ => return None,
            };
            Some((
                rank,
                r["position"].as_i64().unwrap_or(0),
                r["column_name"].as_str()?.to_string(),
                r["type"].as_str().unwrap_or_default().to_string(),
            ))
        })
        .collect();
    keys.sort();
    Ok(Described { keys: keys.into_iter().map(|(_, _, n, t)| (n, t)).collect() })
}

#[tauri::command]
pub async fn cassandra_describe(
    open: State<'_, crate::Open>,
    sessions: State<'_, Sessions>,
    name: String,
    table: String,
) -> Result<Described> {
    open.unlocked()?;
    let s = sessions.get(&name).await?;
    describe(&s, &table).await
}

#[derive(Debug, Serialize)]
pub struct Page {
    rows: Vec<Value>,
    next: Option<String>,
}

/// One page of a statement's rows, from where `token` (an earlier page's) left off.
async fn page(s: &Session, cql: &str, size: i32, token: Option<&str>) -> Result<Page> {
    let state = match token {
        Some(t) => {
            PagingState::new_from_raw_bytes(base64::engine::general_purpose::STANDARD.decode(t).map_err(failed)?)
        }
        None => PagingState::start(),
    };
    let mut statement = Statement::new(cql);
    statement.set_page_size(size);
    let (result, more) = s.cql.query_single_page(statement, &[], state).await.map_err(failed)?;
    let next = match more.into_paging_control_flow() {
        ControlFlow::Continue(state) => {
            state.as_bytes_slice().map(|b| base64::engine::general_purpose::STANDARD.encode(b.as_ref()))
        }
        ControlFlow::Break(()) => None,
    };
    Ok(Page { rows: rows_of(result)?, next })
}

/// A page of a table's rows: all of them, or those `filter` (a CQL condition) matches, which Cassandra reads the
/// whole table to find (`ALLOW FILTERING`).
#[tauri::command]
#[expect(clippy::too_many_arguments, reason = "a Tauri command's arguments are its states and the request's fields")]
pub async fn cassandra_page(
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
    table: String,
    filter: String,
    size: i32,
    next: Option<String>,
) -> Result<Page> {
    open.unlocked()?;
    let s = sessions.get(&name).await?;
    let filter = filter.trim();
    if filter.contains(';') {
        return Err(Error::Invalid("a filter is one condition, such as age > 30"));
    }
    let t = s.table(&table)?;
    let cql = if filter.is_empty() {
        format!("SELECT JSON * FROM {t}")
    } else {
        format!("SELECT JSON * FROM {t} WHERE {filter} ALLOW FILTERING")
    };
    timed(&prefs, page(&s, &cql, size.clamp(1, 1000), next.as_deref())).await
}

async fn timed<T>(prefs: &crate::Prefs, f: impl Future<Output = Result<T>>) -> Result<T> {
    match crate::query_limit(prefs) {
        Some(d) => tokio::time::timeout(d, f).await.map_err(|_| Error::QueryTimeout(d.as_secs()))?,
        None => f.await,
    }
}

/// Whether a CQL statement only reads.
pub fn reads(cql: &str) -> bool {
    matches!(crate::sql::driver::first_word(cql).as_str(), "select" | "describe" | "desc" | "list")
}

/// A SELECT as `SELECT JSON`, so every type comes back as JSON.
fn as_json(cql: &str) -> String {
    let trimmed = cql.trim_start();
    let rest = &trimmed[6.min(trimmed.len())..];
    if crate::sql::driver::first_word(cql) == "select" && crate::sql::driver::first_word(rest) != "json" {
        format!("SELECT JSON{rest}")
    } else {
        cql.to_string()
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Docs { title: String, docs: Vec<Value>, more: bool },
    Done { title: String, text: String },
}

/// Runs the console's statements in order, each with its own result; `USE` moves the session to a keyspace.
#[tauri::command]
#[expect(clippy::too_many_arguments, reason = "a Tauri command's arguments are its states and the request's fields")]
pub async fn cassandra_run(
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
    text: String,
    limit: i32,
    confirmed: bool,
    unlock: Option<Unlock>,
) -> Result<Vec<Outcome>> {
    open.unlocked()?;
    let mut s = sessions.get(&name).await?;
    let statements = crate::sql::driver::duckdb::split_statements(&text);
    if statements.is_empty() {
        return Err(Error::Invalid("type a CQL statement, such as SELECT * FROM orders"));
    }
    safety::check(s.saved.safety, statements.iter().all(|c| reads(c)), confirmed, unlock.as_ref(), s.password.as_ref())
        .await?;
    crate::app::console::record(&text);
    timed(&prefs, async {
        // The keyspace the view shows is the one statements without one go to.
        if let Some(ks) = &s.keyspace {
            s.cql.use_keyspace(ks, true).await.map_err(failed)?;
        }
        let mut out = vec![];
        for (i, cql) in statements.iter().enumerate() {
            let title = format!("Statement {}", i + 1);
            if crate::sql::driver::first_word(cql) == "use" {
                let ks = cql.trim().trim_end_matches(';')[3..].trim().trim_matches('"').to_string();
                s.cql.use_keyspace(&ks, true).await.map_err(failed)?;
                s.keyspace = Some(ks.clone());
                out.push(Outcome::Done { title, text: format!("Using keyspace {ks}") });
            } else if reads(cql) {
                let p = page(&s, &as_json(cql), limit.clamp(1, 50_000), None).await?;
                out.push(Outcome::Docs { title, docs: p.rows, more: p.next.is_some() });
            } else {
                let rows = s.query(cql, &[]).await?;
                out.push(match rows.first().and_then(|r| r["[applied]"].as_bool()) {
                    Some(false) => Outcome::Done { title, text: "Not applied: the condition didn't hold".into() },
                    _ => Outcome::Done { title, text: "Done".into() },
                });
            }
        }
        Ok(out)
    })
    .await
}

/// A change from the grid or the row editor. `key` is the row's primary key values as JSON (`{"id": 1}`); `value`
/// and `row` are JSON, read by Cassandra's `fromJson()`.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    Set { table: String, key: Map<String, Value>, field: String, value: String },
    Remove { table: String, key: Map<String, Value>, field: String },
    Replace { table: String, key: Map<String, Value>, row: String },
    Insert { table: String, row: String },
    Delete { table: String, keys: Vec<Map<String, Value>> },
}

/// `"a" = fromJson(?) AND "b" = fromJson(?)` for a key, and its values.
fn key_where(key: &Map<String, Value>) -> (String, Vec<String>) {
    let parts: Vec<String> = key.keys().map(|k| format!("{} = fromJson(?)", quote(k))).collect();
    (parts.join(" AND "), key.values().map(Value::to_string).collect())
}

fn row_of(text: &str) -> Result<Map<String, Value>> {
    match serde_json::from_str(text).map_err(|e| Error::Cassandra(format!("the row isn't JSON: {e}")))? {
        Value::Object(m) => Ok(m),
        _ => Err(Error::Invalid("a row is a JSON object, { \"column\": value }")),
    }
}

async fn apply(s: &Session, edit: Edit) -> Result<String> {
    let gone = || Error::Invalid("the row is gone; reload");
    match edit {
        Edit::Set { table, key, field, value } => {
            if key.contains_key(&field) {
                return Err(Error::Invalid("a primary key column can't change; copy the row instead"));
            }
            serde_json::from_str::<Value>(&value)
                .map_err(|e| Error::Cassandra(format!("the value isn't JSON: {e}")))?;
            let (cond, mut values) = key_where(&key);
            values.insert(0, value);
            let cql = format!("UPDATE {} SET {} = fromJson(?) WHERE {cond} IF EXISTS", s.table(&table)?, quote(&field));
            if s.applied(&cql, &values).await? { Ok("Saved".into()) } else { Err(gone()) }
        }
        Edit::Remove { table, key, field } => {
            if key.contains_key(&field) {
                return Err(Error::Invalid("a primary key column can't be removed"));
            }
            let (cond, values) = key_where(&key);
            let cql = format!("DELETE {} FROM {} WHERE {cond} IF EXISTS", quote(&field), s.table(&table)?);
            if s.applied(&cql, &values).await? { Ok("Removed the value".into()) } else { Err(gone()) }
        }
        Edit::Replace { table, key, row } => {
            let row = row_of(&row)?;
            if key.iter().any(|(k, v)| row.get(k) != Some(v)) {
                return Err(Error::Invalid("the primary key can't change; copy the row instead"));
            }
            let (cond, values) = key_where(&key);
            let exists = s.query(&format!("SELECT JSON * FROM {} WHERE {cond}", s.table(&table)?), &values).await?;
            if exists.is_empty() {
                return Err(gone());
            }
            // Columns left out become null, as the whole row is written.
            s.query(&format!("INSERT INTO {} JSON ?", s.table(&table)?), &[Value::Object(row).to_string()]).await?;
            Ok("Saved".into())
        }
        Edit::Insert { table, row } => {
            let row = row_of(&row)?;
            let cql = format!("INSERT INTO {} JSON ? IF NOT EXISTS", s.table(&table)?);
            if s.applied(&cql, &[Value::Object(row).to_string()]).await? {
                Ok("Inserted".into())
            } else {
                Err(Error::Invalid("a row with that primary key exists already"))
            }
        }
        Edit::Delete { table, keys } => {
            for key in &keys {
                let (cond, values) = key_where(key);
                s.query(&format!("DELETE FROM {} WHERE {cond}", s.table(&table)?), &values).await?;
            }
            Ok(format!("Deleted {}", keys.len()))
        }
    }
}

#[tauri::command]
pub async fn cassandra_edit(
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
    edit: Edit,
    confirmed: bool,
    unlock: Option<Unlock>,
) -> Result<String> {
    open.unlocked()?;
    let s = sessions.get(&name).await?;
    safety::check(s.saved.safety, false, confirmed, unlock.as_ref(), s.password.as_ref()).await?;
    crate::app::console::record(&format!("{edit:?}"));
    timed(&prefs, apply(&s, edit)).await
}

#[derive(Debug, Serialize)]
pub struct ConnectionInfo {
    server: String,
    driver: String,
    cipher: Option<String>,
    ssh: Option<String>,
}

#[tauri::command]
pub async fn cassandra_connection_info(sessions: State<'_, Sessions>, name: String) -> Result<ConnectionInfo> {
    let s = sessions.get(&name).await?;
    let ssh = s.saved.ssh.as_ref().map(|t| format!("{}@{}", t.user, t.host));
    Ok(ConnectionInfo {
        server: s.server.clone(),
        driver: "scylla (CQL native protocol)".into(),
        cipher: s.encrypted.then(|| "TLS".into()),
        ssh,
    })
}

#[cfg(test)]
mod tests;

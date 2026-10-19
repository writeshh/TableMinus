//! Amazon `DynamoDB` through its JSON API, signed with Signature Version 4 ([`sigv4`]). Not SQL, so like `MongoDB` it
//! has its own sessions and commands: the sidebar lists tables, a table's items come back as `DynamoDB`'s typed
//! JSON (`{"S": "…"}`, `{"N": "…"}`…) for the grid, and the console runs `PartiQL` (`ExecuteStatement`). Edits go
//! by the item's key, with values typed in `PartiQL`'s literal syntax ([`literal`]).

pub mod literal;
pub mod sigv4;

use crate::connect::store::{self, Saved};
use crate::connect::tls::TlsMode;
use crate::error::{Error, Result};
use crate::safety::{self, Unlock};
use crate::sql::db::Engine;
use crate::sql::driver::http::{Endpoint, https_connector};
use hyper::header::{self, HeaderName, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::time::SystemTime;
use tauri::{AppHandle, State};

/// An answer holds a page of items: `DynamoDB` caps one at 1 MB, so this is plenty.
const MAX_ANSWER: usize = 64 * 1024 * 1024;
/// The most tables listed.
const MAX_TABLES: usize = 10_000;

pub(crate) struct Session {
    endpoint: Endpoint,
    region: String,
    access_key: String,
    secret: SecretString,
    /// Shown as the server: `DynamoDB Local` or `DynamoDB (us-east-1)`.
    server: String,
    encrypted: bool,
    saved: Saved,
    password: Option<SecretString>,
    _tunnel: Option<crate::connect::ssh::Tunnel>,
}

/// The open sessions, by session key.
pub(crate) type Sessions = crate::nosql::sessions::Sessions<Session>;

impl crate::nosql::sessions::Session for Session {
    fn saved_mut(&mut self) -> &mut Saved {
        &mut self.saved
    }
}

/// The region from `dynamodb.<region>.amazonaws.com`, or the URL's `?region=`, else `us-east-1`.
fn region_of(u: &url::Url) -> String {
    let host = u.host_str().unwrap_or_default();
    let from_host = host.strip_prefix("dynamodb.").and_then(|h| h.split_once('.')).map(|(r, _)| r.to_string());
    u.query_pairs()
        .find(|(k, _)| k == "region")
        .map(|(_, v)| v.into_owned())
        .or(from_host)
        .unwrap_or_else(|| "us-east-1".into())
}

impl Session {
    /// Calls `action` (`ListTables`, `ExecuteStatement`…) with `body`, and returns its answer.
    async fn call(&self, action: &str, body: &Value) -> Result<Value> {
        let body = body.to_string().into_bytes();
        let (amz_date, date) = sigv4::amz_date(SystemTime::now());
        let target = format!("DynamoDB_20120810.{action}");
        let host = self.endpoint.host_header();
        let content_type = "application/x-amz-json-1.0";
        let signed = sigv4::Request {
            method: "POST",
            path: "/",
            query: "",
            headers: &[
                ("content-type", content_type),
                ("host", &host),
                ("x-amz-date", &amz_date),
                ("x-amz-target", &target),
            ],
            body: &body,
        };
        let auth = sigv4::authorization(
            &signed,
            &self.access_key,
            self.secret.expose_secret(),
            &self.region,
            "dynamodb",
            (&amz_date, &date),
        );
        let value = |v: &str| {
            HeaderValue::from_str(v).map_err(|_| Error::Invalid("the access key has characters a header can't hold"))
        };
        let mut auth = value(&auth)?;
        auth.set_sensitive(true);
        let headers = [
            (header::CONTENT_TYPE, value(content_type)?),
            (HeaderName::from_static("x-amz-date"), value(&amz_date)?),
            (HeaderName::from_static("x-amz-target"), value(&target)?),
            (header::AUTHORIZATION, auth),
        ];
        let (status, bytes) = self.endpoint.post("/", &headers, body, MAX_ANSWER).await?;
        let answer: Value =
            serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({ "message": String::from_utf8_lossy(&bytes) }));
        if !(200..300).contains(&status) {
            let kind =
                answer.get("__type").and_then(Value::as_str).and_then(|t| t.rsplit('#').next()).unwrap_or("error");
            let message = answer.get("message").or_else(|| answer.get("Message")).and_then(Value::as_str).unwrap_or("");
            return Err(Error::Dynamo(format!("{kind}: {message}").trim_end_matches(": ").to_string()));
        }
        Ok(answer)
    }

    async fn tables(&self) -> Result<Vec<String>> {
        let mut names = vec![];
        let mut start: Option<String> = None;
        loop {
            let mut body = json!({ "Limit": 100 });
            if let Some(s) = &start {
                body["ExclusiveStartTableName"] = json!(s);
            }
            let page = self.call("ListTables", &body).await?;
            names.extend(
                page["TableNames"].as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from),
            );
            start = page.get("LastEvaluatedTableName").and_then(Value::as_str).map(String::from);
            if start.is_none() || names.len() >= MAX_TABLES {
                return Ok(names);
            }
        }
    }
}

async fn open_session(saved: Saved, secret: Option<SecretString>, ssh_secret: Option<SecretString>) -> Result<Session> {
    if !saved.options.startup_script.trim().is_empty() {
        crate::sql::db::run_startup_script(&saved.options.startup_script).await?;
    }
    let u = url::Url::parse(&saved.url)?;
    let host =
        u.host_str().ok_or(Error::Invalid("enter the endpoint's host, e.g. dynamodb.us-east-1.amazonaws.com"))?;
    let local = matches!(host, "127.0.0.1" | "localhost" | "[::1]");
    let tls = if saved.tls.mode == TlsMode::Disable || (local && saved.tls.mode == TlsMode::Prefer) {
        None
    } else {
        https_connector(&saved.tls)?
    };
    let port = u.port().unwrap_or(if tls.is_some() { 443 } else { 8000 });
    let tunnel = match &saved.ssh {
        Some(ssh) => Some(crate::connect::ssh::open_for(Engine::Dynamodb, &saved.url, ssh, ssh_secret.as_ref()).await?),
        None => None,
    };
    let encrypted = tls.is_some();
    let endpoint = Endpoint::new("DynamoDB", host, port, tls, tunnel.as_ref().map(|t| t.port));
    let access_key = url::form_urlencoded::parse(format!("k={}", u.username()).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .filter(|k| !k.is_empty())
        .ok_or(Error::Invalid("enter the access key ID"))?;
    let region = region_of(&u);
    let server =
        if host.ends_with(".amazonaws.com") { format!("DynamoDB ({region})") } else { "DynamoDB Local".into() };
    let s = Session {
        endpoint,
        region,
        access_key,
        secret: secret.clone().unwrap_or_else(|| SecretString::from(String::new())),
        server,
        encrypted,
        saved,
        password: secret,
        _tunnel: tunnel,
    };
    s.call("ListTables", &json!({ "Limit": 1 })).await?; // says now whether the keys and endpoint are right
    Ok(s)
}

/// What the workspace header shows, in the shape `connect` gives for SQL: the tables are its tables.
#[derive(Debug, Serialize)]
pub struct Info {
    server: String,
    database: Option<String>,
    can_switch: bool,
    tables: Vec<String>,
}

async fn info(s: &Session) -> Result<Info> {
    let mut tables = s.tables().await?;
    tables.sort();
    Ok(Info { server: s.server.clone(), database: Some(s.region.clone()), can_switch: false, tables })
}

pub async fn test(saved: Saved, secret: Option<SecretString>, ssh_secret: Option<SecretString>) -> Result<String> {
    let s = open_session(saved, secret, ssh_secret).await?;
    let i = info(&s).await?;
    Ok(format!("Connected · {} · {} tables", i.server, i.tables.len()))
}

#[tauri::command]
pub async fn dynamo_connect(
    app: AppHandle,
    open: State<'_, crate::Open>,
    sessions: State<'_, Sessions>,
    name: String,
) -> Result<Info> {
    open.unlocked()?;
    if let Some(existing) = sessions.existing(&name).await {
        return info(&*existing.lock().await).await;
    }
    let saved_name = name.split_once('\u{1f}').map_or(name.as_str(), |(n, _)| n);
    let saved = store::load(&crate::dir(&app)?)?
        .into_iter()
        .find(|c| c.name == saved_name)
        .ok_or_else(|| Error::NoSuchConnection(saved_name.to_string()))?;
    if saved.kind != Engine::Dynamodb {
        return Err(Error::Invalid("not a DynamoDB connection"));
    }
    let secret = store::password_for(&saved)?;
    let ssh_secret = if saved.ssh.is_some() { store::stored(store::Secret::Ssh, &saved.name)? } else { None };
    let s = open_session(saved, secret, ssh_secret).await?;
    let r = info(&s).await;
    sessions.insert_new(name, s).await;
    r
}

/// A table's key and size, from `DescribeTable`.
#[derive(Debug, Serialize, PartialEq)]
pub struct Described {
    /// The key's attributes, partition key first, with their types (`S`, `N`, `B`).
    keys: Vec<(String, String)>,
    /// `DynamoDB`'s count, updated about every six hours.
    items: Option<u64>,
    indexes: Vec<String>,
}

fn described(answer: &Value) -> Described {
    let t = &answer["Table"];
    let types: HashMap<&str, &str> = t["AttributeDefinitions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| Some((d["AttributeName"].as_str()?, d["AttributeType"].as_str()?)))
        .collect();
    let mut keys: Vec<(String, String, bool)> = t["KeySchema"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|k| {
            let name = k["AttributeName"].as_str()?;
            Some((name.to_string(), types.get(name).copied().unwrap_or("S").to_string(), k["KeyType"] == "HASH"))
        })
        .collect();
    keys.sort_by_key(|(_, _, hash)| !hash);
    let indexes = ["GlobalSecondaryIndexes", "LocalSecondaryIndexes"]
        .iter()
        .flat_map(|k| t[*k].as_array().into_iter().flatten())
        .filter_map(|i| i["IndexName"].as_str().map(String::from))
        .collect();
    Described { keys: keys.into_iter().map(|(n, t, _)| (n, t)).collect(), items: t["ItemCount"].as_u64(), indexes }
}

#[tauri::command]
pub async fn dynamo_describe(
    open: State<'_, crate::Open>,
    sessions: State<'_, Sessions>,
    name: String,
    table: String,
) -> Result<Described> {
    open.unlocked()?;
    let s = sessions.get(&name).await?;
    Ok(described(&s.call("DescribeTable", &json!({ "TableName": table })).await?))
}

/// One page of a statement's items, and the token for the next page.
#[derive(Debug, Serialize)]
pub struct Page {
    items: Vec<Value>,
    next: Option<String>,
    /// For a statement that changes data: it ran.
    done: bool,
}

/// Whether a `PartiQL` statement only reads.
pub fn reads(statement: &str) -> bool {
    crate::sql::driver::first_word(statement) == "select"
}

/// A table's items as `"table"`, quoted as `PartiQL` quotes names.
pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

async fn statement(s: &Session, statement: &str, limit: Option<u32>, next: Option<String>) -> Result<Page> {
    let mut body = json!({ "Statement": statement.trim().trim_end_matches(';') });
    if let Some(l) = limit {
        body["Limit"] = json!(l);
    }
    if let Some(n) = next {
        body["NextToken"] = json!(n);
    }
    let answer = s.call("ExecuteStatement", &body).await?;
    Ok(Page {
        items: answer["Items"].as_array().cloned().unwrap_or_default(),
        next: answer.get("NextToken").and_then(Value::as_str).map(String::from),
        done: !reads(statement),
    })
}

/// A page of a table's items: all of them, or those `filter` (a `PartiQL` condition, `"age" > 30`) matches. `limit`
/// caps the items read (not matched), as `DynamoDB` counts it.
#[tauri::command]
#[expect(clippy::too_many_arguments, reason = "a Tauri command's arguments are its states and the request's fields")]
pub async fn dynamo_page(
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
    table: String,
    filter: String,
    limit: u32,
    next: Option<String>,
) -> Result<Page> {
    open.unlocked()?;
    let s = sessions.get(&name).await?;
    let filter = filter.trim();
    let sql = if filter.is_empty() {
        format!("SELECT * FROM {}", quote(&table))
    } else {
        format!("SELECT * FROM {} WHERE {filter}", quote(&table))
    };
    if !reads(&sql) || sql.contains(';') {
        return Err(Error::Invalid("a filter is one condition, such as \"age\" > 30"));
    }
    timed(&prefs, statement(&s, &sql, Some(limit.clamp(1, 1000)), next)).await
}

async fn timed<T>(prefs: &crate::Prefs, f: impl Future<Output = Result<T>>) -> Result<T> {
    match crate::query_limit(prefs) {
        Some(d) => tokio::time::timeout(d, f).await.map_err(|_| Error::QueryTimeout(d.as_secs()))?,
        None => f.await,
    }
}

/// Runs one `PartiQL` statement from the console. Safe mode asks before anything but a SELECT.
#[tauri::command]
#[expect(clippy::too_many_arguments, reason = "a Tauri command's arguments are its states and the request's fields")]
pub async fn dynamo_run(
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
    statement_text: String,
    next: Option<String>,
    confirmed: bool,
    unlock: Option<Unlock>,
) -> Result<Page> {
    open.unlocked()?;
    let s = sessions.get(&name).await?;
    if statement_text.trim().is_empty() {
        return Err(Error::Invalid("type a PartiQL statement, such as SELECT * FROM \"orders\""));
    }
    safety::check(s.saved.safety, reads(&statement_text), confirmed, unlock.as_ref(), s.password.as_ref()).await?;
    crate::app::console::record(&statement_text);
    timed(&prefs, statement(&s, &statement_text, None, next)).await
}

/// A change from the grid or the item editor. `key` is the item's key attributes in typed JSON; `value` and
/// `item` are typed in `PartiQL`'s literal syntax.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    Set { table: String, key: Map<String, Value>, field: String, value: String },
    Remove { table: String, key: Map<String, Value>, field: String },
    Replace { table: String, key: Map<String, Value>, item: String },
    Insert { table: String, key_names: Vec<String>, item: String },
    Delete { table: String, keys: Vec<Map<String, Value>> },
}

/// `attribute_exists(#k0) and …`, or `not` that, over the key's attributes, with their names in `names`.
fn key_condition(keys: &[&String], exists: bool, names: &mut Map<String, Value>) -> String {
    keys.iter()
        .enumerate()
        .map(|(i, k)| {
            names.insert(format!("#k{i}"), json!(k));
            if exists { format!("attribute_exists(#k{i})") } else { format!("attribute_not_exists(#k{i})") }
        })
        .collect::<Vec<_>>()
        .join(" and ")
}

fn item_of(text: &str) -> Result<Map<String, Value>> {
    match literal::parse(text)? {
        Value::Object(mut m) => match m.remove("M") {
            Some(Value::Object(item)) => Ok(item),
            _ => Err(Error::Invalid("an item is written {'name': value, …}")),
        },
        _ => Err(Error::Invalid("an item is written {'name': value, …}")),
    }
}

async fn apply(s: &Session, edit: Edit) -> Result<String> {
    match edit {
        Edit::Set { table, key, field, value } => {
            if key.contains_key(&field) {
                return Err(Error::Invalid("a key attribute can't change; copy the item instead"));
            }
            let mut names = Map::new();
            let condition = key_condition(&key.keys().collect::<Vec<_>>(), true, &mut names);
            names.insert("#f".into(), json!(field));
            s.call(
                "UpdateItem",
                &json!({
                    "TableName": table, "Key": key, "UpdateExpression": "SET #f = :v", "ConditionExpression": condition,
                    "ExpressionAttributeNames": names, "ExpressionAttributeValues": { ":v": literal::parse(&value)? },
                }),
            )
            .await?;
            Ok("Saved".into())
        }
        Edit::Remove { table, key, field } => {
            if key.contains_key(&field) {
                return Err(Error::Invalid("a key attribute can't be removed"));
            }
            let mut names = Map::new();
            let condition = key_condition(&key.keys().collect::<Vec<_>>(), true, &mut names);
            names.insert("#f".into(), json!(field));
            s.call(
                "UpdateItem",
                &json!({
                    "TableName": table, "Key": key, "UpdateExpression": "REMOVE #f", "ConditionExpression": condition,
                    "ExpressionAttributeNames": names,
                }),
            )
            .await?;
            Ok("Removed the attribute".into())
        }
        Edit::Replace { table, key, item } => {
            let item = item_of(&item)?;
            if key.iter().any(|(k, v)| item.get(k) != Some(v)) {
                return Err(Error::Invalid("the key attributes can't change; copy the item instead"));
            }
            let mut names = Map::new();
            let condition = key_condition(&key.keys().collect::<Vec<_>>(), true, &mut names);
            s.call("PutItem", &json!({ "TableName": table, "Item": item, "ConditionExpression": condition, "ExpressionAttributeNames": names }))
                .await?;
            Ok("Saved".into())
        }
        Edit::Insert { table, key_names, item } => {
            let item = item_of(&item)?;
            if let Some(missing) = key_names.iter().find(|k| !item.contains_key(*k)) {
                return Err(Error::Dynamo(format!("the item needs its key attribute {missing}")));
            }
            let mut names = Map::new();
            let condition = key_condition(&key_names.iter().collect::<Vec<_>>(), false, &mut names);
            s.call("PutItem", &json!({ "TableName": table, "Item": item, "ConditionExpression": condition, "ExpressionAttributeNames": names }))
                .await
                .map_err(|e| match e {
                    Error::Dynamo(m) if m.starts_with("ConditionalCheckFailed") => Error::Invalid("an item with that key exists already"),
                    e => e,
                })?;
            Ok("Inserted".into())
        }
        Edit::Delete { table, keys } => {
            for key in &keys {
                s.call("DeleteItem", &json!({ "TableName": table, "Key": key })).await?;
            }
            Ok(format!("Deleted {}", keys.len()))
        }
    }
}

/// Makes an edit, after safe mode's question; says what it did.
#[tauri::command]
pub async fn dynamo_edit(
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
    timed(&prefs, apply(&s, edit)).await.map_err(|e| match e {
        Error::Dynamo(m) if m.starts_with("ConditionalCheckFailed") => Error::Invalid("the item is gone; reload"),
        e => e,
    })
}

#[derive(Debug, Serialize)]
pub struct ConnectionInfo {
    server: String,
    driver: String,
    cipher: Option<String>,
    ssh: Option<String>,
}

#[tauri::command]
pub async fn dynamo_connection_info(sessions: State<'_, Sessions>, name: String) -> Result<ConnectionInfo> {
    let s = sessions.get(&name).await?;
    let tls = s.encrypted;
    let ssh = s.saved.ssh.as_ref().map(|t| format!("{}@{}", t.user, t.host));
    Ok(ConnectionInfo {
        server: s.server.clone(),
        driver: "DynamoDB JSON API (Signature Version 4)".into(),
        cipher: tls.then(|| "TLS".into()),
        ssh,
    })
}

#[cfg(test)]
mod tests;

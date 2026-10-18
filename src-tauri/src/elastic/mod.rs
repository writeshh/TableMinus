//! Elasticsearch over its REST API. Not SQL underneath, so like `MongoDB` it has its own sessions and commands: the
//! sidebar lists indices, documents come back as their `_source` with `_id` for the grid (a `query_string` filter,
//! `field:asc` sorts), and the console takes either Elasticsearch SQL or a request written as Kibana's Dev Tools
//! does (`GET /orders/_search` and a JSON body). Documents are found again by `_id`.

use crate::db::Engine;
use crate::driver::http::{Endpoint, https_connector};
use crate::error::{Error, Result};
use crate::safety::{self, Unlock};
use crate::store::{self, Saved};
use crate::tls::TlsMode;
use hyper::Method;
use hyper::header::{self, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tauri::{AppHandle, State};

/// The most an answer may hold.
const MAX_ANSWER: usize = 256 * 1024 * 1024;
/// Elasticsearch's own cap on `from + size` (`index.max_result_window`).
const MAX_WINDOW: u64 = 10_000;

pub(crate) struct Session {
    endpoint: Endpoint,
    auth: Option<HeaderValue>,
    server: String,
    cluster: String,
    encrypted: bool,
    saved: Saved,
    password: Option<SecretString>,
    _tunnel: Option<crate::ssh::Tunnel>,
}

pub(crate) type Sessions = crate::sessions::Sessions<Session>;

impl crate::sessions::Session for Session {
    fn saved_mut(&mut self) -> &mut Saved {
        &mut self.saved
    }
}

/// An index or document id in a URL path.
fn segment(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20")
}

impl Session {
    /// Sends one request and returns the answer as JSON; an error answer is Elasticsearch's own reason.
    async fn call(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut headers = vec![];
        if let Some(a) = &self.auth {
            headers.push((header::AUTHORIZATION, a.clone()));
        }
        let bytes = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
        let (status, answer) = self.endpoint.send(method, path, &headers, bytes, MAX_ANSWER).await?;
        let value: Value = serde_json::from_slice(&answer)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&answer).trim().to_string()));
        if !(200..300).contains(&status) {
            let reason = value
                .pointer("/error/root_cause/0/reason")
                .or_else(|| value.pointer("/error/reason"))
                .and_then(Value::as_str)
                .map_or_else(|| value.to_string().chars().take(500).collect(), String::from);
            let kind = value.pointer("/error/type").and_then(Value::as_str).unwrap_or("error");
            return Err(Error::Elastic(match status {
                401 | 403 => format!("the server refused the sign-in ({status}): {reason}"),
                _ => format!("{kind}: {reason}"),
            }));
        }
        Ok(value)
    }

    async fn indices(&self) -> Result<Vec<String>> {
        let list = self.call(Method::GET, "/_cat/indices?format=json&h=index&expand_wildcards=open", None).await?;
        let mut names: Vec<String> = list
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| i["index"].as_str())
            .filter(|n| !n.starts_with('.'))
            .map(String::from)
            .collect();
        names.sort();
        Ok(names)
    }
}

async fn open_session(
    saved: Saved,
    password: Option<SecretString>,
    ssh_secret: Option<SecretString>,
) -> Result<Session> {
    if !saved.options.startup_script.trim().is_empty() {
        crate::db::run_startup_script(&saved.options.startup_script).await?;
    }
    let u = url::Url::parse(&saved.url)?;
    let host = u.host_str().ok_or(Error::Invalid("enter the server's host"))?.to_string();
    let local = matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]");
    // `elastics://` (TablePlus's spelling) is always TLS; otherwise as for the other HTTP engines: Prefer checks the
    // certificate, except to a server on this computer, which goes plain.
    let tls_on = u.scheme() == "elastics"
        || !(saved.tls.mode == TlsMode::Disable || (local && saved.tls.mode == TlsMode::Prefer));
    let tls = match saved.tls.mode {
        _ if !tls_on => None,
        // elastics:// with SSL set to Disable: encrypt, as the scheme says, without checking.
        TlsMode::Disable => https_connector(&crate::tls::Tls { mode: TlsMode::Require, ..saved.tls.clone() })?,
        _ => https_connector(&saved.tls)?,
    };
    let port = u.port().unwrap_or(9200);
    let tunnel = match &saved.ssh {
        Some(ssh) => Some(crate::ssh::open_for(Engine::Elasticsearch, &saved.url, ssh, ssh_secret.as_ref()).await?),
        None => None,
    };
    let encrypted = tls.is_some();
    let endpoint = Endpoint::new("Elasticsearch", &host, port, tls, tunnel.as_ref().map(|t| t.port));
    let user = url::form_urlencoded::parse(format!("u={}", u.username()).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    let secret = password.as_ref().map(ExposeSecret::expose_secret).filter(|p| !p.is_empty());
    // A user signs in with Basic; a password alone is an API key.
    let auth = match (user.is_empty(), secret) {
        (_, None) => None,
        (false, Some(p)) => Some(format!("Basic {}", base64(&format!("{user}:{p}")))),
        (true, Some(key)) => Some(format!("ApiKey {key}")),
    }
    .map(|a| {
        let mut v =
            HeaderValue::from_str(&a).map_err(|_| Error::Invalid("the password has characters a header can't hold"))?;
        v.set_sensitive(true);
        Ok::<_, Error>(v)
    })
    .transpose()?;
    let mut s = Session {
        endpoint,
        auth,
        server: String::new(),
        cluster: String::new(),
        encrypted,
        saved,
        password,
        _tunnel: tunnel,
    };
    let root = s.call(Method::GET, "/", None).await?;
    let flavor = if root.pointer("/version/distribution").and_then(Value::as_str) == Some("opensearch") {
        "OpenSearch"
    } else {
        "Elasticsearch"
    };
    s.server = format!("{flavor} {}", root.pointer("/version/number").and_then(Value::as_str).unwrap_or_default())
        .trim()
        .to_string();
    s.cluster = root["cluster_name"].as_str().unwrap_or_default().to_string();
    Ok(s)
}

fn base64(s: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(s)
}

/// What the workspace header shows: the indices are its tables.
#[derive(Debug, Serialize)]
pub struct Info {
    server: String,
    database: Option<String>,
    can_switch: bool,
    tables: Vec<String>,
}

async fn info(s: &Session) -> Result<Info> {
    Ok(Info {
        server: s.server.clone(),
        database: Some(s.cluster.clone()).filter(|c| !c.is_empty()),
        can_switch: false,
        tables: s.indices().await?,
    })
}

pub async fn test(saved: Saved, password: Option<SecretString>, ssh_secret: Option<SecretString>) -> Result<String> {
    let s = open_session(saved, password, ssh_secret).await?;
    let i = info(&s).await?;
    Ok(format!("Connected · {} · {} indices", i.server, i.tables.len()))
}

#[tauri::command]
pub async fn elastic_connect(
    app: AppHandle,
    open: State<'_, crate::Open>,
    sessions: State<'_, Sessions>,
    name: String,
) -> Result<Info> {
    open.unlocked()?;
    if let Some(existing) = sessions.existing(&name).await {
        return info(&*existing.lock().await).await;
    }
    let saved_name = crate::sessions::connection(&name);
    let saved = store::load(&crate::dir(&app)?)?
        .into_iter()
        .find(|c| c.name == saved_name)
        .ok_or_else(|| Error::NoSuchConnection(saved_name.to_string()))?;
    if saved.kind != Engine::Elasticsearch {
        return Err(Error::Invalid("not an Elasticsearch connection"));
    }
    let password = store::password_for(&saved)?;
    let ssh_secret = if saved.ssh.is_some() { store::stored(store::Secret::Ssh, &saved.name)? } else { None };
    let s = open_session(saved, password, ssh_secret).await?;
    let r = info(&s).await;
    sessions.insert_new(name, s).await;
    r
}

/// Hits as documents: each `_source` with its `_id` first.
fn documents(answer: &Value) -> Vec<Value> {
    answer
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|h| {
            let mut doc = Map::new();
            doc.insert("_id".into(), h["_id"].clone());
            if let Some(source) = h["_source"].as_object() {
                doc.extend(source.clone());
            }
            Value::Object(doc)
        })
        .collect()
}

/// `field:asc, other:desc` as Elasticsearch's sort.
fn sort_of(text: &str) -> Value {
    Value::Array(
        text.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                let (field, order) =
                    s.rsplit_once(':').filter(|(_, o)| matches!(*o, "asc" | "desc")).unwrap_or((s, "asc"));
                json!({ field.trim(): { "order": order } })
            })
            .collect(),
    )
}

/// `query_string` for a filter typed as Lucene's query syntax (`status:active AND age:>30`); everything without one.
fn query_of(filter: &str) -> Value {
    let filter = filter.trim();
    if filter.is_empty() { json!({ "match_all": {} }) } else { json!({ "query_string": { "query": filter } }) }
}

#[derive(Debug, Serialize)]
pub struct Page {
    docs: Vec<Value>,
    /// The hits the query matches, as shown (`10,000+` when Elasticsearch stopped counting).
    total: Option<String>,
    more: bool,
}

#[tauri::command]
#[expect(clippy::too_many_arguments, reason = "a Tauri command's arguments are its states and the request's fields")]
pub async fn elastic_page(
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
    index: String,
    filter: String,
    sort: String,
    from: u64,
    size: u64,
) -> Result<Page> {
    open.unlocked()?;
    let s = sessions.get(&name).await?;
    let size = size.clamp(1, 1000);
    if from + size > MAX_WINDOW {
        return Err(Error::Invalid("Elasticsearch pages no further than 10,000 documents; narrow the filter or sort"));
    }
    let mut body = json!({ "from": from, "size": size, "query": query_of(&filter), "track_total_hits": MAX_WINDOW });
    if !sort.trim().is_empty() {
        body["sort"] = sort_of(&sort);
    }
    let answer = timed(&prefs, s.call(Method::POST, &format!("/{}/_search", segment(&index)), Some(&body))).await?;
    let total = answer.pointer("/hits/total/value").and_then(Value::as_u64);
    let capped = answer.pointer("/hits/total/relation").and_then(Value::as_str) == Some("gte");
    let docs = documents(&answer);
    let more =
        total.is_some_and(|t| from + (docs.len() as u64) < t.min(MAX_WINDOW)) || capped && from + size < MAX_WINDOW;
    Ok(Page { docs, total: total.map(|t| if capped { format!("{t}+") } else { t.to_string() }), more })
}

async fn timed<T>(prefs: &crate::Prefs, f: impl Future<Output = Result<T>>) -> Result<T> {
    match crate::query_limit(prefs) {
        Some(d) => tokio::time::timeout(d, f).await.map_err(|_| Error::QueryTimeout(d.as_secs()))?,
        None => f.await,
    }
}

/// One console entry: SQL, or a request (`GET /index/_search` then its JSON body).
#[derive(Debug, PartialEq)]
enum Entry {
    Sql(String),
    Request { method: Method, path: String, body: Option<Value> },
}

const METHODS: [&str; 5] = ["GET", "POST", "PUT", "DELETE", "HEAD"];

fn entry(text: &str) -> Result<Entry> {
    let text = text.trim();
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let mut words = first.split_whitespace();
    let method = words.next().unwrap_or_default().to_ascii_uppercase();
    if !METHODS.contains(&method.as_str()) {
        return Ok(Entry::Sql(text.trim_end_matches(';').to_string()));
    }
    let path = words.next().ok_or(Error::Invalid("a request is METHOD /path, e.g. GET /orders/_search"))?;
    if words.next().is_some() || path.bytes().any(|b| b.is_ascii_control()) {
        return Err(Error::Invalid("a request's first line is METHOD /path, its body on the lines after"));
    }
    let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    let body = if rest.trim().is_empty() {
        None
    } else {
        Some(serde_json::from_str(rest).map_err(|e| Error::Elastic(format!("the body isn't JSON: {e}")))?)
    };
    Ok(Entry::Request { method: Method::from_bytes(method.as_bytes()).unwrap_or(Method::GET), path, body })
}

/// Whether a console entry only reads: SQL (Elasticsearch's SQL is SELECT, SHOW and DESCRIBE only), GET and HEAD,
/// and POST to the endpoints that search.
fn reads(e: &Entry) -> bool {
    match e {
        Entry::Sql(_) => true,
        Entry::Request { method, path, .. } => {
            let endpoint = path.split('?').next().unwrap_or_default().rsplit('/').next().unwrap_or_default();
            *method == Method::GET
                || *method == Method::HEAD
                || (*method == Method::POST
                    && matches!(
                        endpoint,
                        "_search"
                            | "_count"
                            | "_msearch"
                            | "_mget"
                            | "_sql"
                            | "_field_caps"
                            | "_validate"
                            | "query"
                            | "translate"
                            | "_explain"
                    ))
        }
    }
}

/// What a console entry gave, in the document view's shapes.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Docs { title: String, docs: Vec<Value>, more: bool },
    Value { title: String, value: Value },
}

/// SQL's columns and rows as documents.
fn sql_documents(answer: &Value) -> Vec<Value> {
    let names: Vec<&str> =
        answer["columns"].as_array().into_iter().flatten().filter_map(|c| c["name"].as_str()).collect();
    answer["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            let cells = r.as_array().cloned().unwrap_or_default();
            Value::Object(names.iter().map(|n| (*n).to_string()).zip(cells).collect())
        })
        .collect()
}

#[tauri::command]
#[expect(clippy::too_many_arguments, reason = "a Tauri command's arguments are its states and the request's fields")]
pub async fn elastic_run(
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
    text: String,
    limit: u64,
    confirmed: bool,
    unlock: Option<Unlock>,
) -> Result<Vec<Outcome>> {
    open.unlocked()?;
    let s = sessions.get(&name).await?;
    let e = entry(&text)?;
    safety::check(s.saved.safety, reads(&e), confirmed, unlock.as_ref(), s.password.as_ref()).await?;
    crate::console::record(&text);
    timed(&prefs, run(&s, e, limit)).await
}

async fn run(s: &Session, e: Entry, limit: u64) -> Result<Vec<Outcome>> {
    match e {
        Entry::Sql(query) => {
            let answer = s
                .call(
                    Method::POST,
                    "/_sql?format=json",
                    Some(&json!({ "query": query, "fetch_size": limit.clamp(1, 10_000) })),
                )
                .await?;
            let more = answer.get("cursor").is_some();
            if let Some(cursor) = answer["cursor"].as_str() {
                // Read only one page; the cursor holds server memory until closed.
                let _ = s.call(Method::POST, "/_sql/close", Some(&json!({ "cursor": cursor }))).await; // best effort
            }
            Ok(vec![Outcome::Docs { title: "SQL".into(), docs: sql_documents(&answer), more }])
        }
        Entry::Request { method, path, body } => {
            let title = format!("{method} {path}");
            let answer = s.call(method, &path, body.as_ref()).await?;
            let mut out = vec![];
            if answer.pointer("/hits/hits").is_some() {
                out.push(Outcome::Docs { title: format!("{title} · hits"), docs: documents(&answer), more: false });
            }
            out.push(Outcome::Value { title, value: answer });
            Ok(out)
        }
    }
}

/// A change from the grid or the document editor; values and documents are JSON.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    Set { index: String, id: String, field: String, value: String },
    Remove { index: String, id: String, field: String },
    Replace { index: String, id: String, document: String },
    Insert { index: String, document: String },
    Delete { index: String, ids: Vec<String> },
}

fn json_of(text: &str, what: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|e| Error::Elastic(format!("the {what} isn't JSON: {e}")))
}

fn document_of(text: &str) -> Result<Map<String, Value>> {
    match json_of(text, "document")? {
        Value::Object(m) => Ok(m),
        _ => Err(Error::Invalid("a document is a JSON object, { \"field\": value }")),
    }
}

fn field_ok(field: &str) -> Result<()> {
    if field.is_empty() || field.starts_with('_') {
        return Err(Error::Invalid("fields starting with _ are Elasticsearch's own"));
    }
    Ok(())
}

async fn apply(s: &Session, edit: Edit) -> Result<String> {
    let doc = |index: &str, id: &str| format!("/{}/_doc/{}?refresh=wait_for", segment(index), segment(id));
    let update = |index: &str, id: &str| format!("/{}/_update/{}?refresh=wait_for", segment(index), segment(id));
    match edit {
        Edit::Set { index, id, field, value } => {
            field_ok(&field)?;
            s.call(Method::POST, &update(&index, &id), Some(&json!({ "doc": { field: json_of(&value, "value")? } })))
                .await?;
            Ok("Saved".into())
        }
        Edit::Remove { index, id, field } => {
            field_ok(&field)?;
            let script = json!({ "script": { "source": "ctx._source.remove(params.f)", "params": { "f": field } } });
            s.call(Method::POST, &update(&index, &id), Some(&script)).await?;
            Ok("Removed the field".into())
        }
        Edit::Replace { index, id, document } => {
            let mut d = document_of(&document)?;
            match d.remove("_id") {
                Some(Value::String(given)) if given != id => {
                    return Err(Error::Invalid("a document's _id can't change"));
                }
                _ => {}
            }
            // The document must still be there: a replace that creates it would bring back a deleted one.
            s.call(Method::HEAD, &format!("/{}/_doc/{}", segment(&index), segment(&id)), None)
                .await
                .map_err(|_| Error::Invalid("the document is gone; reload"))?;
            s.call(Method::PUT, &doc(&index, &id), Some(&Value::Object(d))).await?;
            Ok("Saved".into())
        }
        Edit::Insert { index, document } => {
            let mut d = document_of(&document)?;
            let r = match d.remove("_id") {
                Some(Value::String(id)) => {
                    s.call(
                        Method::PUT,
                        &format!("/{}/_create/{}?refresh=wait_for", segment(&index), segment(&id)),
                        Some(&Value::Object(d)),
                    )
                    .await?
                }
                Some(_) => return Err(Error::Invalid("an _id is text")),
                None => {
                    s.call(
                        Method::POST,
                        &format!("/{}/_doc?refresh=wait_for", segment(&index)),
                        Some(&Value::Object(d)),
                    )
                    .await?
                }
            };
            Ok(format!("Inserted, _id {}", r["_id"].as_str().unwrap_or_default()))
        }
        Edit::Delete { index, ids } => {
            for id in &ids {
                s.call(Method::DELETE, &doc(&index, id), None).await?;
            }
            Ok(format!("Deleted {}", ids.len()))
        }
    }
}

#[tauri::command]
pub async fn elastic_edit(
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
    crate::console::record(&format!("{edit:?}"));
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
pub async fn elastic_connection_info(sessions: State<'_, Sessions>, name: String) -> Result<ConnectionInfo> {
    let s = sessions.get(&name).await?;
    let ssh = s.saved.ssh.as_ref().map(|t| format!("{}@{}", t.user, t.host));
    Ok(ConnectionInfo {
        server: s.server.clone(),
        driver: "Elasticsearch REST API".into(),
        cipher: s.encrypted.then(|| "TLS".into()),
        ssh,
    })
}

#[cfg(test)]
mod tests;

//! Google BigQuery through its REST API: `jobs.query` runs a statement and `getQueryResults` waits for it and pages
//! through its rows. A service account's JSON key signs a token request (RS256, through `ring`); the emulator takes
//! no key. Datasets stand in for databases: the current one is each query's default dataset.
//!
//! Values travel as STRING parameters (`@p1`…), so the SQL above the driver converts them to their column's type
//! ([`from_text`]). A transaction is kept here, as D1's is: BigQuery runs one only inside a script, so from `begin`
//! to `commit` statements are queued with their values written in, then sent as one `BEGIN TRANSACTION; …; COMMIT
//! TRANSACTION;` script.
// ponytail: a queued statement reports no rows changed, so `Conn::save_rows` can't catch a key matching several
// rows inside a transaction (BigQuery keys aren't enforced); real BigQuery sessions (`createSession`) would allow it.

use super::clickhouse::split_statements;
use super::http::{Endpoint, bearer, https_connector};
use super::{Driver, Results, RowSink, Stopper, first_word};
use crate::db::Dialect;
use crate::error::{Error, Result};
use crate::tls::{Tls, TlsMode};
use async_trait::async_trait;
use base64::Engine as _;
use hyper::{Method, header};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value, json};
use std::any::Any;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

const API_HOST: &str = "bigquery.googleapis.com";
const TOKEN_HOST: &str = "oauth2.googleapis.com";
const SCOPE: &str = "https://www.googleapis.com/auth/bigquery";
/// The most one answer may hold.
const MAX_ANSWER: usize = 256 * 1024 * 1024;
/// Rows asked for per page.
const PAGE: u32 = 10_000;
/// How long one request waits for a job to finish before asking again, in milliseconds.
const WAIT_MS: u32 = 10_000;
/// Safe mode's bounds: BigQuery has no read-only transaction, so read-only rests on the app's parse of the SQL, as
/// for D1. These are comments, which [`BigQuery::one`] skips.
const READ_ONLY_ON: &str = "-- read-only on";
const READ_ONLY_OFF: &str = "-- read-only off";

pub struct BigQuery {
    endpoint: Endpoint,
    auth: Option<Auth>,
    project: String,
    dataset: Option<String>,
    /// Statements queued since `begin`, with their values written in, sent at `commit`.
    queued: Option<Vec<String>>,
    cancel: Arc<Notify>,
}

/// A service account's key, and the access token it got last.
struct Auth {
    endpoint: Endpoint,
    email: String,
    key: ring::signature::RsaKeyPair,
    token: Option<(header::HeaderValue, Instant)>,
}

#[derive(serde::Deserialize)]
struct ServiceKey {
    client_email: String,
    private_key: SecretString,
}

/// The SQL that turns a STRING parameter `mark` into a value of `data_type`; BigQuery compares and assigns no
/// other type to a STRING. Bytes are base64, as they show; ARRAY and STRUCT values have no text form to read.
pub fn from_text(mark: &str, data_type: &str) -> Result<String> {
    let base = data_type.split(['(', '<', ' ']).next().unwrap_or_default().to_ascii_uppercase();
    Ok(match base.as_str() {
        "STRING" | "" => mark.to_string(),
        "BYTES" => format!("from_base64({mark})"),
        "JSON" => format!("parse_json({mark})"),
        "GEOGRAPHY" => format!("st_geogfromtext({mark})"),
        "ARRAY" | "STRUCT" | "RECORD" | "RANGE" => {
            return Err(Error::Invalid("ARRAY and STRUCT values change in a SQL tab"));
        }
        _ => format!("cast({mark} as {data_type})"),
    })
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// A TIMESTAMP (microseconds since 1970) as BigQuery's console writes it.
fn timestamp(micros: &str) -> String {
    // The emulator ignores useInt64Timestamp and sends seconds with a fraction.
    let micros = micros.parse::<i64>().ok().or_else(|| micros.parse::<f64>().ok().map(seconds_to_micros));
    let Some(t) = micros.and_then(chrono::DateTime::from_timestamp_micros) else {
        return micros.unwrap_or_default().to_string();
    };
    if t.timestamp_subsec_micros() == 0 {
        t.format("%Y-%m-%d %H:%M:%S UTC").to_string()
    } else {
        t.format("%Y-%m-%d %H:%M:%S%.6f UTC").to_string()
    }
}

#[expect(clippy::cast_possible_truncation, reason = "a timestamp in microseconds fits an i64")]
fn seconds_to_micros(seconds: f64) -> i64 {
    (seconds * 1e6).round() as i64
}

/// A nested value as JSON: numbers and booleans as such, structs as objects, everything else as text.
fn json_value(field: &Value, v: &Value) -> Value {
    if v.is_null() {
        return Value::Null;
    }
    if field.get("mode").and_then(Value::as_str) == Some("REPEATED") {
        let one = {
            let mut f = field.clone();
            f["mode"] = json!("NULLABLE");
            f
        };
        let items = v.as_array().map(|a| a.iter().map(|i| json_value(&one, &i["v"])).collect()).unwrap_or_default();
        return Value::Array(items);
    }
    let kind = field.get("type").and_then(Value::as_str).unwrap_or_default();
    let text = v.as_str().unwrap_or_default();
    match kind {
        "RECORD" | "STRUCT" => {
            let fields = field.get("fields").and_then(Value::as_array).cloned().unwrap_or_default();
            let values = v.get("f").and_then(Value::as_array).cloned().unwrap_or_default();
            let object: Map<String, Value> = fields
                .iter()
                .zip(&values)
                .map(|(f, x)| (f["name"].as_str().unwrap_or_default().to_string(), json_value(f, &x["v"])))
                .collect();
            Value::Object(object)
        }
        "INTEGER" | "INT64" | "FLOAT" | "FLOAT64" => {
            text.parse::<serde_json::Number>().map_or_else(|_| json!(text), Value::Number)
        }
        "BOOLEAN" | "BOOL" => Value::Bool(text == "true"),
        "TIMESTAMP" => Value::String(timestamp(text)),
        _ => Value::String(text.to_string()),
    }
}

/// A cell as text: scalars as BigQuery sends them (timestamps written out), arrays and structs as JSON.
fn cell(field: &Value, v: &Value) -> Option<String> {
    let kind = field.get("type").and_then(Value::as_str).unwrap_or_default();
    let nested = field.get("mode").and_then(Value::as_str) == Some("REPEATED") || matches!(kind, "RECORD" | "STRUCT");
    match v {
        Value::Null => None,
        _ if nested => Some(json_value(field, v).to_string()),
        Value::String(text) if kind == "TIMESTAMP" => Some(timestamp(text)),
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

/// One answer's rows as cells, by the schema's fields.
fn rows(fields: &[Value], answer: &Value) -> Vec<Vec<Option<String>>> {
    let Some(rows) = answer.get("rows").and_then(Value::as_array) else { return vec![] };
    rows.iter()
        .map(|r| {
            let values = r.get("f").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
            fields.iter().zip(values).map(|(f, x)| cell(f, &x["v"])).collect()
        })
        .collect()
}

/// `sql` with its `@pN` parameters written in as literals, for a queued statement. Higher numbers first, so `@p1`
/// never eats the start of `@p12`.
fn written_in(sql: &str, params: &[Option<&str>]) -> String {
    let mut out = sql.to_string();
    for (n, p) in params.iter().enumerate().rev() {
        let literal = p.map_or_else(|| "NULL".to_string(), |v| Dialect::Bq.literal(v));
        out = out.replace(&format!("@p{}", n + 1), &literal);
    }
    out
}

/// The parsed request path's parts of a `bigquery://host[:port]/project[/dataset]` URL.
fn project_and_dataset(u: &url::Url) -> Result<(String, Option<String>)> {
    let mut parts = u.path().trim_matches('/').split('/').filter(|p| !p.is_empty());
    let project = parts.next().ok_or(Error::Invalid("enter the Google Cloud project ID"))?;
    let ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b));
    let dataset = parts.next().map(String::from);
    if !ok(project) || dataset.as_deref().is_some_and(|d| !ok(d)) {
        return Err(Error::Invalid("project and dataset IDs are letters, digits, dashes and underscores"));
    }
    Ok((project.to_string(), dataset))
}

impl Auth {
    fn new(key_json: &SecretString, tls: &Tls) -> Result<Auth> {
        let key: ServiceKey = serde_json::from_str(key_json.expose_secret()).map_err(|_| {
            Error::Invalid("the password must be a service account's JSON key, as Google Cloud gives it")
        })?;
        let pem = key.private_key.expose_secret();
        let body: String =
            pem.lines().filter(|l| !l.starts_with("-----")).collect::<String>().split_whitespace().collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(body)
            .map_err(|_| Error::Invalid("the key's private_key isn't a PEM private key"))?;
        let key_pair = ring::signature::RsaKeyPair::from_pkcs8(&der)
            .map_err(|_| Error::Invalid("the key's private_key isn't an RSA key in PKCS#8"))?;
        let checked = Tls { mode: TlsMode::VerifyFull, ..tls.clone() };
        let endpoint = Endpoint::new("Google sign-in", TOKEN_HOST, 443, https_connector(&checked)?, None);
        Ok(Auth { endpoint, email: key.client_email, key: key_pair, token: None })
    }

    /// The signed JWT that asks Google for an access token, issued at `now` (seconds) and good for an hour.
    fn assertion(&self, now: i64) -> Result<String> {
        let claims = json!({ "iss": self.email, "scope": SCOPE, "aud": format!("https://{TOKEN_HOST}/token"), "iat": now, "exp": now + 3600 });
        let unsigned =
            format!("{}.{}", b64url(br#"{"alg":"RS256","typ":"JWT"}"#), b64url(claims.to_string().as_bytes()));
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(
                &ring::signature::RSA_PKCS1_SHA256,
                &ring::rand::SystemRandom::new(),
                unsigned.as_bytes(),
                &mut signature,
            )
            .map_err(|_| Error::Invalid("the service account key couldn't sign the sign-in request"))?;
        Ok(format!("{unsigned}.{}", b64url(&signature)))
    }

    /// The `Authorization` header, from a token that's still good for a minute, or a new one.
    async fn header(&mut self) -> Result<header::HeaderValue> {
        if let Some((token, until)) = &self.token
            && Instant::now() + Duration::from_secs(60) < *until
        {
            return Ok(token.clone());
        }
        let assertion = self.assertion(chrono::Utc::now().timestamp())?;
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer")
            .append_pair("assertion", &assertion)
            .finish();
        let headers = [(header::CONTENT_TYPE, header::HeaderValue::from_static("application/x-www-form-urlencoded"))];
        let (status, bytes) = self.endpoint.post("/token", &headers, form.into_bytes(), 64 * 1024).await?;
        let answer: Value = serde_json::from_slice(&bytes).unwrap_or_default();
        let Some(token) = answer.get("access_token").and_then(Value::as_str).filter(|_| status == 200) else {
            let why = answer.get("error_description").or_else(|| answer.get("error")).and_then(Value::as_str);
            return Err(self
                .endpoint
                .error(format!("the service account was refused ({status}): {}", why.unwrap_or("no reason given"))));
        };
        let lasts = Duration::from_secs(answer.get("expires_in").and_then(Value::as_u64).unwrap_or(3600));
        let value = bearer(token)?;
        self.token = Some((value.clone(), Instant::now() + lasts));
        Ok(value)
    }
}

impl BigQuery {
    /// Opens `bigquery://host[:port]/project[/dataset]`. `key` is a service account's JSON key; none for the
    /// emulator. Without a dataset, the project's first one is current.
    pub async fn open(url: &str, key: Option<&SecretString>, tls: &Tls, tunnel_port: Option<u16>) -> Result<BigQuery> {
        let u = url::Url::parse(url)?;
        let host = u.host_str().filter(|h| !h.is_empty()).unwrap_or(API_HOST);
        let (project, dataset) = project_and_dataset(&u)?;
        let local = matches!(host, "127.0.0.1" | "localhost" | "[::1]");
        let connector =
            if local && tls.mode == TlsMode::Prefer && tunnel_port.is_none() { None } else { https_connector(tls)? };
        let port = u.port().unwrap_or(if connector.is_some() { 443 } else { 9050 });
        let endpoint = Endpoint::new("BigQuery", host, port, connector, tunnel_port);
        let auth = match key.filter(|k| !k.expose_secret().trim().is_empty()) {
            Some(k) => Some(Auth::new(k, tls)?),
            None if local => None,
            None => return Err(Error::Invalid("paste a service account's JSON key as the password")),
        };
        let mut c = BigQuery { endpoint, auth, project, dataset, queued: None, cancel: Arc::new(Notify::new()) };
        if c.dataset.is_none() {
            c.dataset = c.list_datasets().await?.into_iter().next();
        } else {
            c.ping().await?;
        }
        Ok(c)
    }

    /// Sends one request and returns its JSON answer, or BigQuery's error message.
    async fn request(&mut self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut headers = vec![];
        if let Some(auth) = &mut self.auth {
            headers.push((header::AUTHORIZATION, auth.header().await?));
        }
        let body = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
        let send = self.endpoint.send(method, path, &headers, body, MAX_ANSWER);
        let cancel = self.cancel.clone();
        let (status, bytes) = tokio::select! {
            r = send => r?,
            () = cancel.notified() => return Err(Error::Cancelled),
        };
        let answer: Value = serde_json::from_slice(&bytes).map_err(|_| {
            let text: String = String::from_utf8_lossy(&bytes).trim().chars().take(300).collect();
            self.endpoint.error(format!("{status}: {text}"))
        })?;
        if let Some(e) = answer.get("error") {
            let message = e.get("message").and_then(Value::as_str).unwrap_or("the request failed");
            return Err(self.endpoint.error(message.to_string()));
        }
        if !(200..300).contains(&status) {
            return Err(self.endpoint.error(format!("the request failed ({status})")));
        }
        Ok(answer)
    }

    async fn list_datasets(&mut self) -> Result<Vec<String>> {
        let (mut names, mut token) = (vec![], None::<String>);
        loop {
            let page = token.as_deref().map(|t| format!("&pageToken={}", urlencode(t))).unwrap_or_default();
            let path = format!("/bigquery/v2/projects/{}/datasets?maxResults=1000{page}", self.project);
            let answer = self.request(Method::GET, &path, None).await?;
            let list = answer.get("datasets").and_then(Value::as_array).cloned().unwrap_or_default();
            names.extend(list.iter().filter_map(|d| d["datasetReference"]["datasetId"].as_str().map(String::from)));
            token = answer.get("nextPageToken").and_then(Value::as_str).map(String::from);
            if token.is_none() {
                names.sort();
                return Ok(names);
            }
        }
    }

    /// Runs one statement and hands its rows to `on_row` (columns first); returns the rows a DML statement changed.
    async fn one(
        &mut self,
        sql: &str,
        params: &[Option<&str>],
        on_columns: &mut (dyn FnMut(Vec<String>) -> Result<()> + Send),
        on_row: &mut (dyn FnMut(Vec<Option<String>>) -> Result<bool> + Send),
    ) -> Result<Option<u64>> {
        let words: Vec<String> = sql.split_whitespace().take(3).map(str::to_ascii_lowercase).collect();
        match words.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["begin"] | ["begin", "transaction"] => {
                if self.queued.is_some() {
                    return Err(Error::Invalid("a transaction is already open"));
                }
                self.queued = Some(vec![]);
                return Ok(None);
            }
            ["commit"] | ["commit", "transaction"] => {
                let queued = self.queued.take().unwrap_or_default();
                if queued.is_empty() {
                    return Ok(None);
                }
                let script = format!("BEGIN TRANSACTION;\n{};\nCOMMIT TRANSACTION;", queued.join(";\n"));
                return self.job(&script, &[], on_columns, on_row).await;
            }
            ["rollback"] | ["rollback", "transaction"] => {
                self.queued = None;
                return Ok(None);
            }
            // Safe mode's bounds, and anything else without a statement.
            _ if super::only_comments(sql) => return Ok(None),
            _ => {}
        }
        if let Some(queue) = &mut self.queued
            && !matches!(first_word(sql).as_str(), "select" | "with")
        {
            queue.push(written_in(sql, params));
            return Ok(Some(0));
        }
        self.job(sql, params, on_columns, on_row).await
    }

    /// Runs `sql` as a query job, waiting for it and reading every page while `on_row` wants more.
    async fn job(
        &mut self,
        sql: &str,
        params: &[Option<&str>],
        on_columns: &mut (dyn FnMut(Vec<String>) -> Result<()> + Send),
        on_row: &mut (dyn FnMut(Vec<Option<String>>) -> Result<bool> + Send),
    ) -> Result<Option<u64>> {
        let mut body = json!({
            "query": sql, "useLegacySql": false, "maxResults": PAGE, "timeoutMs": WAIT_MS,
            "formatOptions": { "useInt64Timestamp": true },
        });
        if let Some(dataset) = &self.dataset {
            body["defaultDataset"] = json!({ "projectId": self.project, "datasetId": dataset });
        }
        if !params.is_empty() {
            body["parameterMode"] = json!("NAMED");
            body["queryParameters"] = params
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let value = p.map_or_else(|| json!({}), |v| json!({ "value": v }));
                    json!({ "name": format!("p{}", i + 1), "parameterType": { "type": "STRING" }, "parameterValue": value })
                })
                .collect();
        }
        let path = format!("/bigquery/v2/projects/{}/queries", self.project);
        let mut answer = self.request(Method::POST, &path, Some(&body)).await?;
        let job = answer.get("jobReference").cloned().unwrap_or_default();
        let project = self.project.clone();
        let results = |token: Option<&str>| {
            let location = job["location"].as_str().map(|l| format!("&location={}", urlencode(l))).unwrap_or_default();
            let page = token.map(|t| format!("&pageToken={}", urlencode(t))).unwrap_or_default();
            format!(
                "/bigquery/v2/projects/{project}/queries/{}?maxResults={PAGE}&timeoutMs={WAIT_MS}&formatOptions.useInt64Timestamp=true{location}{page}",
                urlencode(job["jobId"].as_str().unwrap_or_default())
            )
        };
        while answer.get("jobComplete") == Some(&Value::Bool(false)) {
            answer = match self.request(Method::GET, &results(None), None).await {
                Err(Error::Cancelled) => return Err(self.cancel_job(&job).await),
                r => r?,
            };
        }
        let affected = answer.get("numDmlAffectedRows").and_then(Value::as_str).and_then(|n| n.parse().ok());
        let fields = answer["schema"]["fields"].as_array().cloned().unwrap_or_default();
        if fields.is_empty() {
            return Ok(Some(affected.unwrap_or(0)));
        }
        on_columns(fields.iter().map(|f| f["name"].as_str().unwrap_or_default().to_string()).collect())?;
        loop {
            for row in rows(&fields, &answer) {
                if !on_row(row)? {
                    return Ok(None);
                }
            }
            let Some(token) = answer.get("pageToken").and_then(Value::as_str).map(String::from) else {
                return Ok(None);
            };
            answer = self.request(Method::GET, &results(Some(&token)), None).await?;
        }
    }

    /// Asks BigQuery to stop a job the app stopped waiting for; returns the error to report.
    async fn cancel_job(&mut self, job: &Value) -> Error {
        if let Some(id) = job["jobId"].as_str() {
            let location = job["location"].as_str().map(|l| format!("?location={}", urlencode(l))).unwrap_or_default();
            let path = format!("/bigquery/v2/projects/{}/jobs/{}/cancel{location}", self.project, urlencode(id));
            // Best effort: the query was stopped here either way.
            let _ = self.request(Method::POST, &path, Some(&json!({}))).await;
        }
        Error::Cancelled
    }
}

/// A schema field's type as `GoogleSQL` writes it (`INT64`, `STRING(20)`, `ARRAY<STRUCT<…>>`); the API still uses
/// the legacy names.
fn sql_type(field: &Value) -> String {
    let kind = field["type"].as_str().unwrap_or("STRING");
    let number = |key: &str| field.get(key).and_then(Value::as_str).filter(|n| !n.is_empty());
    let base = match kind {
        "INTEGER" => "INT64".to_string(),
        "FLOAT" => "FLOAT64".to_string(),
        "BOOLEAN" => "BOOL".to_string(),
        "RECORD" | "STRUCT" => {
            let fields = field["fields"].as_array().map(Vec::as_slice).unwrap_or_default();
            let inner: Vec<String> = fields
                .iter()
                .map(|f| format!("{} {}", Dialect::Bq.ident(f["name"].as_str().unwrap_or_default()), sql_type(f)))
                .collect();
            format!("STRUCT<{}>", inner.join(", "))
        }
        "STRING" | "BYTES" if number("maxLength").is_some() => {
            format!("{kind}({})", number("maxLength").unwrap_or_default())
        }
        "NUMERIC" | "BIGNUMERIC" if number("precision").is_some() => match number("scale") {
            Some(scale) => format!("{kind}({}, {scale})", number("precision").unwrap_or_default()),
            None => format!("{kind}({})", number("precision").unwrap_or_default()),
        },
        other => other.to_string(),
    };
    if field["mode"].as_str() == Some("REPEATED") { format!("ARRAY<{base}>") } else { base }
}

/// A table's primary key columns, from its `tableConstraints` (BigQuery keeps them unenforced).
fn primary_key(table: &Value) -> Vec<String> {
    table["tableConstraints"]["primaryKey"]["columns"]
        .as_array()
        .map(|c| c.iter().filter_map(|c| c.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

fn fields(table: &Value) -> &[Value] {
    table["schema"]["fields"].as_array().map(Vec::as_slice).unwrap_or_default()
}

/// The columns of a table resource (`tables.get`).
pub fn columns(table: &Value) -> Vec<crate::db::Column> {
    let key = primary_key(table);
    fields(table)
        .iter()
        .map(|f| {
            let name = f["name"].as_str().unwrap_or_default().to_string();
            crate::db::Column {
                primary: key.contains(&name),
                data_type: sql_type(f),
                nullable: f["mode"].as_str() != Some("REQUIRED"),
                default: f["defaultValueExpression"].as_str().map(String::from),
                name,
            }
        })
        .collect()
}

/// A table resource as the structure view shows it: columns with descriptions, the primary key and foreign keys
/// (both unenforced), and the table's description. BigQuery has no indexes to speak of, no checks and no triggers.
pub fn structure(table: &Value) -> crate::structure::Structure {
    use crate::structure::{ColumnInfo, ForeignKeyInfo, IndexInfo, Structure};
    let key = primary_key(table);
    let columns = fields(table)
        .iter()
        .map(|f| {
            let name = f["name"].as_str().unwrap_or_default().to_string();
            ColumnInfo {
                primary: key.contains(&name),
                data_type: sql_type(f),
                nullable: f["mode"].as_str() != Some("REQUIRED"),
                default: f["defaultValueExpression"].as_str().map(String::from),
                comment: f["description"].as_str().filter(|d| !d.is_empty()).map(String::from),
                name,
                ..ColumnInfo::default()
            }
        })
        .collect();
    let indexes = if key.is_empty() {
        vec![]
    } else {
        vec![IndexInfo {
            name: "PRIMARY KEY".into(),
            columns: key,
            unique: true,
            primary: true,
            method: None,
            constraint: true,
        }]
    };
    let foreign_keys = table["tableConstraints"]["foreignKeys"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|k| {
            let pairs = k["columnReferences"].as_array().map(Vec::as_slice).unwrap_or_default();
            let column = |key: &str| pairs.iter().filter_map(|p| p[key].as_str().map(String::from)).collect();
            ForeignKeyInfo {
                name: k["name"].as_str().unwrap_or_default().to_string(),
                columns: column("referencingColumn"),
                ref_table: k["referencedTable"]["tableId"].as_str().unwrap_or_default().to_string(),
                ref_columns: column("referencedColumn"),
                on_update: "NO ACTION".into(),
                on_delete: "NO ACTION".into(),
            }
        })
        .collect();
    let comment = table["description"].as_str().filter(|d| !d.is_empty()).map(String::from);
    Structure { columns, indexes, foreign_keys, checks: vec![], triggers: vec![], comment, options: None }
}

/// A CREATE statement written from a table resource, for when `INFORMATION_SCHEMA`'s `ddl` isn't there (the
/// emulator): a view's query, or a table's columns, key and descriptions.
pub fn create_sql(name: &str, table: &Value) -> String {
    let d = Dialect::Bq;
    let t = d.table_ident(name);
    if let Some(query) = table["view"]["query"].as_str() {
        return format!("CREATE VIEW {t} AS\n{};", query.trim().trim_end_matches(';'));
    }
    let described = |text: Option<&str>| {
        text.filter(|x| !x.is_empty()).map(|x| format!(" OPTIONS(description = {})", d.literal(x))).unwrap_or_default()
    };
    let mut lines: Vec<String> = fields(table)
        .iter()
        .map(|f| {
            let mut line = format!("  {} {}", d.ident(f["name"].as_str().unwrap_or_default()), sql_type(f));
            if let Some(default) = f["defaultValueExpression"].as_str() {
                line += " DEFAULT ";
                line += default;
            }
            if f["mode"].as_str() == Some("REQUIRED") {
                line += " NOT NULL";
            }
            line + &described(f["description"].as_str())
        })
        .collect();
    let key = primary_key(table);
    if !key.is_empty() {
        let key: Vec<String> = key.iter().map(|k| d.ident(k)).collect();
        lines.push(format!("  PRIMARY KEY ({}) NOT ENFORCED", key.join(", ")));
    }
    format!("CREATE TABLE {t} (\n{}\n){};", lines.join(",\n"), described(table["description"].as_str()))
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

#[async_trait]
impl Driver for BigQuery {
    fn dialect(&self) -> Dialect {
        Dialect::Bq
    }

    async fn batch(&mut self, sql: &str) -> Result<()> {
        self.run(sql, &mut Results::new(Some(0))).await
    }

    async fn run(&mut self, sql: &str, out: &mut Results) -> Result<()> {
        // A script with variables (DECLARE) only works sent whole; otherwise each statement shows its own result.
        let statements = split_statements(sql);
        let whole = statements.iter().any(|s| first_word(s) == "declare");
        for statement in if whole { vec![sql] } else { statements } {
            let shared = std::sync::Mutex::new((&mut *out, false));
            let lock = || shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let changed = self
                .one(
                    statement,
                    &[],
                    &mut |names| {
                        let mut g = lock();
                        g.1 = true;
                        g.0.begin(names);
                        Ok(())
                    },
                    &mut |row| {
                        let mut g = lock();
                        if g.0.keep() {
                            g.0.push(row);
                            Ok(true)
                        } else {
                            Ok(false)
                        }
                    },
                )
                .await?;
            let (out, began) = shared.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !began {
                out.begin(vec![]);
            }
            out.end(changed.unwrap_or(0));
        }
        Ok(())
    }

    async fn execute_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<u64> {
        Ok(self.one(sql, params, &mut |_| Ok(()), &mut |_| Ok(true)).await?.unwrap_or(0))
    }

    async fn fetch_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Vec<Vec<Option<String>>>> {
        let mut rows = vec![];
        self.one(sql, params, &mut |_| Ok(()), &mut |r| {
            rows.push(r);
            Ok(true)
        })
        .await?;
        Ok(rows)
    }

    async fn stream(&mut self, sql: &str, params: &[Option<&str>], sink: &mut dyn RowSink) -> Result<()> {
        let sink = std::sync::Mutex::new(sink);
        let lock = || sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.one(sql, params, &mut |names| lock().columns(names), &mut |row| lock().row(row).map(|()| true))
            .await
            .map(|_| ())
    }

    async fn ping(&mut self) -> Result<()> {
        self.job("select 1", &[], &mut |_| Ok(()), &mut |_| Ok(true)).await.map(|_| ())
    }

    fn stopper(&self) -> Stopper {
        Stopper::Notify(self.cancel.clone())
    }

    fn read_only_bounds(&self) -> (&'static str, &'static str) {
        (READ_ONLY_ON, READ_ONLY_OFF)
    }

    async fn use_database(&mut self, db: &str) -> Result<()> {
        if !self.list_datasets().await?.iter().any(|d| d == db) {
            return Err(Error::Invalid("no such dataset in this project"));
        }
        self.dataset = Some(db.to_string());
        Ok(())
    }

    fn database(&self) -> Option<String> {
        self.dataset.clone()
    }

    /// The default dataset's tables, views and the rest (`tables.list`): each with its type (TABLE, VIEW,
    /// `MATERIALIZED_VIEW`…), sorted by name.
    async fn list_tables(&mut self) -> Result<Vec<(String, String)>> {
        let Some(dataset) = self.dataset.clone() else { return Ok(vec![]) };
        let (mut out, mut token) = (vec![], None::<String>);
        loop {
            let page = token.as_deref().map(|t| format!("&pageToken={}", urlencode(t))).unwrap_or_default();
            let path = format!(
                "/bigquery/v2/projects/{}/datasets/{}/tables?maxResults=1000{page}",
                self.project,
                urlencode(&dataset)
            );
            let answer = self.request(Method::GET, &path, None).await?;
            for t in answer.get("tables").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default() {
                if let Some(name) = t["tableReference"]["tableId"].as_str() {
                    out.push((name.to_string(), t["type"].as_str().unwrap_or("TABLE").to_string()));
                }
            }
            token = answer.get("nextPageToken").and_then(Value::as_str).map(String::from);
            if token.is_none() {
                out.sort();
                return Ok(out);
            }
        }
    }

    /// The table resource (`tables.get`): its schema, key, description, and a view's query. `table` may be
    /// `dataset.table`.
    async fn describe(&mut self, table: &str) -> Result<Value> {
        let (dataset, name) = match (table.split_once('.'), &self.dataset) {
            (Some((d, t)), _) => (d.to_string(), t.to_string()),
            (None, Some(d)) => (d.clone(), table.to_string()),
            (None, None) => return Err(Error::Invalid("choose a dataset first")),
        };
        let path = format!(
            "/bigquery/v2/projects/{}/datasets/{}/tables/{}",
            self.project,
            urlencode(&dataset),
            urlencode(&name)
        );
        self.request(Method::GET, &path, None).await
    }

    async fn databases(&mut self) -> Result<Vec<String>> {
        self.list_datasets().await
    }

    // BigQuery takes up to 10,000 query parameters.
    fn max_params(&self) -> usize {
        10_000
    }

    fn param(&self, n: usize) -> String {
        format!("@p{n}")
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{Auth, cell, from_text, timestamp, written_in};
    use crate::tls::Tls;
    use base64::Engine as _;
    use secrecy::SecretString;
    use serde_json::json;

    #[test]
    fn assertion_should_be_a_jwt_signed_by_the_service_accounts_key() {
        // A 2048-bit key made for this test only, its PKCS#8 body in base64.
        let body = include_str!("testdata/rs256-test-key.b64").trim();
        let key = json!({ "client_email": "bot@tm.iam.gserviceaccount.com", "private_key": format!("-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----\n") });
        let auth = Auth::new(&SecretString::from(key.to_string()), &Tls::default()).unwrap();
        let jwt = auth.assertion(1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        let decode = |p: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).unwrap();
        let claims: serde_json::Value = serde_json::from_slice(&decode(parts[1])).unwrap();
        assert_eq!(
            (claims["iss"].as_str(), claims["exp"].as_i64()),
            (Some("bot@tm.iam.gserviceaccount.com"), Some(1_700_003_600))
        );
        let public = ring::signature::UnparsedPublicKey::new(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            auth.key.public().as_ref(),
        );
        assert!(public.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &decode(parts[2])).is_ok());
    }

    #[test]
    fn auth_should_refuse_a_key_that_isnt_a_service_account_json() {
        assert!(Auth::new(&SecretString::from("not json".to_string()), &Tls::default()).is_err());
    }

    #[test]
    fn written_in_should_not_let_p1_eat_the_start_of_p12() {
        let params: Vec<Option<&str>> = (1..=12)
            .map(|n| {
                if n == 1 {
                    Some("it's")
                } else if n == 12 {
                    None
                } else {
                    Some("x")
                }
            })
            .collect();
        let sql = written_in("update t set a = @p1, b = @p12", &params);
        assert_eq!(sql, "update t set a = 'it\\'s', b = NULL");
    }

    #[test]
    fn from_text_should_convert_by_column_type_and_refuse_arrays() {
        assert_eq!(from_text("@p1", "INT64").unwrap(), "cast(@p1 as INT64)");
        assert_eq!(from_text("@p1", "STRING(10)").unwrap(), "@p1");
        assert_eq!(from_text("@p1", "BYTES").unwrap(), "from_base64(@p1)");
        assert!(from_text("@p1", "ARRAY<STRING>").is_err());
    }

    #[test]
    fn timestamp_should_read_microseconds_and_the_emulators_seconds() {
        assert_eq!(timestamp("1614834367123456"), "2021-03-04 05:06:07.123456 UTC");
        assert_eq!(timestamp("1614834367.123456"), "2021-03-04 05:06:07.123456 UTC");
        assert_eq!(timestamp("1614834367000000"), "2021-03-04 05:06:07 UTC");
    }

    #[test]
    fn cell_should_write_arrays_and_structs_as_json() {
        let tags = json!({ "name": "tags", "type": "STRING", "mode": "REPEATED" });
        assert_eq!(cell(&tags, &json!([{ "v": "a" }, { "v": "b" }])).as_deref(), Some(r#"["a","b"]"#));
        let owner = json!({ "name": "owner", "type": "RECORD", "fields": [{ "name": "n", "type": "INTEGER" }, { "name": "c", "type": "STRING" }] });
        assert_eq!(
            cell(&owner, &json!({ "f": [{ "v": "3" }, { "v": "oslo" }] })).as_deref(),
            Some(r#"{"n":3,"c":"oslo"}"#)
        );
        assert_eq!(cell(&json!({ "type": "FLOAT" }), &json!("1.50")).as_deref(), Some("1.50"));
        assert_eq!(cell(&json!({ "type": "STRING" }), &json!(null)), None);
    }

    #[test]
    #[ignore = "needs the BigQuery emulator on 9050, project tm, dataset app (scripts/test-env.sh up); replaces table t"]
    fn local_bigquery_should_browse_and_edit_with_values_converted_to_their_columns_types() {
        use crate::db::{Conn, Engine, Options};
        tauri::async_runtime::block_on(async {
            let url = "bigquery://127.0.0.1:9050/tm";
            let mut c =
                Conn::open(Engine::Bigquery, url, None, &Tls::default(), &Options::default(), None).await.unwrap();
            assert_eq!(c.current_database().await.unwrap().as_deref(), Some("app"));
            c.query_results(
                "drop table if exists t; create table t (id int64, name string, born date, tags array<string>); \
                 insert into t values (1, 'rex', date '2020-01-02', ['a', 'b']), (2, 'tom', null, [])",
                None,
            )
            .await
            .unwrap();
            let browse = json!({ "table": "t", "limit": 10, "offset": 0, "filters": [{ "column": "id", "op": "ge", "value": "2" }] });
            let page = c.browse(&serde_json::from_value(browse).unwrap()).await.unwrap();
            assert_eq!(page.rows, [[Some("2".into()), Some("tom".into()), None, Some("[]".into())]]);
            let changes = json!({ "deletes": [], "inserts": [], "log_originals": false,
                "updates": [{ "key": [["id", "2"]], "set": [["born", "2021-03-04"], ["name", "it's"]] }] });
            c.save_rows("t", &serde_json::from_value(changes).unwrap()).await.unwrap();
            let rows = c
                .fetch_with("select name, born, tags from t where id = 1 or born is not null order by id", &[])
                .await
                .unwrap();
            assert_eq!(rows[1], [Some("it's".into()), Some("2021-03-04".into()), Some("[]".into())]);
            assert_eq!(rows[0][2].as_deref(), Some(r#"["a","b"]"#));
        });
    }
}

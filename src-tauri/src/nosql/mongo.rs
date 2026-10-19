//! `MongoDB` through its official driver. Not SQL, so like Redis it has its own sessions and commands: the sidebar
//! lists collections, documents come back as relaxed Extended JSON for the grid, and scripts in mongosh syntax
//! ([`shell`]) run as the operations they record, each through safe mode.

pub mod shell;

use crate::connect::store::{self, Saved};
use crate::connect::tls::TlsMode;
use crate::error::{Error, Result};
use crate::safety::{self, Unlock};
use crate::sql::db::{CONNECT_TIMEOUT, Engine};
use futures_util::TryStreamExt;
use mongodb::bson::{Bson, Document, doc};
use mongodb::options::{ClientOptions, Credential, IndexOptions, ServerAddress, Tls, TlsOptions};
use mongodb::{Client, IndexModel};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use shell::{Operation, to_bson};
use tauri::{AppHandle, State};

/// The most documents one operation shows; the grid pages with skip and limit for more.
const MAX_DOCS: usize = 50_000;

/// Commands that only read; any other `runCommand` asks first in safe mode.
const READ_COMMANDS: &[&str] = &[
    "buildinfo",
    "collstats",
    "connectionstatus",
    "count",
    "currentop",
    "datasize",
    "dbhash",
    "dbstats",
    "distinct",
    "explain",
    "features",
    "find",
    "getcmdlineopts",
    "getlog",
    "getparameter",
    "hello",
    "hostinfo",
    "ismaster",
    "listcollections",
    "listdatabases",
    "listindexes",
    "ping",
    "replsetgetstatus",
    "rolesinfo",
    "serverstatus",
    "top",
    "usersinfo",
    "whatsmyuri",
];

pub(crate) struct Session {
    client: Client,
    /// The database `db` means.
    db: String,
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

/// The driver's options for a saved connection: its URL (`mongodb://` or `mongodb+srv://`, which looks up the
/// servers in DNS), the password from the keychain, the TLS settings, and an SSH tunnel's local port.
async fn client_options(
    saved: &Saved,
    password: Option<&SecretString>,
    tunnel_port: Option<u16>,
) -> Result<ClientOptions> {
    let mut o = ClientOptions::parse(&saved.url).await?;
    if let Some(port) = tunnel_port {
        // One server, through the tunnel: a replica set's other members aren't reachable from here.
        o.hosts = vec![ServerAddress::Tcp { host: "127.0.0.1".into(), port: Some(port) }];
        o.direct_connection = Some(true);
    }
    if let Some(password) = password.filter(|p| !p.expose_secret().is_empty()) {
        let credential = o.credential.get_or_insert_with(Credential::default);
        if credential.username.is_none() {
            return Err(Error::Invalid("a password needs a user name"));
        }
        credential.password = Some(password.expose_secret().to_string());
    }
    let tls = &saved.tls;
    let files = |path: &str| (!path.is_empty()).then(|| crate::connect::ssh::expand_home(path)).transpose();
    if tls.cert != tls.key {
        return Err(Error::Invalid(
            "MongoDB takes the client certificate and its key in one PEM file: choose that file for both",
        ));
    }
    match tls.mode {
        TlsMode::Disable => o.tls = Some(Tls::Disabled),
        // Prefer leaves TLS to the URL (on for mongodb+srv://, or with ?tls=true).
        TlsMode::Prefer if tls.ca.is_empty() && tls.cert.is_empty() => {}
        mode => {
            o.tls = Some(Tls::Enabled(
                TlsOptions::builder()
                    // With rustls the host name can't be skipped on its own, so Verify CA checks it too.
                    .allow_invalid_certificates(matches!(mode, TlsMode::Prefer | TlsMode::Require) && tls.ca.is_empty())
                    .ca_file_path(files(&tls.ca)?)
                    .cert_key_file_path(files(&tls.cert)?)
                    .build(),
            ));
        }
    }
    o.server_selection_timeout = Some(CONNECT_TIMEOUT);
    o.connect_timeout = Some(CONNECT_TIMEOUT);
    o.app_name = Some("TableMinus".into());
    Ok(o)
}

async fn open_session(
    saved: Saved,
    password: Option<SecretString>,
    ssh_secret: Option<SecretString>,
    db: Option<String>,
) -> Result<Session> {
    if !saved.options.startup_script.trim().is_empty() {
        crate::sql::db::run_startup_script(&saved.options.startup_script).await?;
    }
    if saved.ssh.is_some() && saved.url.starts_with("mongodb+srv:") {
        return Err(Error::Invalid("a mongodb+srv:// connection can't go over SSH; use the server's own address"));
    }
    let tunnel = match &saved.ssh {
        Some(ssh) => Some(crate::connect::ssh::open_for(Engine::Mongodb, &saved.url, ssh, ssh_secret.as_ref()).await?),
        None => None,
    };
    let options = client_options(&saved, password.as_ref(), tunnel.as_ref().map(|t| t.port)).await?;
    let db = db.or_else(|| options.default_database.clone()).unwrap_or_else(|| "test".into());
    let client = Client::with_options(options)?;
    // The driver connects lazily: ask now so a wrong address or password is reported here.
    tokio::time::timeout(CONNECT_TIMEOUT * 2, client.database("admin").run_command(doc! { "ping": 1 }))
        .await
        .map_err(|_| Error::ConnectTimeout(CONNECT_TIMEOUT.as_secs()))??;
    Ok(Session { client, db, saved, password, _tunnel: tunnel })
}

/// What the workspace header shows, in the shape `connect` gives for SQL: the collections are its tables.
#[derive(Debug, Serialize)]
pub struct Info {
    server: String,
    database: Option<String>,
    can_switch: bool,
    tables: Vec<String>,
}

async fn info(s: &Session) -> Result<Info> {
    let build = s.client.database("admin").run_command(doc! { "buildInfo": 1 }).await?;
    let version = build.get_str("version").unwrap_or_default();
    let mut tables = s.client.database(&s.db).list_collection_names().await?;
    tables.sort();
    Ok(Info { server: format!("MongoDB {version}"), database: Some(s.db.clone()), can_switch: true, tables })
}

/// Runs `f` within the query timeout (Settings › General).
async fn timed<T>(prefs: &crate::Prefs, f: impl Future<Output = Result<T>>) -> Result<T> {
    match crate::query_limit(prefs) {
        Some(d) => tokio::time::timeout(d, f).await.map_err(|_| Error::QueryTimeout(d.as_secs()))?,
        None => f.await,
    }
}

/// Test in the connection form: opens a session, says what answered, and closes it.
pub async fn test(saved: Saved, password: Option<SecretString>, ssh_secret: Option<SecretString>) -> Result<String> {
    let s = open_session(saved, password, ssh_secret, None).await?;
    let i = info(&s).await?;
    Ok(format!("Connected · {} · {} collections in {}", i.server, i.tables.len(), s.db))
}

#[tauri::command]
pub async fn mongo_connect(
    app: AppHandle,
    open: State<'_, crate::Open>,
    sessions: State<'_, Sessions>,
    name: String,
    database: Option<String>,
) -> Result<Info> {
    open.unlocked()?;
    if let Some(existing) = sessions.existing(&name).await {
        let mut s = existing.lock().await;
        if let Some(db) = database {
            s.db = db; // one client reaches every database
        }
        return info(&s).await;
    }
    let saved_name = name.split_once('\u{1f}').map_or(name.as_str(), |(n, _)| n);
    let saved = store::load(&crate::dir(&app)?)?
        .into_iter()
        .find(|c| c.name == saved_name)
        .ok_or_else(|| Error::NoSuchConnection(saved_name.to_string()))?;
    if saved.kind != Engine::Mongodb {
        return Err(Error::Invalid("not a MongoDB connection"));
    }
    let password = store::password_for(&saved)?;
    let ssh_secret = if saved.ssh.is_some() { store::stored(store::Secret::Ssh, &saved.name)? } else { None };
    let badge = saved.needs_credentials.then(|| saved.name.clone());
    let s = open_session(saved, password, ssh_secret, database).await?;
    if let Some(badge) = badge {
        // The badge is only a hint: failing to take it off must not fail a connect that worked.
        let _ = store::clear_needs_credentials(&crate::dir(&app)?, &badge);
    }
    let r = info(&s).await;
    sessions.insert_new(name, s).await;
    r
}

#[tauri::command]
pub async fn mongo_databases(
    open: State<'_, crate::Open>,
    sessions: State<'_, Sessions>,
    name: String,
) -> Result<Vec<String>> {
    open.unlocked()?;
    let s = sessions.get(&name).await?;
    Ok(s.client.list_database_names().await?)
}

/// What one operation gave, for its own result tab.
#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// Documents as relaxed Extended JSON; `more` when there were more than were read.
    Docs {
        title: String,
        docs: Vec<Value>,
        more: bool,
    },
    Value {
        title: String,
        value: Value,
    },
    Done {
        title: String,
        text: String,
    },
}

fn json(b: Bson) -> Value {
    b.into_relaxed_extjson()
}

fn document(v: Option<Value>, what: &str) -> Result<Document> {
    match v.map(to_bson).transpose()? {
        None => Ok(Document::new()),
        Some(Bson::Document(d)) => Ok(d),
        Some(_) => Err(Error::Mongo(format!("the {what} must be a document"))),
    }
}

/// A whole number of documents from a JavaScript number.
fn count(n: Option<f64>) -> Option<u64> {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "checked to be a whole number ≥ 0")]
    n.filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0).map(|n| n as u64)
}

/// Whether `op` only reads.
pub fn reads(op: &Operation) -> bool {
    match op.op.as_str() {
        "find"
        | "countDocuments"
        | "estimatedDocumentCount"
        | "distinct"
        | "getIndexes"
        | "listCollections"
        | "listDatabases"
        | "use"
        | "value" => true,
        "aggregate" => !op
            .pipeline
            .iter()
            .flatten()
            .any(|stage| stage.as_object().is_some_and(|s| s.contains_key("$out") || s.contains_key("$merge"))),
        "runCommand" => op
            .command
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|c| c.keys().next())
            .is_some_and(|name| READ_COMMANDS.contains(&name.to_ascii_lowercase().as_str())),
        _ => false,
    }
}

/// Reads up to `max` documents from a cursor; `more` when there were others.
async fn take(mut cursor: mongodb::Cursor<Document>, max: usize) -> Result<(Vec<Value>, bool)> {
    let mut docs = Vec::new();
    while let Some(d) = cursor.try_next().await? {
        if docs.len() == max {
            return Ok((docs, true));
        }
        docs.push(json(Bson::Document(d)));
    }
    Ok((docs, false))
}

/// Runs one operation on `client`, `db` being the database `db` means (`use` changes it).
#[expect(clippy::too_many_lines, reason = "one arm per operation the shell knows, each short")]
async fn execute(client: &Client, db: &mut String, op: Operation, max: usize) -> Result<Outcome> {
    let database = client.database(op.db.as_deref().unwrap_or(db));
    let name = op.collection.clone().unwrap_or_default();
    let title = if name.is_empty() { op.op.clone() } else { format!("{name}.{}", op.op) };
    let coll = database.collection::<Document>(&name);
    let filter = document(op.filter.clone(), "filter")?;
    let done = |text: String| Ok(Outcome::Done { title: title.clone(), text });
    let value = |value: Value| Ok(Outcome::Value { title: title.clone(), value });
    match op.op.as_str() {
        "find" => {
            if op.explain == Some(true) {
                let mut find = doc! { "find": &name, "filter": filter };
                if let Some(sort) = op.sort {
                    find.insert("sort", to_bson(sort)?);
                }
                let plan = database.run_command(doc! { "explain": find, "verbosity": "queryPlanner" }).await?;
                return value(json(Bson::Document(plan)));
            }
            let limit = count(op.limit).map_or(max, |l| usize::try_from(l).unwrap_or(max).min(max));
            let mut find = coll.find(filter).limit(i64::try_from(limit + 1).unwrap_or(i64::MAX));
            if let Some(sort) = op.sort {
                find = find.sort(document(Some(sort), "sort")?);
            }
            if let Some(skip) = count(op.skip) {
                find = find.skip(skip);
            }
            if let Some(p) = op.projection {
                find = find.projection(document(Some(p), "projection")?);
            }
            let (docs, more) = take(find.await?, limit).await?;
            Ok(Outcome::Docs { title, docs, more: more && count(op.limit).is_none_or(|l| l > limit as u64) })
        }
        "aggregate" => {
            let pipeline = op
                .pipeline
                .unwrap_or_default()
                .into_iter()
                .map(|s| document(Some(s), "pipeline stage"))
                .collect::<Result<Vec<_>>>()?;
            let (docs, more) = take(coll.aggregate(pipeline).await?, max).await?;
            Ok(Outcome::Docs { title, docs, more })
        }
        "countDocuments" => {
            let mut c = coll.count_documents(filter);
            if let Some(skip) = count(op.skip) {
                c = c.skip(skip);
            }
            if let Some(limit) = count(op.limit) {
                c = c.limit(limit);
            }
            value(Value::from(c.await?))
        }
        "estimatedDocumentCount" => value(Value::from(coll.estimated_document_count().await?)),
        "distinct" => {
            let field = op.field.ok_or(Error::Invalid("distinct needs a field name"))?;
            value(Value::Array(coll.distinct(field, filter).await?.into_iter().map(json).collect()))
        }
        "insertOne" => {
            let r = coll.insert_one(document(op.document, "document")?).await?;
            done(format!("Inserted 1 document, _id {}", json(r.inserted_id)))
        }
        "insertMany" => {
            let docs = op
                .documents
                .unwrap_or_default()
                .into_iter()
                .map(|d| document(Some(d), "document"))
                .collect::<Result<Vec<_>>>()?;
            if docs.is_empty() {
                return Err(Error::Invalid("insertMany needs at least one document"));
            }
            let r = coll.insert_many(docs).await?;
            done(format!("Inserted {} documents", r.inserted_ids.len()))
        }
        "updateOne" | "updateMany" | "replaceOne" => {
            let upsert = op.options.as_ref().and_then(|o| o.get("upsert")).and_then(Value::as_bool).unwrap_or(false);
            let r = match op.op.as_str() {
                "replaceOne" => {
                    coll.replace_one(filter, document(op.replacement, "replacement")?).upsert(upsert).await?
                }
                many => {
                    let update = match to_bson(op.update.ok_or(Error::Invalid("an update needs its changes"))?)? {
                        Bson::Document(d) => mongodb::options::UpdateModifications::Document(d),
                        Bson::Array(stages) => mongodb::options::UpdateModifications::Pipeline(
                            stages
                                .into_iter()
                                .map(|s| match s {
                                    Bson::Document(d) => Ok(d),
                                    _ => Err(Error::Mongo("an update pipeline's stages are documents".into())),
                                })
                                .collect::<Result<_>>()?,
                        ),
                        _ => return Err(Error::Mongo("an update is a document or a pipeline".into())),
                    };
                    if many == "updateMany" {
                        coll.update_many(filter, update).upsert(upsert).await?
                    } else {
                        coll.update_one(filter, update).upsert(upsert).await?
                    }
                }
            };
            let upserted = r.upserted_id.map(|id| format!(", upserted _id {}", json(id))).unwrap_or_default();
            done(format!("Matched {}, modified {}{upserted}", r.matched_count, r.modified_count))
        }
        "deleteOne" => done(format!("Deleted {}", coll.delete_one(filter).await?.deleted_count)),
        "deleteMany" => done(format!("Deleted {}", coll.delete_many(filter).await?.deleted_count)),
        "findOneAndUpdate" | "findOneAndReplace" | "findOneAndDelete" => {
            let options = op.options.as_ref();
            let after = options.and_then(|o| o.get("returnDocument")).and_then(Value::as_str) == Some("after")
                || options.and_then(|o| o.get("returnNewDocument")).and_then(Value::as_bool) == Some(true);
            let when =
                if after { mongodb::options::ReturnDocument::After } else { mongodb::options::ReturnDocument::Before };
            let upsert = options.and_then(|o| o.get("upsert")).and_then(Value::as_bool).unwrap_or(false);
            let sort = options.and_then(|o| o.get("sort")).cloned().map(|s| document(Some(s), "sort")).transpose()?;
            let found = match op.op.as_str() {
                "findOneAndDelete" => {
                    coll.find_one_and_delete(filter).optional(sort, mongodb::action::FindOneAndDelete::sort).await?
                }
                "findOneAndReplace" => {
                    coll.find_one_and_replace(filter, document(op.replacement, "replacement")?)
                        .return_document(when)
                        .upsert(upsert)
                        .optional(sort, mongodb::action::FindOneAndReplace::sort)
                        .await?
                }
                _ => {
                    coll.find_one_and_update(filter, document(op.update, "update")?)
                        .return_document(when)
                        .upsert(upsert)
                        .optional(sort, mongodb::action::FindOneAndUpdate::sort)
                        .await?
                }
            };
            let docs = found.map(|d| json(Bson::Document(d))).into_iter().collect();
            Ok(Outcome::Docs { title, docs, more: false })
        }
        "createIndex" => {
            let keys = document(op.keys, "index keys")?;
            let options: Option<IndexOptions> = op
                .options
                .map(|o| document(Some(o), "index options"))
                .transpose()?
                .map(mongodb::bson::from_document)
                .transpose()
                .map_err(|e| Error::Mongo(format!("index options: {e}")))?;
            let r = coll.create_index(IndexModel::builder().keys(keys).options(options).build()).await?;
            done(format!("Created index {}", r.index_name))
        }
        "dropIndex" => {
            let index = to_bson(op.index.ok_or(Error::Invalid("dropIndex needs the index's name or keys"))?)?;
            database.run_command(doc! { "dropIndexes": &name, "index": index }).await?;
            done("Dropped the index".into())
        }
        "getIndexes" => {
            let mut docs = Vec::new();
            let mut cursor = coll.list_indexes().await?;
            while let Some(index) = cursor.try_next().await? {
                docs.push(json(mongodb::bson::to_bson(&index).map_err(|e| Error::Mongo(e.to_string()))?));
            }
            Ok(Outcome::Docs { title, docs, more: false })
        }
        "drop" => {
            coll.drop().await?;
            done(format!("Dropped {name}"))
        }
        "renameCollection" => {
            let to = op.to.ok_or(Error::Invalid("renameCollection needs the new name"))?;
            let from = format!("{}.{name}", database.name());
            let target = format!("{}.{to}", database.name());
            client.database("admin").run_command(doc! { "renameCollection": from, "to": target }).await?;
            done(format!("Renamed {name} to {to}"))
        }
        "createCollection" => {
            let new = op.name.ok_or(Error::Invalid("createCollection needs a name"))?;
            let mut command = doc! { "create": &new };
            command.extend(document(op.options, "collection options")?);
            database.run_command(command).await?;
            done(format!("Created {new}"))
        }
        "dropDatabase" => {
            database.drop().await?;
            done(format!("Dropped database {}", database.name()))
        }
        "listCollections" => {
            let mut names = database.list_collection_names().await?;
            names.sort();
            Ok(Outcome::Docs {
                title,
                docs: names.into_iter().map(|n| serde_json::json!({ "name": n })).collect(),
                more: false,
            })
        }
        "listDatabases" => {
            let docs = client
                .list_databases()
                .await?
                .into_iter()
                .map(|d| serde_json::json!({ "name": d.name, "sizeOnDisk": d.size_on_disk, "empty": d.empty }))
                .collect();
            Ok(Outcome::Docs { title, docs, more: false })
        }
        "runCommand" => {
            let command = document(op.command, "command")?;
            if command.is_empty() {
                return Err(Error::Invalid("runCommand needs a command document"));
            }
            value(json(Bson::Document(database.run_command(command).await?)))
        }
        "use" => {
            db.clone_from(op.db.as_ref().ok_or(Error::Invalid("use needs a database name"))?);
            done(format!("switched to db {db}"))
        }
        "value" => value(op.value.unwrap_or(Value::Null)),
        other => Err(Error::Mongo(format!("{other} isn't something TableMinus can run"))),
    }
}

/// Lets a builder take an optional setting in a chain.
trait Optional: Sized {
    fn optional<T>(self, value: Option<T>, set: impl FnOnce(Self, T) -> Self) -> Self {
        match value {
            Some(v) => set(self, v),
            None => self,
        }
    }
}
impl<T> Optional for T {}

/// Runs a script (mongosh syntax): every operation it records, in order, each with its own result. Safe mode asks
/// before a script that does more than read. `max_docs` caps each operation's documents.
#[tauri::command]
#[expect(clippy::too_many_arguments, reason = "a Tauri command's arguments are its states and the request's fields")]
pub async fn mongo_run(
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    sessions: State<'_, Sessions>,
    name: String,
    script: String,
    max_docs: Option<usize>,
    confirmed: bool,
    unlock: Option<Unlock>,
) -> Result<Vec<Outcome>> {
    open.unlocked()?;
    let mut s = sessions.get(&name).await?;
    let ops = shell::operations(&script, &s.db)?;
    if ops.is_empty() {
        return Err(Error::Invalid("the script asks for nothing: try db.collection.find()"));
    }
    let read_only = ops.iter().all(reads);
    safety::check(s.saved.safety, read_only, confirmed, unlock.as_ref(), s.password.as_ref()).await?;
    crate::app::console::record(&script);
    let max = max_docs.unwrap_or(1000).clamp(1, MAX_DOCS);
    let client = s.client.clone();
    let mut db = s.db.clone();
    let outcomes = timed(&prefs, async {
        let mut out = Vec::with_capacity(ops.len());
        for op in ops {
            out.push(execute(&client, &mut db, op, max).await?);
        }
        Ok(out)
    })
    .await;
    s.db = db; // `use` lasts, even when a later operation failed
    outcomes
}

/// A change made from the grid or the document editor. `id`, `value` and `document` are shell expressions
/// (`ObjectId("…")`, `42`, `"text"`, `{ a: 1 }`), read as BSON by [`shell::values`].
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    Set { collection: String, id: String, field: String, value: String },
    Unset { collection: String, id: String, field: String },
    Replace { collection: String, id: String, document: String },
    Insert { collection: String, document: String },
    Delete { collection: String, ids: Vec<String> },
}

fn field_ok(field: &str) -> Result<()> {
    if field.is_empty() || field.starts_with('$') || field == "_id" || field.contains('\0') {
        return Err(Error::Invalid(
            "that field can't be changed here: _id never changes, and a name can't start with $",
        ));
    }
    Ok(())
}

fn one(values: Vec<Bson>) -> Result<Bson> {
    values.into_iter().next().ok_or(Error::Mongo("no value".into()))
}

fn as_document(b: Bson) -> Result<Document> {
    match b {
        Bson::Document(d) => Ok(d),
        _ => Err(Error::Invalid("a document is written { field: value, … }")),
    }
}

/// Runs an edit, after safe mode's question; says what it did.
#[tauri::command]
pub async fn mongo_edit(
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

/// Makes an edit's change on the session's database.
async fn apply(s: &Session, edit: Edit) -> Result<String> {
    let database = s.client.database(&s.db);
    match edit {
        Edit::Set { collection, id, field, value } => {
            field_ok(&field)?;
            let v = shell::values(&[&id, &value])?;
            let [id, value]: [Bson; 2] = v.try_into().map_err(|_| Error::Mongo("no value".into()))?;
            let r = database
                .collection::<Document>(&collection)
                .update_one(doc! { "_id": id }, doc! { "$set": { field: value } })
                .await?;
            if r.matched_count == 0 {
                return Err(Error::Invalid("the document is gone; reload"));
            }
            Ok("Saved".into())
        }
        Edit::Unset { collection, id, field } => {
            field_ok(&field)?;
            let id = one(shell::values(&[&id])?)?;
            database
                .collection::<Document>(&collection)
                .update_one(doc! { "_id": id }, doc! { "$unset": { field: "" } })
                .await?;
            Ok("Removed the field".into())
        }
        Edit::Replace { collection, id, document } => {
            let v = shell::values(&[&id, &document])?;
            let [id, document]: [Bson; 2] = v.try_into().map_err(|_| Error::Mongo("no value".into()))?;
            let mut document = as_document(document)?;
            // The _id stays: a replacement can't change it, so one left out or typed the same is fine.
            match document.get("_id") {
                Some(given) if *given != id => return Err(Error::Invalid("a document's _id can't change")),
                _ => {
                    document.remove("_id");
                }
            }
            let r = database.collection::<Document>(&collection).replace_one(doc! { "_id": id }, document).await?;
            if r.matched_count == 0 {
                return Err(Error::Invalid("the document is gone; reload"));
            }
            Ok("Saved".into())
        }
        Edit::Insert { collection, document } => {
            let document = as_document(one(shell::values(&[&document])?)?)?;
            let r = database.collection::<Document>(&collection).insert_one(document).await?;
            Ok(format!("Inserted, _id {}", json(r.inserted_id)))
        }
        Edit::Delete { collection, ids } => {
            let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            let ids = shell::values(&refs)?;
            let r = database.collection::<Document>(&collection).delete_many(doc! { "_id": { "$in": ids } }).await?;
            Ok(format!("Deleted {}", r.deleted_count))
        }
    }
}

/// The session's connection, for Connection › Info.
#[derive(Debug, Serialize)]
pub struct ConnectionInfo {
    server: String,
    driver: String,
    cipher: Option<String>,
    ssh: Option<String>,
}

#[tauri::command]
pub async fn mongo_connection_info(sessions: State<'_, Sessions>, name: String) -> Result<ConnectionInfo> {
    let s = sessions.get(&name).await?;
    let server = info(&s).await?.server;
    let options = client_options(&s.saved, None, None).await?;
    let tls = matches!(options.tls, Some(Tls::Enabled(_)));
    let ssh = s.saved.ssh.as_ref().map(|t| format!("{}@{}", t.user, t.host));
    Ok(ConnectionInfo {
        server,
        driver: "mongodb (official Rust driver)".into(),
        cipher: tls.then(|| "TLS".into()),
        ssh,
    })
}

#[cfg(test)]
mod tests;

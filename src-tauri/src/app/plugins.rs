//! Plugins (Settings › Plugins): small JavaScript programs that add actions to the grid's row menu and the sidebar's
//! table menu. A plugin is a folder with `plugin.json` (its manifest) and one script, installed from a Git
//! repository or a folder into `plugins/<id>` in the config folder.
//!
//! Each action runs in its own QuickJS runtime: no files, no network, no access to the app, a memory and stack cap
//! and a time limit. It gets the action's input as `input` and returns what the app should do with the result (copy
//! it, insert SQL into the editor, show a message). A plugin that asks for the `query` permission, and was granted
//! it at install, also gets `query(sql)`: read-only SQL on the workspace's connection, as safe mode allows.
//!
//! Installing is two steps, so the user sees the permissions before granting them: `plugin_fetch` reads and checks
//! the plugin and holds it, `plugin_install` writes it with the permissions granted.

use crate::error::{Error, Result};
use rquickjs::{CatchResultExt, Context, Function, Runtime};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State};

const MANIFEST: &str = "plugin.json";
const STATE: &str = "plugins.json";
const MAX_MANIFEST: u64 = 64 * 1024;
const MAX_SCRIPT: u64 = 1024 * 1024;
/// The action's input (selected rows) and its output.
const MAX_IO: usize = 16 * 1024 * 1024;
const MEMORY: usize = 64 * 1024 * 1024;
const STACK: usize = 1024 * 1024;
const TIME_LIMIT: Duration = Duration::from_secs(10);
/// Rows one `query` hands a plugin.
const QUERY_ROWS: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Read-only SQL on the workspace's connection.
    Query,
}

/// What an action works on, and so which menu it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// The selected rows of a grid.
    Rows,
    /// A table in the sidebar.
    Table,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub title: String,
    pub on: Target,
    /// The script's function to call with the input.
    pub run: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub permissions: Vec<Permission>,
    /// The script's file name, next to the manifest.
    #[serde(default = "default_main")]
    pub main: String,
    pub actions: Vec<Action>,
}

fn default_main() -> String {
    "main.js".into()
}

fn plain(s: &str, max: usize, first: fn(char) -> bool, rest: fn(char) -> bool) -> bool {
    let mut chars = s.chars();
    s.len() <= max && chars.next().is_some_and(first) && chars.all(rest)
}

impl Manifest {
    /// Checks what a manifest from outside may hold: names that are safe as folder and function names, short texts,
    /// and actions with distinct ids.
    fn check(&self) -> Result<()> {
        let id_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if !plain(&self.id, 64, |c| c.is_ascii_lowercase() || c.is_ascii_digit(), id_char) {
            return Err(Error::Plugin("the plugin's id must be lowercase letters, digits and dashes".into()));
        }
        let file_char = |c: char| c.is_ascii_alphanumeric() || "_.-".contains(c);
        if !plain(&self.main, 64, |c| c.is_ascii_alphanumeric(), file_char)
            || Path::new(&self.main).extension().is_none_or(|e| e != "js")
        {
            return Err(Error::Plugin("the plugin's script must be a .js file next to plugin.json".into()));
        }
        if self.name.trim().is_empty() || self.name.len() > 80 || self.version.len() > 40 {
            return Err(Error::Plugin("the plugin needs a name (up to 80 characters) and a short version".into()));
        }
        if self.description.len() > 1000 || self.author.len() > 200 {
            return Err(Error::Plugin("the plugin's description or author is too long".into()));
        }
        if self.actions.is_empty() || self.actions.len() > 50 {
            return Err(Error::Plugin("a plugin has from 1 to 50 actions".into()));
        }
        let ident = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
        for (i, a) in self.actions.iter().enumerate() {
            if !plain(&a.id, 64, |c| c.is_ascii_alphanumeric(), file_char)
                || self.actions[..i].iter().any(|b| b.id == a.id)
            {
                return Err(Error::Plugin(format!("action ids must be distinct and plain: {:?}", a.id)));
            }
            if !plain(&a.run, 64, |c| c.is_ascii_alphabetic() || c == '_' || c == '$', ident) {
                return Err(Error::Plugin(format!("{:?} isn't a function name", a.run)));
            }
            if a.title.trim().is_empty() || a.title.len() > 80 {
                return Err(Error::Plugin("each action needs a title of up to 80 characters".into()));
            }
        }
        Ok(())
    }
}

/// Where an installed plugin came from, so Update knows where to look.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Origin {
    Git(String),
    Folder(PathBuf),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Kept {
    origin: Origin,
    #[serde(default)]
    commit: String,
    granted: Vec<Permission>,
    enabled: bool,
}

/// `plugins.json`: by plugin id.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptFile {
    plugins: BTreeMap<String, Kept>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A file of at most `max` bytes, as text.
fn read_capped(path: &Path, max: u64) -> Result<String> {
    let failed = |source| Error::File { path: path.display().to_string(), source };
    let meta = std::fs::symlink_metadata(path).map_err(failed)?;
    if !meta.is_file() {
        return Err(Error::Plugin(format!("{} isn't a plain file", path.display())));
    }
    if meta.len() > max {
        return Err(Error::Plugin(format!("{} is too large", path.display())));
    }
    std::fs::read_to_string(path).map_err(failed)
}

/// A plugin read and checked: its manifest and script.
#[derive(Debug, Clone)]
struct Read {
    manifest: Manifest,
    manifest_text: String,
    script: String,
}

fn read_plugin(dir: &Path) -> Result<Read> {
    let manifest_text = read_capped(&dir.join(MANIFEST), MAX_MANIFEST)?;
    let manifest: Manifest = serde_json::from_str(&manifest_text)
        .map_err(|e| Error::Plugin(format!("plugin.json isn't a valid manifest: {e}")))?;
    manifest.check()?;
    let script = read_capped(&dir.join(&manifest.main), MAX_SCRIPT)?;
    Ok(Read { manifest, manifest_text, script })
}

/// Plugins fetched but not installed yet, by the token the UI got for each.
/// A fetched plugin, where it came from, and its commit.
type StagedPlugin = (Read, Origin, String);
static STAGED: LazyLock<Mutex<HashMap<String, StagedPlugin>>> = LazyLock::new(Mutex::default);

fn dirs(app: &AppHandle) -> Result<(PathBuf, PathBuf)> {
    let config = app.path().app_config_dir()?;
    Ok((config.join("plugins"), config.join(STATE)))
}

fn load_kept(file: &Path) -> Result<KeptFile> {
    match std::fs::read(file) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Error::Plugin(format!("plugins.json isn't valid ({e}); fix or move it"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KeptFile::default()),
        Err(source) => Err(Error::File { path: file.display().to_string(), source }),
    }
}

fn save_kept(file: &Path, kept: &KeptFile) -> Result<()> {
    crate::assistant::mcp::write_private(file, &serde_json::to_vec_pretty(kept)?)
}

/// Writes the plugin's two files into `plugins/<id>`, replacing an older version only once the new one is complete.
fn write_plugin(root: &Path, read: &Read) -> Result<()> {
    let failed = |path: &Path| {
        let path = path.display().to_string();
        move |source| Error::File { path, source }
    };
    std::fs::create_dir_all(root).map_err(failed(root))?;
    let target = root.join(&read.manifest.id);
    let fresh = root.join(format!(".{}.new", read.manifest.id));
    // A leftover from an install that stopped halfway; none is fine.
    let _ = std::fs::remove_dir_all(&fresh);
    std::fs::create_dir_all(&fresh).map_err(failed(&fresh))?;
    std::fs::write(fresh.join(MANIFEST), &read.manifest_text).map_err(failed(&fresh))?;
    std::fs::write(fresh.join(&read.manifest.main), &read.script).map_err(failed(&fresh))?;
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(failed(&target))?;
    }
    std::fs::rename(&fresh, &target).map_err(failed(&target))
}

/// Backs a script's `query(sql)`: JSON of the rows, or an error message.
type Query = Box<dyn Fn(String) -> std::result::Result<String, String>>;

/// Runs `function` of `script` with `input` (JSON) in a fresh QuickJS runtime, stopped after `limit`, and returns its
/// result as JSON. Without `query` the script's `query` throws.
fn run_script(script: &str, function: &str, input: &str, query: Option<Query>, limit: Duration) -> Result<String> {
    let runtime = Runtime::new().map_err(|e| Error::Plugin(e.to_string()))?;
    runtime.set_memory_limit(MEMORY);
    runtime.set_max_stack_size(STACK);
    let deadline = Instant::now() + limit;
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let context = Context::full(&runtime).map_err(|e| Error::Plugin(e.to_string()))?;
    let out = context.with(|ctx| -> std::result::Result<String, String> {
        let globals = ctx.globals();
        let fail = |e: rquickjs::Error| e.to_string();
        globals.set("__input", input).map_err(fail)?;
        let allowed = query.is_some();
        let host = Function::new(ctx.clone(), move |sql: String| -> String {
            let reply = match &query {
                Some(q) => q(sql),
                None => Err("this plugin wasn't given the query permission".into()),
            };
            reply.unwrap_or_else(|e| serde_json::json!({ "error": e }).to_string())
        })
        .map_err(fail)?;
        globals.set("__query", host).map_err(fail)?;
        let bootstrap = format!(
            "\"use strict\";
             globalThis.input = JSON.parse(__input);
             globalThis.console = {{ log() {{}}, warn() {{}}, error() {{}} }};
             globalThis.canQuery = {allowed};
             globalThis.query = (sql) => {{ const r = JSON.parse(__query(String(sql))); if (r.error) throw new Error(r.error); return r.rows; }};"
        );
        ctx.eval::<(), _>(bootstrap).catch(&ctx).map_err(|e| e.to_string())?;
        ctx.eval::<(), _>(script).catch(&ctx).map_err(|e| e.to_string())?;
        let call = format!(
            "(() => {{ if (typeof globalThis[{name}] !== 'function') throw new Error('the script has no function ' + {name}); \
             const out = globalThis[{name}](globalThis.input); return JSON.stringify(out === undefined ? null : out); }})()",
            name = serde_json::to_string(function).unwrap_or_default()
        );
        ctx.eval::<String, _>(call).catch(&ctx).map_err(|e| e.to_string())
    });
    out.map_err(|e| {
        if Instant::now() > deadline {
            Error::Plugin(format!("the plugin ran for over {} seconds and was stopped", limit.as_secs_f32()))
        } else {
            Error::Plugin(e.trim().to_string())
        }
    })
}

/// What an action asks the app to do with its result.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    #[serde(default)]
    pub clipboard: Option<String>,
    /// SQL to put in the editor.
    #[serde(default)]
    pub insert: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

fn output(json: &str) -> Result<Output> {
    if json.len() > MAX_IO {
        return Err(Error::Plugin("the plugin's result is too large".into()));
    }
    match serde_json::from_str::<Value>(json)? {
        Value::Null => Ok(Output::default()),
        Value::String(s) => Ok(Output { message: Some(s), ..Output::default() }),
        v => serde_json::from_value(v)
            .map_err(|e| Error::Plugin(format!("an action returns {{ clipboard, insert, message }} or a string: {e}"))),
    }
}

// ---------- Commands ----------

/// An installed plugin as Settings › Plugins and the menus see it. `problem` says why one can't be read.
#[derive(Debug, Serialize)]
pub struct Installed {
    id: String,
    manifest: Option<Manifest>,
    problem: Option<String>,
    origin: Origin,
    commit: String,
    granted: Vec<Permission>,
    enabled: bool,
}

#[tauri::command]
pub async fn plugin_list(app: AppHandle) -> Result<Vec<Installed>> {
    let (root, file) = dirs(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let kept = load_kept(&file)?;
        Ok(kept
            .plugins
            .into_iter()
            .map(|(id, k)| {
                let read = read_plugin(&root.join(&id));
                let (manifest, problem) = match read {
                    Ok(r) => (Some(r.manifest), None),
                    Err(e) => (None, Some(e.to_string())),
                };
                Installed {
                    id,
                    manifest,
                    problem,
                    origin: k.origin,
                    commit: k.commit,
                    granted: k.granted,
                    enabled: k.enabled,
                }
            })
            .collect())
    })
    .await?
}

/// A plugin fetched for review: what it is, what it asks for, and what an installed version was granted.
#[derive(Debug, Serialize)]
pub struct Staged {
    token: String,
    manifest: Manifest,
    commit: String,
    /// Set when this updates an installed plugin.
    granted: Option<Vec<Permission>>,
}

/// Reads a plugin from a Git repository (https) or a folder and holds it for `plugin_install`.
#[tauri::command]
pub async fn plugin_fetch(app: AppHandle, origin: Origin) -> Result<Staged> {
    let (_, file) = dirs(&app)?;
    let (read, commit) = match &origin {
        Origin::Git(url) => {
            let url = crate::app::themes::checked_url(url)?;
            let scratch = crate::app::themes::scratch("plugin")?;
            let commit = crate::app::themes::shallow_clone(url.as_str(), "https", &scratch.0).await?;
            let dir = scratch.0.clone();
            let read = tauri::async_runtime::spawn_blocking(move || read_plugin(&dir)).await??;
            (read, commit)
        }
        Origin::Folder(dir) => {
            let dir = dir.clone();
            (tauri::async_runtime::spawn_blocking(move || read_plugin(&dir)).await??, String::new())
        }
    };
    let granted = load_kept(&file)?.plugins.get(&read.manifest.id).map(|k| k.granted.clone());
    let token = crate::assistant::mcp::random_hex(16)?;
    let staged = Staged { token: token.clone(), manifest: read.manifest.clone(), commit: commit.clone(), granted };
    lock(&STAGED).insert(token, (read, origin, commit));
    Ok(staged)
}

/// Installs (or updates) a fetched plugin with the permissions the user granted, which it must have asked for.
#[tauri::command]
pub async fn plugin_install(app: AppHandle, token: String, grant: Vec<Permission>) -> Result<String> {
    let (root, file) = dirs(&app)?;
    let (read, origin, commit) = lock(&STAGED).remove(&token).ok_or(Error::Plugin("fetch the plugin again".into()))?;
    if grant.iter().any(|g| !read.manifest.permissions.contains(g)) {
        return Err(Error::Plugin("a permission the plugin didn't ask for can't be granted".into()));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut kept = load_kept(&file)?;
        write_plugin(&root, &read)?;
        let mut granted = grant;
        granted.sort();
        granted.dedup();
        kept.plugins.insert(read.manifest.id.clone(), Kept { origin, commit, granted, enabled: true });
        save_kept(&file, &kept)?;
        Ok(read.manifest.name)
    })
    .await?
}

#[tauri::command]
pub async fn plugin_remove(app: AppHandle, id: String) -> Result<()> {
    let (root, file) = dirs(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut kept = load_kept(&file)?;
        if kept.plugins.remove(&id).is_none() {
            return Ok(());
        }
        save_kept(&file, &kept)?;
        let dir = root.join(&id);
        // The id came from plugins.json, but it still names only a folder inside plugins/.
        if dir.parent() == Some(root.as_path()) && dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|source| Error::File { path: dir.display().to_string(), source })?;
        }
        Ok(())
    })
    .await?
}

#[tauri::command]
pub async fn plugin_enable(app: AppHandle, id: String, enabled: bool) -> Result<()> {
    let (_, file) = dirs(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut kept = load_kept(&file)?;
        let k = kept.plugins.get_mut(&id).ok_or(Error::Plugin("no such plugin".into()))?;
        k.enabled = enabled;
        save_kept(&file, &kept)
    })
    .await?
}

/// Runs an action with `input`. `connection` is the workspace's session, which `query` uses when granted.
#[tauri::command]
pub async fn plugin_run(
    app: AppHandle,
    open: State<'_, crate::Open>,
    prefs: State<'_, crate::Prefs>,
    id: String,
    action: String,
    input: Value,
    connection: Option<String>,
) -> Result<Output> {
    let (root, file) = dirs(&app)?;
    let text = input.to_string();
    if text.len() > MAX_IO {
        return Err(Error::Plugin("too much to hand a plugin; select fewer rows".into()));
    }
    let (open, prefs) = (open.inner().clone(), prefs.inner().clone());
    let json = tauri::async_runtime::spawn_blocking(move || {
        let kept = load_kept(&file)?;
        let k = kept.plugins.get(&id).filter(|k| k.enabled).ok_or(Error::Plugin("the plugin is off or gone".into()))?;
        let read = read_plugin(&root.join(&id))?;
        let act = read
            .manifest
            .actions
            .iter()
            .find(|a| a.id == action)
            .ok_or(Error::Plugin("the plugin has no such action".into()))?;
        let query: Option<Query> = match (k.granted.contains(&Permission::Query), connection) {
            (true, Some(name)) => Some(Box::new(move |sql: String| {
                // A blocking thread, outside the async runtime, so waiting here holds up nothing else.
                tauri::async_runtime::block_on(async {
                    crate::guard(&open, &name, |_| true, false, None).await?;
                    crate::run_read_only(&open, &prefs, &name, &sql, QUERY_ROWS).await
                })
                .map(|r| {
                    let rows: Vec<serde_json::Map<String, Value>> = r
                        .rows
                        .into_iter()
                        .map(|row| {
                            r.columns
                                .iter()
                                .cloned()
                                .zip(row.into_iter().map(|v| v.map_or(Value::Null, Value::String)))
                                .collect()
                        })
                        .collect();
                    serde_json::json!({ "rows": rows }).to_string()
                })
                .map_err(|e| match e {
                    Error::ConfirmRequired | Error::PasswordRequired => {
                        "safe mode asks before every query on this connection, so plugins can't query it".to_string()
                    }
                    e => e.to_string(),
                })
            })),
            _ => None,
        };
        run_script(&read.script, &act.run, &text, query, TIME_LIMIT)
    })
    .await??;
    output(&json)
}

#[cfg(test)]
mod tests {
    use super::{Manifest, Output, Query, Read, TIME_LIMIT, output, read_plugin, write_plugin};
    use std::path::PathBuf;
    use std::time::Duration;

    fn run_script(script: &str, function: &str, input: &str, query: Option<Query>) -> crate::error::Result<String> {
        super::run_script(script, function, input, query, TIME_LIMIT)
    }

    fn manifest(json: &str) -> Manifest {
        serde_json::from_str(json).unwrap()
    }

    const GOOD: &str = r#"{"id":"md-table","name":"Markdown","version":"1.0","actions":[{"id":"copy","title":"Copy as Markdown","on":"rows","run":"copyMd"}]}"#;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tableminus-plugins-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); // a leftover from an earlier run; none is fine
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn manifest_check_should_accept_a_plain_plugin() {
        assert!(manifest(GOOD).check().is_ok());
    }

    #[test]
    fn manifest_check_should_refuse_paths_in_the_id_or_script() {
        assert!(manifest(&GOOD.replace("md-table", "../evil")).check().is_err());
        let with_main = GOOD.replace(r#""version":"1.0","#, r#""version":"1.0","main":"../x.js","#);
        assert!(manifest(&with_main).check().is_err());
    }

    #[test]
    fn manifest_check_should_refuse_a_run_that_isnt_a_function_name() {
        assert!(manifest(&GOOD.replace("copyMd", "a; b()")).check().is_err());
    }

    #[test]
    fn manifest_should_refuse_unknown_fields_and_permissions() {
        assert!(
            serde_json::from_str::<Manifest>(&GOOD.replace(r#""id":"md-table","#, r#""id":"md-table","x":1,"#))
                .is_err()
        );
        assert!(
            serde_json::from_str::<Manifest>(
                &GOOD.replace(r#""id":"md-table","#, r#""id":"md-table","permissions":["files"],"#)
            )
            .is_err()
        );
    }

    #[test]
    fn run_script_should_call_the_function_with_the_input() {
        let out = run_script(
            "function copyMd(i) { return { clipboard: i.rows.map((r) => r.a).join(',') }; }",
            "copyMd",
            r#"{"rows":[{"a":"1"},{"a":"2"}]}"#,
            None,
        )
        .unwrap();
        assert_eq!(output(&out).unwrap(), Output { clipboard: Some("1,2".into()), ..Output::default() });
    }

    #[test]
    fn run_script_should_stop_a_script_that_never_ends() {
        let e = super::run_script("function f() { for (;;) {} }", "f", "{}", None, Duration::from_millis(200))
            .unwrap_err()
            .to_string();
        assert!(e.contains("seconds"), "{e}");
    }

    #[test]
    fn run_script_should_stop_a_script_that_eats_memory() {
        let e = run_script("function f() { const a = []; for (;;) a.push('x'.repeat(1e6)); }", "f", "{}", None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("memory"), "{e}");
    }

    #[test]
    fn run_script_should_offer_no_query_without_the_permission() {
        let e = run_script("function f() { return query('select 1'); }", "f", "{}", None).unwrap_err().to_string();
        assert!(e.contains("query permission"), "{e}");
    }

    #[test]
    fn run_script_should_hand_query_rows_to_the_script() {
        let q: Query = Box::new(|sql| {
            assert_eq!(sql, "select n");
            Ok(r#"{"rows":[{"n":"7"}]}"#.into())
        });
        let out = run_script("function f() { return 'n=' + query('select n')[0].n; }", "f", "{}", Some(q)).unwrap();
        assert_eq!(output(&out).unwrap().message.as_deref(), Some("n=7"));
    }

    #[test]
    fn run_script_should_reach_no_files_or_network() {
        let out = run_script(
            "function f() { return [typeof require, typeof fetch, typeof std, typeof os].join(); }",
            "f",
            "{}",
            None,
        )
        .unwrap();
        assert_eq!(out, r#""undefined,undefined,undefined,undefined""#);
    }

    #[test]
    fn output_should_refuse_unknown_fields() {
        assert!(output(r#"{"exec":"rm -rf /"}"#).is_err());
        assert_eq!(output("null").unwrap(), Output::default());
    }

    #[test]
    fn read_and_write_plugin_should_round_trip_and_replace_an_older_version() {
        let src = dir("src");
        std::fs::write(src.join("plugin.json"), GOOD).unwrap();
        std::fs::write(src.join("main.js"), "function copyMd() {}").unwrap();
        let read: Read = read_plugin(&src).unwrap();
        let root = dir("root");
        write_plugin(&root, &read).unwrap();
        std::fs::write(src.join("main.js"), "function copyMd() { return 2; }").unwrap();
        write_plugin(&root, &read_plugin(&src).unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("md-table").join("main.js")).unwrap(),
            "function copyMd() { return 2; }"
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "no leftovers");
    }

    #[cfg(unix)]
    #[test]
    fn read_plugin_should_not_follow_a_linked_script() {
        let src = dir("link");
        std::fs::write(src.join("plugin.json"), GOOD).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", src.join("main.js")).unwrap();
        assert!(read_plugin(&src).is_err());
    }

    /// The example in examples/plugins stays installable and working.
    #[test]
    fn the_example_plugin_should_read_and_run() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/plugins/json-tools");
        let read = read_plugin(&dir).unwrap();
        let rows =
            run_script(&read.script, "copyJson", r#"{"table":"t","columns":["a"],"rows":[{"a":"1"}]}"#, None).unwrap();
        assert!(output(&rows).unwrap().clipboard.unwrap().contains(r#""a": "1""#));
        let q: Query = Box::new(|sql| {
            assert_eq!(sql, r#"select count(*) as n from "public"."t""#);
            Ok(r#"{"rows":[{"n":"3"}]}"#.into())
        });
        let count =
            run_script(&read.script, "countRows", r#"{"table":"public.t","engine":"postgres"}"#, Some(q)).unwrap();
        assert_eq!(output(&count).unwrap().message.as_deref(), Some("public.t has 3 rows"));
    }
}

//! The MCP server itself: streamable HTTP on 127.0.0.1, a bearer token on every request, and the four tools.
//! It is stateless (no MCP sessions), so the stdio bridge can forward each message as its own request.

use super::{Activity, Shared, Token, now_ms};
use crate::error::{Error, Result};
use crate::readonly::is_read_only;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response, StatusCode, header};
use hyper_util::rt::TokioIo;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, JsonObject, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Most rows a query returns to an agent, whatever the editor's row limit says.
const MAX_ROWS: usize = 10_000;

/// A running server; dropping it stops accepting connections.
pub struct Running {
    pub port: u16,
    /// The port asked for, so a settings change can tell whether to move.
    pub preferred: u16,
    accept: tauri::async_runtime::JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        // ponytail: connections already accepted finish on their own; track and cancel them if a long-running
        // request after disabling ever matters.
        self.accept.abort();
    }
}

/// Listens on 127.0.0.1, on `preferred` or, when that's taken, on any free port.
pub async fn start(shared: Arc<Shared>, preferred: u16) -> Result<Running> {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", preferred)).await {
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?,
        other => other?,
    };
    let port = listener.local_addr()?.port();
    // Stateless with plain JSON replies: each message is one request, which is all the bridge does. The default
    // allowed hosts (loopback only) stop DNS-rebinding pages from reaching the server.
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None);
    let tools = shared.clone();
    let mcp =
        StreamableHttpService::new(move || Ok(Tools(tools.clone())), Arc::new(LocalSessionManager::default()), config);
    let accept = tauri::async_runtime::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let (shared, mcp) = (shared.clone(), mcp.clone());
            let service = hyper::service::service_fn(move |req| {
                let (shared, mcp) = (shared.clone(), mcp.clone());
                async move { Ok::<_, std::convert::Infallible>(authorized(&shared, &mcp, req).await) }
            });
            tauri::async_runtime::spawn(async move {
                // A client hanging up mid-request is its own business.
                let _ =
                    hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
            });
        }
    });
    Ok(Running { port, preferred, accept })
}

type Reply = Response<http_body_util::combinators::BoxBody<Bytes, std::convert::Infallible>>;

/// Hands the request to the MCP service with the caller's token attached, or answers 401.
async fn authorized(
    shared: &Shared,
    mcp: &StreamableHttpService<Tools, LocalSessionManager>,
    mut req: Request<Incoming>,
) -> Reply {
    let bearer = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
    let Some(token) = bearer.and_then(|b| b.strip_prefix("Bearer ")).and_then(|b| shared.caller(b.trim())) else {
        let mut reply = Response::new(
            Full::new(Bytes::from_static(
                b"A Table Minus MCP token is required: Settings > MCP lists them and shows how to connect.",
            ))
            .boxed(),
        );
        *reply.status_mut() = StatusCode::UNAUTHORIZED;
        reply.headers_mut().insert(header::WWW_AUTHENTICATE, header::HeaderValue::from_static("Bearer"));
        return reply;
    };
    req.extensions_mut().insert(token);
    mcp.handle(req).await
}

/// The tool handler, made for each request.
#[derive(Clone)]
struct Tools(Arc<Shared>);

fn schema(properties: &[(&str, &str)]) -> Arc<JsonObject> {
    let props: serde_json::Map<String, serde_json::Value> = properties
        .iter()
        .map(|(name, about)| ((*name).to_string(), serde_json::json!({ "type": "string", "description": about })))
        .collect();
    let required: Vec<&str> = properties.iter().map(|(name, _)| *name).collect();
    let schema = serde_json::json!({ "type": "object", "properties": props, "required": required });
    Arc::new(match schema {
        serde_json::Value::Object(o) => o,
        _ => JsonObject::new(),
    })
}

fn tools() -> Vec<Tool> {
    const CONNECTION: (&str, &str) = ("connection", "Connection name, from list_connections");
    vec![
        Tool::new(
            "list_connections",
            "The database connections this token may use, with their engine and whether they are open.",
            schema(&[]),
        ),
        Tool::new("list_tables", "The tables (and views) in a connection's current database.", schema(&[CONNECTION])),
        Tool::new(
            "describe_table",
            "A table's columns: name, type, nullable, default and whether it is in the primary key.",
            schema(&[CONNECTION, ("table", "Table name, as list_tables gives it")]),
        ),
        Tool::new(
            "run_query",
            "Runs SQL and returns columns and rows as JSON (rows are capped; use LIMIT). Anything that isn't \
             provably read-only may need the user's approval in Table Minus, and read-only tokens can't run it.",
            schema(&[CONNECTION, ("sql", "The SQL to run; several statements may be separated by semicolons")]),
        ),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    #[serde(default)]
    connection: Option<String>,
    #[serde(default)]
    table: Option<String>,
    #[serde(default)]
    sql: Option<String>,
}

/// A failed call, worded for the agent; the log gets the same text.
struct Refusal(String);

impl From<Error> for Refusal {
    fn from(e: Error) -> Self {
        Refusal(e.to_string())
    }
}

fn need<'a>(value: Option<&'a String>, what: &str) -> std::result::Result<&'a str, Refusal> {
    value.map(String::as_str).filter(|v| !v.trim().is_empty()).ok_or_else(|| Refusal(format!("`{what}` is required")))
}

impl ServerHandler for Tools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("table-minus", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Read and query the databases the user has set up in Table Minus. Start with list_connections. \
                 Writes may wait for the user to allow them in the app.",
            )
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = std::result::Result<ListToolsResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(tools())))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|t| t.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, ErrorData> {
        let Some(token) =
            context.extensions.get::<hyper::http::request::Parts>().and_then(|p| p.extensions.get::<Token>()).cloned()
        else {
            return Err(ErrorData::internal_error("the request reached the tools without a token", None));
        };
        let args: Args = serde_json::from_value(serde_json::Value::Object(request.arguments.unwrap_or_default()))
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        let tool = request.name.to_string();
        let mut entry = Activity {
            at: now_ms(),
            token: token.name.clone(),
            tool: tool.clone(),
            connection: args.connection.clone(),
            sql: args.sql.clone(),
            outcome: String::new(),
        };
        let result = match tool.as_str() {
            "list_connections" => self.list_connections(&token).await,
            "list_tables" => self.list_tables(&token, &args).await,
            "describe_table" => self.describe_table(&token, &args).await,
            "run_query" => self.run_query(&token, &args).await,
            _ => return Err(ErrorData::invalid_params(format!("no tool named {tool:?}"), None)),
        };
        let reply = match result {
            Ok((json, outcome)) => {
                entry.outcome = outcome;
                CallToolResult::success(vec![ContentBlock::text(json)])
            }
            Err(Refusal(why)) => {
                entry.outcome.clone_from(&why);
                CallToolResult::error(vec![ContentBlock::text(why)])
            }
        };
        self.0.log(entry);
        Ok(reply.into())
    }
}

/// What [`Tools::reach`] finds out about a connection.
type Reached = (crate::db::Engine, crate::safety::Safety, Option<secrecy::SecretString>);

/// A tool's JSON reply and the outcome line for the activity log.
type Done = std::result::Result<(String, String), Refusal>;

fn json(value: &impl serde::Serialize) -> std::result::Result<String, Refusal> {
    serde_json::to_string(value).map_err(|e| Refusal(e.to_string()))
}

impl Tools {
    /// The saved connection's engine, safe mode level and stored password, after checking the token may reach it.
    /// Opens it when the token names it and it isn't open yet.
    async fn reach(&self, token: &Token, name: &str) -> std::result::Result<Reached, Refusal> {
        let open = &self.0.open;
        open.unlocked()?;
        // Open in any workspace tab counts; the server itself works on the connection's first session.
        if !token.reaches(name, open.any_open(name).await) {
            return Err(Refusal(format!(
                "this token can't use {name:?}: it reaches only the connections it lists, or open ones when it \
                 allows all"
            )));
        }
        if !open.is_open(name).await {
            let live = crate::open_saved(open, &self.0.dir, name).await?;
            open.insert(name.to_string(), live).await;
        }
        let live = open.get(name).await?;
        Ok((live.saved.kind, live.saved.safety, live.password.clone()))
    }

    async fn list_connections(&self, token: &Token) -> Done {
        let saved = crate::store::load(&self.0.dir)?;
        self.0.open.unlocked()?;
        let conns = self.0.open.conns.lock().await;
        let open: std::collections::HashSet<&str> = conns.keys().map(|k| crate::saved_name(k)).collect();
        let list: Vec<_> = saved
            .iter()
            .filter(|c| token.reaches(&c.name, open.contains(c.name.as_str())))
            .map(|c| {
                // Only what an agent needs to pick one: never the URL, user or anything from the keychain.
                serde_json::json!({ "name": c.name, "engine": c.kind, "tag": c.tag, "open": open.contains(c.name.as_str()) })
            })
            .collect();
        Ok((json(&list)?, format!("{} connections", list.len())))
    }

    async fn list_tables(&self, token: &Token, args: &Args) -> Done {
        let name = need(args.connection.as_ref(), "connection")?;
        self.reach(token, name).await?;
        let mut live = self.0.open.get(name).await?;
        let system = live.saved.options.system_schemas;
        let r = live.conn.tables(system).await;
        let tables = live.check(r)?;
        Ok((json(&tables)?, format!("{} tables", tables.len())))
    }

    async fn describe_table(&self, token: &Token, args: &Args) -> Done {
        let name = need(args.connection.as_ref(), "connection")?;
        let table = need(args.table.as_ref(), "table")?;
        self.reach(token, name).await?;
        let mut live = self.0.open.get(name).await?;
        let r = live.conn.columns(table).await;
        let columns = live.check(r)?;
        Ok((json(&columns)?, format!("{} columns", columns.len())))
    }

    /// Read-only tokens may only run read-only SQL. A query waits for the user's Allow when approval is on and it
    /// may write, or when the connection's safe mode level asks for it (Alert 1 and Safe 1 ask even for reads; Safe
    /// levels want the password with the Allow).
    /// A read-only token's query also runs where the database refuses writes, which catches a function that writes.
    async fn run_query(&self, token: &Token, args: &Args) -> Done {
        let name = need(args.connection.as_ref(), "connection")?;
        let sql = need(args.sql.as_ref(), "sql")?.to_string();
        let (engine, level, password) = self.reach(token, name).await?;
        let read_only = is_read_only(engine, &sql);
        if !read_only && token.read_only {
            return Err(Refusal("this token is read-only, and the SQL isn't provably read-only".into()));
        }
        let approval = super::lock(&self.0.config).approval && !read_only;
        if approval || level.asks(read_only) {
            let safe = (level.asks(read_only) && level.wants_password()).then_some(super::SafeCheck(password));
            // The lock on open connections isn't held while waiting, so the app stays usable.
            if !self.0.approve(token, name, &sql, safe).await {
                return Err(Refusal("the user denied the query in Table Minus (or didn't answer in time)".into()));
            }
        }
        let limit = match self.0.prefs.max_rows.load(Ordering::Relaxed) {
            0 => MAX_ROWS,
            n => usize::try_from(n).unwrap_or(MAX_ROWS).min(MAX_ROWS),
        };
        let mut live = self.0.open.get(name).await?;
        let (start, end) = live.conn.read_only_bounds();
        if token.read_only {
            let r = live.conn.query(start).await;
            live.check(r)?;
        }
        let timeout = crate::query_limit(&self.0.prefs);
        // The time limit covers only the query, so the end below still runs after a stopped one.
        let r = crate::with_limit(&mut live, timeout, move |c| {
            Box::pin(async move { c.query_limited(&sql, Some(limit)).await })
        })
        .await;
        if token.read_only {
            let ended = live.conn.query(end).await;
            if r.is_ok() {
                live.check(ended)?;
            }
        }
        let result = live.check(r)?;
        let outcome = if result.columns.is_empty() {
            format!("{} rows affected", result.affected)
        } else {
            format!("{} rows{}", result.rows.len(), if result.truncated { " (capped)" } else { "" })
        };
        Ok((json(&result)?, outcome))
    }
}

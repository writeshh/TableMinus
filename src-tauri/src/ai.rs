//! The AI assistant's link to a model provider: an OpenAI-compatible chat completions endpoint (OpenAI itself, or a
//! local server such as Ollama or LM Studio) or Anthropic's Messages API. The UI keeps the conversation in one
//! provider-neutral form ([`Turn`]); each request is translated here, sent with the API key from the keychain, and the
//! answer translated back.
//!
//! The model has one tool, `run_query`. A call to it comes back to the UI, which shows the SQL and runs it only when
//! the user approves, and then only read-only ([`crate::ai_run`]).

use crate::error::{Error, Result};
use crate::store::{self, Secret};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::{Request, header};
use hyper_util::rt::TokioIo;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// Anything that speaks OpenAI's `/chat/completions`, at the base URL given.
    OpenAi,
    Anthropic,
}

impl Provider {
    /// The keychain account the provider's API key is kept under.
    fn account(self) -> &'static str {
        match self {
            Provider::OpenAi => "openai",
            Provider::Anthropic => "anthropic",
        }
    }
}

/// One step of the conversation, the same for every provider.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Turn {
    User {
        text: String,
    },
    Assistant {
        text: String,
        #[serde(default)]
        call: Option<Call>,
    },
    /// What the model's `run_query` call `id` gave: the rows, the error, or that the user declined it.
    Result {
        id: String,
        text: String,
    },
}

/// The model asking to run `sql`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub id: String,
    pub sql: String,
}

/// A request from the assistant pane.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chat {
    pub provider: Provider,
    pub base_url: String,
    pub model: String,
    /// The database's engine and version as the UI shows it, such as "PostgreSQL 16".
    pub server: String,
    /// The tables and columns, as text.
    pub schema: String,
    pub turns: Vec<Turn>,
    /// An HTTP proxy to tunnel through (`http://host:port`); empty for none.
    #[serde(default)]
    pub proxy: String,
}

/// The model's answer: text, and perhaps a query it wants run.
#[derive(Debug, PartialEq, Serialize)]
pub struct Reply {
    pub text: String,
    pub call: Option<Call>,
}

/// Longer than a useful conversation; keeps a runaway one from growing without end.
const MAX_TURNS: usize = 200;
/// The request's text (schema and turns together). Providers cap their context well below this anyway.
const MAX_TEXT: usize = 2 * 1024 * 1024;
/// A chat answer is a few kilobytes; a bigger body isn't one.
const MAX_REPLY: usize = 8 * 1024 * 1024;
/// The most the model may write in one answer.
const MAX_TOKENS: u32 = 4096;
/// Models can take a while to answer, but not this long.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(180);
const ANTHROPIC_VERSION: &str = "2023-06-01";
const TOOL: &str = "run_query";
const TOOL_ABOUT: &str = "Run one read-only SQL statement (SELECT, SHOW, EXPLAIN…) on the user's database and see \
     up to 50 rows. The user approves each call; a statement that could write is refused.";

fn system_prompt(server: &str, schema: &str) -> String {
    format!(
        "You help a user of TableMinus, a database client, with a {server} database. Answer briefly. Write SQL for \
         this engine, in ```sql fenced blocks; the user can insert a block into the editor. Call {TOOL} when you need \
         to see data, not to show the user a query. The schema:\n{schema}"
    )
}

/// The `sql` argument of a tool call, from its JSON.
fn sql_of(arguments: &Value) -> Option<String> {
    arguments.get("sql").and_then(Value::as_str).map(String::from)
}

/// The body of an OpenAI-compatible `/chat/completions` request.
fn openai_body(chat: &Chat) -> Value {
    let mut messages = vec![json!({ "role": "system", "content": system_prompt(&chat.server, &chat.schema) })];
    for t in &chat.turns {
        messages.push(match t {
            Turn::User { text } => json!({ "role": "user", "content": text }),
            Turn::Assistant { text, call: None } => json!({ "role": "assistant", "content": text }),
            Turn::Assistant { text, call: Some(c) } => json!({
                "role": "assistant",
                "content": if text.is_empty() { Value::Null } else { Value::String(text.clone()) },
                "tool_calls": [{ "id": c.id, "type": "function",
                    "function": { "name": TOOL, "arguments": json!({ "sql": c.sql }).to_string() } }],
            }),
            Turn::Result { id, text } => json!({ "role": "tool", "tool_call_id": id, "content": text }),
        });
    }
    json!({
        "model": chat.model,
        "max_tokens": MAX_TOKENS,
        "messages": messages,
        "tools": [{ "type": "function", "function": { "name": TOOL, "description": TOOL_ABOUT, "parameters": {
            "type": "object", "properties": { "sql": { "type": "string" } }, "required": ["sql"] } } }],
    })
}

/// The body of an Anthropic `/v1/messages` request.
fn anthropic_body(chat: &Chat) -> Value {
    let messages: Vec<Value> = chat
        .turns
        .iter()
        .map(|t| match t {
            Turn::User { text } => json!({ "role": "user", "content": text }),
            Turn::Assistant { text, call } => {
                let mut content = Vec::new();
                if !text.is_empty() {
                    content.push(json!({ "type": "text", "text": text }));
                }
                if let Some(c) = call {
                    content.push(json!({ "type": "tool_use", "id": c.id, "name": TOOL, "input": { "sql": c.sql } }));
                }
                json!({ "role": "assistant", "content": content })
            }
            Turn::Result { id, text } => {
                json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": id, "content": text }] })
            }
        })
        .collect();
    json!({
        "model": chat.model,
        "max_tokens": MAX_TOKENS,
        "system": system_prompt(&chat.server, &chat.schema),
        "messages": messages,
        "tools": [{ "name": TOOL, "description": TOOL_ABOUT, "input_schema": {
            "type": "object", "properties": { "sql": { "type": "string" } }, "required": ["sql"] } }],
    })
}

/// The answer in an OpenAI-compatible response. Only the first tool call is kept, so the conversation stays one
/// call and one result at a time.
fn openai_reply(v: &Value) -> Option<Reply> {
    let message = v.get("choices")?.get(0)?.get("message")?;
    let text = message.get("content").and_then(Value::as_str).unwrap_or_default().to_string();
    let call = message.get("tool_calls").and_then(|c| c.get(0)).and_then(|c| {
        let function = c.get("function")?;
        let arguments: Value = serde_json::from_str(function.get("arguments")?.as_str()?).ok()?;
        Some(Call { id: c.get("id")?.as_str()?.to_string(), sql: sql_of(&arguments)? })
    });
    Some(Reply { text, call })
}

/// The answer in an Anthropic response: its text blocks joined, and its first `tool_use`.
fn anthropic_reply(v: &Value) -> Option<Reply> {
    let blocks = v.get("content")?.as_array()?;
    let text = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n\n");
    let call = blocks
        .iter()
        .find(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        .and_then(|b| Some(Call { id: b.get("id")?.as_str()?.to_string(), sql: sql_of(b.get("input")?)? }));
    Some(Reply { text, call })
}

fn is_local(url: &Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

/// The endpoint for `provider` under `base`. A key may only travel encrypted, so plain HTTP is for this computer.
fn endpoint(provider: Provider, base: &str) -> Result<Url> {
    let mut url = Url::parse(base.trim())?;
    match url.scheme() {
        "https" => {}
        "http" if is_local(&url) => {}
        _ => return Err(Error::Invalid("the AI base URL must start with https:// (http:// only for this computer)")),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Invalid("put the API key in its own field, not in the AI base URL"));
    }
    let suffix = match provider {
        Provider::OpenAi => "chat/completions",
        Provider::Anthropic => "v1/messages",
    };
    let path = format!("{}/{suffix}", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

/// The keychain account (under [`Secret::Ai`]) of the proxy's password.
const PROXY_ACCOUNT: &str = "proxy";

/// The proxy to use, if any. Only plain `http://` proxies, which tunnel HTTPS with CONNECT. A user name may be in the
/// address; its password never is (it's in the keychain).
fn proxy_url(proxy: &str) -> Result<Option<Url>> {
    let proxy = proxy.trim();
    if proxy.is_empty() {
        return Ok(None);
    }
    let u = Url::parse(proxy)?;
    if u.scheme() != "http" || u.host_str().is_none() {
        return Err(Error::Invalid("the proxy must be an http:// address, such as http://proxy.example.com:8080"));
    }
    if u.password().is_some() {
        return Err(Error::Invalid(
            "put only the user name in the proxy address (http://user@proxy:8080); save its password below it",
        ));
    }
    Ok(Some(u))
}

/// The proxy and its `Proxy-Authorization` value, when its address names a user.
struct Proxy {
    url: Url,
    auth: Option<SecretString>,
}

/// `Basic` credentials for `user` (percent-decoded from the address) and `password`.
fn basic_auth(user: &str, password: &SecretString) -> SecretString {
    use base64::Engine;
    let user = url::form_urlencoded::parse(format!("u={user}").as_bytes()).next().map(|(_, v)| v.into_owned());
    let pair = format!("{}:{}", user.unwrap_or_default(), password.expose_secret());
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair)).into()
}

/// Opens a tunnel to `host:port` through the HTTP proxy at `proxy` (CONNECT), ready for TLS.
async fn tunnel(proxy: &Proxy, host: &str, port: u16) -> Result<tokio::net::TcpStream> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Proxy { url: proxy, auth } = proxy;
    let at =
        proxy.host_str().ok_or(Error::Invalid("the proxy has no host"))?.trim_start_matches('[').trim_end_matches(']');
    let mut stream = tokio::net::TcpStream::connect((at, proxy.port_or_known_default().unwrap_or(80))).await?;
    let target = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let credentials =
        auth.as_ref().map(|a| format!("Proxy-Authorization: {}\r\n", a.expose_secret())).unwrap_or_default();
    stream.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n{credentials}\r\n").as_bytes()).await?;
    // The proxy's answer is a status line and headers; anything longer isn't one.
    let mut answer = Vec::new();
    let mut byte = [0u8; 1];
    while !answer.ends_with(b"\r\n\r\n") {
        if answer.len() > 8192 || stream.read(&mut byte).await? == 0 {
            return Err(Error::Ai("the proxy closed the connection".into()));
        }
        answer.push(byte[0]);
    }
    let status = String::from_utf8_lossy(&answer).lines().next().unwrap_or_default().to_string();
    let code = status.split_whitespace().nth(1);
    if code == Some("407") {
        return Err(Error::Ai(if auth.is_some() {
            "the proxy refused its user name and password".into()
        } else {
            "the proxy wants a user name and password: put the user in its address (http://user@proxy:8080) and \
             save the password in Settings › AI"
                .into()
        }));
    }
    if code != Some("200") {
        return Err(Error::Ai(format!("the proxy refused the tunnel: {status}")));
    }
    Ok(stream)
}

/// Sends one request on an open connection and reads the whole answer, up to [`MAX_REPLY`].
async fn exchange<I>(io: I, request: Request<Full<Bytes>>) -> Result<(u16, Bytes)>
where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let failed = |e: hyper::Error| Error::Ai(e.to_string());
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await.map_err(failed)?;
    tauri::async_runtime::spawn(conn);
    let response = sender.send_request(request).await.map_err(failed)?;
    let status = response.status().as_u16();
    let body = Limited::new(response.into_body(), MAX_REPLY)
        .collect()
        .await
        .map_err(|_| Error::Ai("the answer was too large, or the connection broke".into()))?
        .to_bytes();
    Ok((status, body))
}

/// POSTs `body` to `url` with `headers`, over TLS for https.
async fn post(
    url: &Url,
    proxy: Option<&Proxy>,
    headers: Vec<(header::HeaderName, header::HeaderValue)>,
    body: Vec<u8>,
) -> Result<(u16, Bytes)> {
    let host = url.host_str().ok_or(Error::Invalid("the AI base URL has no host"))?;
    let port = url.port_or_known_default().ok_or(Error::Invalid("the AI base URL has no port"))?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let mut request = Request::post(&url[url::Position::BeforePath..url::Position::AfterQuery])
        .header(header::HOST, &url[url::Position::BeforeHost..url::Position::AfterPort])
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let request = request.body(Full::new(Bytes::from(body))).map_err(|e| Error::Ai(e.to_string()))?;
    // This computer is reached directly; plain http goes nowhere else (endpoint).
    let proxy = proxy.filter(|_| url.scheme() == "https" && !is_local(url));
    let connect = async {
        match proxy {
            Some(p) => tunnel(p, bare, port).await,
            None => Ok(tokio::net::TcpStream::connect((bare, port)).await?),
        }
    };
    let tcp = tokio::time::timeout(crate::db::CONNECT_TIMEOUT, connect)
        .await
        .map_err(|_| Error::ConnectTimeout(crate::db::CONNECT_TIMEOUT.as_secs()))??;
    let send = async {
        if url.scheme() == "https" {
            let tls = tokio_native_tls::TlsConnector::from(native_tls::TlsConnector::new()?);
            let stream = tokio::time::timeout(crate::db::CONNECT_TIMEOUT, tls.connect(bare, tcp))
                .await
                .map_err(|_| Error::ConnectTimeout(crate::db::CONNECT_TIMEOUT.as_secs()))??;
            exchange(stream, request).await
        } else {
            exchange(tcp, request).await
        }
    };
    tokio::time::timeout(ANSWER_TIMEOUT, send)
        .await
        .map_err(|_| Error::Ai(format!("no answer after {} seconds", ANSWER_TIMEOUT.as_secs())))?
}

/// The provider's own error message, cut to a readable length, or the start of the body.
fn error_message(status: u16, body: &[u8]) -> Error {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|v| v.get("error"))
        .and_then(|e| e.get("message").and_then(Value::as_str).or_else(|| e.as_str()))
        .map_or_else(|| String::from_utf8_lossy(body).into_owned(), String::from);
    let short: String = message.chars().take(500).collect();
    Error::Ai(format!("the provider answered {status}: {short}"))
}

/// Sends the conversation and returns the model's answer.
pub async fn chat(chat: &Chat, key: Option<&SecretString>, proxy_password: Option<&SecretString>) -> Result<Reply> {
    if chat.turns.len() > MAX_TURNS {
        return Err(Error::Invalid("the conversation is too long; start a new chat"));
    }
    let size = chat.schema.len()
        + chat
            .turns
            .iter()
            .map(|t| match t {
                Turn::User { text } | Turn::Result { text, .. } => text.len(),
                Turn::Assistant { text, call } => text.len() + call.as_ref().map_or(0, |c| c.sql.len()),
            })
            .sum::<usize>();
    if size > MAX_TEXT {
        return Err(Error::Invalid("the conversation is too long; start a new chat"));
    }
    if chat.model.trim().is_empty() {
        return Err(Error::Invalid("choose a model in Settings › AI"));
    }
    let url = endpoint(chat.provider, &chat.base_url)?;
    let proxy = proxy_url(&chat.proxy)?.map(|url| {
        let auth = (!url.username().is_empty())
            .then(|| basic_auth(url.username(), proxy_password.unwrap_or(&SecretString::from(String::new()))));
        Proxy { url, auth }
    });
    let mut headers = Vec::new();
    let secret = |value: String| {
        let mut v = header::HeaderValue::from_str(&value)
            .map_err(|_| Error::Invalid("the API key has characters a request header can't hold"))?;
        v.set_sensitive(true);
        Ok::<_, Error>(v)
    };
    let body = match chat.provider {
        Provider::OpenAi => {
            // Local servers often need no key.
            if let Some(k) = key {
                headers.push((header::AUTHORIZATION, secret(format!("Bearer {}", k.expose_secret()))?));
            }
            openai_body(chat)
        }
        Provider::Anthropic => {
            let k = key.ok_or(Error::Invalid("add an Anthropic API key in Settings › AI"))?;
            headers.push((header::HeaderName::from_static("x-api-key"), secret(k.expose_secret().to_string())?));
            headers.push((
                header::HeaderName::from_static("anthropic-version"),
                header::HeaderValue::from_static(ANTHROPIC_VERSION),
            ));
            anthropic_body(chat)
        }
    };
    let (status, bytes) = post(&url, proxy.as_ref(), headers, body.to_string().into_bytes()).await?;
    if !(200..300).contains(&status) {
        return Err(error_message(status, &bytes));
    }
    let v: Value = serde_json::from_slice(&bytes).map_err(|_| Error::Ai("the answer wasn't JSON".into()))?;
    let reply = match chat.provider {
        Provider::OpenAi => openai_reply(&v),
        Provider::Anthropic => anthropic_reply(&v),
    }
    .ok_or_else(|| Error::Ai("the answer wasn't in the provider's format".into()))?;
    if reply.text.is_empty() && reply.call.is_none() {
        return Err(Error::Ai("the model sent an empty answer".into()));
    }
    Ok(reply)
}

#[tauri::command]
pub async fn ai_chat(request: Chat) -> Result<Reply> {
    let account = request.provider.account();
    let (key, proxy_password) = tauri::async_runtime::spawn_blocking(move || {
        Ok::<_, Error>((store::stored(Secret::Ai, account)?, store::stored(Secret::Ai, PROXY_ACCOUNT)?))
    })
    .await
    .map_err(|_| Error::Invalid("reading the API key stopped unexpectedly"))??;
    chat(&request, key.as_ref(), proxy_password.as_ref()).await
}

/// Keeps the AI proxy's password in the keychain; an empty one removes it.
#[tauri::command]
pub async fn ai_set_proxy_password(password: SecretString) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || store::update(Secret::Ai, PROXY_ACCOUNT, Some(&password)))
        .await
        .map_err(|_| Error::Invalid("saving the proxy password stopped unexpectedly"))?
}

/// Whether the AI proxy has a password in the keychain.
#[tauri::command]
pub async fn ai_has_proxy_password() -> Result<bool> {
    tauri::async_runtime::spawn_blocking(move || Ok(store::stored(Secret::Ai, PROXY_ACCOUNT)?.is_some()))
        .await
        .map_err(|_| Error::Invalid("reading the proxy password stopped unexpectedly"))?
}

/// Whether the provider has a key in the keychain. The key itself never goes back to the UI.
#[tauri::command]
pub async fn ai_has_key(provider: Provider) -> Result<bool> {
    tauri::async_runtime::spawn_blocking(move || Ok(store::stored(Secret::Ai, provider.account())?.is_some()))
        .await
        .map_err(|_| Error::Invalid("reading the API key stopped unexpectedly"))?
}

/// Keeps the provider's API key in the keychain; an empty key removes it.
#[tauri::command]
pub async fn ai_set_key(provider: Provider, key: SecretString) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || store::update(Secret::Ai, provider.account(), Some(&key)))
        .await
        .map_err(|_| Error::Invalid("saving the API key stopped unexpectedly"))?
}

#[cfg(test)]
mod tests {
    use super::{
        Call, Chat, Provider, Proxy, Reply, Turn, anthropic_body, anthropic_reply, basic_auth, endpoint, openai_body,
        openai_reply, proxy_url, tunnel,
    };
    use secrecy::{ExposeSecret, SecretString};
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn conversation(provider: Provider, base_url: &str) -> Chat {
        Chat {
            provider,
            base_url: base_url.into(),
            model: "m".into(),
            server: "SQLite 3.46".into(),
            schema: "people(id integer, name text)".into(),
            turns: vec![
                Turn::User { text: "how many people?".into() },
                Turn::Assistant {
                    text: "Let me count.".into(),
                    call: Some(Call { id: "c1".into(), sql: "select count(*) from people".into() }),
                },
                Turn::Result { id: "c1".into(), text: "count(*)\n3".into() },
            ],
            proxy: String::new(),
        }
    }

    #[test]
    fn openai_body_should_send_the_call_and_its_result_as_tool_messages() {
        let body = openai_body(&conversation(Provider::OpenAi, "https://x"));
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert!(messages[0]["content"].as_str().unwrap().contains("people(id integer, name text)"));
        assert_eq!(messages[2]["tool_calls"][0]["id"], "c1");
        let arguments: Value =
            serde_json::from_str(messages[2]["tool_calls"][0]["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(arguments, json!({ "sql": "select count(*) from people" }));
        assert_eq!(messages[3], json!({ "role": "tool", "tool_call_id": "c1", "content": "count(*)\n3" }));
    }

    #[test]
    fn anthropic_body_should_send_the_system_prompt_apart_and_the_result_as_a_user_block() {
        let body = anthropic_body(&conversation(Provider::Anthropic, "https://x"));
        assert!(body["system"].as_str().unwrap().contains("SQLite 3.46"));
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["content"][1]["type"], "tool_use");
        assert_eq!(messages[1]["content"][1]["input"], json!({ "sql": "select count(*) from people" }));
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "c1");
    }

    #[test]
    fn openai_reply_should_read_text_and_the_first_tool_call() {
        let v = json!({ "choices": [{ "message": { "content": null, "tool_calls": [
            { "id": "a", "type": "function", "function": { "name": "run_query", "arguments": "{\"sql\":\"select 1\"}" } },
            { "id": "b", "type": "function", "function": { "name": "run_query", "arguments": "{\"sql\":\"select 2\"}" } },
        ] } }] });
        assert_eq!(
            openai_reply(&v),
            Some(Reply { text: String::new(), call: Some(Call { id: "a".into(), sql: "select 1".into() }) })
        );
    }

    #[test]
    fn anthropic_reply_should_join_text_blocks() {
        let v = json!({ "content": [{ "type": "text", "text": "One." }, { "type": "text", "text": "Two." }] });
        assert_eq!(anthropic_reply(&v), Some(Reply { text: "One.\n\nTwo.".into(), call: None }));
    }

    #[test]
    fn endpoint_should_keep_the_base_path_and_query() {
        let url = endpoint(Provider::OpenAi, "https://example.com/openai/v1/?api-version=2").unwrap();
        assert_eq!(url.as_str(), "https://example.com/openai/v1/chat/completions?api-version=2");
        let url = endpoint(Provider::Anthropic, "https://api.anthropic.com").unwrap();
        assert_eq!(url.as_str(), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn endpoint_should_allow_plain_http_only_to_this_computer() {
        assert!(endpoint(Provider::OpenAi, "http://localhost:11434/v1").is_ok());
        assert!(endpoint(Provider::OpenAi, "http://[::1]:8080/v1").is_ok());
        assert!(endpoint(Provider::OpenAi, "http://example.com/v1").is_err());
        assert!(endpoint(Provider::OpenAi, "ftp://localhost/v1").is_err());
    }

    #[test]
    fn endpoint_should_refuse_credentials_in_the_url() {
        assert!(endpoint(Provider::OpenAi, "https://user:key@example.com/v1").is_err());
    }

    #[test]
    fn turn_should_refuse_unknown_fields() {
        let ok: Turn = serde_json::from_str(r#"{"kind":"user","text":"hi"}"#).unwrap();
        assert_eq!(ok, Turn::User { text: "hi".into() });
        assert!(serde_json::from_str::<Turn>(r#"{"kind":"user","text":"hi","extra":1}"#).is_err());
    }

    /// A one-shot HTTP server on a free port: answers one request with `status` and `body`, and hands back the
    /// request it read.
    async fn fake_server(status: u16, body: &'static str) -> (u16, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 8192];
            // Reads until the headers and the whole Content-Length body are in.
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                seen.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&seen).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text
                        .lines()
                        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length: ").map(String::from))
                        .and_then(|n| n.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if seen.len() >= end + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let answer = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(answer.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&seen).to_string()
        });
        (port, task)
    }

    #[test]
    fn chat_should_post_to_an_openai_compatible_server_with_the_key() {
        tauri::async_runtime::block_on(async {
            let (port, seen) =
                fake_server(200, r#"{"choices":[{"message":{"role":"assistant","content":"Three people."}}]}"#).await;
            let chat = conversation(Provider::OpenAi, &format!("http://127.0.0.1:{port}/v1"));
            let key = SecretString::from("sk-test");
            let reply = super::chat(&chat, Some(&key), None).await.unwrap();
            assert_eq!(reply, Reply { text: "Three people.".into(), call: None });
            let request = seen.await.unwrap();
            assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1"), "{request}");
            assert!(request.to_ascii_lowercase().contains("authorization: bearer sk-test"));
        });
    }

    #[test]
    fn chat_should_report_the_providers_error_message() {
        tauri::async_runtime::block_on(async {
            let (port, _seen) =
                fake_server(401, r#"{"type":"error","error":{"type":"authentication_error","message":"bad key"}}"#)
                    .await;
            let chat = conversation(Provider::Anthropic, &format!("http://localhost:{port}"));
            let key = SecretString::from("k");
            let e = super::chat(&chat, Some(&key), None).await.unwrap_err().to_string();
            assert!(e.contains("401") && e.contains("bad key"), "{e}");
        });
    }

    #[test]
    fn chat_should_want_a_key_for_anthropic() {
        tauri::async_runtime::block_on(async {
            let chat = conversation(Provider::Anthropic, "https://api.anthropic.com");
            assert!(super::chat(&chat, None, None).await.is_err());
        });
    }

    #[test]
    fn proxy_url_should_take_only_plain_http_and_no_password() {
        assert!(proxy_url("").unwrap().is_none());
        assert!(proxy_url("http://proxy:8080").unwrap().is_some());
        assert!(proxy_url("http://alice@proxy:8080").unwrap().is_some());
        assert!(proxy_url("https://proxy:8080").is_err());
        assert!(proxy_url("socks5://proxy:1080").is_err());
        assert!(proxy_url("http://u:p@proxy:8080").is_err());
    }

    #[test]
    fn basic_auth_should_decode_the_user_from_the_address() {
        let auth = basic_auth("al%40ice", &SecretString::from("pa:ss".to_string()));
        assert_eq!(auth.expose_secret(), "Basic YWxAaWNlOnBhOnNz"); // "al@ice:pa:ss"
    }

    fn open_proxy(port: u16) -> Proxy {
        Proxy { url: proxy_url(&format!("http://127.0.0.1:{port}")).unwrap().unwrap(), auth: None }
    }

    /// A one-shot proxy: reads the CONNECT request up to its blank line, answers `status`, hands back what it read.
    async fn fake_proxy(status: &'static str) -> (u16, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 1024];
            while !seen.ends_with(b"\r\n\r\n") {
                let n = socket.read(&mut buf).await.unwrap();
                seen.extend_from_slice(&buf[..n]);
            }
            socket.write_all(format!("{status}\r\n\r\n").as_bytes()).await.unwrap();
            String::from_utf8_lossy(&seen).to_string()
        });
        (port, task)
    }

    #[test]
    fn tunnel_should_connect_through_the_proxy() {
        tauri::async_runtime::block_on(async {
            let (port, seen) = fake_proxy("HTTP/1.1 200 Connection established").await;
            tunnel(&open_proxy(port), "api.example.com", 443).await.unwrap();
            let seen = seen.await.unwrap();
            assert!(seen.starts_with("CONNECT api.example.com:443 HTTP/1.1\r\n"));
            assert!(!seen.contains("Proxy-Authorization"));
        });
    }

    #[test]
    fn tunnel_should_send_the_proxy_credentials() {
        tauri::async_runtime::block_on(async {
            let (port, seen) = fake_proxy("HTTP/1.1 200 Connection established").await;
            let proxy = Proxy { auth: Some(SecretString::from("Basic eDp5".to_string())), ..open_proxy(port) };
            tunnel(&proxy, "api.example.com", 443).await.unwrap();
            assert!(seen.await.unwrap().contains("\r\nProxy-Authorization: Basic eDp5\r\n"));
        });
    }

    #[test]
    fn tunnel_should_report_a_refusal() {
        tauri::async_runtime::block_on(async {
            let (port, _seen) = fake_proxy("HTTP/1.1 403 Forbidden").await;
            let e = tunnel(&open_proxy(port), "api.example.com", 443).await.unwrap_err().to_string();
            assert!(e.contains("403"), "{e}");
        });
    }

    #[test]
    fn tunnel_should_ask_for_credentials_when_the_proxy_wants_them() {
        tauri::async_runtime::block_on(async {
            let (port, _seen) = fake_proxy("HTTP/1.1 407 Proxy Authentication Required").await;
            let e = tunnel(&open_proxy(port), "api.example.com", 443).await.unwrap_err().to_string();
            assert!(e.contains("wants a user name and password"), "{e}");
        });
    }
}

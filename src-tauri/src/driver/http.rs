//! A small HTTP/1.1 client for the engines whose servers answer JSON over HTTP (libSQL, Cloudflare D1). Each request
//! is a connection of its own, so a dropped request is a closed socket and nothing is left half read.

use crate::error::{Error, Result};
use crate::tls::{Tls, TlsMode};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Request, header};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

/// Where requests go: the server's name (for TLS and the `Host` header), and where the socket connects, which is a
/// local tunnel port when the connection goes over SSH.
pub struct Endpoint {
    host: String,
    port: u16,
    dial: (String, u16),
    tls: Option<tokio_native_tls::TlsConnector>,
    /// The engine's name, for errors.
    engine: &'static str,
}

/// The TLS connector for an HTTPS API: unlike the database protocols, Prefer (the default) checks the certificate
/// and its host name, since these servers sit on the internet and take a bearer token. Disable is plain HTTP;
/// Require encrypts without checking (a self-hosted server with its own certificate).
pub fn https_connector(tls: &Tls) -> Result<Option<tokio_native_tls::TlsConnector>> {
    let checked = match tls.mode {
        TlsMode::Disable => return Ok(None),
        TlsMode::Prefer => Tls { mode: TlsMode::VerifyFull, ..tls.clone() },
        _ => tls.clone(),
    };
    Ok(Some(tokio_native_tls::TlsConnector::from(checked.connector()?)))
}

impl Endpoint {
    pub fn new(
        engine: &'static str,
        host: &str,
        port: u16,
        tls: Option<tokio_native_tls::TlsConnector>,
        tunnel_port: Option<u16>,
    ) -> Endpoint {
        let host = host.trim_matches(['[', ']']).to_string();
        let dial = match tunnel_port {
            Some(p) => ("127.0.0.1".to_string(), p),
            None => (host.clone(), port),
        };
        Endpoint { host, port, dial, tls, engine }
    }

    pub fn error(&self, message: impl Into<String>) -> Error {
        Error::Server { engine: self.engine, message: message.into() }
    }

    /// The `Host` header's value, which a signature covers.
    pub fn host_header(&self) -> String {
        let default_port = if self.tls.is_some() { 443 } else { 80 };
        if self.port == default_port { self.host.clone() } else { format!("{}:{}", self.host, self.port) }
    }

    /// POSTs `body` (JSON unless `headers` name another content type) to `path` and returns the status and up to
    /// `max` bytes of the answer.
    pub async fn post(
        &self,
        path: &str,
        headers: &[(header::HeaderName, header::HeaderValue)],
        body: Vec<u8>,
        max: usize,
    ) -> Result<(u16, Bytes)> {
        let mut request = Request::post(path)
            .header(header::HOST, self.host_header())
            .header(header::ACCEPT, "application/json")
            .header(header::CONNECTION, "close");
        if !headers.iter().any(|(name, _)| name == header::CONTENT_TYPE) {
            request = request.header(header::CONTENT_TYPE, "application/json");
        }
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let request = request.body(Full::new(Bytes::from(body))).map_err(|e| self.error(e.to_string()))?;
        let (host, port) = &self.dial;
        let timeout = || Error::ConnectTimeout(crate::db::CONNECT_TIMEOUT.as_secs());
        let tcp = tokio::time::timeout(crate::db::CONNECT_TIMEOUT, TcpStream::connect((host.as_str(), *port)))
            .await
            .map_err(|_| timeout())??;
        tcp.set_nodelay(true)?;
        match &self.tls {
            Some(tls) => {
                let stream = tokio::time::timeout(crate::db::CONNECT_TIMEOUT, tls.connect(&self.host, tcp))
                    .await
                    .map_err(|_| timeout())??;
                self.exchange(stream, request, max).await
            }
            None => self.exchange(tcp, request, max).await,
        }
    }

    async fn exchange<I>(&self, io: I, request: Request<Full<Bytes>>, max: usize) -> Result<(u16, Bytes)>
    where
        I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let failed = |e: hyper::Error| self.error(e.to_string());
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await.map_err(failed)?;
        tauri::async_runtime::spawn(conn);
        let response = sender.send_request(request).await.map_err(failed)?;
        let status = response.status().as_u16();
        let body = Limited::new(response.into_body(), max)
            .collect()
            .await
            .map_err(|_| self.error("the answer was too large, or the connection broke"))?
            .to_bytes();
        Ok((status, body))
    }
}

/// A bearer token as a header value, kept out of logs and debug output.
pub fn bearer(token: &str) -> Result<header::HeaderValue> {
    let mut v = header::HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| Error::Invalid("the token has characters a request header can't hold"))?;
    v.set_sensitive(true);
    Ok(v)
}

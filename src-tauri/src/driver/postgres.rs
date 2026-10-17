//! `PostgreSQL`, and the servers that speak its protocol (`CockroachDB`), through tokio-postgres.

use super::{Driver, Results, RowSink, Stopper};
use crate::db::{Dialect, Options};
use crate::error::{Error, Result};
use crate::tls::Tls;
use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use std::any::Any;

pub struct Postgres(tokio_postgres::Client);

/// A Postgres parameter sent as text, which the server parses as the target column's type, exactly as it
/// would a quoted literal. Lets one code path bind values for every column type.
#[derive(Debug)]
struct PgText<'a>(Option<&'a str>);

impl tokio_postgres::types::ToSql for PgText<'_> {
    fn to_sql(
        &self,
        _: &tokio_postgres::types::Type,
        out: &mut bytes::BytesMut,
    ) -> std::result::Result<tokio_postgres::types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
        use tokio_postgres::types::IsNull;
        Ok(match self.0 {
            Some(s) => {
                out.extend_from_slice(s.as_bytes());
                IsNull::No
            }
            None => IsNull::Yes,
        })
    }

    fn accepts(_: &tokio_postgres::types::Type) -> bool {
        true
    }

    fn encode_format(&self, _: &tokio_postgres::types::Type) -> tokio_postgres::types::Format {
        tokio_postgres::types::Format::Text
    }

    tokio_postgres::types::to_sql_checked!();
}

/// The server's name and version from `select version()` on `CockroachDB`, e.g. "`CockroachDB` 25.1.0" from
/// "`CockroachDB` CCL v25.1.0 (aarch64-unknown-linux-gnu, …)"; None for any other server.
pub fn cockroach_version(version: &str) -> Option<String> {
    let rest = version.strip_prefix("CockroachDB ")?;
    let v = rest.split_whitespace().find_map(|w| w.strip_prefix('v')).unwrap_or_default();
    Some(format!("CockroachDB {v}"))
}

/// The config for dialling `url` through a local tunnel port. `host` stays the real name, so TLS is checked against
/// it, while `hostaddr` sends the connection to 127.0.0.1.
fn tunnelled_config(url: &str, tunnel_port: u16) -> Result<tokio_postgres::Config> {
    let mut u = url::Url::parse(url)?;
    // The URL's port is replaced, not added to: tokio-postgres fills in 5432 for a URL without one, so adding the
    // tunnel's port with `Config::port` would leave two ports for one host, which it refuses.
    let _ = u.set_port(Some(tunnel_port)); // Err only for URLs that can't have a port, which parse() then reports
    let mut cfg: tokio_postgres::Config = u.as_str().parse()?;
    cfg.hostaddr(std::net::Ipv4Addr::LOCALHOST.into());
    Ok(cfg)
}

impl Postgres {
    pub async fn open(
        url: &str,
        password: Option<&SecretString>,
        tls: &Tls,
        options: &Options,
        tunnel_port: Option<u16>,
    ) -> Result<Postgres> {
        let mut cfg: tokio_postgres::Config = match tunnel_port {
            Some(port) => tunnelled_config(url, port)?,
            None => url.parse()?,
        };
        if let Some(password) = password {
            cfg.password(password.expose_secret());
        }
        let (mode, connector) = tls.postgres()?;
        cfg.ssl_mode(mode);
        if options.direct_tls {
            cfg.ssl_negotiation(tokio_postgres::config::SslNegotiation::Direct);
        }
        if options.no_channel_binding {
            cfg.channel_binding(tokio_postgres::config::ChannelBinding::Disable);
        }
        let (client, connection) = cfg.connect(connector).await?;
        tauri::async_runtime::spawn(connection); // drives the socket; ends when the client is dropped
        Ok(Postgres(client))
    }
}

/// `params` as the text parameters the driver wants.
fn texts<'a>(params: &[Option<&'a str>]) -> Vec<PgText<'a>> {
    params.iter().copied().map(PgText).collect()
}

#[async_trait]
impl Driver for Postgres {
    fn dialect(&self) -> Dialect {
        Dialect::Pg
    }

    async fn batch(&mut self, sql: &str) -> Result<()> {
        Ok(self.0.batch_execute(sql).await?)
    }

    // The simple-query (text) protocol renders every Postgres type as text, so no per-type decoding.
    async fn run(&mut self, sql: &str, out: &mut Results) -> Result<()> {
        use futures_util::StreamExt;
        use tokio_postgres::SimpleQueryMessage as M;
        let stream = self.0.simple_query_raw(sql).await?;
        futures_util::pin_mut!(stream);
        while let Some(msg) = stream.next().await {
            match msg? {
                M::RowDescription(cols) => out.begin(cols.iter().map(|c| c.name().to_string()).collect()),
                M::Row(row) if out.keep() => {
                    out.push((0..row.len()).map(|i| row.get(i).map(String::from)).collect());
                }
                M::CommandComplete(n) => out.end(n),
                _ => {}
            }
        }
        Ok(())
    }

    async fn execute_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<u64> {
        let params = texts(params);
        let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        Ok(self.0.execute(sql, &refs).await?)
    }

    async fn fetch_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Vec<Vec<Option<String>>>> {
        let params = texts(params);
        let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        self.0
            .query(sql, &refs)
            .await?
            .iter()
            .map(|row| (0..row.len()).map(|i| row.try_get(i)).collect::<std::result::Result<Vec<_>, _>>())
            .collect::<std::result::Result<_, _>>()
            .map_err(Error::from)
    }

    async fn stream(&mut self, sql: &str, params: &[Option<&str>], sink: &mut dyn RowSink) -> Result<()> {
        use futures_util::StreamExt;
        if params.is_empty() {
            use tokio_postgres::SimpleQueryMessage as M;
            let stream = self.0.simple_query_raw(sql).await?;
            futures_util::pin_mut!(stream);
            let mut described = false;
            while let Some(msg) = stream.next().await {
                match msg? {
                    M::RowDescription(_) if described => {
                        return Err(Error::Invalid("export one statement at a time"));
                    }
                    M::RowDescription(cols) => {
                        described = true;
                        sink.columns(cols.iter().map(|c| c.name().to_string()).collect())?;
                    }
                    M::Row(row) => sink.row((0..row.len()).map(|i| row.get(i).map(String::from)).collect())?,
                    _ => {}
                }
            }
            return Ok(());
        }
        let st = self.0.prepare(sql).await?;
        sink.columns(st.columns().iter().map(|c| c.name().to_string()).collect())?;
        let stream = self.0.query_raw(&st, texts(params)).await?;
        futures_util::pin_mut!(stream);
        while let Some(row) = stream.next().await {
            let row = row?;
            sink.row((0..row.len()).map(|i| row.try_get(i)).collect::<std::result::Result<_, _>>()?)?;
        }
        Ok(())
    }

    async fn ping(&mut self) -> Result<()> {
        self.0.simple_query("").await?;
        Ok(())
    }

    fn stopper(&self) -> Stopper {
        Stopper::Pg(self.0.cancel_token())
    }

    fn lost_by(&self, _err: &Error) -> bool {
        self.0.is_closed()
    }

    fn read_only_bounds(&self) -> (&'static str, &'static str) {
        ("begin read only", "rollback")
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use tokio_postgres::config::Host;

    #[test]
    fn cockroach_version_should_name_cockroachdb_and_leave_postgres_alone() {
        let crdb = "CockroachDB CCL v25.1.0 (aarch64-unknown-linux-gnu, built 2025/02/17 13:21:41, go1.22.8 X:nocoverageredesign)";
        assert_eq!(super::cockroach_version(crdb).as_deref(), Some("CockroachDB 25.1.0"));
        assert_eq!(super::cockroach_version("PostgreSQL 16.15 on aarch64-unknown-linux-musl"), None);
    }

    #[test]
    fn tunnelled_config_should_have_one_port_whether_or_not_the_url_names_one() {
        for url in ["postgresql://u@db.internal/app", "postgresql://u@db.internal:6543/app"] {
            let cfg = super::tunnelled_config(url, 40_001).unwrap();
            assert_eq!(cfg.get_ports(), [40_001], "{url}");
            assert_eq!(cfg.get_hostaddrs(), [std::net::Ipv4Addr::LOCALHOST]);
            assert!(matches!(cfg.get_hosts(), [Host::Tcp(h)] if h == "db.internal"), "{url}: TLS checks the real name");
        }
    }
}

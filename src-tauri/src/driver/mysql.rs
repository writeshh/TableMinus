//! MySQL and MariaDB through `mysql_async`.

use super::{Driver, Results, RowSink, Stopper};
use crate::db::{Dialect, Options};
use crate::error::{Error, Result};
use crate::tls::{Tls, TlsMode};
use async_trait::async_trait;
use mysql_async::prelude::Queryable;
use secrecy::{ExposeSecret, SecretString};
use std::any::Any;

pub struct Mysql(mysql_async::Conn);

/// The file a `LOAD DATA [LOW_PRIORITY | CONCURRENT] LOCAL INFILE '…'` statement names, when `sql` starts with one.
/// The server, not the statement, says which file it wants, so this is the only file the client will send.
fn local_infile(sql: &str) -> Option<String> {
    fn keyword<'a>(s: &'a str, word: &str) -> Option<&'a str> {
        let (head, rest) = (s.get(..word.len())?, s.get(word.len()..)?);
        let ends = !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_');
        (head.eq_ignore_ascii_case(word) && ends).then(|| rest.trim_start())
    }
    let s = keyword(keyword(sql.trim_start(), "load")?, "data")?;
    let s = keyword(s, "low_priority").or_else(|| keyword(s, "concurrent")).unwrap_or(s);
    let s = keyword(keyword(s, "local")?, "infile")?;
    // A MySQL string literal: backslash escapes, and a doubled quote stands for one.
    let mut chars = s.chars().peekable();
    let quote = chars.next().filter(|q| matches!(q, '\'' | '"'))?;
    let mut path = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => path.push(match chars.next()? {
                'n' => '\n',
                't' => '\t',
                '0' => '\0',
                other => other,
            }),
            q if q == quote && chars.peek() == Some(&quote) => {
                chars.next();
                path.push(quote);
            }
            q if q == quote => return Some(path),
            other => path.push(other),
        }
    }
    None
}

/// Streams `file` to the server in 64 KiB pieces, for one LOAD DATA LOCAL request.
fn infile_data(
    file: std::fs::File,
) -> impl Future<Output = mysql_async::Result<mysql_async::InfileData>> + Send + Sync {
    let file = tokio::fs::File::from_std(file);
    async move {
        use tokio::io::AsyncReadExt;
        let chunks = futures_util::stream::unfold(Some(file), |file| async move {
            let mut file = file?;
            let mut buf = vec![0; 64 * 1024];
            match file.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    Some((Ok(bytes::Bytes::from(buf)), Some(file)))
                }
                Err(e) => Some((Err(e), None)), // the error ends the stream
            }
        });
        Ok(Box::pin(chunks) as mysql_async::InfileData)
    }
}

/// A MySQL row as text cells, the way the text protocol would show them. Prepared statements return typed
/// values, so dates and times are formatted from the column's type and fractional-second digits.
fn row_cells(row: mysql_async::Row) -> Vec<Option<String>> {
    let columns = row.columns();
    row.unwrap().into_iter().zip(columns.iter()).map(|(v, col)| cell(v, col)).collect()
}

fn cell(value: mysql_async::Value, col: &mysql_async::Column) -> Option<String> {
    use mysql_async::Value as V;
    use mysql_async::consts::ColumnType;
    let frac = |micros: u32| match col.decimals() {
        digits @ 1..=6 => format!(".{}", &format!("{micros:06}")[..usize::from(digits)]),
        _ => String::new(),
    };
    let date_only = matches!(col.column_type(), ColumnType::MYSQL_TYPE_DATE | ColumnType::MYSQL_TYPE_NEWDATE);
    Some(match value {
        V::NULL => return None,
        V::Bytes(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        V::Int(n) => n.to_string(),
        V::UInt(n) => n.to_string(),
        V::Float(n) => n.to_string(),
        V::Double(n) => n.to_string(),
        V::Date(year, month, day, ..) if date_only => format!("{year:04}-{month:02}-{day:02}"),
        V::Date(year, month, day, hour, minute, second, micros) => {
            format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}{}", frac(micros))
        }
        V::Time(negative, days, hour, minute, second, micros) => {
            let hours = days * 24 + u32::from(hour);
            format!("{}{hours:02}:{minute:02}:{second:02}{}", if negative { "-" } else { "" }, frac(micros))
        }
    })
}

/// Feeds a MySQL result to `sink` row by row.
async fn stream_result<P: mysql_async::prelude::Protocol>(
    mut res: mysql_async::QueryResult<'_, '_, P>,
    sink: &mut dyn RowSink,
) -> Result<()> {
    sink.columns(res.columns_ref().iter().map(|c| c.name_str().into_owned()).collect())?;
    while let Some(row) = res.next().await? {
        sink.row(row_cells(row))?;
    }
    Ok(())
}

fn values(params: &[Option<&str>]) -> Vec<mysql_async::Value> {
    params.iter().copied().map(mysql_async::Value::from).collect()
}

impl Mysql {
    pub async fn open(
        url: &str,
        password: Option<&SecretString>,
        tls: &Tls,
        options: &Options,
        tunnel_port: Option<u16>,
    ) -> Result<Mysql> {
        let opts = mysql_async::Opts::from_url(url).map_err(mysql_async::Error::from)?;
        let host = opts.ip_or_hostname().to_string();
        let mut builder = mysql_async::OptsBuilder::from_opts(opts)
            .enable_cleartext_plugin(options.cleartext_auth)
            .prefer_socket(false);
        if let Some(password) = password {
            builder = builder.pass(Some(password.expose_secret()));
        }
        if let Some(port) = tunnel_port {
            builder = builder.ip_or_hostname("127.0.0.1").tcp_port(port);
        } else if !options.socket.is_empty() {
            builder = builder.socket(Some(options.socket.as_str()));
        }
        let on_socket = tunnel_port.is_none() && !options.socket.is_empty();
        // The name TLS checks: an explicit SNI, else the real host when the address dialled is a tunnel's.
        let verify_as =
            if options.sni.is_empty() { tunnel_port.map(|_| host.as_str()) } else { Some(options.sni.as_str()) };
        // A Unix socket never leaves the machine (MySQL counts it as a secure transport), and the driver's TLS
        // handshake over one fails with "Packets out of sync", so the TLS setting doesn't apply there.
        let ssl = if on_socket { None } else { tls.mysql(verify_as)? };
        let conn = match mysql_async::Conn::new(builder.clone().ssl_opts(ssl)).await {
            // Prefer: a server without TLS gets a plain connection, as libmysqlclient's PREFERRED does.
            Err(mysql_async::Error::Driver(mysql_async::DriverError::NoClientSslFlagFromServer))
                if tls.mode == TlsMode::Prefer =>
            {
                mysql_async::Conn::new(builder.ssl_opts(None)).await?
            }
            other => other?,
        };
        Ok(Mysql(conn))
    }
}

#[async_trait]
impl Driver for Mysql {
    fn dialect(&self) -> Dialect {
        Dialect::My
    }

    async fn batch(&mut self, sql: &str) -> Result<()> {
        Ok(self.0.query_drop(sql).await?)
    }

    async fn run(&mut self, sql: &str, out: &mut Results) -> Result<()> {
        // With no handler the driver refuses LOCAL INFILE requests; this one answers once, with the file the
        // statement names, whatever the server asked for.
        if let Some(path) = local_infile(sql) {
            let file = std::fs::File::open(&path).map_err(|source| Error::File { path, source })?;
            self.0.set_infile_handler(infile_data(file));
        }
        let mut res = self.0.query_iter(sql).await?;
        // One pass per result set; reading past a set's last row moves on to the next one.
        loop {
            let columns: Vec<String> = res.columns_ref().iter().map(|c| c.name_str().into_owned()).collect();
            // Read now: for a statement without rows it's this one's count, and later it may be the next one's.
            let affected = if columns.is_empty() { res.affected_rows() } else { 0 };
            out.begin(columns);
            while let Some(row) = res.next().await? {
                if out.keep() {
                    out.push(row_cells(row));
                }
            }
            out.end(affected);
            if res.is_empty() {
                break;
            }
        }
        Ok(())
    }

    async fn execute_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<u64> {
        self.0.exec_drop(sql, values(params)).await?;
        Ok(self.0.affected_rows())
    }

    async fn fetch_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Vec<Vec<Option<String>>>> {
        let rows: Vec<mysql_async::Row> = self.0.exec(sql, values(params)).await?;
        Ok(rows.into_iter().map(row_cells).collect())
    }

    async fn stream(&mut self, sql: &str, params: &[Option<&str>], sink: &mut dyn RowSink) -> Result<()> {
        if params.is_empty() {
            // The text protocol: some statements (SHOW …) can't be prepared.
            stream_result(self.0.query_iter(sql).await?, sink).await
        } else {
            stream_result(self.0.exec_iter(sql, values(params)).await?, sink).await
        }
    }

    async fn ping(&mut self) -> Result<()> {
        Ok(self.0.ping().await?)
    }

    fn stopper(&self) -> Stopper {
        Stopper::My(self.0.id())
    }

    fn lost_by(&self, err: &Error) -> bool {
        match err {
            Error::MySql(mysql_async::Error::Io(_)) => true,
            Error::MySql(mysql_async::Error::Driver(d)) => {
                matches!(d, mysql_async::DriverError::ConnectionClosed)
            }
            _ => false,
        }
    }

    fn read_only_bounds(&self) -> (&'static str, &'static str) {
        ("start transaction read only", "rollback")
    }

    fn param(&self, _n: usize) -> String {
        "?".into()
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::local_infile;

    #[test]
    fn local_infile_should_find_the_file_a_load_data_local_statement_names() {
        assert_eq!(local_infile("LOAD DATA LOCAL INFILE '/tmp/a.csv' INTO TABLE t").as_deref(), Some("/tmp/a.csv"));
        assert_eq!(local_infile("  load data concurrent local infile \"x\" into table t").as_deref(), Some("x"));
        assert_eq!(local_infile(r"load data local infile 'it''s\\a\'b' into table t").as_deref(), Some(r"it's\a'b"));
    }

    #[test]
    fn local_infile_should_ignore_other_statements() {
        assert_eq!(local_infile("load data infile '/srv/a.csv' into table t"), None, "server-side file");
        assert_eq!(local_infile("select 'load data local infile'"), None);
        assert_eq!(local_infile("load data local infile '/tmp/unterminated"), None);
        assert_eq!(local_infile("load database local infile 'x'"), None);
    }
}

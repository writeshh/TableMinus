//! `SQL Server` through tiberius (the TDS protocol). Values come back typed, and [`cell`] writes each type as text.
//!
//! A script runs batch by batch, split at `GO` lines as SSMS does, so variables and temporary state live as long as
//! their batch does. SQL Server has no per-statement row counts in a result stream, so a batch of `INSERT`, `UPDATE`
//! or `DELETE` ends with `SELECT @@ROWCOUNT`, which is read back as the batch's affected rows.

use super::{Driver, Results, RowSink, Stopper, first_word, only_comments};
use crate::db::{Dialect, Options};
use crate::error::{Error, Result};
use crate::tls::{Tls, TlsMode};
use async_trait::async_trait;
use futures_util::TryStreamExt;
use secrecy::{ExposeSecret, SecretString};
use std::any::Any;
use std::sync::Arc;
use tiberius::{AuthMethod, Client, ColumnData, Config, EncryptionLevel, FromSql, QueryItem, ToSql};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

type Tds = Client<Compat<TcpStream>>;

/// The column [`run`] adds to read a batch's row count back.
const ROWS: &str = "__tableminus_rows";

pub struct SqlServer {
    client: Tds,
    /// Woken by the stopper: the running call drops its stream and sends the server an attention signal.
    cancel: Arc<Notify>,
}

impl SqlServer {
    pub async fn open(
        url: &str,
        password: Option<&SecretString>,
        tls: &Tls,
        _options: &Options,
        tunnel_port: Option<u16>,
    ) -> Result<SqlServer> {
        let u = url::Url::parse(url)?;
        let host = u.host_str().unwrap_or("localhost").to_string();
        let instance =
            u.query_pairs().find(|(k, _)| k == "instance").map(|(_, v)| v.into_owned()).filter(|i| !i.is_empty());
        // A named instance listens where SQL Browser says, unless the connection gives the port.
        let port = match (u.port(), &instance, tunnel_port) {
            (Some(p), _, _) => p,
            (None, Some(_), Some(_)) => {
                return Err(Error::Invalid(
                    "through SSH, a named instance needs its port: SQL Browser answers over UDP",
                ));
            }
            (None, Some(name), None) => instance_port(&host, name).await?,
            (None, None, _) => 1433,
        };
        let decode = |s: &str| percent_decode(s);
        let mut config = Config::new();
        config.host(&host);
        config.port(port);
        let database = decode(u.path().trim_start_matches('/'));
        if !database.is_empty() {
            config.database(database);
        }
        config.application_name("TableMinus");
        let secret = password.map(ExposeSecret::expose_secret).unwrap_or_default();
        config.authentication(AuthMethod::sql_server(decode(u.username()), secret));
        apply_tls(&mut config, tls)?;
        // Through a tunnel the socket goes to the local port, while TLS still checks the real host name.
        let tcp = match tunnel_port {
            Some(p) => TcpStream::connect(("127.0.0.1", p)).await?,
            None => TcpStream::connect((host.as_str(), port)).await?,
        };
        tcp.set_nodelay(true)?;
        // Boxed: tiberius's connect future is big.
        let client = Box::pin(Client::connect(config, tcp.compat_write())).await?;
        Ok(SqlServer { client, cancel: Arc::new(Notify::new()) })
    }
}

/// Asks the SQL Browser service on `host` (UDP 1434) which TCP port instance `name` listens on.
async fn instance_port(host: &str, name: &str) -> Result<u16> {
    let socket = tokio::net::UdpSocket::bind(if host.contains(':') { "[::]:0" } else { "0.0.0.0:0" }).await?;
    socket.connect((host, 1434)).await?;
    // CLNT_UCAST_INST: 0x04, then the instance's name.
    let mut ask = vec![4u8];
    ask.extend_from_slice(name.as_bytes());
    socket.send(&ask).await?;
    let mut answer = vec![0u8; 4096];
    let n = tokio::time::timeout(crate::db::CONNECT_TIMEOUT, socket.recv(&mut answer))
        .await
        .map_err(|_| Error::Invalid("SQL Browser didn't answer on UDP 1434; enter the instance's port instead"))??;
    instance_port_in(answer.get(..n).unwrap_or_default())
        .ok_or(Error::Invalid("SQL Browser doesn't know that instance, or it doesn't listen on TCP"))
}

/// The TCP port in SQL Browser's answer: 0x05, a two-byte length, then `ServerName;…;InstanceName;…;tcp;1433;;`.
fn instance_port_in(answer: &[u8]) -> Option<u16> {
    let text = String::from_utf8_lossy(answer.get(3..)?.split(|b| *b == 0).next()?).into_owned();
    let fields: Vec<&str> = text.split(';').collect();
    let at = fields.iter().position(|f| f.eq_ignore_ascii_case("tcp"))?;
    fields.get(at + 1)?.parse().ok()
}

fn percent_decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("v={}", s.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// The TLS settings as tiberius has them: Prefer encrypts when the server can, Require insists, and the verifying
/// modes check the certificate (tiberius always checks the host name too when it checks at all).
fn apply_tls(config: &mut Config, tls: &Tls) -> Result<()> {
    config.encryption(match tls.mode {
        TlsMode::Disable => EncryptionLevel::NotSupported,
        TlsMode::Prefer => EncryptionLevel::On,
        TlsMode::Require | TlsMode::VerifyCa | TlsMode::VerifyFull => EncryptionLevel::Required,
    });
    let verify = matches!(tls.mode, TlsMode::VerifyCa | TlsMode::VerifyFull) || !tls.ca.is_empty();
    if !verify {
        config.trust_cert();
    } else if !tls.ca.is_empty() {
        config.trust_cert_ca(crate::ssh::expand_home(&tls.ca)?.display().to_string());
    }
    if !tls.cert.is_empty() && !tls.key.is_empty() {
        config.client_certificate(crate::ssh::expand_home(&tls.cert)?, crate::ssh::expand_home(&tls.key)?);
    }
    Ok(())
}

/// Splits a script into batches at the lines that say only `GO`, leaving out the empty ones.
pub fn split_batches(sql: &str) -> Vec<String> {
    let mut batches = vec![String::new()];
    for line in sql.split_inclusive('\n') {
        if line.trim().eq_ignore_ascii_case("go") {
            batches.push(String::new());
        } else if let Some(last) = batches.last_mut() {
            last.push_str(line);
        }
    }
    batches.retain(|b| !only_comments(b));
    batches
}

/// A value as text: numbers and strings as they are, dates and times in the usual `YYYY-MM-DD HH:MM:SS.fff` form,
/// binary as `0x…` hex, bits as `1` and `0`. `None` for NULL.
pub fn cell(data: ColumnData<'static>) -> Option<String> {
    use ColumnData as C;
    use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};
    use std::fmt::Write as _;
    Some(match data {
        C::U8(v) => v?.to_string(),
        C::I16(v) => v?.to_string(),
        C::I32(v) => v?.to_string(),
        C::I64(v) => v?.to_string(),
        C::F32(v) => v?.to_string(),
        C::F64(v) => v?.to_string(),
        C::Bit(v) => (if v? { "1" } else { "0" }).into(),
        C::String(v) => v?.into_owned(),
        C::Guid(v) => v?.to_string().to_uppercase(),
        C::Numeric(v) => v?.to_string(),
        C::Xml(v) => v?.into_owned().into_string(),
        C::Binary(v) => v?.iter().fold(String::from("0x"), |mut s, b| {
            let _ = write!(s, "{b:02X}"); // writing to a String can't fail
            s
        }),
        // `datetime` counts in 1/300 s, which a nanosecond format would print as .123333333.
        d @ C::DateTime(_) => NaiveDateTime::from_sql(&d).ok()??.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
        d @ C::SmallDateTime(_) => NaiveDateTime::from_sql(&d).ok()??.format("%Y-%m-%d %H:%M:%S").to_string(),
        d @ C::DateTime2(_) => NaiveDateTime::from_sql(&d).ok()??.format("%Y-%m-%d %H:%M:%S%.f").to_string(),
        d @ C::Date(_) => NaiveDate::from_sql(&d).ok()??.format("%Y-%m-%d").to_string(),
        d @ C::Time(_) => NaiveTime::from_sql(&d).ok()??.format("%H:%M:%S%.f").to_string(),
        d @ C::DateTimeOffset(_) => {
            DateTime::<FixedOffset>::from_sql(&d).ok()??.format("%Y-%m-%d %H:%M:%S%.f %:z").to_string()
        }
    })
}

fn texts(row: tiberius::Row) -> Vec<Option<String>> {
    row.into_iter().map(cell).collect()
}

fn lost(e: &tiberius::error::Error) -> bool {
    matches!(e, tiberius::error::Error::Io { .. })
}

/// Runs one batch, handing every result set to `out`.
async fn run_batch(client: &mut Tds, sql: &str, out: &mut Results) -> Result<()> {
    let counted = matches!(first_word(sql).as_str(), "insert" | "update" | "delete" | "merge");
    let text = if counted { format!("{sql}\n;SELECT @@ROWCOUNT AS [{ROWS}]") } else { sql.to_string() };
    let mut stream = client.simple_query(text).await?;
    let (mut sets, mut open, mut counting, mut affected) = (0, false, false, 0);
    while let Some(item) = stream.try_next().await? {
        match item {
            QueryItem::Metadata(meta) => {
                if open {
                    out.end(0);
                }
                let columns: Vec<String> = meta.columns().iter().map(|c| c.name().to_string()).collect();
                counting = counted && columns.len() == 1 && columns[0] == ROWS;
                open = !counting;
                if open {
                    sets += 1;
                    out.begin(columns);
                }
            }
            QueryItem::Row(row) if counting => {
                affected = texts(row).first().and_then(|c| c.as_deref()?.parse().ok()).unwrap_or(0);
            }
            QueryItem::Row(row) => {
                if out.keep() {
                    out.push(texts(row));
                }
            }
        }
    }
    if open {
        out.end(0);
    }
    if sets == 0 {
        out.end(affected); // a batch without result sets is one outcome: its row count, or nothing
    }
    Ok(())
}

async fn stream_rows(client: &mut Tds, sql: &str, params: &[Option<&str>], sink: &mut dyn RowSink) -> Result<()> {
    let mut stream = if params.is_empty() {
        client.simple_query(sql).await?
    } else {
        let args: Vec<&dyn ToSql> = params.iter().map(|p| p as &dyn ToSql).collect();
        client.query(sql, &args).await?
    };
    let mut described = false;
    while let Some(item) = stream.try_next().await? {
        match item {
            QueryItem::Metadata(_) if described => return Err(Error::Invalid("export one statement at a time")),
            QueryItem::Metadata(meta) => {
                described = true;
                sink.columns(meta.columns().iter().map(|c| c.name().to_string()).collect())?;
            }
            QueryItem::Row(row) => sink.row(texts(row))?,
        }
    }
    Ok(())
}

/// Runs `$work`; when the stopper fires first, drops it (and its borrow of the client), tells the server to stop,
/// and reports the call cancelled. Returns from the calling function either way.
macro_rules! guarded {
    ($self:ident, $work:expr) => {{
        let cancel = $self.cancel.clone();
        {
            let work = $work;
            tokio::pin!(work);
            tokio::select! {
                r = &mut work => return r,
                () = cancel.notified() => {}
            }
        }
        // The server stops at once, but tiberius then looks for an attention acknowledgement the response has
        // already folded into its error; the connection is fine, so that one complaint is let through, and the
        // round trip after it proves the stream is back in step.
        match $self.client.cancel_query().await {
            Ok(()) => {}
            Err(tiberius::error::Error::Protocol(m)) if m.contains("Attention") => {}
            Err(e) => return Err(e.into()),
        }
        $self.client.simple_query("SELECT 1").await?.into_results().await?;
        Err(Error::Cancelled)
    }};
}

#[async_trait]
impl Driver for SqlServer {
    fn dialect(&self) -> Dialect {
        Dialect::Mssql
    }

    async fn batch(&mut self, sql: &str) -> Result<()> {
        guarded!(self, async {
            self.client.simple_query(sql).await?.into_results().await?;
            Ok(())
        })
    }

    async fn run(&mut self, sql: &str, out: &mut Results) -> Result<()> {
        let batches = split_batches(sql);
        guarded!(self, async {
            for batch in &batches {
                if let Err(e) = run_batch(&mut self.client, batch, out).await {
                    // Explain turns SHOWPLAN on for the session; one that failed would leave it showing plans for
                    // every later statement. The batch's own error is the one to report, so this is best effort.
                    let _ =
                        async { self.client.simple_query("SET SHOWPLAN_TEXT OFF").await?.into_results().await }.await;
                    return Err(e);
                }
            }
            Ok(())
        })
    }

    async fn execute_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<u64> {
        let args: Vec<&dyn ToSql> = params.iter().map(|p| p as &dyn ToSql).collect();
        guarded!(self, async { Ok(self.client.execute(sql, &args).await?.total()) })
    }

    async fn fetch_with(&mut self, sql: &str, params: &[Option<&str>]) -> Result<Vec<Vec<Option<String>>>> {
        let args: Vec<&dyn ToSql> = params.iter().map(|p| p as &dyn ToSql).collect();
        guarded!(self, async {
            let rows = self.client.query(sql, &args).await?.into_first_result().await?;
            Ok(rows.into_iter().map(texts).collect())
        })
    }

    async fn stream(&mut self, sql: &str, params: &[Option<&str>], sink: &mut dyn RowSink) -> Result<()> {
        guarded!(self, stream_rows(&mut self.client, sql, params, sink))
    }

    async fn ping(&mut self) -> Result<()> {
        self.client.simple_query("SELECT 1").await?.into_results().await?;
        Ok(())
    }

    fn stopper(&self) -> Stopper {
        Stopper::Notify(self.cancel.clone())
    }

    fn lost_by(&self, err: &Error) -> bool {
        matches!(err, Error::Mssql(e) if lost(e))
    }

    // ponytail: SQL Server has no read-only transaction, so these are harmless and the read-only guarantee for safe
    // mode and MCP read-only tokens rests on the SQL parse alone. A read-only login is the sure way.
    fn read_only_bounds(&self) -> (&'static str, &'static str) {
        ("set nocount off", "set nocount off")
    }

    fn param(&self, n: usize) -> String {
        format!("@P{n}")
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::split_batches;

    #[test]
    fn instance_port_in_should_read_the_tcp_port_from_sql_browsers_answer() {
        let mut answer = vec![5u8, 0, 0];
        answer.extend_from_slice(
            b"ServerName;DB1;InstanceName;SQLEXPRESS;IsClustered;No;Version;16.0.1000.6;tcp;49724;;",
        );
        assert_eq!(super::instance_port_in(&answer), Some(49724));
        let mut pipes_only = vec![5u8, 0, 0];
        pipes_only.extend_from_slice(b"ServerName;DB1;InstanceName;X;np;\\\\DB1\\pipe\\sql\\query;;");
        assert_eq!(super::instance_port_in(&pipes_only), None);
    }

    #[test]
    fn split_batches_should_split_at_go_lines_and_drop_empty_batches() {
        assert_eq!(
            split_batches("select 1\nGO\n-- nothing\ngo\nselect 2;\nselect 3\n  Go  \n"),
            ["select 1\n", "select 2;\nselect 3\n"]
        );
        assert_eq!(split_batches("select 'go'"), ["select 'go'"]);
        assert_eq!(split_batches("select 1 -- go"), ["select 1 -- go"]);
    }

    use crate::db::{Conn, Engine, Options};
    use crate::driver::Stopper;
    use crate::error::Error;
    use crate::tls::Tls;
    use secrecy::SecretString;

    /// A connection to a local SQL Server (`scripts/test-env.sh up`), on a scratch database that `scratch` creates.
    async fn local(database: &str) -> Conn {
        let (tls, options) = (Tls::default(), Options::default());
        let password = SecretString::from("TestPass1!word");
        let url = std::env::var("DBM_MSSQL_URL").unwrap_or_else(|_| "mssql://sa@127.0.0.1:1433".into());
        let mut c = Conn::open(Engine::Mssql, &url, Some(&password), &tls, &options, None).await.unwrap();
        c.query(&format!(
            "if db_id('{database}') is not null drop database [{database}]; create database [{database}]"
        ))
        .await
        .unwrap();
        c.use_database(database).await.unwrap();
        c
    }

    async fn drop_database(mut c: Conn, database: &str) {
        c.use_database("master").await.unwrap();
        c.query(&format!(
            "alter database [{database}] set single_user with rollback immediate; drop database [{database}]"
        ))
        .await
        .unwrap();
    }

    #[test]
    #[ignore = "needs a local SQL Server (scripts/test-env.sh up); creates and drops a scratch database"]
    fn local_sql_server_should_run_batches_count_rows_and_write_every_type_as_text() {
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_values").await;
            let script = "create table t(id int identity primary key, n bigint, d decimal(10,2), f float, b bit, s nvarchar(20), \
                 g uniqueidentifier, bin varbinary(8), dt datetime, dt2 datetime2(3), da date, ti time(3), dto datetimeoffset(2), x xml)\n\
                 GO\n\
                 insert into t(n, d, f, b, s, g, bin, dt, dt2, da, ti, dto, x) values \
                   (9000000000, 12.50, 1.5, 1, N'héllo', '6F9619FF-8B86-D011-B42D-00C04FC964FF', 0xCAFE, '2024-01-02 03:04:05.123', \
                    '2024-01-02 03:04:05.123', '2024-01-02', '03:04:05.123', '2024-01-02 03:04:05.12 +02:00', '<a>1</a>'), \
                   (null, null, null, null, null, null, null, null, null, null, null, null, null)\n\
                 GO\n\
                 select * from t order by id\n\
                 GO\n\
                 update t set n = 1 where id > 0\n\
                 GO\n\
                 select 1 as one; select 'two' as two";
            let r = c.query_results(script, None).await.unwrap();
            assert_eq!(r.len(), 6, "create, insert, select, update, two selects: {r:?}");
            assert_eq!((r[0].columns.len(), r[0].affected), (0, 0));
            assert_eq!((r[1].columns.len(), r[1].affected), (0, 2), "the insert's count");
            assert_eq!(
                r[2].rows[0],
                [
                    "1",
                    "9000000000",
                    "12.50",
                    "1.5",
                    "1",
                    "héllo",
                    "6F9619FF-8B86-D011-B42D-00C04FC964FF",
                    "0xCAFE",
                    "2024-01-02 03:04:05.123",
                    "2024-01-02 03:04:05.123",
                    "2024-01-02",
                    "03:04:05.123",
                    "2024-01-02 03:04:05.120 +02:00",
                    "<a>1</a>"
                ]
                .map(|v| Some(v.to_string()))
            );
            assert!(r[2].rows[1][1..].iter().all(Option::is_none), "NULLs stay NULL");
            assert_eq!((r[3].columns.len(), r[3].affected), (0, 2), "the update's count");
            assert_eq!((r[4].rows.len(), r[4].columns.clone(), r[5].rows.len()), (1, vec!["one".to_string()], 1));
            assert!(c.query("select nope").await.is_err());
            assert_eq!(c.query("select 7 as seven").await.unwrap().rows, [[Some("7".into())]], "usable after an error");
            drop_database(c, "tm_values").await;
        });
    }

    struct Collect(Vec<String>, Vec<Vec<Option<String>>>);
    impl crate::driver::RowSink for Collect {
        fn columns(&mut self, names: Vec<String>) -> crate::error::Result<()> {
            self.0 = names;
            Ok(())
        }
        fn row(&mut self, row: Vec<Option<String>>) -> crate::error::Result<()> {
            self.1.push(row);
            Ok(())
        }
    }

    const SCRIPT: &str = "create table owners(id int primary key, name nvarchar(50) not null default N'x')\n\
        GO\n\
        create table pets(id int identity primary key, owner int references owners(id) on delete cascade, \
          age int check (age > 0), tags nvarchar(max), note varchar(10), constraint pets_owner_age unique (owner, age))\n\
        GO\n\
        create index pi on pets(age, owner)\n\
        GO\n\
        create schema extra\n\
        GO\n\
        create table extra.t(a int)\n\
        GO\n\
        create view adults as select id, age from pets where age > 2\n\
        GO\n\
        insert into owners values (1, N'ann'), (2, N'bob')\n\
        GO\n\
        insert into pets(owner, age, tags) values (1, 3, N'a'), (2, 1, null)";

    #[test]
    #[ignore = "needs a local SQL Server (scripts/test-env.sh up); creates and drops a scratch database"]
    fn local_sql_server_catalog_should_list_tables_columns_structure_and_ddl() {
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_catalog").await;
            c.query_results(SCRIPT, None).await.unwrap();
            assert_eq!(c.tables(false).await.unwrap(), ["adults", "owners", "pets", "extra.t"]);
            let cols = c.columns("pets").await.unwrap();
            let seen: Vec<_> =
                cols.iter().map(|c| (c.name.as_str(), c.data_type.as_str(), c.nullable, c.primary)).collect();
            assert_eq!(seen[0], ("id", "int", false, true), "{seen:?}");
            assert_eq!(seen[3], ("tags", "nvarchar(max)", true, false), "{seen:?}");
            assert_eq!(seen[4], ("note", "varchar(10)", true, false), "{seen:?}");
            assert_eq!(c.columns("extra.t").await.unwrap()[0].name, "a");
            let schema = c.schema().await.unwrap();
            assert!(
                schema.contains(&("pets".into(), "age".into())) && schema.contains(&("extra.t".into(), "a".into()))
            );
            assert_eq!(c.items(false).await.unwrap().iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["adults"]);
            assert!(c.is_view("adults").await.unwrap() && !c.is_view("pets").await.unwrap());
            assert_eq!(c.current_database().await.unwrap().as_deref(), Some("tm_catalog"));
            assert!(c.databases().await.unwrap().contains(&"tm_catalog".to_string()));
            assert!(c.server_version().await.unwrap().starts_with("SQL Server 16."));
            assert_eq!(c.tls_cipher().await.unwrap().as_deref(), Some("TLS"));

            let s = c.structure("pets").await.unwrap();
            assert!(s.columns[0].auto_increment && s.columns[0].primary);
            let idx: Vec<_> =
                s.indexes.iter().map(|i| (i.columns.clone(), i.unique, i.primary, i.constraint)).collect();
            assert!(idx.contains(&(vec!["id".into()], true, true, true)), "{idx:?}");
            assert!(idx.contains(&(vec!["owner".into(), "age".into()], true, false, true)), "{idx:?}");
            assert!(idx.contains(&(vec!["age".into(), "owner".into()], false, false, false)), "{idx:?}");
            let fk = &s.foreign_keys[0];
            assert_eq!(
                (fk.columns.clone(), fk.ref_table.as_str(), fk.on_delete.as_str()),
                (vec!["owner".into()], "owners", "CASCADE")
            );
            assert_eq!(s.checks[0].definition, "([age]>(0))");
            assert!(c.structure("nope").await.is_err());
            let ddl = c.ddl("pets").await.unwrap();
            assert!(
                ddl.contains("[id] int IDENTITY(1,1) NOT NULL")
                    && ddl.contains("CREATE INDEX [pi] ON [pets] ([age], [owner]);"),
                "{ddl}"
            );
            assert!(c.ddl("adults").await.unwrap().to_uppercase().starts_with("CREATE VIEW"));
            assert_eq!(
                c.view_body("adults").await.unwrap().to_lowercase().replace(['[', ']'], ""),
                "select id, age from pets where age > 2"
            );

            let diagram = c.diagram(None).await.unwrap();
            let names: Vec<_> = diagram.tables.iter().map(|t| (t.name.as_str(), t.view)).collect();
            assert_eq!(names, [("adults", true), ("extra.t", false), ("owners", false), ("pets", false)]);
            assert_eq!((diagram.links[0].table.as_str(), diagram.links[0].ref_table.as_str()), ("pets", "owners"));
            let o = c.overview("pets").await.unwrap();
            assert_eq!(o.rows, Some(2));
            assert!(o.bytes.is_some() && o.created.is_some());
            drop_database(c, "tm_catalog").await;
        });
    }

    #[test]
    #[ignore = "needs a local SQL Server (scripts/test-env.sh up); creates and drops a scratch database"]
    fn local_sql_server_should_browse_edit_and_stream_rows() {
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_rows").await;
            c.query_results(
                "create table people(id int primary key, name nvarchar(40), age int, note nvarchar(40))\n\
                 GO\n\
                 insert into people values (1, N'Ada', 36, N'50% off'), (2, N'Bob', 25, null), (3, N'Cy', 41, N'a_b'), \
                 (4, N'O''Hara', 25, N''), (5, N'[x]', 30, N'range [a-z]')",
                None,
            )
            .await
            .unwrap();
            let browse = |filters: serde_json::Value, extra: serde_json::Value| {
                let mut v = serde_json::json!({
                    "table": "people", "filters": filters, "sort": [{"column": "id", "desc": false}],
                    "limit": 100, "offset": 0, "count": true
                });
                v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
                serde_json::from_value::<crate::browse::Browse>(v).unwrap()
            };
            let ids = |p: crate::browse::Page| p.rows.into_iter().map(|r| r[0].clone().unwrap()).collect::<Vec<_>>();
            let none = serde_json::json!({});
            let page =
                c.browse(&browse(serde_json::json!([]), serde_json::json!({"limit": 2, "offset": 1}))).await.unwrap();
            assert_eq!(
                (page.columns.clone(), page.total),
                (vec!["id".into(), "name".into(), "age".into(), "note".into()], Some(5))
            );
            assert_eq!(ids(page), ["2", "3"]);
            let unsorted =
                c.browse(&browse(serde_json::json!([]), serde_json::json!({"sort": [], "limit": 2}))).await.unwrap();
            assert_eq!(unsorted.rows.len(), 2, "paging works without a sort too");
            let f = |column: serde_json::Value, op: &str, value: &str| serde_json::json!([{"column": column, "op": op, "value": value}]);
            for (filters, want) in [
                (f("name".into(), "eq", "O'Hara"), vec!["4"]),
                (f("name".into(), "eq", "x' or '1'='1"), vec![]),
                (f("note".into(), "contains", "%"), vec!["1"]),
                (f("note".into(), "contains", "_"), vec!["3"]),
                (f("note".into(), "contains", "[a-z]"), vec!["5"]),
                (f("name".into(), "contains", "ADA"), vec!["1"]),
                (f(serde_json::Value::Null, "contains", "41"), vec!["3"]),
                (f("id".into(), "in", "1, 3"), vec!["1", "3"]),
                (f("age".into(), "between", "26"), vec![]),
                (f("note".into(), "is_null", ""), vec!["2"]),
                (f("note".into(), "is_empty", ""), vec!["4"]),
                (f("age".into(), "raw", ""), vec![]),
            ] {
                let Ok(page) = c.browse(&browse(filters.clone(), none.clone())).await else {
                    assert_eq!(want, Vec::<&str>::new(), "{filters}");
                    continue;
                };
                assert_eq!(ids(page), want, "{filters}");
            }
            let sql =
                c.filter_sql(&browse(f("name".into(), "eq", "Ada"), serde_json::json!({"limit": 10}))).await.unwrap();
            assert_eq!(
                sql,
                "select * from [people] where [name] = N'Ada' order by [id] asc offset 0 rows fetch next 10 rows only"
            );

            let changes: crate::rows::Changes = serde_json::from_value(serde_json::json!({
                "updates": [{"key": [["id", "2"]], "set": [["name", "Robert"], ["note", null], ["age", "26"]]}],
                "deletes": [{"key": [["id", "4"]]}],
                "inserts": [[["id", "6"], ["name", "Eve"]]]
            }))
            .unwrap();
            assert_eq!(c.save_rows("people", &changes).await.unwrap(), 3);
            let r = c.query("select id, name, age from people order by id").await.unwrap();
            let names: Vec<_> = r.rows.iter().map(|r| (r[1].clone().unwrap(), r[2].clone())).collect();
            assert_eq!(names[1], ("Robert".into(), Some("26".into())));
            assert_eq!(names.len(), 5);
            let keyless: crate::rows::Changes =
                serde_json::from_value(serde_json::json!({"deletes": [{"key": [], "original": [["id", "1"]]}]}))
                    .unwrap();
            assert!(c.save_rows("people", &keyless).await.is_err(), "no key, no stable identity");
            let bad: crate::rows::Changes = serde_json::from_value(
                serde_json::json!({"updates": [{"key": [["id", "1"]], "set": [["age", "old"]]}]}),
            )
            .unwrap();
            assert!(c.save_rows("people", &bad).await.is_err());
            assert_eq!(
                c.query("select count(*) from people").await.unwrap().rows[0][0].as_deref(),
                Some("5"),
                "rolled back"
            );

            let mut sink = Collect(vec![], vec![]);
            c.stream("select id from people where age > @P1 order by id", &[Some("28")], &mut sink).await.unwrap();
            assert_eq!((sink.0, sink.1.len()), (vec!["id".to_string()], 3));
            drop_database(c, "tm_rows").await;
        });
    }

    #[test]
    #[ignore = "needs a local SQL Server (scripts/test-env.sh up); creates and drops a scratch database"]
    #[expect(clippy::too_many_lines, reason = "one scenario against one server, step by step")]
    fn local_sql_server_should_alter_tables_and_run_table_operations() {
        use crate::ddl::{Change, ColumnDef, Rule, TableOp};
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_ddl").await;
            let col = |name: &str, data_type: &str| ColumnDef {
                name: name.into(),
                data_type: data_type.into(),
                nullable: true,
                default: None,
                comment: None,
                auto_increment: false,
                primary: false,
            };
            let id = ColumnDef { primary: true, nullable: false, auto_increment: true, ..col("id", "bigint") };
            let create = [Change::AddColumn { column: id }, Change::AddColumn { column: col("name", "nvarchar(40)") }];
            let sql = c.alter_sql("owners", true, &create).await.unwrap();
            c.run_ddl(&sql).await.unwrap();
            let sql = c
                .alter_sql(
                    "pets",
                    true,
                    &[
                        Change::AddColumn { column: ColumnDef { primary: true, nullable: false, ..col("id", "int") } },
                        Change::AddColumn { column: col("owner", "bigint") },
                        Change::AddColumn { column: col("age", "int") },
                    ],
                )
                .await
                .unwrap();
            c.run_ddl(&sql).await.unwrap();
            assert!(c.structure("owners").await.unwrap().columns[0].auto_increment, "created with an identity");

            let changes = [
                Change::AddColumn { column: col("nick", "nvarchar(20)") },
                Change::ModifyColumn {
                    from: "age".into(),
                    column: ColumnDef { nullable: false, ..col("years", "bigint") },
                },
                Change::AddIndex { name: "pn".into(), columns: vec!["nick".into()], unique: false, method: None },
                Change::AddForeignKey {
                    name: "pets_owner".into(),
                    columns: vec!["owner".into()],
                    ref_table: "owners".into(),
                    ref_columns: vec!["id".into()],
                    on_update: Rule::NoAction,
                    on_delete: Rule::Cascade,
                },
                Change::AddCheck { name: "years_positive".into(), expression: "[years] > 0".into() },
            ];
            let sql = c.alter_sql("pets", false, &changes).await.unwrap();
            c.run_ddl(&sql).await.unwrap();
            let s = c.structure("pets").await.unwrap();
            let seen: Vec<_> = s.columns.iter().map(|c| (c.name.as_str(), c.data_type.as_str(), c.nullable)).collect();
            assert!(
                seen.contains(&("nick", "nvarchar(20)", true)) && seen.contains(&("years", "bigint", false)),
                "{seen:?}"
            );
            assert!(s.indexes.iter().any(|i| i.name == "pn") && s.foreign_keys[0].name == "pets_owner");
            assert_eq!(s.checks[0].name, "years_positive");

            let undo = [
                Change::DropIndex { name: "pn".into() },
                Change::DropForeignKey { name: "pets_owner".into() },
                Change::DropCheck { name: "years_positive".into() },
                Change::DropColumn { name: "nick".into() },
                Change::ModifyColumn { from: "years".into(), column: col("age", "bigint") },
            ];
            let sql = c.alter_sql("pets", false, &undo).await.unwrap();
            c.run_ddl(&sql).await.unwrap();
            let s = c.structure("pets").await.unwrap();
            assert_eq!(
                (s.indexes.len(), s.foreign_keys.len(), s.checks.len()),
                (1, 0, 0),
                "only the primary key is left"
            );
            assert!(s.columns.iter().any(|c| c.name == "age") && !s.columns.iter().any(|c| c.name == "nick"));

            // Comments, defaults and AFTER triggers are made (local_sql_server_structure_should_change_…).
            for refused in [
                Change::ModifyColumn {
                    from: "age".into(),
                    column: ColumnDef { auto_increment: true, ..col("age", "bigint") },
                },
                Change::AddTrigger {
                    name: "t".into(),
                    timing: crate::ddl::TriggerTiming::Before,
                    events: vec![crate::ddl::TriggerEvent::Insert],
                    body: "x".into(),
                },
            ] {
                assert!(c.alter_sql("pets", false, &[refused]).await.is_err());
            }

            c.query("insert into pets(id, owner, age) values (1, null, 5)").await.unwrap();
            for (op, check) in [
                (TableOp::Clone { table: "pets".into(), to: "pets2".into(), data: true }, "select count(*) from pets2"),
                (TableOp::Rename { table: "pets2".into(), to: "pets3".into() }, "select count(*) from pets3"),
                (
                    TableOp::Truncate {
                        table: "pets3".into(),
                        cascade: false,
                        restart_identity: false,
                        ignore_fk: false,
                    },
                    "select count(*) from pets3",
                ),
                (
                    TableOp::Drop { table: "pets3".into(), cascade: false, ignore_fk: false },
                    "select count(*) from owners",
                ),
            ] {
                let sql = c.op_sql(&op).await.unwrap();
                c.run_ddl(&sql).await.unwrap();
                c.query(check).await.unwrap();
            }
            assert!(c.query("select * from pets3").await.is_err(), "dropped");
            c.query("insert into owners(name) values (N'a'), (N'b'); delete from owners where name = N'b'")
                .await
                .unwrap();
            let sql = c.op_sql(&TableOp::ResetAutoIncrement { table: "owners".into() }).await.unwrap();
            c.run_ddl(&sql).await.unwrap();
            c.query("insert into owners(name) values (N'c')").await.unwrap();
            let r = c.query("select max(id) from owners").await.unwrap();
            assert_eq!(r.rows[0][0].as_deref(), Some("2"), "continues from the largest identity");

            let db = crate::ddl::DbOp::Create {
                name: "tm_ddl_other".into(),
                encoding: None,
                collation: Some("Latin1_General_CI_AS".into()),
            };
            let sql = c.database_sql(&db).await.unwrap();
            c.run_ddl(&sql).await.unwrap();
            let sql = c
                .database_sql(&crate::ddl::DbOp::Rename { name: "tm_ddl_other".into(), to: "tm_ddl_renamed".into() })
                .await
                .unwrap();
            c.run_ddl(&sql).await.unwrap();
            assert!(c.databases().await.unwrap().contains(&"tm_ddl_renamed".to_string()));
            let sql = c.database_sql(&crate::ddl::DbOp::Drop { name: "tm_ddl_renamed".into() }).await.unwrap();
            c.run_ddl(&sql).await.unwrap();
            assert!(c.database_options().await.unwrap().collations.iter().any(|(n, _)| n == "Latin1_General_CI_AS"));
            assert!(
                c.database_sql(&crate::ddl::DbOp::Create {
                    name: "x".into(),
                    encoding: None,
                    collation: Some("a; drop".into())
                })
                .await
                .is_err()
            );
            drop_database(c, "tm_ddl").await;
        });
    }

    #[test]
    #[ignore = "needs a local SQL Server (scripts/test-env.sh up)"]
    fn local_sql_server_stopper_should_cancel_a_long_query_and_leave_the_connection_usable() {
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_stop").await;
            let stopper = c.stopper();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                if let Stopper::Notify(cancel) = stopper {
                    cancel.notify_waiters();
                }
            });
            let started = std::time::Instant::now();
            let err = c.query("waitfor delay '00:00:30'").await.unwrap_err();
            assert!(matches!(err, Error::Cancelled), "{err}");
            assert!(started.elapsed() < std::time::Duration::from_secs(10), "it didn't wait out the delay");
            assert_eq!(c.query("select 1 as one").await.unwrap().rows, [[Some("1".into())]]);
            drop_database(c, "tm_stop").await;
        });
    }

    #[test]
    #[ignore = "needs a local SQL Server (scripts/test-env.sh up)"]
    fn local_sql_server_process_list_should_show_other_sessions_and_kill_one() {
        tauri::async_runtime::block_on(async {
            let mut me = local("tm_proc").await;
            let mut other = local("tm_proc_other").await;
            let other_id = other.query("select @@spid").await.unwrap().rows[0][0].clone().unwrap();
            let my_id = me.query("select @@spid").await.unwrap().rows[0][0].clone().unwrap();
            let r = me.query(crate::processes::MSSQL).await.unwrap();
            assert_eq!(r.columns[0], "Session");
            let ids: Vec<_> = r.rows.iter().map(|row| row[0].clone().unwrap()).collect();
            assert!(ids.contains(&other_id) && !ids.contains(&my_id), "{ids:?}");
            me.query(&format!("kill {other_id}")).await.unwrap();
            assert!(other.query("select 1").await.is_err(), "the killed session is gone");
            drop_database(me, "tm_proc").await;
        });
    }

    #[test]
    #[ignore = "needs a local SQL Server (scripts/test-env.sh up)"]
    fn local_sql_server_explain_should_show_a_plan_and_not_leave_showplan_on_after_an_error() {
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_explain").await;
            c.query("create table t(a int)").await.unwrap();
            let results = c
                .query_results("SET SHOWPLAN_TEXT ON\nGO\nselect * from t\nGO\nSET SHOWPLAN_TEXT OFF", None)
                .await
                .unwrap();
            let shown =
                results.iter().flat_map(|r| &r.rows).any(|r| r[0].as_deref().is_some_and(|t| t.contains("Table Scan")));
            assert!(shown, "{results:?}");
            assert_eq!(c.query("select 5 as five").await.unwrap().rows, [[Some("5".into())]], "showplan is off again");
            assert!(c.query("SET SHOWPLAN_TEXT ON\nGO\nselect * from nope\nGO\nSET SHOWPLAN_TEXT OFF").await.is_err());
            assert_eq!(c.query("select 6 as six").await.unwrap().rows, [[Some("6".into())]], "and after a failure");
            drop_database(c, "tm_explain").await;
        });
    }

    #[test]
    #[ignore = "needs a local SQL Server on 1433 (scripts/test-env.sh up) and UDP 1434 free; answers as SQL Browser"]
    fn local_sql_server_named_instance_should_connect_on_the_port_sql_browser_gives() {
        tauri::async_runtime::block_on(async {
            // A stand-in SQL Browser that sends instance TM to the server on 1433.
            let browser = tokio::net::UdpSocket::bind("127.0.0.1:1434").await.unwrap();
            tauri::async_runtime::spawn(async move {
                let mut ask = [0u8; 256];
                let (n, from) = browser.recv_from(&mut ask).await.unwrap();
                assert_eq!(&ask[..n], b"\x04TM");
                let text = b"ServerName;LOCAL;InstanceName;TM;IsClustered;No;Version;16.0;tcp;1433;;";
                let mut answer = vec![5u8, u8::try_from(text.len()).unwrap(), 0];
                answer.extend_from_slice(text);
                browser.send_to(&answer, from).await.unwrap();
            });
            let password = SecretString::from("TestPass1!word");
            let url = "mssql://sa@127.0.0.1/master?instance=TM";
            let mut c = Conn::open(Engine::Mssql, url, Some(&password), &Tls::default(), &Options::default(), None)
                .await
                .unwrap();
            assert_eq!(c.fetch_with("select 1", &[]).await.unwrap(), [[Some("1".into())]]);
        });
    }

    #[test]
    #[ignore = "needs a local SQL Server (scripts/test-env.sh up); creates and drops a scratch database"]
    fn local_sql_server_should_list_edit_and_drop_functions_procedures_and_sequences() {
        use crate::catalog::ItemKind;
        use crate::objects::{routine_sql, routine_template, sequence_sql};
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_items").await;
            let d = crate::db::Dialect::Mssql;
            for (kind, name) in [(ItemKind::Function, "first"), (ItemKind::Procedure, "first_proc")] {
                let text = routine_template(d, kind, name).unwrap();
                c.run_ddl(&routine_sql(d, kind, name, &text, true).unwrap()).await.unwrap();
            }
            c.query("create sequence ticket as bigint start with 10 increment by 5").await.unwrap();
            let items: Vec<(ItemKind, String)> =
                c.items(false).await.unwrap().into_iter().map(|i| (i.kind, i.name)).collect();
            assert!(items.contains(&(ItemKind::Function, "first".into())), "{items:?}");
            assert!(
                items.contains(&(ItemKind::Procedure, "first_proc".into()))
                    && items.contains(&(ItemKind::Sequence, "ticket".into()))
            );

            // Saving an existing routine replaces it.
            let def = c.routine_definitions(ItemKind::Function, "first").await.unwrap().remove(0);
            assert!(def.starts_with("CREATE OR ALTER FUNCTION"), "{def}");
            c.run_ddl(
                &routine_sql(d, ItemKind::Function, "first", &def.replace("RETURN 1", "RETURN 2"), false).unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(c.fetch_with("select dbo.first()", &[]).await.unwrap(), [[Some("2".into())]]);

            c.query("select next value for ticket").await.unwrap();
            let cur = c.sequence("ticket").await.unwrap();
            assert_eq!(
                (cur.last_value.as_deref(), cur.increment.as_str(), cur.start.as_str()),
                (Some("10"), "5", "10")
            );
            let want = crate::objects::Sequence {
                last_value: Some("100".into()),
                comment: Some("tickets".into()),
                ..cur.clone()
            };
            let sql = sequence_sql(d, "ticket", &cur, &want).unwrap();
            c.run_ddl(&sql).await.unwrap();
            assert_eq!(c.fetch_with("select next value for ticket", &[]).await.unwrap(), [[Some("105".into())]]);
            assert_eq!(c.sequence("ticket").await.unwrap().comment.as_deref(), Some("tickets"));
            let moved = crate::objects::Sequence { start: "1".into(), ..cur.clone() };
            assert!(sequence_sql(d, "ticket", &cur, &moved).is_err(), "SQL Server can't move the start");

            for (kind, name) in
                [(ItemKind::Function, "first"), (ItemKind::Procedure, "first_proc"), (ItemKind::Sequence, "ticket")]
            {
                let sql = c.drop_item_sql(kind, name, false).await.unwrap();
                c.run_ddl(&sql).await.unwrap();
            }
            assert_eq!(c.items(false).await.unwrap().len(), 0, "all dropped");
            drop_database(c, "tm_items").await;
        });
    }

    #[test]
    #[ignore = "needs a local SQL Server (scripts/test-env.sh up); creates and drops a scratch database"]
    #[expect(clippy::too_many_lines, reason = "one scenario against a live server: create, change, then drop")]
    fn local_sql_server_structure_should_change_defaults_comments_and_triggers() {
        use crate::ddl::{Change, ColumnDef, TriggerEvent, TriggerTiming};
        tauri::async_runtime::block_on(async {
            let mut c = local("tm_gaps").await;
            let col = |name: &str, data_type: &str| ColumnDef {
                name: name.into(),
                data_type: data_type.into(),
                nullable: true,
                default: None,
                comment: None,
                auto_increment: false,
                primary: false,
            };
            let create = [
                Change::AddColumn { column: ColumnDef { primary: true, nullable: false, ..col("id", "int") } },
                Change::AddColumn {
                    column: ColumnDef {
                        default: Some("0".into()),
                        comment: Some("points".into()),
                        ..col("score", "int")
                    },
                },
                Change::AddColumn { column: col("seen", "datetime2") },
                Change::Comment { comment: Some("all the pets".into()) },
            ];
            {
                let sql = c.alter_sql("app.pets", true, &create).await.unwrap();
                c.run_ddl(&sql).await
            }
            .unwrap_err();
            c.query("create schema app").await.unwrap();
            {
                let sql = c.alter_sql("app.pets", true, &create).await.unwrap();
                c.run_ddl(&sql).await
            }
            .unwrap();
            let s = c.structure("app.pets").await.unwrap();
            assert_eq!(s.comment.as_deref(), Some("all the pets"));
            let score = &s.columns[1];
            assert_eq!((score.default.as_deref(), score.comment.as_deref()), (Some("((0))"), Some("points")));

            // A new default, a new type (the default comes back), a changed comment, a trigger.
            let changes = [
                Change::ModifyColumn {
                    from: "score".into(),
                    column: ColumnDef {
                        default: Some("7".into()),
                        comment: Some("stars".into()),
                        ..col("score", "bigint")
                    },
                },
                Change::ModifyColumn {
                    from: "seen".into(),
                    column: ColumnDef { default: Some("sysdatetime()".into()), ..col("seen", "datetime2") },
                },
                Change::AddTrigger {
                    name: "stamp".into(),
                    timing: TriggerTiming::After,
                    events: vec![TriggerEvent::Insert, TriggerEvent::Update],
                    body: "UPDATE p SET seen = sysdatetime() FROM app.pets p JOIN inserted i ON i.id = p.id;".into(),
                },
                Change::Comment { comment: None },
            ];
            {
                let sql = c.alter_sql("app.pets", false, &changes).await.unwrap();
                c.run_ddl(&sql).await
            }
            .unwrap();
            let s = c.structure("app.pets").await.unwrap();
            let score = &s.columns[1];
            assert_eq!(
                (score.data_type.as_str(), score.default.as_deref(), score.comment.as_deref()),
                ("bigint", Some("((7))"), Some("stars"))
            );
            assert!(s.columns[2].default.is_some() && s.comment.is_none());
            let t = &s.triggers[0];
            assert_eq!(
                (t.timing.as_deref(), t.events.clone()),
                (Some("AFTER"), vec!["INSERT".to_string(), "UPDATE".to_string()])
            );
            assert!(t.body.as_deref().is_some_and(|b| b.starts_with("UPDATE p SET seen")), "{:?}", t.body);
            c.query("insert into app.pets (id) values (1)").await.unwrap();
            assert_eq!(c.fetch_with("select score from app.pets", &[]).await.unwrap(), [[Some("7".into())]]);
            assert!(c.fetch_with("select seen from app.pets", &[]).await.unwrap()[0][0].is_some());
            let before = [Change::AddTrigger {
                name: "x".into(),
                timing: TriggerTiming::Before,
                events: vec![TriggerEvent::Insert],
                body: "x".into(),
            }];
            assert!(c.alter_sql("app.pets", false, &before).await.is_err());

            // Dropping the trigger, a default (to none), then a column that had one.
            let changes = [
                Change::DropTrigger { name: "stamp".into() },
                Change::ModifyColumn { from: "seen".into(), column: col("seen", "datetime2") },
                Change::DropColumn { name: "score".into() },
            ];
            {
                let sql = c.alter_sql("app.pets", false, &changes).await.unwrap();
                c.run_ddl(&sql).await
            }
            .unwrap();
            let s = c.structure("app.pets").await.unwrap();
            assert!(s.triggers.is_empty() && s.columns.len() == 2 && s.columns[1].default.is_none());
            drop_database(c, "tm_gaps").await;
        });
    }
}

//! SSH tunnels: a local port forwarded through an SSH server to the database host.

use crate::db::{CONNECT_TIMEOUT, Engine};
use crate::error::{Error, Result};
use russh::keys::{PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate, known_hosts};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// SSH settings saved with a connection. Blank fields fall back to `~/.ssh/config`, then OpenSSH's defaults.
/// The password (or the key's passphrase, when `key` is set) lives in the keychain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ssh {
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub user: String,
    /// Private key file. Blank: `IdentityFile` from `~/.ssh/config`, the SSH agent, then the default keys.
    #[serde(default)]
    pub key: String,
}

/// An open tunnel. Connect to `127.0.0.1:port`; dropping it closes the tunnel and every connection through it.
pub struct Tunnel {
    pub port: u16,
    task: tauri::async_runtime::JoinHandle<()>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Checks the server's host key against `known_hosts`: a match passes, a changed key is refused, and an unknown
/// host is recorded on first use (OpenSSH's `StrictHostKeyChecking accept-new`).
struct HostKeyCheck {
    host: String,
    port: u16,
    known_hosts: PathBuf,
}

impl HostKeyCheck {
    fn verify(&self, key: &PublicKey) -> Result<()> {
        match known_hosts::check_known_hosts_path(&self.host, self.port, key, &self.known_hosts) {
            Ok(true) => Ok(()),
            Ok(false) => Ok(known_hosts::learn_known_hosts_path(&self.host, self.port, key, &self.known_hosts)?),
            Err(russh::keys::Error::KeyChanged { line }) => {
                Err(Error::SshHostKeyChanged { host: self.host.clone(), line })
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl russh::client::Handler for HostKeyCheck {
    type Error = Error;

    fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> impl Future<Output = Result<bool>> + Send {
        // Decided synchronously (one small file read), so the future doesn't borrow `key`.
        let verdict = match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => self.verify(key).map(|()| true),
            PublicKeyOrCertificate::Certificate(_) => Err(Error::SshAuth("SSH host certificates aren't supported yet")),
        };
        std::future::ready(verdict)
    }
}

fn home() -> Result<PathBuf> {
    std::env::home_dir().ok_or(Error::Invalid("no home folder"))
}

/// `~/x` becomes a path in the home folder; anything else is used as given.
pub fn expand_home(path: &str) -> Result<PathBuf> {
    Ok(match path.strip_prefix("~/") {
        Some(rest) => home()?.join(rest),
        None => PathBuf::from(path),
    })
}

/// Opens a tunnel to the host and port in `url`. The driver then dials `127.0.0.1:<tunnel.port>` but keeps
/// the real host name for TLS checks.
pub async fn open_for(engine: Engine, url: &str, ssh: &Ssh, secret: Option<&SecretString>) -> Result<Tunnel> {
    let default_port = match engine {
        Engine::Postgres => 5432,
        Engine::Mysql | Engine::Mariadb => 3306,
        Engine::Redis => 6379,
        Engine::Mssql => 1433,
        Engine::Clickhouse => 8123,
        Engine::Sqlite | Engine::Duckdb => return Err(Error::Invalid("database files can't be opened over SSH")),
    };
    let u = url::Url::parse(url)?;
    let target_host = u.host_str().unwrap_or("127.0.0.1").to_string();
    let target_port = u.port().unwrap_or(default_port);
    tokio::time::timeout(CONNECT_TIMEOUT, open(ssh, secret, target_host, target_port))
        .await
        .map_err(|_| Error::ConnectTimeout(CONNECT_TIMEOUT.as_secs()))?
}

async fn open(ssh: &Ssh, secret: Option<&SecretString>, target_host: String, target_port: u16) -> Result<Tunnel> {
    let cfg = match russh_config::parse_home(&ssh.host) {
        Ok(cfg) => cfg,
        Err(russh_config::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            russh_config::Config::default(&ssh.host)
        }
        Err(e) => return Err(e.into()),
    };
    if cfg.host_config.proxy_jump.is_some() || cfg.host_config.proxy_command.is_some() {
        return Err(Error::SshAuth("ProxyJump and ProxyCommand in ~/.ssh/config aren't supported yet"));
    }
    // The form's values win over ~/.ssh/config, which wins over OpenSSH's defaults.
    let host = cfg.host().to_string();
    let port = ssh.port.or(cfg.host_config.port).unwrap_or(22);
    let user = if ssh.user.is_empty() { cfg.user() } else { ssh.user.clone() };
    let known_hosts = match &cfg.host_config.user_known_hosts_file {
        Some(path) => path.clone(),
        None => home()?.join(".ssh/known_hosts"),
    };

    let config = russh::client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        nodelay: true,
        ..Default::default()
    };
    let check = HostKeyCheck { host: host.clone(), port, known_hosts };
    let mut session = russh::client::connect(Arc::new(config), (host.as_str(), port), check).await?;
    authenticate(&mut session, &user, ssh, &cfg, secret).await?;

    // Open one channel now so an unreachable database host is reported here, not as a dropped connection later.
    let probe = session
        .channel_open_direct_tcpip(target_host.as_str(), target_port.into(), "127.0.0.1", 0)
        .await
        .map_err(|e| Error::SshForward { host: target_host.clone(), port: target_port, reason: e.to_string() })?;
    let _ = probe.close().await; // the channel did its job by opening; a failed close changes nothing

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let local_port = listener.local_addr()?.port();
    let session = Arc::new(session);
    let task = tauri::async_runtime::spawn(async move {
        // Each forwarded connection runs in this set, so aborting the tunnel task stops them all.
        let mut conns = tokio::task::JoinSet::new();
        while let Ok((mut socket, peer)) = listener.accept().await {
            while conns.try_join_next().is_some() {}
            let session = session.clone();
            let (host, port) = (target_host.clone(), target_port);
            conns.spawn(async move {
                let origin = peer.ip().to_string();
                let Ok(channel) =
                    session.channel_open_direct_tcpip(host, port.into(), origin, peer.port().into()).await
                else {
                    return; // dropping the socket tells the database driver the connection failed
                };
                // Either side closing ends the copy; the error says only which side, so it's not needed.
                let _ = tokio::io::copy_bidirectional(&mut socket, &mut channel.into_stream()).await;
            });
        }
    });
    Ok(Tunnel { port: local_port, task })
}

/// A key file from the form uses the keychain secret as its passphrase; otherwise the secret is a password.
/// With neither: the SSH agent, then `IdentityFile` from `~/.ssh/config`, then the default key files.
async fn authenticate(
    session: &mut russh::client::Handle<HostKeyCheck>,
    user: &str,
    ssh: &Ssh,
    cfg: &russh_config::Config,
    secret: Option<&SecretString>,
) -> Result<()> {
    let secret = secret.map(ExposeSecret::expose_secret).filter(|s| !s.is_empty());
    if !ssh.key.is_empty() {
        let key = match russh::keys::load_secret_key(expand_home(&ssh.key)?, secret) {
            Err(russh::keys::Error::KeyIsEncrypted) => {
                return Err(Error::SshAuth("the private key is encrypted; enter its passphrase as the SSH password"));
            }
            other => other?,
        };
        return if try_key(session, user, key).await? {
            Ok(())
        } else {
            Err(Error::SshAuth("the server rejected the private key"))
        };
    }
    if let Some(password) = secret {
        return if session.authenticate_password(user, password).await?.success() {
            Ok(())
        } else {
            Err(Error::SshAuth("wrong user name or password"))
        };
    }
    if try_agent(session, user).await? {
        return Ok(());
    }
    let defaults = ["id_ed25519", "id_ecdsa", "id_rsa"].map(|f| home().map(|h| h.join(".ssh").join(f)));
    let configured = cfg.host_config.identity_file.iter().flatten().cloned().map(Ok);
    for path in configured.chain(defaults) {
        let path = path?;
        // Skip missing or passphrase-protected files: those need the key field and its passphrase.
        let Ok(key) = russh::keys::load_secret_key(&path, None) else { continue };
        if try_key(session, user, key).await? {
            return Ok(());
        }
    }
    Err(Error::SshAuth(
        "no SSH agent key or key file was accepted; enter a password, or choose a private key and its passphrase",
    ))
}

async fn try_key(
    session: &mut russh::client::Handle<HostKeyCheck>,
    user: &str,
    key: russh::keys::PrivateKey,
) -> Result<bool> {
    let hash = session.best_supported_rsa_hash().await?.flatten();
    let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
    Ok(session.authenticate_publickey(user, key).await?.success())
}

/// Tries each key the SSH agent holds. No agent running counts as "no keys", not as an error.
async fn try_agent(session: &mut russh::client::Handle<HostKeyCheck>, user: &str) -> Result<bool> {
    let Ok(mut agent) = russh::keys::agent::client::AgentClient::connect_env().await else { return Ok(false) };
    let Ok(identities) = agent.request_identities().await else { return Ok(false) };
    let hash = session.best_supported_rsa_hash().await?.flatten();
    for identity in identities {
        let russh::keys::agent::AgentIdentity::PublicKey { key, .. } = identity else { continue };
        // A key the agent can't sign with (locked, removed meanwhile) is just skipped.
        if let Ok(result) = session.authenticate_publickey_with(user, key, hash, &mut agent).await
            && result.success()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{HostKeyCheck, Ssh, open_for};
    use crate::db::{Conn, Engine};
    use crate::error::Error;
    use crate::tls::Tls;
    use russh::keys::PublicKey;

    const KEY_A: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJTCjG4ZXZcrALy2p8gWVxHE4NCwRhwQFp3FrlWcU5jH";
    const KEY_B: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFCZxOLLXSW1+tNRkqLg+JLnjZAkM3bOKuyxf8PMWTd2";

    fn check(test: &str) -> HostKeyCheck {
        let dir = std::env::temp_dir().join(format!("tableminus-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        HostKeyCheck { host: "db.example.com".into(), port: 2222, known_hosts: dir.join("known_hosts") }
    }

    #[test]
    fn verify_should_record_an_unknown_host_and_accept_it_again() {
        let check = check("learn");
        let key = PublicKey::from_openssh(KEY_A).unwrap();
        check.verify(&key).unwrap();
        check.verify(&key).unwrap();
        let file = std::fs::read_to_string(&check.known_hosts).unwrap();
        let recorded: Vec<_> = file.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(recorded, [format!("[db.example.com]:2222 {KEY_A}")], "{file}");
    }

    #[test]
    fn verify_should_refuse_a_changed_host_key() {
        let check = check("changed");
        check.verify(&PublicKey::from_openssh(KEY_A).unwrap()).unwrap();
        let err = check.verify(&PublicKey::from_openssh(KEY_B).unwrap()).unwrap_err();
        let Error::SshHostKeyChanged { line, .. } = err else { panic!("{err}") };
        let file = std::fs::read_to_string(&check.known_hosts).unwrap();
        assert!(file.lines().nth(line - 1).unwrap().ends_with(KEY_A), "the error names the recorded key's line");
    }

    /// Through a throwaway sshd to the local MySQL. Run it with `scripts/test-ssh.sh`, which sets up the server,
    /// an accepted `~/.ssh/id_ed25519` and a rejected `~/wrong_key` under a temporary HOME.
    #[test]
    #[ignore = "needs a test sshd on 127.0.0.1:2222 and MySQL on 3306"]
    fn tunnel_to_local_mysql() {
        tauri::async_runtime::block_on(async {
            let home = std::env::home_dir().unwrap();
            let mut ssh = Ssh { host: "127.0.0.1".into(), port: Some(2222), user: String::new(), key: String::new() };
            // No key given: falls back to the default key file.
            let url_local = "mysql://root@127.0.0.1:3306";
            let tunnel = open_for(Engine::Mysql, url_local, &ssh, None).await.unwrap();
            let mut conn = Conn::open(
                Engine::Mysql,
                url_local,
                None,
                &Tls::default(),
                &crate::db::Options::default(),
                Some(tunnel.port),
            )
            .await
            .unwrap();
            assert_eq!(conn.query("select 40 + 2").await.unwrap().rows[0][0].as_deref(), Some("42"));
            drop(conn);
            drop(tunnel);

            // A key the server doesn't accept.
            ssh.key = home.join("wrong_key").to_string_lossy().into_owned();
            let err = open_for(Engine::Mysql, url_local, &ssh, None).await.err().unwrap();
            assert_eq!(err.to_string(), "SSH sign-in failed: the server rejected the private key");

            // The SSH server can't reach the database (nothing listens on port 1).
            ssh.key = "~/.ssh/id_ed25519".into();
            let err = open_for(Engine::Mysql, "mysql://root@127.0.0.1:1", &ssh, None).await.err().unwrap();
            assert!(matches!(err, Error::SshForward { port: 1, .. }), "{err}");
        });
    }
}

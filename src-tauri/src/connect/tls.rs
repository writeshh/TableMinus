//! TLS settings saved with a connection, and the driver configuration built from them.

use crate::connect::ssh::expand_home;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// libpq's `sslmode` values, which MySQL's `ssl-mode` mirrors.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    Disable,
    /// TLS when the server offers it, plain otherwise.
    #[default]
    Prefer,
    Require,
    /// Require, and the certificate must chain to a trusted CA.
    VerifyCa,
    /// Verify CA, and the certificate must name the host.
    VerifyFull,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tls {
    #[serde(default)]
    pub mode: TlsMode,
    /// CA certificate (PEM) to trust in addition to the system's.
    #[serde(default)]
    pub ca: String,
    /// Client certificate and its key (PEM), for servers that ask for one.
    #[serde(default)]
    pub cert: String,
    #[serde(default)]
    pub key: String,
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|source| Error::File { path: path.display().to_string(), source })
}

impl Tls {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Whether to check the certificate chain and the host name. As in libpq, Prefer and Require only
    /// encrypt, unless a CA file is given, which makes them check the chain.
    fn checks(&self) -> (bool, bool) {
        match self.mode {
            TlsMode::Disable | TlsMode::Prefer | TlsMode::Require => (!self.ca.is_empty(), false),
            TlsMode::VerifyCa => (true, false),
            TlsMode::VerifyFull => (true, true),
        }
    }

    fn ca(&self) -> Result<Option<PathBuf>> {
        if self.ca.is_empty() { Ok(None) } else { expand_home(&self.ca).map(Some) }
    }

    fn client_identity(&self) -> Result<Option<(PathBuf, PathBuf)>> {
        match (self.cert.is_empty(), self.key.is_empty()) {
            (true, true) => Ok(None),
            (false, false) => Ok(Some((expand_home(&self.cert)?, expand_home(&self.key)?))),
            _ => Err(Error::Invalid("a client certificate needs both the certificate file and its key file")),
        }
    }

    /// A native-tls connector that checks what the mode asks, with the CA and client certificate given.
    pub(crate) fn connector(&self) -> Result<native_tls::TlsConnector> {
        let (chain, hostname) = self.checks();
        let mut b = native_tls::TlsConnector::builder();
        b.danger_accept_invalid_certs(!chain).danger_accept_invalid_hostnames(!hostname);
        if let Some(ca) = self.ca()? {
            b.add_root_certificate(native_tls::Certificate::from_pem(&read(&ca)?)?);
        }
        if let Some((cert, key)) = self.client_identity()? {
            // native-tls takes PKCS#8 keys ("BEGIN PRIVATE KEY"); convert others with `openssl pkcs8 -topk8`.
            b.identity(native_tls::Identity::from_pkcs8(&read(&cert)?, &read(&key)?)?);
        }
        Ok(b.build()?)
    }

    /// A rustls configuration for drivers that take one (Cassandra's), or None for plain. Like Redis, Prefer means
    /// plain. Require encrypts without checking the certificate; the verifying modes check it against the system's
    /// certificates and the CA given (and, rustls can't skip that alone, the host name for Verify CA too).
    pub(crate) fn rustls(&self) -> Result<Option<std::sync::Arc<rustls::ClientConfig>>> {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
        let failed = |e: &dyn std::fmt::Display| Error::TlsFiles(e.to_string());
        if matches!(self.mode, TlsMode::Disable | TlsMode::Prefer) {
            return Ok(None);
        }
        crate::sql::db::install_tls_provider();
        let builder = rustls::ClientConfig::builder();
        let builder = if self.mode == TlsMode::Require && self.ca.is_empty() {
            builder.dangerous().with_custom_certificate_verifier(std::sync::Arc::new(NoCheck::default()))
        } else {
            let mut roots = rustls::RootCertStore::empty();
            for cert in rustls_native_certs::load_native_certs().certs {
                let _ = roots.add(cert); // a system certificate rustls can't read is one it doesn't need
            }
            if let Some(ca) = self.ca()? {
                for cert in CertificateDer::pem_file_iter(&ca).map_err(|e| failed(&e))? {
                    roots.add(cert.map_err(|e| failed(&e))?).map_err(|e| failed(&e))?;
                }
            }
            builder.with_root_certificates(roots)
        };
        let config = match self.client_identity()? {
            Some((cert, key)) => {
                let chain = CertificateDer::pem_file_iter(&cert)
                    .map_err(|e| failed(&e))?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| failed(&e))?;
                let key = PrivateKeyDer::from_pem_file(&key).map_err(|e| failed(&e))?;
                builder.with_client_auth_cert(chain, key).map_err(|e| failed(&e))?
            }
            None => builder.with_no_client_auth(),
        };
        Ok(Some(std::sync::Arc::new(config)))
    }

    /// Redis has no way to ask for TLS on a plain connection, so Prefer means plain, and Require and up mean TLS from
    /// the first byte.
    pub fn redis(&self) -> Result<Option<native_tls::TlsConnector>> {
        match self.mode {
            TlsMode::Disable | TlsMode::Prefer => Ok(None),
            TlsMode::Require | TlsMode::VerifyCa | TlsMode::VerifyFull => Ok(Some(self.connector()?)),
        }
    }

    pub fn postgres(&self) -> Result<(tokio_postgres::config::SslMode, postgres_native_tls::MakeTlsConnector)> {
        use tokio_postgres::config::SslMode;
        let connector = self.connector()?;
        let mode = match self.mode {
            TlsMode::Disable => SslMode::Disable,
            TlsMode::Prefer => SslMode::Prefer,
            TlsMode::Require | TlsMode::VerifyCa | TlsMode::VerifyFull => SslMode::Require,
        };
        Ok((mode, postgres_native_tls::MakeTlsConnector::new(connector)))
    }

    /// The MySQL TLS options; None means plain. `verify_as` is the host name to check the certificate against
    /// when connecting through a tunnel, where the address dialled is 127.0.0.1.
    pub fn mysql(&self, verify_as: Option<&str>) -> Result<Option<mysql_async::SslOpts>> {
        if self.mode == TlsMode::Disable {
            return Ok(None);
        }
        let (chain, hostname) = self.checks();
        let mut opts = mysql_async::SslOpts::default()
            .with_danger_accept_invalid_certs(!chain)
            .with_danger_skip_domain_validation(!hostname)
            .with_danger_tls_hostname_override(verify_as.map(str::to_string));
        if let Some(ca) = self.ca()? {
            read(&ca)?; // the driver reads it later; a missing file is clearer reported here
            opts = opts.with_root_certs(vec![ca.into()]);
        }
        if let Some((cert, key)) = self.client_identity()? {
            read(&cert)?;
            read(&key)?;
            opts = opts.with_client_identity(Some(mysql_async::ClientIdentity::new(cert.into(), key.into())));
        }
        Ok(Some(opts))
    }

    /// Settings from a legacy URL parameter (`sslmode=…` for Postgres, `require_ssl=true` for MySQL), which
    /// connections saved before TLS had its own settings carry.
    pub fn from_legacy_param(key: &str, value: &str) -> Option<TlsMode> {
        Some(match (key, value) {
            ("sslmode", "disable") | ("require_ssl", "false") => TlsMode::Disable,
            ("sslmode", "prefer") => TlsMode::Prefer,
            ("sslmode", "require") | ("require_ssl", "true") => TlsMode::Require,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Tls, TlsMode};

    #[test]
    fn checks_should_only_encrypt_for_prefer_and_require() {
        for mode in [TlsMode::Prefer, TlsMode::Require] {
            assert_eq!(Tls { mode, ..Tls::default() }.checks(), (false, false), "{mode:?}");
        }
    }

    #[test]
    fn checks_should_verify_the_chain_when_a_ca_is_given() {
        let tls = Tls { mode: TlsMode::Require, ca: "/ca.pem".into(), ..Tls::default() };
        assert_eq!(tls.checks(), (true, false));
    }

    #[test]
    fn checks_should_verify_the_host_only_for_verify_full() {
        assert_eq!(Tls { mode: TlsMode::VerifyCa, ..Tls::default() }.checks(), (true, false));
        assert_eq!(Tls { mode: TlsMode::VerifyFull, ..Tls::default() }.checks(), (true, true));
    }

    #[test]
    fn postgres_should_refuse_a_certificate_without_its_key() {
        let tls = Tls { mode: TlsMode::Require, cert: "/client.pem".into(), ..Tls::default() };
        let err = tls.postgres().err().unwrap();
        assert_eq!(err.to_string(), "a client certificate needs both the certificate file and its key file");
    }

    #[test]
    fn mysql_should_report_a_missing_ca_file() {
        let tls = Tls { mode: TlsMode::VerifyCa, ca: "/no/such/ca.pem".into(), ..Tls::default() };
        let err = tls.mysql(None).err().unwrap();
        assert!(err.to_string().starts_with("can't read /no/such/ca.pem:"), "{err}");
    }
}

/// Require's certificate check for rustls: none, as libpq's `require` (the signature is still checked, so the
/// connection is encrypted to whoever holds the certificate's key).
#[derive(Debug)]
struct NoCheck(std::sync::Arc<rustls::crypto::CryptoProvider>);

impl Default for NoCheck {
    fn default() -> Self {
        NoCheck(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
    }
}

impl rustls::client::danger::ServerCertVerifier for NoCheck {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

//! TLS via rustls with the ring provider.
//!
//! Trust anchors: the Mozilla root program (webpki-roots) plus any extra PEM
//! bundles (`--extra-ca`, `SSL_CERT_FILE`). A certificate the user explicitly
//! accepted ("Accept the Risk and Continue") is trusted for the session, but only
//! for that host and only that exact certificate (its DER bytes), like a
//! certificate override; handshake signatures are still verified.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::CertificateError;
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

/// A certificate that failed verification: what the certificate error page shows and what
/// "Accept the Risk and Continue" pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateProblem {
    /// The host name (or IP address) the certificate was presented for.
    pub host: String,
    /// The end-entity certificate, DER-encoded.
    pub der: Vec<u8>,
    /// A short stable code, e.g. `CERT_EXPIRED`.
    pub code: &'static str,
    /// A human-readable reason, e.g. "the certificate expired on 2026-07-01".
    pub reason: String,
}

/// Exceptions (host, DER) and the most recent failure per host.
#[derive(Debug, Default)]
struct CertState {
    exceptions: RwLock<HashSet<(String, Vec<u8>)>>,
    failures: Mutex<HashMap<String, CertificateProblem>>,
}

#[derive(Debug)]
struct ExceptionVerifier {
    inner: Arc<WebPkiServerVerifier>,
    state: Arc<CertState>,
}

/// Civil date (UTC) of a Unix time, as YYYY-MM-DD (H. Hinnant's days-from-civil inverse).
fn date(unix: UnixTime) -> String {
    let days = (unix.as_secs() / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A stable code and a readable reason for a certificate error.
pub(crate) fn describe(e: &CertificateError, host: &str) -> (&'static str, String) {
    use CertificateError as C;
    match e {
        C::ExpiredContext { not_after, .. } => {
            ("CERT_EXPIRED", format!("the certificate expired on {}", date(*not_after)))
        }
        C::Expired => ("CERT_EXPIRED", "the certificate has expired".into()),
        C::NotValidYetContext { not_before, .. } => {
            ("CERT_NOT_YET_VALID", format!("the certificate isn't valid until {}", date(*not_before)))
        }
        C::NotValidYet => ("CERT_NOT_YET_VALID", "the certificate isn't valid yet".into()),
        C::UnknownIssuer => (
            "CERT_UNKNOWN_ISSUER",
            "the certificate wasn't issued by a trusted authority (it may be self-signed)".into(),
        ),
        C::NotValidForNameContext { presented, .. } if !presented.is_empty() => (
            "CERT_WRONG_NAME",
            format!("the certificate is only valid for {}, not {host}", presented.join(", ")),
        ),
        C::NotValidForName | C::NotValidForNameContext { .. } => {
            ("CERT_WRONG_NAME", format!("the certificate isn't valid for {host}"))
        }
        C::Revoked => ("CERT_REVOKED", "the certificate has been revoked".into()),
        C::BadSignature
        | C::UnsupportedSignatureAlgorithmContext { .. }
        | C::UnsupportedSignatureAlgorithmForPublicKeyContext { .. } => {
            ("CERT_BAD_SIGNATURE", "the certificate's signature can't be verified".into())
        }
        C::BadEncoding => ("CERT_BAD_ENCODING", "the certificate is malformed".into()),
        C::InvalidPurpose | C::InvalidPurposeContext { .. } => {
            ("CERT_WRONG_PURPOSE", "the certificate isn't meant for websites".into())
        }
        C::UnhandledCriticalExtension => (
            "CERT_UNSUPPORTED",
            "the certificate uses a critical extension BannedBrowser doesn't support".into(),
        ),
        other => ("CERT_INVALID", format!("the certificate couldn't be verified ({other:?})")),
    }
}

impl ServerCertVerifier for ExceptionVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match self.inner.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now) {
            Ok(v) => Ok(v),
            Err(e) => {
                let name = server_name.to_str().into_owned();
                let key = (name.clone(), end_entity.as_ref().to_vec());
                let allowed = self.state.exceptions.read().map(|s| s.contains(&key)).unwrap_or(false);
                if allowed {
                    log::warn!("accepting untrusted certificate for {name} (user exception): {e}");
                    return Ok(ServerCertVerified::assertion());
                }
                if let rustls::Error::InvalidCertificate(c) = &e {
                    let (code, reason) = describe(c, &name);
                    let problem = CertificateProblem { host: name.clone(), der: key.1, code, reason };
                    if let Ok(mut f) = self.state.failures.lock() {
                        f.insert(name, problem);
                    }
                }
                Err(e)
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

pub(crate) struct TlsContext {
    pub config: Option<Arc<ClientConfig>>,
    state: Arc<CertState>,
}

impl TlsContext {
    pub fn new(extra_ca_files: &[std::path::PathBuf]) -> Self {
        let state = Arc::new(CertState::default());
        let config = Self::build(extra_ca_files, state.clone());
        if config.is_none() {
            log::error!("TLS initialisation failed; https: will be unavailable");
        }
        TlsContext { config, state }
    }

    fn build(extra: &[std::path::PathBuf], state: Arc<CertState>) -> Option<Arc<ClientConfig>> {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for path in extra {
            match CertificateDer::pem_file_iter(path) {
                Ok(iter) => {
                    let mut n = 0;
                    for cert in iter.flatten() {
                        if roots.add(cert).is_ok() {
                            n += 1;
                        }
                    }
                    log::info!("loaded {n} extra CA certificate(s) from {}", path.display());
                }
                Err(e) => log::warn!("cannot read CA file {}: {e:?}", path.display()),
            }
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let inner =
            WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build().ok()?;
        let verifier = Arc::new(ExceptionVerifier { inner, state });
        let mut cfg = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .ok()?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Some(Arc::new(cfg))
    }

    /// Trust exactly this certificate for this host for the rest of the session.
    pub fn add_exception(&self, host: &str, der: &[u8]) {
        if let Ok(mut s) = self.state.exceptions.write() {
            s.insert((host.to_string(), der.to_vec()));
        }
    }

    /// The certificate that most recently failed verification for `host`.
    pub fn take_failure(&self, host: &str) -> Option<CertificateProblem> {
        self.state.failures.lock().ok()?.remove(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(date(UnixTime::since_unix_epoch(std::time::Duration::from_secs(0))), "1970-01-01");
        // 2026-07-01T12:00:00Z
        assert_eq!(
            date(UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_782_907_200))),
            "2026-07-01"
        );
        // 2000-02-29 (leap day) and 2100-03-01.
        assert_eq!(
            date(UnixTime::since_unix_epoch(std::time::Duration::from_secs(951_782_400))),
            "2000-02-29"
        );
        assert_eq!(
            date(UnixTime::since_unix_epoch(std::time::Duration::from_secs(4_107_542_400))),
            "2100-03-01"
        );
    }

    #[test]
    fn readable_reasons() {
        let t = |s| UnixTime::since_unix_epoch(std::time::Duration::from_secs(s));
        let (code, text) = describe(
            &CertificateError::ExpiredContext { time: t(1_790_000_000), not_after: t(1_782_907_200) },
            "example.com",
        );
        assert_eq!((code, text.as_str()), ("CERT_EXPIRED", "the certificate expired on 2026-07-01"));
        let (code, text) = describe(
            &CertificateError::NotValidForNameContext {
                expected: ServerName::try_from("example.com").unwrap().to_owned(),
                presented: vec!["a.example".into(), "b.example".into()],
            },
            "example.com",
        );
        assert_eq!(code, "CERT_WRONG_NAME");
        assert_eq!(text, "the certificate is only valid for a.example, b.example, not example.com");
        assert_eq!(describe(&CertificateError::UnknownIssuer, "x").0, "CERT_UNKNOWN_ISSUER");
    }
}

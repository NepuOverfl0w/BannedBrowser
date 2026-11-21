//! TLS via rustls with the ring provider.
//!
//! Trust anchors: the Mozilla root program (webpki-roots) plus any extra PEM
//! bundles (`--extra-ca`, `SSL_CERT_FILE`). Hosts the user explicitly accepted
//! ("Accept the Risk and Continue") skip certificate validation for the
//! session; signatures are still verified.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};

#[derive(Debug)]
struct ExceptionVerifier {
    inner: Arc<WebPkiServerVerifier>,
    exceptions: Arc<RwLock<HashSet<String>>>,
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
                let name = server_name.to_str();
                let allowed = self.exceptions.read().map(|s| s.contains(name.as_ref())).unwrap_or(false);
                if allowed {
                    log::warn!("accepting untrusted certificate for {name} (user exception): {e}");
                    Ok(ServerCertVerified::assertion())
                } else {
                    Err(e)
                }
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
    exceptions: Arc<RwLock<HashSet<String>>>,
}

impl TlsContext {
    pub fn new(extra_ca_files: &[std::path::PathBuf]) -> Self {
        let exceptions = Arc::new(RwLock::new(HashSet::new()));
        let config = Self::build(extra_ca_files, exceptions.clone());
        if config.is_none() {
            log::error!("TLS initialisation failed; https: will be unavailable");
        }
        TlsContext { config, exceptions }
    }

    fn build(
        extra: &[std::path::PathBuf],
        exceptions: Arc<RwLock<HashSet<String>>>,
    ) -> Option<Arc<ClientConfig>> {
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
        let verifier = Arc::new(ExceptionVerifier { inner, exceptions });
        let mut cfg = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .ok()?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Some(Arc::new(cfg))
    }

    pub fn add_exception(&self, host: &str) {
        if let Ok(mut s) = self.exceptions.write() {
            s.insert(host.to_string());
        }
    }
}

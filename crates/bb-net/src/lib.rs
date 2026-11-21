//! `bb-net` — BannedBrowser's networking layer.
//!
//! * HTTP/1.1 client with keep-alive connection pooling, chunked transfer
//!   coding, `Content-Length`, and `gzip`/`deflate`/`br` content codings.
//! * HTTPS through `rustls` (ring provider, webpki roots + extra CA files).
//! * HTTP `CONNECT` proxies from `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY`.
//! * Redirects (up to 20), timeouts and cancellation.
//! * `file:`, `data:` and `about:` schemes.
//! * Encoding detection for HTML (BOM → Content-Type → `<meta>` prescan →
//!   fallback), see [`encoding`].
#![forbid(unsafe_code)]

pub mod data_url;
pub mod encoding;
mod file;
pub mod headers;
mod http;
pub mod mime;
mod proxy;
mod tls;

pub use headers::Headers;
pub use mime::{parse_content_type, ContentType};

use bb_url::Url;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The User-Agent string BannedBrowser sends.
#[cfg(target_os = "windows")]
pub const USER_AGENT: &str =
    concat!("Mozilla/5.0 (Windows NT 10.0; Win64; x64) BannedBrowser/", env!("CARGO_PKG_VERSION"));
/// The User-Agent string BannedBrowser sends.
#[cfg(target_os = "macos")]
pub const USER_AGENT: &str =
    concat!("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) BannedBrowser/", env!("CARGO_PKG_VERSION"));
/// The User-Agent string BannedBrowser sends.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub const USER_AGENT: &str =
    concat!("Mozilla/5.0 (X11; Linux x86_64) BannedBrowser/", env!("CARGO_PKG_VERSION"));

/// What kind of failure a fetch hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetErrorKind {
    InvalidUrl,
    UnsupportedScheme,
    Dns,
    Connect,
    /// TLS handshake failed because the certificate is not trusted.
    Certificate,
    Tls,
    Timeout,
    Cancelled,
    Protocol,
    TooManyRedirects,
    FileNotFound,
    Io,
    /// Blocked by policy (e.g. tracking protection, HTTPS-only).
    Blocked,
}

/// A fetch error with a human-readable message.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct NetError {
    pub kind: NetErrorKind,
    pub message: String,
}

impl NetError {
    pub fn new(kind: NetErrorKind, message: impl Into<String>) -> Self {
        NetError { kind, message: message.into() }
    }
}

pub type NetResult<T> = Result<T, NetError>;

/// A cancellation token shared between the requester and the fetch.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Progress callback: (bytes received so far, total if known).
pub type ProgressFn = Arc<dyn Fn(u64, Option<u64>) + Send + Sync>;

/// An outgoing request.
#[derive(Clone)]
pub struct Request {
    pub url: Url,
    pub method: String,
    pub headers: Headers,
    pub body: Option<Vec<u8>>,
    /// Follow redirects (up to [`MAX_REDIRECTS`]).
    pub follow_redirects: bool,
    /// Overall deadline for the whole fetch.
    pub timeout: Duration,
    pub cancel: CancelToken,
    pub progress: Option<ProgressFn>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request").field("method", &self.method).field("url", &self.url).finish()
    }
}

impl Request {
    pub fn get(url: Url) -> Self {
        Request {
            url,
            method: "GET".into(),
            headers: Headers::new(),
            body: None,
            follow_redirects: true,
            timeout: Duration::from_secs(60),
            cancel: CancelToken::new(),
            progress: None,
        }
    }

    pub fn post(url: Url, content_type: &str, body: Vec<u8>) -> Self {
        let mut r = Self::get(url);
        r.method = "POST".into();
        r.headers.set("Content-Type", content_type);
        r.body = Some(body);
        r
    }
}

/// A received response (after redirects and content decoding).
#[derive(Debug, Clone)]
pub struct Response {
    /// Final URL after redirects.
    pub url: Url,
    /// Every URL visited, in order (first is the request URL).
    pub url_list: Vec<Url>,
    pub status: u16,
    pub status_text: String,
    pub headers: Headers,
    /// Decoded body (content codings removed).
    pub body: Vec<u8>,
    /// Whether the response came over a TLS connection.
    pub secure: bool,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Parsed `Content-Type`, if present.
    pub fn content_type(&self) -> Option<ContentType> {
        self.headers.get("content-type").map(parse_content_type)
    }

    pub(crate) fn synthetic(url: Url, status: u16, content_type: &str, body: Vec<u8>) -> Response {
        let mut headers = Headers::new();
        headers.set("Content-Type", content_type);
        headers.set("Content-Length", &body.len().to_string());
        Response {
            url_list: vec![url.clone()],
            url,
            status,
            status_text: if status == 200 { "OK".into() } else { String::new() },
            headers,
            body,
            secure: false,
        }
    }
}

/// Maximum redirects followed (Fetch §4.4 step 5 uses 20).
pub const MAX_REDIRECTS: usize = 20;

/// Handler for `about:` URLs: given the path (e.g. `"blank"`), returns
/// `(content_type, body)`.
pub type AboutHandler = Arc<dyn Fn(&str) -> Option<(String, Vec<u8>)> + Send + Sync>;

/// Client configuration.
#[derive(Clone, Default)]
pub struct ClientConfig {
    /// Extra PEM CA bundles to trust in addition to the webpki roots.
    pub extra_ca_files: Vec<std::path::PathBuf>,
    /// Ignore proxy environment variables.
    pub no_proxy_env: bool,
    pub about_handler: Option<AboutHandler>,
    /// Send `DNT: 1` and `Sec-GPC: 1`.
    pub do_not_track: bool,
    /// Override the `User-Agent` header (default: [`USER_AGENT`]).
    pub user_agent: Option<String>,
}

impl ClientConfig {
    /// The `User-Agent` this client sends.
    pub fn user_agent(&self) -> &str {
        self.user_agent.as_deref().unwrap_or(USER_AGENT)
    }
}

/// A shareable HTTP client with a keep-alive connection pool.
pub struct Client {
    config: ClientConfig,
    tls: tls::TlsContext,
    proxies: proxy::ProxyConfig,
    pool: Mutex<http::Pool>,
}

impl Client {
    pub fn new(config: ClientConfig) -> Self {
        let mut ca_files = config.extra_ca_files.clone();
        // Honour the conventional SSL_CERT_FILE variable (corporate / test CAs).
        if let Some(p) = std::env::var_os("SSL_CERT_FILE") {
            ca_files.push(p.into());
        }
        let tls = tls::TlsContext::new(&ca_files);
        let proxies =
            if config.no_proxy_env { proxy::ProxyConfig::default() } else { proxy::ProxyConfig::from_env() };
        Client { config, tls, proxies, pool: Mutex::new(http::Pool::default()) }
    }

    /// Accept an untrusted certificate for `host` for the rest of the session
    /// (the "Accept the Risk and Continue" button).
    pub fn add_certificate_exception(&self, host: &str) {
        self.tls.add_exception(host);
    }

    /// Perform a fetch, following redirects if requested.
    pub fn fetch(&self, req: Request) -> NetResult<Response> {
        let deadline = std::time::Instant::now() + req.timeout;
        let mut req = req;
        let mut url_list = vec![req.url.clone()];
        for _ in 0..=MAX_REDIRECTS {
            if req.cancel.is_cancelled() {
                return Err(NetError::new(NetErrorKind::Cancelled, "cancelled"));
            }
            let mut resp = self.fetch_one(&req, deadline)?;
            let is_redirect = matches!(resp.status, 301 | 302 | 303 | 307 | 308);
            if !(req.follow_redirects && is_redirect) {
                resp.url_list = url_list;
                return Ok(resp);
            }
            let Some(loc) = resp.headers.get("location") else {
                resp.url_list = url_list;
                return Ok(resp);
            };
            let next = req.url.join(loc.trim()).map_err(|e| {
                NetError::new(NetErrorKind::Protocol, format!("bad redirect location {loc:?}: {e}"))
            })?;
            // Fetch §4.4 HTTP-redirect fetch, steps 11–13.
            if (matches!(resp.status, 301 | 302) && req.method == "POST")
                || (resp.status == 303 && req.method != "GET" && req.method != "HEAD")
            {
                req.method = "GET".into();
                req.body = None;
                req.headers.remove("content-type");
                req.headers.remove("content-length");
            }
            if !next.origin().same_origin(&req.url.origin()) {
                req.headers.remove("authorization");
            }
            log::debug!("redirect {} -> {}", req.url, next);
            req.url = next.clone();
            url_list.push(next);
        }
        Err(NetError::new(NetErrorKind::TooManyRedirects, "too many redirects"))
    }

    fn fetch_one(&self, req: &Request, deadline: std::time::Instant) -> NetResult<Response> {
        match req.url.scheme() {
            "http" | "https" => http::fetch(self, req, deadline),
            "data" => data_url::fetch(&req.url),
            "file" => file::fetch(&req.url),
            "about" => {
                let path = req.url.pathname();
                if let Some(h) = &self.config.about_handler {
                    if let Some((ct, body)) = h(&path) {
                        return Ok(Response::synthetic(req.url.clone(), 200, &ct, body));
                    }
                }
                if path == "blank" {
                    return Ok(Response::synthetic(
                        req.url.clone(),
                        200,
                        "text/html; charset=utf-8",
                        Vec::new(),
                    ));
                }
                Err(NetError::new(NetErrorKind::FileNotFound, format!("unknown page about:{path}")))
            }
            s => Err(NetError::new(NetErrorKind::UnsupportedScheme, format!("unsupported scheme {s}:"))),
        }
    }
}

/// Convenience: fetch a URL with a fresh default client.
pub fn get(url: &str) -> NetResult<Response> {
    let url = Url::parse(url).map_err(|e| NetError::new(NetErrorKind::InvalidUrl, e.to_string()))?;
    Client::new(ClientConfig::default()).fetch(Request::get(url))
}

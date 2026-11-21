//! Integration tests against `tests/server/testserver.py` (needs python3).

use bb_net::{Client, ClientConfig, NetErrorKind, Request};
use bb_url::Url;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

struct Server {
    _child: Child,
    port: u16,
    https_port: u16,
}

fn repo() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn server() -> &'static Server {
    static S: OnceLock<Server> = OnceLock::new();
    S.get_or_init(|| {
        let py = std::env::var("PYTHON").unwrap_or_else(|_| {
            if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            }
        });
        let mut child = Command::new(py)
            .arg(repo().join("tests/server/testserver.py"))
            .args(["--port", "0", "--https-port", "0", "--exit-on-stdin-eof"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("failed to start python3 test server");
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let (mut port, mut https_port) = (0, 0);
        let mut line = String::new();
        while port == 0 || https_port == 0 {
            line.clear();
            if out.read_line(&mut line).unwrap() == 0 {
                panic!("test server exited early");
            }
            if let Some(p) = line.trim().strip_prefix("HTTPS_PORT ") {
                https_port = p.parse().unwrap();
            } else if let Some(p) = line.trim().strip_prefix("PORT ") {
                port = p.parse().unwrap();
            }
        }
        Server { _child: child, port, https_port }
    })
}

fn client() -> Client {
    Client::new(ClientConfig {
        extra_ca_files: vec![repo().join("tests/certs/ca.pem")],
        no_proxy_env: true,
        ..Default::default()
    })
}

fn url(path: &str) -> Url {
    Url::parse(&format!("http://127.0.0.1:{}{path}", server().port)).unwrap()
}

fn get(c: &Client, path: &str) -> bb_net::Response {
    c.fetch(Request::get(url(path))).unwrap_or_else(|e| panic!("GET {path}: {e}"))
}

#[test]
fn plain_get_with_content_length() {
    let r = get(&client(), "/hello");
    assert_eq!(r.status, 200);
    assert_eq!(r.body, b"Hello, BannedBrowser!");
    assert_eq!(r.content_type().unwrap().essence, "text/plain");
}

#[test]
fn redirects_are_followed() {
    let r = get(&client(), "/redirect/5");
    assert_eq!(r.status, 200);
    assert_eq!(r.body, b"done");
    assert_eq!(r.url.pathname(), "/redirect/0");
    assert_eq!(r.url_list.len(), 6);
}

#[test]
fn redirect_limit() {
    let e = client().fetch(Request::get(url("/redirect-loop"))).unwrap_err();
    assert_eq!(e.kind, NetErrorKind::TooManyRedirects);
    let ok = get(&client(), "/redirect/20");
    assert_eq!(ok.body, b"done");
    let e = client().fetch(Request::get(url("/redirect/21"))).unwrap_err();
    assert_eq!(e.kind, NetErrorKind::TooManyRedirects);
}

#[test]
fn redirect_without_following() {
    let mut req = Request::get(url("/redirect/1"));
    req.follow_redirects = false;
    let r = client().fetch(req).unwrap();
    assert_eq!(r.status, 302);
    assert_eq!(r.headers.get("location"), Some("/redirect/0"));
}

#[test]
fn post_303_becomes_get_and_307_keeps_post() {
    let c = client();
    let r = c.fetch(Request::post(url("/redirect-303"), "text/plain", b"payload".to_vec())).unwrap();
    let body = String::from_utf8(r.body).unwrap();
    assert!(body.contains("\"method\": \"GET\""), "{body}");
    let r = c.fetch(Request::post(url("/redirect-307"), "text/plain", b"payload".to_vec())).unwrap();
    let body = String::from_utf8(r.body).unwrap();
    assert!(body.contains("\"method\": \"POST\"") && body.contains("payload"), "{body}");
}

#[test]
fn chunked_body() {
    let r = get(&client(), "/chunked");
    let mut expect = b"Hello, chunked world".to_vec();
    expect.extend(std::iter::repeat_n(b'!', 1000));
    assert_eq!(r.body, expect);
}

#[test]
fn content_codings() {
    let c = client();
    assert_eq!(get(&c, "/gzip").body, b"gzip body ".repeat(100));
    assert_eq!(get(&c, "/deflate").body, b"deflate body ".repeat(100));
    assert_eq!(get(&c, "/br").body, b"Hello, brotli! ".repeat(20));
}

#[test]
fn not_found_and_server_errors() {
    let c = client();
    let r = get(&c, "/does-not-exist.html");
    assert_eq!(r.status, 404);
    assert!(!r.ok());
    assert_eq!(get(&c, "/status/500").status, 500);
}

#[test]
fn keep_alive_reuses_connection() {
    let c = client();
    let a = get(&c, "/conn-id").body;
    let b = get(&c, "/conn-id").body;
    assert_eq!(a, b, "second request should reuse the pooled connection");
}

#[test]
fn connection_close_body_until_eof() {
    let c = client();
    assert_eq!(get(&c, "/close").body, b"until close ".repeat(10));
    // The client must not reuse the closed connection.
    assert_eq!(get(&c, "/hello").body, b"Hello, BannedBrowser!");
}

#[test]
fn static_files_and_head() {
    let c = client();
    let r = get(&c, "/index.html");
    assert!(String::from_utf8_lossy(&r.body).contains("BannedBrowser test pages"));
    let mut head = Request::get(url("/index.html"));
    head.method = "HEAD".into();
    let r = c.fetch(head).unwrap();
    assert_eq!(r.status, 200);
    assert!(r.body.is_empty());
}

#[test]
fn timeout_and_cancel() {
    let c = client();
    let mut req = Request::get(url("/slow?3"));
    req.timeout = Duration::from_millis(500);
    let e = c.fetch(req).unwrap_err();
    assert_eq!(e.kind, NetErrorKind::Timeout);

    let req = Request::get(url("/slow?3"));
    let token = req.cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        token.cancel();
    });
    let t0 = std::time::Instant::now();
    let e = c.fetch(req).unwrap_err();
    assert_eq!(e.kind, NetErrorKind::Cancelled);
    assert!(t0.elapsed() < Duration::from_secs(2));
}

#[test]
fn charset_detection_from_header() {
    let r = get(&client(), "/charset/shift_jis");
    let ct = r.content_type().unwrap();
    let (text, enc) = bb_net::encoding::decode_html(&r.body, ct.charset.as_deref());
    assert_eq!(enc, encoding_rs_shift_jis());
    assert!(text.contains("日本語"), "{text}");
    let r = get(&client(), "/charset/iso-8859-1");
    let ct = r.content_type().unwrap();
    let (text, _) = bb_net::encoding::decode_html(&r.body, ct.charset.as_deref());
    assert!(text.contains("café") && text.contains("naïve"), "{text}");
}

fn encoding_rs_shift_jis() -> &'static bb_net::encoding::Encoding {
    bb_net::encoding::encoding_for_label("shift_jis").unwrap()
}

#[test]
fn https_with_extra_ca() {
    let u = Url::parse(&format!("https://127.0.0.1:{}/hello", server().https_port)).unwrap();
    let r = client().fetch(Request::get(u.clone())).unwrap();
    assert!(r.secure);
    assert_eq!(r.body, b"Hello, BannedBrowser!");
    // Without the extra CA the certificate is rejected...
    let strict = Client::new(ClientConfig { no_proxy_env: true, ..Default::default() });
    let e = strict.fetch(Request::get(u.clone())).unwrap_err();
    assert_eq!(e.kind, NetErrorKind::Certificate, "{e}");
    // ...unless the user accepted the risk for that host.
    strict.add_certificate_exception("127.0.0.1");
    assert_eq!(strict.fetch(Request::get(u)).unwrap().body, b"Hello, BannedBrowser!");
}

#[test]
fn data_file_and_about() {
    let c = client();
    let r = c.fetch(Request::get(Url::parse("data:text/plain,hi%20there").unwrap())).unwrap();
    assert_eq!(r.body, b"hi there");
    let path = repo().join("tests/pages/index.html").canonicalize().unwrap();
    let fu = Url::from_file_path(&path.to_string_lossy()).unwrap();
    let r = c.fetch(Request::get(fu)).unwrap();
    assert!(r.content_type().unwrap().is_html());
    let r = c.fetch(Request::get(Url::parse("about:blank").unwrap())).unwrap();
    assert!(r.body.is_empty());
    let e = c.fetch(Request::get(Url::parse("about:nonexistent").unwrap())).unwrap_err();
    assert_eq!(e.kind, NetErrorKind::FileNotFound);
    let e = c.fetch(Request::get(Url::parse("gopher://x/").unwrap())).unwrap_err();
    assert_eq!(e.kind, NetErrorKind::UnsupportedScheme);
}

#[test]
fn connection_refused() {
    // Port 9 (discard) is almost certainly closed on 127.0.0.1.
    let e = client().fetch(Request::get(Url::parse("http://127.0.0.1:9/").unwrap())).unwrap_err();
    assert!(matches!(e.kind, NetErrorKind::Connect | NetErrorKind::Timeout), "{e:?}");
}

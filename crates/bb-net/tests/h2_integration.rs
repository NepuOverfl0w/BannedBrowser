//! HTTP/2 against an independent server implementation (`tests/server/h2server.py`, built on
//! the Python `h2` package: `pip install h2`). ALPN, HPACK, flow control both ways,
//! multiplexing under SETTINGS_MAX_CONCURRENT_STREAMS, RST_STREAM REFUSED_STREAM retries,
//! GOAWAY, PING, trailers, CONTINUATION, malformed responses.

use bb_net::{Client, ClientConfig, NetErrorKind, Request};
use bb_url::Url;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

struct Server {
    _child: Child,
    port: u16,
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
            .args(["--port", "0", "--h2-port", "0", "--exit-on-stdin-eof"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("failed to start the python3 test server");
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        let mut port = 0;
        while port == 0 {
            line.clear();
            if out.read_line(&mut line).unwrap() == 0 {
                panic!("test server exited early");
            }
            if let Some(p) = line.trim().strip_prefix("H2_PORT ") {
                port = p.parse().unwrap();
            } else if let Some(why) = line.trim().strip_prefix("H2_UNAVAILABLE") {
                panic!(
                    "the HTTP/2 test server needs the Python h2 package (pip install -r tests/server/requirements.txt):{why}"
                );
            }
        }
        // Keep draining stdout so the server never blocks on it.
        std::thread::spawn(move || {
            let mut l = String::new();
            while out.read_line(&mut l).map(|n| n > 0).unwrap_or(false) {
                l.clear();
            }
        });
        Server { _child: child, port }
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
    Url::parse(&format!("https://127.0.0.1:{}{path}", server().port)).unwrap()
}

fn get(c: &Client, path: &str) -> bb_net::Response {
    c.fetch(Request::get(url(path))).unwrap_or_else(|e| panic!("GET {path}: {e}"))
}

/// Which server connection answered.
fn conn_id(c: &Client) -> String {
    String::from_utf8(get(c, "/conn-id").body).unwrap()
}

#[test]
fn hello_over_h2() {
    let r = get(&client(), "/hello");
    assert_eq!((r.version, r.status), ("HTTP/2", 200));
    assert_eq!(r.body, b"Hello, HTTP/2!");
    assert!(r.secure && r.complete);
    assert_eq!(r.headers.get("content-type"), Some("text/plain"));
}

#[test]
fn request_headers_and_post_bodies() {
    let c = client();
    let mut req = Request::get(url("/echo?a=1&b=%20"));
    req.headers.set("X-Custom", "value");
    req.headers.set("Connection", "keep-alive"); // connection-specific: must not be sent
    let r = c.fetch(req).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(v["method"], "GET");
    assert_eq!(v["path"], "/echo?a=1&b=%20");
    assert_eq!(v["headers"]["x-custom"], "value");
    assert!(v["headers"]["user-agent"].as_str().unwrap().contains("BannedBrowser"));
    assert_eq!(v["headers"][":authority"], format!("127.0.0.1:{}", server().port));
    assert!(v["headers"].get("connection").is_none());
    // A body bigger than the server's 64 KiB initial window: we must wait for WINDOW_UPDATEs.
    let body: Vec<u8> = (0..300_000u32).map(|i| b'a' + (i % 26) as u8).collect();
    let r = c.fetch(Request::post(url("/echo"), "text/plain", body.clone())).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(v["method"], "POST");
    assert_eq!(v["body"].as_str().unwrap().as_bytes(), &body[..]);
    assert_eq!(v["headers"]["content-length"], "300000");
}

#[test]
fn large_downloads_use_flow_control() {
    // 20 MB: more than our 16 MB connection window and 4 MB stream window.
    let n = 20 * 1024 * 1024;
    let r = get(&client(), &format!("/big?n={n}"));
    assert_eq!(r.body.len(), n);
    assert!(r.body.iter().enumerate().all(|(i, &b)| b == (i % 256) as u8));
}

#[test]
fn requests_share_one_connection_and_respect_max_concurrent_streams() {
    let c = Arc::new(client());
    let first = conn_id(&c);
    let start = Instant::now();
    let handles: Vec<_> = (0..20)
        .map(|_| {
            let c = c.clone();
            std::thread::spawn(move || get(&c, "/slow?ms=300").body)
        })
        .collect();
    for h in handles {
        assert_eq!(
            String::from_utf8(h.join().unwrap()).unwrap(),
            format!("slow {first}"),
            "all on one connection"
        );
    }
    let took = start.elapsed();
    // 20 requests, 5 at a time (the server's limit), 300 ms each: about 1.2 s, not 6 s.
    assert!(took.as_secs_f32() < 4.0, "took {took:?}");
}

#[test]
fn redirects_cookies_gzip_and_status_codes() {
    let c = client();
    let r = get(&c, "/redirect/3");
    assert_eq!((r.body.as_slice(), r.url_list.len()), (&b"done"[..], 4));
    let r = get(&c, "/set-cookie?c=h2%3Dyes%3B+Path%3D%2F&to=%2Fcookie-title");
    assert!(String::from_utf8_lossy(&r.body).contains("<title>h2=yes</title>"));
    let r = get(&c, "/gzip");
    assert_eq!(r.body, b"gzipped over h2 ".repeat(20));
    assert_eq!(get(&c, "/status/204").status, 204);
    assert_eq!(get(&c, "/status/404").status, 404);
}

#[test]
fn trailers_continuation_and_ping() {
    let c = client();
    let r = get(&c, "/trailers");
    assert_eq!(r.body, b"with trailers");
    assert_eq!(r.headers.get("x-checksum"), Some("abc123"));
    let r = get(&c, "/continuation");
    assert_eq!(r.body, b"big headers");
    assert_eq!(r.headers.get("x-filler-119").map(str::len), Some(200));
    // The server answers only after we ACK its PING.
    assert_eq!(get(&c, "/ping").body, b"pong");
}

#[test]
fn refused_streams_and_goaway_are_retried() {
    let c = client();
    assert_eq!(get(&c, "/refuse?id=retry-me").body, b"accepted");
    let before = conn_id(&c);
    assert_eq!(get(&c, "/goaway").body, b"bye");
    // The connection is gone; the next request opens a new one transparently.
    assert_eq!(get(&c, "/hello").body, b"Hello, HTTP/2!");
    assert_ne!(conn_id(&c), before);
}

#[test]
fn malformed_responses_are_errors() {
    let e = client().fetch(Request::get(url("/bad-length"))).unwrap_err();
    assert_eq!(e.kind, NetErrorKind::Protocol);
}

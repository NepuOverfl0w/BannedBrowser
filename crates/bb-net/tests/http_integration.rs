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
    // The error says which certificate failed and why, readably.
    let problem = *e.certificate.clone().expect("certificate problem attached");
    assert_eq!(problem.host, "127.0.0.1");
    assert_eq!(problem.code, "CERT_UNKNOWN_ISSUER", "{}", problem.reason);
    assert!(problem.reason.contains("trusted authority"), "{}", problem.reason);
    assert!(!problem.der.is_empty());
    // An exception for a *different* certificate on the same host doesn't help...
    let mut other = problem.clone();
    other.der[problem.der.len() / 2] ^= 0xFF;
    strict.add_certificate_exception(&other);
    assert_eq!(strict.fetch(Request::get(u.clone())).unwrap_err().kind, NetErrorKind::Certificate);
    // ...accepting the risk for exactly this certificate does.
    strict.add_certificate_exception(&problem);
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

#[test]
fn user_agent_default_and_override() {
    let body = String::from_utf8(get(&client(), "/echo").body).unwrap();
    assert!(body.contains(bb_net::USER_AGENT), "{body}");
    let custom = Client::new(ClientConfig {
        no_proxy_env: true,
        user_agent: Some("Mozilla/5.0 (test override)".into()),
        ..Default::default()
    });
    let body = String::from_utf8(get(&custom, "/echo").body).unwrap();
    assert!(body.contains("Mozilla/5.0 (test override)"), "{body}");
    assert!(!body.contains("BannedBrowser/"), "{body}");
}

fn echo_cookie(c: &Client, ctx: Option<bb_net::cookies::CookieContext>) -> Option<String> {
    let mut req = Request::get(url("/echo"));
    if let Some(ctx) = ctx {
        req.cookie_context = ctx;
    }
    let r = c.fetch(req).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    v["headers"]["cookie"].as_str().map(str::to_string)
}

fn set_cookie_url(cookies: &[&str], to: Option<&str>) -> Url {
    let enc = |s: &str| {
        let mut o = String::new();
        bb_url::percent::form_encode_into(s, &mut o);
        o
    };
    let mut q: Vec<String> = cookies.iter().map(|c| format!("c={}", enc(c))).collect();
    if let Some(t) = to {
        q.push(format!("to={}", enc(t)));
    }
    url(&format!("/set-cookie?{}", q.join("&")))
}

#[test]
fn cookies_round_trip_through_a_real_server() {
    let c = client();
    assert_eq!(echo_cookie(&c, None), None);
    // Set by a response, sent back on the next request.
    c.fetch(Request::get(set_cookie_url(&["a=1; Path=/", "b=2; Path=/; HttpOnly"], None))).unwrap();
    assert_eq!(echo_cookie(&c, None).as_deref(), Some("a=1; b=2"));
    // A redirect's Set-Cookie applies, and the redirected request already carries it.
    let r = c.fetch(Request::get(set_cookie_url(&["r=3; Path=/"], Some("/echo")))).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(v["headers"]["cookie"].as_str(), Some("a=1; b=2; r=3"));
    // Deleting with Max-Age=0.
    c.fetch(Request::get(set_cookie_url(&["a=; Path=/; Max-Age=0"], None))).unwrap();
    assert_eq!(echo_cookie(&c, None).as_deref(), Some("b=2; r=3"));
    // A request without credentials sends none.
    let mut req = Request::get(url("/echo"));
    req.credentials = false;
    let v: serde_json::Value = serde_json::from_slice(&c.fetch(req).unwrap().body).unwrap();
    assert!(v["headers"]["cookie"].is_null());
    // Another client (a private window) has its own jar.
    assert_eq!(echo_cookie(&client(), None), None);
}

#[test]
fn same_site_cookies_on_cross_site_subresources() {
    let c = client();
    c.fetch(Request::get(set_cookie_url(&["lax=1; Path=/; SameSite=Lax"], None))).unwrap();
    // An image on another site's page: cross-site subresource, Lax cookies stay home.
    let sub = bb_net::cookies::CookieContext {
        site_for_cookies: Some(Url::parse("https://elsewhere.example/").unwrap()),
        top_level_navigation: false,
        ..Default::default()
    };
    assert_eq!(echo_cookie(&c, Some(sub)), None);
    // The same request from our own page sends it.
    let own = bb_net::cookies::CookieContext {
        site_for_cookies: Some(url("/page.html")),
        top_level_navigation: false,
        ..Default::default()
    };
    assert_eq!(echo_cookie(&c, Some(own)).as_deref(), Some("lax=1"));
}

// ----- HTTP cache (RFC 9111) -----

fn cached_client(dir: Option<&std::path::Path>) -> Client {
    let cache = match dir {
        Some(d) => bb_net::cache::HttpCache::on_disk(d, 8 * 1024 * 1024),
        None => bb_net::cache::HttpCache::in_memory(8 * 1024 * 1024),
    };
    Client::new(ClientConfig {
        no_proxy_env: true,
        cache: Some(std::sync::Arc::new(cache)),
        ..Default::default()
    })
}

fn cget(c: &Client, path: &str, mode: bb_net::cache::CacheMode) -> bb_net::Response {
    let mut req = Request::get(url(path));
    req.cache_mode = mode;
    c.fetch(req).unwrap_or_else(|e| panic!("GET {path}: {e}"))
}

fn hits(r: &bb_net::Response) -> String {
    String::from_utf8_lossy(&r.body).into_owned()
}

use bb_net::cache::{CacheMode as M, CacheStatus as S};

#[test]
fn cache_fresh_responses_are_reused() {
    let c = cached_client(None);
    let p = "/cache?id=fresh&cc=max-age%3D600";
    let a = cget(&c, p, M::Default);
    assert_eq!((hits(&a).as_str(), a.cache), ("hits=1", S::Network));
    let b = cget(&c, p, M::Default);
    assert_eq!((hits(&b).as_str(), b.cache), ("hits=1", S::Hit));
    assert!(b.headers.get("age").is_some());
    // Reload (no-cache) revalidates: no validator here, so a full request.
    let r = cget(&c, p, M::NoCache);
    assert_eq!((hits(&r).as_str(), r.cache), ("hits=2", S::Network));
    // Hard reload ignores the entry but stores the response.
    let h = cget(&c, p, M::Reload);
    assert_eq!(hits(&h), "hits=3");
    assert_eq!(hits(&cget(&c, p, M::Default)), "hits=3");
    // no-store neither reads nor writes.
    assert_eq!(hits(&cget(&c, p, M::NoStore)), "hits=4");
    assert_eq!(hits(&cget(&c, p, M::Default)), "hits=3");
}

#[test]
fn cache_stale_responses_are_revalidated() {
    let c = cached_client(None);
    let p = "/cache?id=etag&cc=max-age%3D0&etag=v1";
    assert_eq!(hits(&cget(&c, p, M::Default)), "hits=1");
    let r = cget(&c, p, M::Default);
    // The server saw a second (conditional) request and answered 304; the body is the stored one.
    assert_eq!((r.status, hits(&r).as_str(), r.cache), (200, "hits=1", S::Revalidated));
    assert_eq!(r.headers.get("x-hits"), Some("2"), "freshened from the 304");
    // Last-Modified alone validates too.
    let p = "/cache?id=lm&cc=no-cache&lm=3600";
    cget(&c, p, M::Default);
    assert_eq!(cget(&c, p, M::Default).cache, S::Revalidated);
    // Back/Forward (force-cache) uses a stale entry without asking.
    assert_eq!(cget(&c, p, M::ForceCache).cache, S::Hit);
}

#[test]
fn cache_storability() {
    let c = cached_client(None);
    for (p, cached) in [
        ("/cache?id=s1&cc=no-store%2C+max-age%3D600", false),
        ("/cache?id=s2&cc=max-age%3D600&vary=*", false),
        ("/cache?id=s3", false), // 200 without freshness info: stored, but stale at once (no validator)
        ("/cache?id=s4&expires=600", true),
        ("/cache?id=s5&lm=100000", true), // heuristic freshness
        ("/cache?id=s6&status=302&location=%2Fhello", false),
        ("/cache?id=s7&status=404&cc=max-age%3D600", true),
    ] {
        let mut req = Request::get(url(p));
        req.follow_redirects = false;
        c.fetch(req.clone()).unwrap();
        let r = c.fetch(req).unwrap();
        assert_eq!(r.cache == S::Hit, cached, "{p}");
    }
}

#[test]
fn cache_vary_and_partitions() {
    let c = cached_client(None);
    let p = "/cache?id=vary&cc=max-age%3D600&vary=X-Lang";
    let mut req = Request::get(url(p));
    req.headers.set("X-Lang", "en");
    assert_eq!(hits(&c.fetch(req.clone()).unwrap()), "hits=1");
    assert_eq!(c.fetch(req.clone()).unwrap().cache, S::Hit);
    req.headers.set("X-Lang", "fr");
    assert_eq!(hits(&c.fetch(req.clone()).unwrap()), "hits=2");
    // The same URL requested by a page on another site is a separate entry.
    let p = "/cache?id=part&cc=max-age%3D600";
    assert_eq!(hits(&cget(&c, p, M::Default)), "hits=1");
    let mut other = Request::get(url(p));
    other.cookie_context.site_for_cookies = Some(Url::parse("https://other.example/").unwrap());
    assert_eq!(hits(&c.fetch(other.clone()).unwrap()), "hits=2");
    assert_eq!(c.fetch(other).unwrap().cache, S::Hit);
}

#[test]
fn cache_unsafe_methods_invalidate() {
    let c = cached_client(None);
    let p = "/cache?id=inv&cc=max-age%3D600";
    cget(&c, p, M::Default);
    assert_eq!(cget(&c, p, M::Default).cache, S::Hit);
    c.fetch(Request::post(url(p), "text/plain", b"x".to_vec())).unwrap();
    let r = cget(&c, p, M::Default);
    assert_eq!(r.cache, S::Network);
}

#[test]
fn cache_never_stores_or_replays_cookies() {
    let c = cached_client(None);
    let p = "/cache?id=ck&cc=max-age%3D600&cookie=cached%3D1";
    cget(&c, p, M::Default);
    assert!(c.cookies().all().iter().any(|k| k.name == "cached"));
    c.cookies().clear();
    let r = cget(&c, p, M::Default);
    assert_eq!(r.cache, S::Hit);
    assert!(r.headers.get("set-cookie").is_none());
    assert!(c.cookies().all().is_empty(), "a cache hit doesn't set cookies again");
}

#[test]
fn cache_persists_on_disk_and_survives_damage() {
    let dir = std::env::temp_dir().join(format!("bb-http-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let p = "/cache?id=disk&cc=max-age%3D600";
    {
        let c = cached_client(Some(&dir));
        assert_eq!(hits(&cget(&c, p, M::Default)), "hits=1");
    }
    let c = cached_client(Some(&dir));
    let r = cget(&c, p, M::Default);
    assert_eq!((hits(&r).as_str(), r.cache), ("hits=1", S::Hit), "read back from disk");
    // Damage every entry file: they read as misses and are replaced.
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        std::fs::write(e.path(), b"garbage").unwrap();
    }
    let c = cached_client(Some(&dir));
    assert_eq!(hits(&cget(&c, p, M::Default)), "hits=2");
    assert_eq!(cget(&c, p, M::Default).cache, S::Hit);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cache_evicts_least_recently_used() {
    let cache = std::sync::Arc::new(bb_net::cache::HttpCache::in_memory(80));
    let c =
        Client::new(ClientConfig { no_proxy_env: true, cache: Some(cache.clone()), ..Default::default() });
    // Each body is "hits=1" (6 bytes); at most 80/8 = 10 bytes per entry, 80 in total.
    for i in 0..20 {
        cget(&c, &format!("/cache?id=lru{i}&cc=max-age%3D600"), M::Default);
    }
    let (n, total) = cache.stats();
    assert!(total <= 80 && n <= 13, "{n} entries, {total} bytes");
    assert_eq!(cget(&c, "/cache?id=lru19&cc=max-age%3D600", M::Default).cache, S::Hit);
    assert_eq!(cget(&c, "/cache?id=lru0&cc=max-age%3D600", M::Default).cache, S::Network);
}

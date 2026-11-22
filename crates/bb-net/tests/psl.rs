//! The Public Suffix List: the list's own test vectors (`tests/test_psl.txt` from
//! github.com/publicsuffix/list, fetched by `scripts/fetch-spec-tests.sh`) against the list
//! embedded at build time, and the daily refresh downloading over HTTP.

use bb_net::psl;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

fn need_list() -> &'static str {
    let list = psl::embedded_list();
    assert!(
        !list.is_empty(),
        "no Public Suffix List was embedded: build online, or set BB_PSL_FILE to a copy of {}",
        psl::LIST_URL
    );
    list
}

/// `'text'` or `null` → `Some(text)` / `None`.
fn arg(s: &str) -> Option<Option<String>> {
    let s = s.trim();
    if s == "null" {
        return Some(None);
    }
    Some(Some(s.strip_prefix('\'')?.strip_suffix('\'')?.to_string()))
}

#[test]
fn official_test_vectors() {
    need_list();
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/psl/test_psl.txt");
    let text = std::fs::read_to_string(&file)
        .unwrap_or_else(|e| panic!("{}: {e}; run scripts/fetch-spec-tests.sh", file.display()));
    let (mut total, mut failures) = (0, Vec::new());
    for line in text.lines() {
        let Some(call) = line.trim().strip_prefix("checkPublicSuffix(") else { continue };
        let call = call.strip_suffix(");").expect("checkPublicSuffix(...); line");
        let (input, want) = call.split_once(',').expect("two arguments");
        let (Some(input), Some(want)) = (arg(input), arg(want)) else { panic!("bad line {line}") };
        total += 1;
        // The vectors use Unicode labels; we work on ASCII (punycode) domains, as the list's
        // algorithm says ("canonicalize the domain").
        let ascii = |d: &str| bb_url::idna::domain_to_ascii(d);
        let got = input.as_deref().and_then(ascii).and_then(|d| psl::registrable_domain(&d));
        let want = want.as_deref().and_then(ascii);
        if got != want {
            failures.push(format!("{input:?}: got {got:?}, want {want:?}"));
        }
    }
    println!("test_psl.txt: {}/{total} pass", total - failures.len());
    assert!(total > 50, "only {total} vectors found");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The refresh downloads over HTTP with the real client, installs a newer list, writes the
/// cache, and doesn't download again within a day.
#[test]
fn refresh_downloads_from_a_server() {
    let list = need_list();
    let old = list.lines().find_map(|l| l.strip_prefix("// VERSION:")).unwrap().trim().to_string();
    let body = list.replacen(&format!("// VERSION: {old}"), "// VERSION: 9990-01-01_00-00-00_UTC", 1);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        // Exactly one request is expected.
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let mut req = Vec::new();
        while !req.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = conn.read(&mut buf).unwrap();
            assert!(n > 0, "client closed early");
            req.extend_from_slice(&buf[..n]);
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        conn.write_all(head.as_bytes()).unwrap();
        conn.write_all(body.as_bytes()).unwrap();
        String::from_utf8_lossy(&req).into_owned()
    });
    let dir = std::env::temp_dir().join(format!("bb-psl-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let client = bb_net::Client::new(bb_net::ClientConfig { no_proxy_env: true, ..Default::default() });
    let url = format!("http://127.0.0.1:{port}/list/public_suffix_list.dat");
    let t0 = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
    let out = psl::refresh_from(&client, &url, &dir, t0);
    assert_eq!(out, psl::Refresh::Updated("9990-01-01_00-00-00_UTC".into()));
    let req = server.join().unwrap();
    assert!(req.starts_with("GET /list/public_suffix_list.dat HTTP/1.1\r\n"), "{req}");
    assert!(!req.to_ascii_lowercase().contains("\r\ncookie:"));
    assert!(psl::version().as_str() >= "9990-01-01_00-00-00_UTC");
    assert!(dir.join("public_suffix_list.dat").exists());
    // The server is gone; within a day nothing is fetched.
    let out = psl::refresh_from(&client, &url, &dir, t0 + Duration::from_secs(3600));
    assert_eq!(out, psl::Refresh::Fresh);
    let _ = std::fs::remove_dir_all(&dir);
}

//! WPT's IDNA tests (`url/resources/IdnaTestV2.json`, from Unicode's IdnaTestV2.txt, and
//! `url/resources/toascii.json`), as `url/IdnaTestV2.any.js` runs them: the host of
//! `https://<input>/x`, or a failure. They need `scripts/fetch-spec-tests.sh`.

use bb_url::Url;
use serde_json::Value;

fn run(file: &str) -> (usize, usize, Vec<String>) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../third_party/wpt/url/resources")
        .join(file);
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("skipping {file}: run scripts/fetch-spec-tests.sh");
        return (0, 0, Vec::new());
    };
    let cases: Vec<Value> = serde_json::from_str(&usv(&text)).expect("valid JSON");
    let (mut pass, mut total, mut fails) = (0, 0, Vec::new());
    for c in cases.iter().filter(|c| c.is_object()) {
        let input = c["input"].as_str().unwrap_or("");
        if input.is_empty() {
            continue;
        }
        total += 1;
        let got = Url::parse(&format!("https://{input}/x")).ok().map(|u| u.hostname());
        let want = c["output"].as_str().map(str::to_string);
        if got == want {
            pass += 1;
        } else if fails.len() < 40 {
            fails.push(format!("{input:?}: expected {want:?}, got {got:?}"));
        }
    }
    (pass, total, fails)
}

/// Lone surrogate escapes become U+FFFD, as `new URL` does when it converts its argument to a
/// USVString (serde_json refuses them).
fn usv(json: &str) -> String {
    let b = json.as_bytes();
    let hex = |i: usize| -> Option<u32> {
        if b.get(i) == Some(&b'\\') && b.get(i + 1) == Some(&b'u') {
            u32::from_str_radix(json.get(i + 2..i + 6)?, 16).ok()
        } else {
            None
        }
    };
    let mut out = String::with_capacity(json.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && b.get(i + 1) == Some(&b'\\') {
            out.push_str("\\\\");
            i += 2;
            continue;
        }
        if let Some(u) = hex(i) {
            if (0xD800..0xDC00).contains(&u) && hex(i + 6).is_some_and(|l| (0xDC00..0xE000).contains(&l)) {
                out.push_str(&json[i..i + 12]);
                i += 12;
                continue;
            }
            if (0xD800..0xE000).contains(&u) {
                out.push_str("\\ufffd");
                i += 6;
                continue;
            }
        }
        let c = json[i..].chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

#[test]
fn idna() {
    let mut all = 0;
    let mut ok = 0;
    for f in ["IdnaTestV2.json", "toascii.json"] {
        let (p, t, fails) = run(f);
        println!("{f}: {p}/{t}");
        for l in &fails {
            println!("  {l}");
        }
        ok += p;
        all += t;
    }
    assert_eq!(ok, all, "every IDNA case passes");
}

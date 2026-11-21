//! Runs WPT `url/resources/urltestdata.json` (fetched by
//! `scripts/fetch-spec-tests.sh` into `third_party/`).

use bb_url::Url;
use serde_json::Value;

fn data_path() -> std::path::PathBuf {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.join("third_party/wpt-url/urltestdata.json")
}

fn check(case: &Value) -> Result<(), String> {
    let input = case["input"].as_str().ok_or("no input")?;
    let base = match &case["base"] {
        Value::String(b) => match Url::parse(b) {
            Ok(u) => Some(u),
            Err(_) => return Err(format!("base {b:?} failed to parse")),
        },
        _ => None,
    };
    let result = Url::parse_with_base(input, base.as_ref());
    if case.get("failure").and_then(Value::as_bool) == Some(true) {
        return match result {
            Ok(u) => Err(format!("expected failure, got {}", u.href())),
            Err(_) => Ok(()),
        };
    }
    let u = result.map_err(|e| format!("unexpected failure: {e}"))?;
    let fields: [(&str, String); 9] = [
        ("href", u.href()),
        ("protocol", u.protocol()),
        ("username", u.username().to_string()),
        ("password", u.password().to_string()),
        ("host", u.host_with_port()),
        ("hostname", u.hostname()),
        ("port", u.port().map(|p| p.to_string()).unwrap_or_default()),
        ("pathname", u.pathname()),
        ("search", u.search()),
    ];
    for (k, got) in fields.iter() {
        if let Some(exp) = case[*k].as_str() {
            if exp != got {
                return Err(format!("{k}: expected {exp:?}, got {got:?}"));
            }
        }
    }
    if let Some(exp) = case["hash"].as_str() {
        if exp != u.hash() {
            return Err(format!("hash: expected {exp:?}, got {:?}", u.hash()));
        }
    }
    if let Some(exp) = case["origin"].as_str() {
        let got = u.origin().serialize();
        if exp != got {
            return Err(format!("origin: expected {exp:?}, got {got:?}"));
        }
    }
    Ok(())
}

#[test]
fn wpt_urltestdata() {
    let path = data_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("{} missing: run scripts/fetch-spec-tests.sh", path.display()));
    let cases: Vec<Value> = serde_json::from_str(&text).expect("valid JSON");
    let (mut pass, mut total) = (0, 0);
    let mut failures = Vec::new();
    for case in cases.iter().filter(|c| c.is_object()) {
        total += 1;
        match std::panic::catch_unwind(|| check(case)) {
            Ok(Ok(())) => pass += 1,
            Ok(Err(e)) => failures.push(format!("{:?} (base {:?}): {e}", case["input"], case["base"])),
            Err(_) => failures.push(format!("{:?}: PANIC", case["input"])),
        }
    }
    for f in &failures {
        println!("FAIL {f}");
    }
    let rate = pass as f64 * 100.0 / total as f64;
    println!("urltestdata: {pass}/{total} passed ({rate:.1}%)");
    assert!(failures.iter().all(|f| !f.ends_with("PANIC")), "parser panicked");
    assert!(rate >= 95.0, "pass rate {rate:.1}% below 95%");
}

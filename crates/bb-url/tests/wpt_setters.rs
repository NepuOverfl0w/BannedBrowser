//! Runs WPT `url/resources/setters_tests.json` (fetched by `scripts/fetch-spec-tests.sh`):
//! the URL setters (URL §6.1) with the basic URL parser's state override (§4.4).

use bb_url::Url;
use serde_json::Value;

fn get(u: &Url, k: &str) -> String {
    match k {
        "href" => u.href(),
        "protocol" => u.protocol(),
        "username" => u.username().to_string(),
        "password" => u.password().to_string(),
        "host" => u.host_with_port(),
        "hostname" => u.hostname(),
        "port" => u.port().map(|p| p.to_string()).unwrap_or_default(),
        "pathname" => u.pathname(),
        "search" => u.search(),
        "hash" => u.hash(),
        _ => String::new(),
    }
}

fn set(u: &mut Url, k: &str, v: &str) {
    match k {
        "href" => {
            let _ = u.set_href(v);
        }
        "protocol" => u.set_protocol(v),
        "username" => u.set_username(v),
        "password" => u.set_password(v),
        "host" => u.set_host(v),
        "hostname" => u.set_hostname(v),
        "port" => u.set_port(v),
        "pathname" => u.set_pathname(v),
        "search" => u.set_search(v),
        "hash" => u.set_hash(v),
        _ => {}
    }
}

#[test]
fn wpt_setters() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../third_party/wpt-url/setters_tests.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        panic!("{} missing: run scripts/fetch-spec-tests.sh", path.display());
    };
    let data: Value = serde_json::from_str(&text).expect("JSON");
    let (mut total, mut fails) = (0, Vec::new());
    for (setter, cases) in data.as_object().expect("object") {
        if setter == "comment" {
            continue;
        }
        for case in cases.as_array().into_iter().flatten() {
            let (Some(href), Some(value)) = (case["href"].as_str(), case["new_value"].as_str()) else {
                continue;
            };
            total += 1;
            let Ok(mut u) = Url::parse(href) else {
                fails.push(format!("{setter}: {href} doesn't parse"));
                continue;
            };
            set(&mut u, setter, value);
            for (k, exp) in case["expected"].as_object().into_iter().flatten() {
                let got = get(&u, k);
                if Some(got.as_str()) != exp.as_str() {
                    fails.push(format!("{setter} {href:?} := {value:?}: {k} expected {exp}, got {got:?}"));
                }
            }
        }
    }
    println!("URL setters: {} of {total} cases pass", total - fails.len());
    assert!(fails.is_empty(), "{} failures:\n{}", fails.len(), fails.join("\n"));
}

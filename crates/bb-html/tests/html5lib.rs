//! html5lib-tests conformance (fetched by `scripts/fetch-spec-tests.sh`).
//!
//! * Tokenizer: compares token streams (parse *errors* are not compared).
//! * Tree construction: compares the html5lib tree dump. `#script-on` cases
//!   are excluded because BannedBrowser parses with scripting disabled.

use bb_html::tokenizer::{State, Token, Tokenizer};
use serde_json::Value;
use std::path::PathBuf;

fn suite_dir() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/html5lib-tests");
    assert!(p.is_dir(), "{} missing: run scripts/fetch-spec-tests.sh", p.display());
    p
}

// ---------------- tokenizer ----------------

/// Decode `\uXXXX` escapes; `None` if the result has a lone surrogate.
fn unescape(s: &str) -> Option<String> {
    let mut out = String::new();
    let units: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut pending_high: Option<u32> = None;
    while i < units.len() {
        if units[i] == '\\' && i + 5 < units.len() + 1 && units.get(i + 1) == Some(&'u') {
            let hex: String = units[i + 2..(i + 6).min(units.len())].iter().collect();
            if let Ok(v) = u32::from_str_radix(&hex, 16) {
                i += 6;
                if (0xD800..0xDC00).contains(&v) {
                    if pending_high.is_some() {
                        return None;
                    }
                    pending_high = Some(v);
                    continue;
                }
                if (0xDC00..0xE000).contains(&v) {
                    let h = pending_high.take()?;
                    out.push(char::from_u32(0x10000 + ((h - 0xD800) << 10) + (v - 0xDC00))?);
                    continue;
                }
                if pending_high.is_some() {
                    return None;
                }
                out.push(char::from_u32(v)?);
                continue;
            }
        }
        if pending_high.is_some() {
            return None;
        }
        out.push(units[i]);
        i += 1;
    }
    if pending_high.is_some() {
        return None;
    }
    Some(out)
}

fn tokens_to_json(tokens: &[Token]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for t in tokens {
        let v = match t {
            Token::Doctype(d) => {
                serde_json::json!(["DOCTYPE", d.name, d.public_id, d.system_id, !d.force_quirks])
            }
            Token::StartTag(tag) => {
                let mut attrs = serde_json::Map::new();
                for (k, v) in &tag.attrs {
                    attrs.insert(k.clone(), Value::String(v.clone()));
                }
                if tag.self_closing {
                    serde_json::json!(["StartTag", tag.name, attrs, true])
                } else {
                    serde_json::json!(["StartTag", tag.name, attrs])
                }
            }
            Token::EndTag(tag) => serde_json::json!(["EndTag", tag.name]),
            Token::Comment(c) => serde_json::json!(["Comment", c]),
            Token::Characters(s) => serde_json::json!(["Character", s]),
            Token::Eof => continue,
        };
        out.push(v);
    }
    merge_chars(out)
}

fn merge_chars(tokens: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for t in tokens {
        if t[0] == "Character" {
            if let Some(last) = out.last_mut() {
                if last[0] == "Character" {
                    let s = format!("{}{}", last[1].as_str().unwrap(), t[1].as_str().unwrap());
                    last[1] = Value::String(s);
                    continue;
                }
            }
        }
        out.push(t);
    }
    out
}

fn unescape_value(v: &Value) -> Option<Value> {
    Some(match v {
        Value::String(s) => Value::String(unescape(s)?),
        Value::Array(a) => Value::Array(a.iter().map(unescape_value).collect::<Option<_>>()?),
        Value::Object(o) => {
            let mut m = serde_json::Map::new();
            for (k, v) in o {
                m.insert(unescape(k)?, unescape_value(v)?);
            }
            Value::Object(m)
        }
        other => other.clone(),
    })
}

#[test]
fn html5lib_tokenizer() {
    let dir = suite_dir().join("tokenizer");
    let (mut pass, mut total, mut skipped) = (0, 0, 0);
    let mut failures = Vec::new();
    let mut files: Vec<_> =
        std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).collect();
    files.sort();
    for path in files.iter().filter(|p| p.extension().is_some_and(|e| e == "test")) {
        let json: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let Some(tests) = json["tests"].as_array() else { continue };
        for test in tests {
            let double = test["doubleEscaped"].as_bool() == Some(true);
            let input = test["input"].as_str().unwrap().to_string();
            let (input, expected) = if double {
                match (unescape(&input), unescape_value(&test["output"])) {
                    (Some(i), Some(o)) => (i, o),
                    _ => {
                        skipped += 1; // contains lone surrogates: not representable in Rust strings
                        continue;
                    }
                }
            } else {
                (input, test["output"].clone())
            };
            let expected = merge_chars(expected.as_array().unwrap().clone());
            let states: Vec<String> = test["initialStates"]
                .as_array()
                .map(|a| a.iter().map(|s| s.as_str().unwrap().to_string()).collect())
                .unwrap_or_else(|| vec!["Data state".to_string()]);
            for st in states {
                total += 1;
                let mut tok = Tokenizer::new(&input);
                tok.state = match st.as_str() {
                    "Data state" => State::Data,
                    "PLAINTEXT state" => State::Plaintext,
                    "RCDATA state" => State::Rcdata,
                    "RAWTEXT state" => State::Rawtext,
                    "Script data state" => State::ScriptData,
                    "CDATA section state" => State::CdataSection,
                    other => panic!("unknown state {other}"),
                };
                tok.set_last_start_tag(test["lastStartTag"].as_str());
                let mut toks = Vec::new();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    loop {
                        let t = tok.next_token();
                        if t == Token::Eof {
                            break;
                        }
                        toks.push(t);
                    }
                    tokens_to_json(&toks)
                }));
                match result {
                    Ok(got) if got == expected => pass += 1,
                    Ok(got) => failures.push(format!(
                        "{}: {} [{st}]\n  input:    {input:?}\n  expected: {}\n  got:      {}",
                        path.file_name().unwrap().to_string_lossy(),
                        test["description"],
                        Value::Array(expected.clone()),
                        Value::Array(got)
                    )),
                    Err(_) => failures.push(format!("PANIC {}: {input:?}", test["description"])),
                }
            }
        }
    }
    for f in failures.iter().take(40) {
        println!("FAIL {f}");
    }
    let rate = pass as f64 * 100.0 / total as f64;
    println!("html5lib tokenizer: {pass}/{total} passed ({rate:.2}%), {skipped} skipped (lone surrogates)");
    assert!(!failures.iter().any(|f| f.starts_with("PANIC")), "tokenizer panicked");
    assert!(rate >= 98.0, "tokenizer pass rate {rate:.2}% below 98%");
}

// ---------------- tree construction ----------------

struct TreeTest {
    file: String,
    data: String,
    fragment: Option<String>,
    script: Option<bool>,
    document: String,
}

fn parse_dat(file: &str, text: &str) -> Vec<TreeTest> {
    let mut tests = Vec::new();
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i] != "#data" {
            i += 1;
            continue;
        }
        i += 1;
        let mut data = Vec::new();
        while i < lines.len() && !lines[i].starts_with('#') {
            data.push(lines[i]);
            i += 1;
        }
        let mut fragment = None;
        let mut script = None;
        let mut document = Vec::new();
        while i < lines.len() && lines[i] != "#data" {
            match lines[i] {
                "#errors" | "#new-errors" => {
                    i += 1;
                    while i < lines.len() && !lines[i].starts_with('#') {
                        i += 1;
                    }
                }
                "#document-fragment" => {
                    fragment = Some(lines[i + 1].trim().to_string());
                    i += 2;
                }
                "#script-off" => {
                    script = Some(false);
                    i += 1;
                }
                "#script-on" => {
                    script = Some(true);
                    i += 1;
                }
                "#document" => {
                    i += 1;
                    while i < lines.len() {
                        if lines[i] == "#data" {
                            break;
                        }
                        if lines[i].is_empty() && (i + 1 >= lines.len() || lines[i + 1] == "#data") {
                            i += 1;
                            break;
                        }
                        document.push(lines[i]);
                        i += 1;
                    }
                }
                _ => i += 1,
            }
        }
        tests.push(TreeTest {
            file: file.to_string(),
            data: data.join("\n"),
            fragment,
            script,
            document: document.join("\n"),
        });
    }
    tests
}

fn run_tree_test(t: &TreeTest) -> String {
    match &t.fragment {
        None => {
            let doc = bb_html::parse_document(&t.data);
            bb_dom::dump_html5lib(&doc, bb_dom::Document::ROOT)
        }
        Some(ctx) => {
            let (ns, name) = match ctx.split_once(' ') {
                Some(("svg", n)) => (bb_dom::Namespace::Svg, n),
                Some(("math", n)) => (bb_dom::Namespace::MathMl, n),
                _ => (bb_dom::Namespace::Html, ctx.as_str()),
            };
            let (doc, root) = bb_html::parse_fragment(&t.data, ns, name);
            bb_dom::dump_html5lib(&doc, root)
        }
    }
}

#[test]
fn html5lib_tree_construction() {
    let dir = suite_dir().join("tree-construction");
    let mut files: Vec<_> =
        std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).collect();
    files.sort();
    let (mut pass, mut total, mut script_on) = (0, 0, 0);
    let mut failures = Vec::new();
    let mut per_file: Vec<(String, usize, usize)> = Vec::new();
    for path in files.iter().filter(|p| p.extension().is_some_and(|e| e == "dat")) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(path).unwrap();
        let (mut fp, mut ft) = (0, 0);
        for t in parse_dat(&name, &text) {
            if t.script == Some(true) {
                script_on += 1;
                continue;
            }
            total += 1;
            ft += 1;
            let result = std::panic::catch_unwind(|| run_tree_test(&t));
            match result {
                Ok(got) => {
                    let got = got.trim_end_matches('\n').to_string();
                    if got == t.document {
                        pass += 1;
                        fp += 1;
                    } else {
                        failures.push(format!(
                            "{} {:?} (fragment {:?})\n--- expected\n{}\n--- got\n{}",
                            t.file, t.data, t.fragment, t.document, got
                        ));
                    }
                }
                Err(_) => failures.push(format!("PANIC {} {:?}", t.file, t.data)),
            }
        }
        per_file.push((name, fp, ft));
    }
    let show: usize = std::env::var("BB_SHOW_FAILURES").ok().and_then(|v| v.parse().ok()).unwrap_or(15);
    for f in failures.iter().take(show) {
        println!("FAIL {f}\n");
    }
    for (n, p, t) in &per_file {
        if p != t {
            println!("  {n}: {p}/{t}");
        }
    }
    let rate = pass as f64 * 100.0 / total as f64;
    println!("html5lib tree-construction: {pass}/{total} passed ({rate:.2}%), {script_on} #script-on cases excluded");
    assert!(!failures.iter().any(|f| f.starts_with("PANIC")), "tree builder panicked");
    assert!(rate >= 90.0, "tree-construction pass rate {rate:.2}% below 90%");
}

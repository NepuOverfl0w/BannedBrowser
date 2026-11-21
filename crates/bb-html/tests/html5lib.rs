//! HTML parser conformance (suites fetched by `scripts/fetch-spec-tests.sh`).
//!
//! * Tokenizer: html5lib-tests; compares token streams (parse *errors* are not compared).
//! * Tree construction: WPT `html/syntax/parsing/resources/*.dat` (the tests moved there from
//!   html5lib-tests); compares the html5lib tree dump. `#script-on` cases need our JS engine
//!   (milestone E) and are not run yet; they are counted and reported.

use bb_html::tokenizer::{RawToken, State, Token, Tokenizer};
use bb_str::Wtf8Buf;
use serde_json::Value;
use std::path::PathBuf;

fn third_party(rel: &str) -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party").join(rel);
    assert!(p.is_dir(), "{} missing: run scripts/fetch-spec-tests.sh", p.display());
    p
}

// ---------------- tokenizer ----------------

/// Decode the `\uXXXX` escapes of a `doubleEscaped` test string. The result may contain
/// lone surrogates, so it is WTF-8.
fn unescape(s: &str) -> Wtf8Buf {
    let mut out = Wtf8Buf::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && chars.get(i + 1) == Some(&'u') && i + 6 <= chars.len() {
            let hex: String = chars[i + 2..i + 6].iter().collect();
            if let Ok(v) = u32::from_str_radix(&hex, 16) {
                out.push_code_point(v); // joins surrogate pairs
                i += 6;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// A lossless, comparable text form of a WTF-8 string: `\` is doubled and each lone
/// surrogate is written as `\uXXXX`.
fn esc(w: &Wtf8Buf) -> String {
    let mut out = String::new();
    for cp in w.code_points() {
        match char::from_u32(cp) {
            Some('\\') => out.push_str("\\\\"),
            Some(c) => out.push(c),
            None => out.push_str(&format!("\\u{cp:04X}")),
        }
    }
    out
}

fn tokens_to_json(tokens: &[RawToken]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let e = |w: &Wtf8Buf| Value::String(esc(w));
    let eo = |w: &Option<Wtf8Buf>| w.as_ref().map(e).unwrap_or(Value::Null);
    for t in tokens {
        let v = match t {
            Token::Doctype(d) => {
                serde_json::json!([
                    "DOCTYPE",
                    eo(&d.name),
                    eo(&d.public_id),
                    eo(&d.system_id),
                    !d.force_quirks
                ])
            }
            Token::StartTag(tag) => {
                let mut attrs = serde_json::Map::new();
                for (k, v) in &tag.attrs {
                    attrs.insert(esc(k), e(v));
                }
                if tag.self_closing {
                    serde_json::json!(["StartTag", e(&tag.name), attrs, true])
                } else {
                    serde_json::json!(["StartTag", e(&tag.name), attrs])
                }
            }
            Token::EndTag(tag) => serde_json::json!(["EndTag", e(&tag.name)]),
            Token::Comment(c) => serde_json::json!(["Comment", e(c)]),
            Token::ProcessingInstruction { target, data } => {
                serde_json::json!(["ProcessingInstruction", e(target), e(data)])
            }
            Token::Characters(s) => serde_json::json!(["Character", e(s)]),
            Token::Eof => continue,
        };
        out.push(v);
    }
    merge_chars(out)
}

/// Merge adjacent character tokens. The strings are in `esc` form, where concatenation can't
/// create a surrogate pair (lone surrogates are spelled out), matching the tokenizer, which
/// never splits a pair.
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

/// Turn an expected-output value into the `esc` form used by `tokens_to_json`.
fn expected_value(v: &Value, double: bool) -> Value {
    let conv = |s: &str| esc(&if double { unescape(s) } else { Wtf8Buf::from(s) });
    match v {
        Value::String(s) => Value::String(conv(s)),
        Value::Array(a) => Value::Array(a.iter().map(|x| expected_value(x, double)).collect()),
        Value::Object(o) => {
            let mut m = serde_json::Map::new();
            for (k, v) in o {
                m.insert(conv(k), expected_value(v, double));
            }
            Value::Object(m)
        }
        other => other.clone(),
    }
}

#[test]
fn html5lib_tokenizer() {
    let dir = third_party("html5lib-tests/tokenizer");
    let (mut pass, mut total) = (0, 0);
    let mut failures = Vec::new();
    let mut files: Vec<_> =
        std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).collect();
    files.sort();
    for path in files.iter().filter(|p| p.extension().is_some_and(|e| e == "test")) {
        let json: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let Some(tests) = json["tests"].as_array() else { continue };
        for test in tests {
            let double = test["doubleEscaped"].as_bool() == Some(true);
            let raw_input = test["input"].as_str().unwrap();
            let input = if double { unescape(raw_input) } else { Wtf8Buf::from(raw_input) };
            let expected = expected_value(&test["output"], double);
            let expected = merge_chars(expected.as_array().unwrap().clone());
            let states: Vec<String> = test["initialStates"]
                .as_array()
                .map(|a| a.iter().map(|s| s.as_str().unwrap().to_string()).collect())
                .unwrap_or_else(|| vec!["Data state".to_string()]);
            for st in states {
                total += 1;
                let mut tok = Tokenizer::from_wtf8(&input);
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
                        let t = tok.next_raw_token();
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
    println!("html5lib tokenizer: {pass}/{total} passed ({rate:.2}%)");
    assert!(!failures.iter().any(|f| f.starts_with("PANIC")), "tokenizer panicked");
    assert!(failures.is_empty(), "{} tokenizer cases failed (v0.2 requires 100 %)", failures.len());
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
        while i < lines.len() && lines[i] != "#data" && lines[i] != "#description" {
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
                    // The tree runs until a blank line before the next record, which starts
                    // with `#data` or (WPT) an optional `#description`.
                    let next_record = |l: &str| l == "#data" || l == "#description";
                    i += 1;
                    while i < lines.len() {
                        if next_record(lines[i]) {
                            break;
                        }
                        if lines[i].is_empty() && (i + 1 >= lines.len() || next_record(lines[i + 1])) {
                            i += 1;
                            break;
                        }
                        document.push(lines[i]);
                        i += 1;
                    }
                    // Blank separator lines at the end of a file aren't part of the tree.
                    while document.last() == Some(&"") {
                        document.pop();
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
    let dir = third_party("wpt/html/syntax/parsing/resources");
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
    println!("tree construction (WPT): {pass}/{total} passed ({rate:.2}%), {script_on} #script-on cases not run (need JS, milestone E)");
    assert!(!failures.iter().any(|f| f.starts_with("PANIC")), "tree builder panicked");
    assert!(failures.is_empty(), "{} tree-construction cases failed (v0.2 requires 100 %)", failures.len());
}

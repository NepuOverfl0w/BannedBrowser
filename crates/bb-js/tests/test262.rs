//! test262 (`test/language`, `test/built-ins`, `test/annexB`; pinned in
//! scripts/fetch-spec-tests.sh): the parse phase. Every test is parsed in the variants its
//! flags ask for (strict and sloppy, module, raw); it passes when it fails to parse exactly when
//! its metadata says `negative: phase: parse`. Running the tests is `test262_exec.rs`.
//!
//! The parse phase passes 100 % (`FLOOR` is the whole suite at the pinned commit).
//! `cargo test -p bb-js --test test262 -- --nocapture` prints the numbers; failures go to
//! `target/test262-parse-failures.txt`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// The parse phase passes in full: every test must keep passing.
const FLOOR: usize = 48_803;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/test262")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "js") && !p.to_string_lossy().contains("_FIXTURE") {
            out.push(p);
        }
    }
}

struct Meta {
    flags: Vec<String>,
    negative_parse: bool,
}

fn meta(src: &str) -> Meta {
    let fm = src.find("/*---").and_then(|a| src[a..].find("---*/").map(|b| &src[a + 5..a + b])).unwrap_or("");
    let mut flags = Vec::new();
    let mut negative_parse = false;
    let mut in_negative = false;
    for line in fm.lines() {
        let t = line.trim();
        if let Some(f) = t.strip_prefix("flags:") {
            flags = f
                .trim()
                .trim_matches(['[', ']'])
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if t.starts_with("negative:") {
            in_negative = true;
            continue;
        }
        if in_negative {
            if let Some(p) = t.strip_prefix("phase:") {
                negative_parse = p.trim() == "parse";
            }
            if !line.starts_with(' ') && !line.is_empty() {
                in_negative = false;
            }
        }
    }
    Meta { flags, negative_parse }
}

/// Does the test pass? `Err` = the parser panicked.
fn run(src: &str) -> Result<bool, String> {
    let m = meta(src);
    let has = |f: &str| m.flags.iter().any(|x| x == f);
    let mut variants: Vec<(bool, String)> = Vec::new();
    if has("module") {
        variants.push((true, src.to_string()));
    } else if has("raw") || has("noStrict") {
        variants.push((false, src.to_string()));
    } else if has("onlyStrict") {
        variants.push((false, format!("\"use strict\";\n{src}")));
    } else {
        variants.push((false, src.to_string()));
        variants.push((false, format!("\"use strict\";\n{src}")));
    }
    for (module, code) in variants {
        let r = std::panic::catch_unwind(|| {
            if module {
                bb_js::parse_module(&code).is_ok()
            } else {
                bb_js::parse_script(&code).is_ok()
            }
        });
        let parsed = r.map_err(|e| {
            e.downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default()
        })?;
        if parsed == m.negative_parse {
            return Ok(false);
        }
    }
    Ok(true)
}

#[test]
fn test262_parse_phase() {
    let base = root().join("test");
    assert!(base.join("language").is_dir(), "{} missing: run scripts/fetch-spec-tests.sh", base.display());
    let mut files = Vec::new();
    for d in ["language", "built-ins", "annexB"] {
        collect(&base.join(d), &mut files);
    }
    files.sort();
    let total = files.len();
    let passed = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let panics = Mutex::new(Vec::new());
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    std::panic::set_hook(Box::new(|_| {}));
    std::thread::scope(|s| {
        for _ in 0..threads {
            std::thread::Builder::new()
                .stack_size(256 * 1024 * 1024)
                .spawn_scoped(s, || loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(f) = files.get(i) else { break };
                    let src = std::fs::read_to_string(f).unwrap_or_default();
                    let rel = f.strip_prefix(&base).unwrap_or(f).to_string_lossy().replace('\\', "/");
                    match run(&src) {
                        Ok(true) => {
                            passed.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(false) => failures.lock().unwrap().push(rel),
                        Err(msg) => panics.lock().unwrap().push(format!("{rel}: {msg}")),
                    }
                })
                .expect("spawn");
        }
    });
    let _ = std::panic::take_hook();
    let passed = passed.into_inner();
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    let panics = panics.into_inner().unwrap();
    // Failures by directory (two levels), most first.
    let mut by_dir: std::collections::BTreeMap<String, usize> = Default::default();
    for f in &failures {
        let d: Vec<&str> = f.split('/').take(3).collect();
        *by_dir.entry(d[..d.len().saturating_sub(1).max(1)].join("/")).or_default() += 1;
    }
    let mut dirs: Vec<_> = by_dir.into_iter().collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.1));
    println!("test262 parse phase: {passed}/{total} pass ({:.2} %)", passed as f64 * 100.0 / total as f64);
    for (d, n) in dirs.iter().take(25) {
        println!("  {n:6}  {d}");
    }
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test262-parse-failures.txt");
    let _ = std::fs::write(&out, failures.join("\n"));
    assert!(panics.is_empty(), "the parser panicked on {} tests:\n{}", panics.len(), panics.join("\n"));
    assert!(
        passed >= FLOOR && passed == total,
        "test262 parse phase: {passed}/{total} (expected all {FLOOR})"
    );
}

//! test262 (`test/language`, `test/built-ins`, `test/annexB`; pinned in
//! scripts/fetch-spec-tests.sh): running the tests in our interpreter, following the suite's
//! INTERPRETING.md. Each test gets a fresh realm, the harness (`assert.js`, `sta.js`, its
//! `includes`) as separate scripts, then the test in the variants its flags ask for (sloppy and
//! strict unless restricted). A test passes when every variant completes, or throws the
//! error its `negative` metadata names in the named phase.
//!
//! Not supported yet, and failing for it: modules, generators, async functions,
//! Promise, BigInt, Proxy, typed arrays, RegExp matching, and more built-ins (see PROGRESS.md).
//! `FLOOR` only ever goes up; the goal is all of it.
//!
//! `cargo test -p bb-js --test test262_exec -- --nocapture` prints the numbers; failures go to
//! `target/test262-exec-failures.txt`.

use bb_js::vm::heap::NativeCall;
use bb_js::vm::realm;
use bb_js::{JsResult, Value, Vm};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Tests that pass now; a run with fewer fails.
const FLOOR: usize = 24_151;

/// Instructions per test variant before it counts as hung.
const FUEL: u64 = 20_000_000;

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

#[derive(Default)]
struct Meta {
    flags: Vec<String>,
    includes: Vec<String>,
    negative_phase: Option<String>,
    negative_type: Option<String>,
}

fn list(v: &str) -> Vec<String> {
    v.trim()
        .trim_matches(['[', ']'])
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn meta(src: &str) -> Meta {
    let fm = src.find("/*---").and_then(|a| src[a..].find("---*/").map(|b| &src[a + 5..a + b])).unwrap_or("");
    let mut m = Meta::default();
    let mut section = "";
    for line in fm.lines() {
        let t = line.trim();
        if !line.starts_with(' ') && !line.starts_with('\t') && t.contains(':') {
            let (k, v) = t.split_once(':').unwrap_or((t, ""));
            section = match k {
                "flags" => {
                    m.flags = list(v);
                    "flags"
                }
                "includes" => {
                    m.includes = list(v);
                    "includes"
                }
                "negative" => "negative",
                _ => "",
            };
            continue;
        }
        match section {
            "includes" | "flags" => {
                if let Some(x) = t.strip_prefix("- ") {
                    let target = if section == "includes" { &mut m.includes } else { &mut m.flags };
                    target.push(x.trim().to_string());
                }
            }
            "negative" => {
                if let Some(p) = t.strip_prefix("phase:") {
                    m.negative_phase = Some(p.trim().to_string());
                }
                if let Some(p) = t.strip_prefix("type:") {
                    m.negative_type = Some(p.trim().to_string());
                }
            }
            _ => {}
        }
    }
    m
}

fn harness(name: &str, cache: &mut HashMap<String, String>) -> String {
    cache
        .entry(name.to_string())
        .or_insert_with(|| std::fs::read_to_string(root().join("harness").join(name)).unwrap_or_default())
        .clone()
}

/// The `$262` host object (INTERPRETING.md "Host-Defined Functions"), as far as we can.
fn install_262(vm: &mut Vm) {
    let o = vm.new_object();
    let g = vm.realm.global;
    realm::value(vm, g, "$262", Value::Object(o));
    realm::value(vm, o, "global", Value::Object(g));
    realm::method(vm, o, "evalScript", 1, |vm: &mut Vm, c: NativeCall| -> JsResult<Value> {
        let s = vm.to_string(&c.arg(0))?;
        vm.run_script(&String::from_utf16_lossy(&s))
    });
    realm::method(vm, o, "gc", 0, |_: &mut Vm, _: NativeCall| Ok(Value::Undefined));
    realm::method(vm, o, "createRealm", 0, |vm: &mut Vm, _: NativeCall| {
        vm.type_error("$262.createRealm is not supported yet")
    });
    realm::method(vm, o, "detachArrayBuffer", 1, |vm: &mut Vm, _: NativeCall| {
        vm.type_error("$262.detachArrayBuffer is not supported yet")
    });
}

/// The constructor name of a thrown value (`e.constructor.name`).
fn error_name(vm: &mut Vm, e: &Value) -> String {
    let Value::Object(o) = e else { return String::new() };
    let Ok(Value::Object(c)) = vm.get(*o, &bb_js::vm::PropKey::named("constructor")) else {
        return String::new();
    };
    match vm.get(c, &bb_js::vm::PropKey::named("name")) {
        Ok(Value::String(s)) => String::from_utf16_lossy(&s),
        _ => String::new(),
    }
}

/// Run one variant: `Ok(())` when it behaved as the metadata says.
fn run_variant(m: &Meta, src: &str, strict: bool, cache: &mut HashMap<String, String>) -> Result<(), String> {
    let mut vm = Vm::new();
    install_262(&mut vm);
    let raw = m.flags.iter().any(|f| f == "raw");
    let is_async = m.flags.iter().any(|f| f == "async");
    if !raw {
        let mut files = vec!["assert.js".to_string(), "sta.js".to_string()];
        if is_async {
            files.push("doneprintHandle.js".to_string());
        }
        files.extend(m.includes.iter().cloned());
        for f in files {
            let h = harness(&f, cache);
            vm.fuel = Some(FUEL);
            if let Err(e) = vm.run_script(&h) {
                return Err(format!("harness {f} threw {}", vm.describe(&e)));
            }
        }
    }
    let code = if strict { format!("\"use strict\";\n{src}") } else { src.to_string() };
    vm.fuel = Some(FUEL);
    let r = vm.run_script(&code);
    if vm.terminated {
        return Err("out of fuel".into());
    }
    match (&m.negative_phase, r) {
        (Some(phase), Err(e)) => {
            let want = m.negative_type.clone().unwrap_or_default();
            let got = error_name(&mut vm, &e);
            if got != want {
                return Err(format!("expected {want} ({phase}), got {}", vm.describe(&e)));
            }
            Ok(())
        }
        (Some(phase), Ok(_)) => Err(format!(
            "expected {} in phase {phase}, completed",
            m.negative_type.clone().unwrap_or_default()
        )),
        (None, Err(e)) => Err(vm.describe(&e)),
        (None, Ok(_)) => {
            if is_async {
                let out = vm.output.join("\n");
                if !out.contains("Test262:AsyncTestComplete") {
                    return Err(format!("async test did not complete: {out}"));
                }
            }
            Ok(())
        }
    }
}

fn run(src: &str, cache: &mut HashMap<String, String>) -> Result<(), String> {
    let m = meta(src);
    let has = |f: &str| m.flags.iter().any(|x| x == f);
    if has("module") {
        return Err("modules are not supported yet".into());
    }
    let variants: &[bool] = if has("raw") || has("noStrict") {
        &[false]
    } else if has("onlyStrict") {
        &[true]
    } else {
        &[false, true]
    };
    for &strict in variants {
        run_variant(&m, src, strict, cache)
            .map_err(|e| format!("{}{e}", if strict { "[strict] " } else { "" }))?;
    }
    Ok(())
}

#[test]
fn test262_execution() {
    let base = root().join("test");
    assert!(base.join("language").is_dir(), "{} missing: run scripts/fetch-spec-tests.sh", base.display());
    let mut files = Vec::new();
    for d in ["language", "built-ins", "annexB"] {
        collect(&base.join(d), &mut files);
    }
    files.sort();
    if let Ok(filter) = std::env::var("TEST262_FILTER") {
        files.retain(|f| f.to_string_lossy().contains(&filter));
    }
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
                .spawn_scoped(s, || {
                    let mut cache = HashMap::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(f) = files.get(i) else { break };
                        let src = std::fs::read_to_string(f).unwrap_or_default();
                        let rel = f.strip_prefix(&base).unwrap_or(f).to_string_lossy().replace('\\', "/");
                        let r =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&src, &mut cache)));
                        match r {
                            Ok(Ok(())) => {
                                passed.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(Err(msg)) => failures.lock().unwrap().push(format!("{rel}: {msg}")),
                            Err(p) => {
                                let msg = p
                                    .downcast_ref::<String>()
                                    .cloned()
                                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                                    .unwrap_or_default();
                                panics.lock().unwrap().push(format!("{rel}: {msg}"));
                            }
                        }
                    }
                })
                .expect("spawn");
        }
    });
    let _ = std::panic::take_hook();
    let passed = passed.into_inner();
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    let mut panics = panics.into_inner().unwrap();
    panics.sort();
    let mut by_dir: std::collections::BTreeMap<String, usize> = Default::default();
    for f in &failures {
        let d: Vec<&str> = f.split(':').next().unwrap_or("").split('/').take(3).collect();
        *by_dir.entry(d[..d.len().saturating_sub(1).max(1)].join("/")).or_default() += 1;
    }
    let mut dirs: Vec<_> = by_dir.into_iter().collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.1));
    println!(
        "test262 execution: {passed}/{total} pass ({:.2} %)",
        passed as f64 * 100.0 / total.max(1) as f64
    );
    for (d, n) in dirs.iter().take(40) {
        println!("  {n:6}  {d}");
    }
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test262-exec-failures.txt");
    let _ = std::fs::write(&out, failures.join("\n"));
    assert!(panics.is_empty(), "the interpreter panicked on {} tests:\n{}", panics.len(), panics.join("\n"));
    if std::env::var("TEST262_FILTER").is_err() {
        assert!(passed >= FLOOR, "test262 execution: {passed}/{total} pass, below the floor of {FLOOR}");
    }
}

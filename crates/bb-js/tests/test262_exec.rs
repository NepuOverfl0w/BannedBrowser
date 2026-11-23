//! test262 (`test/language`, `test/built-ins`, `test/annexB`; pinned in
//! scripts/fetch-spec-tests.sh): running the tests in our interpreter, following the suite's
//! INTERPRETING.md. Each test gets a fresh realm, the harness (`assert.js`, `sta.js`, its
//! `includes`) as separate scripts, then the test in the variants its flags ask for (sloppy and
//! strict unless restricted). A test passes when every variant completes, or throws the
//! error its `negative` metadata names in the named phase.
//!
//! `$262.agent` runs each agent on its own thread with its own VM; SharedArrayBuffers
//! share their Shared Data Block across them.
//!
//! Not supported yet, and failing for it: named time zones and non-ISO calendars (no time
//! zone or CLDR data), decorators and a few more (see PROGRESS.md).
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
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// Tests that pass now; a run with fewer fails.
const FLOOR: usize = 48_782;

/// Instructions per test variant before it counts as hung (TEST262_FUEL overrides it
/// for experiments). The heaviest legitimate tests, the generated RegExp property-escape
/// tests (they build strings of all 1.1M code points in JavaScript), need about 25M,
/// a second or two; a hung test stops here.
const FUEL: u64 = 30_000_000;

fn fuel() -> u64 {
    std::env::var("TEST262_FUEL").ok().and_then(|v| v.parse().ok()).unwrap_or(FUEL)
}

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
    // Some tests use CR line terminators.
    for line in fm.split(['\n', '\r']) {
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
    realm::method(vm, o, "createRealm", 0, |vm: &mut Vm, _: NativeCall| -> JsResult<Value> {
        let id = vm.create_realm();
        vm.in_realm(id, |vm| {
            install_262(vm);
            let g = vm.realm.global;
            vm.get(g, &bb_js::vm::PropKey::named("$262"))
        })
    });
    // IsHTMLDDA: returns null when called with no arguments or with "".
    let dda = realm::native(vm, "IsHTMLDDA", 0, |_: &mut Vm, _: NativeCall| Ok(Value::Null));
    vm.heap.obj_mut(dda).html_dda = true;
    realm::value(vm, o, "IsHTMLDDA", Value::Object(dda));
    let ams = vm.realm.abstract_module_source;
    realm::value(vm, o, "AbstractModuleSource", Value::Object(ams));
    realm::method(vm, o, "detachArrayBuffer", 1, |vm: &mut Vm, c: NativeCall| -> JsResult<Value> {
        match c.arg(0) {
            Value::Object(b) => vm.detach_array_buffer(b)?,
            _ => return vm.type_error("not an ArrayBuffer"),
        }
        Ok(Value::Null)
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
/// Resolve a module specifier against the importing file (fixtures are relative paths).
fn resolve_specifier(spec: &str, referrer: Option<&str>) -> PathBuf {
    let base = referrer.map(Path::new).and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    let mut out = PathBuf::new();
    for c in base.join(spec).components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

// ---- $262.agent (INTERPRETING.md "Host-Defined Functions") ----

/// One test's agents: their mailboxes and the reports they send back.
struct Hub {
    agents: Mutex<Vec<mpsc::Sender<Broadcast>>>,
    reports: Mutex<std::collections::VecDeque<String>>,
    epoch: Instant,
}

struct Broadcast {
    block: Arc<bb_js::vm::typed::SharedBlock>,
    id: f64,
    ack: mpsc::Sender<()>,
}

thread_local! {
    /// The current test's hub (the main agent's thread).
    static HUB: std::cell::RefCell<Option<Arc<Hub>>> = const { std::cell::RefCell::new(None) };
    /// An agent thread's hub and its receiveBroadcast callback (a root index).
    static AGENT: std::cell::RefCell<Option<(Arc<Hub>, Option<usize>)>> = const { std::cell::RefCell::new(None) };
}

fn hub() -> Option<Arc<Hub>> {
    HUB.with(|h| h.borrow().clone()).or_else(|| AGENT.with(|a| a.borrow().as_ref().map(|x| x.0.clone())))
}

fn agent_common(vm: &mut Vm, a: bb_js::vm::heap::ObjRef) {
    realm::method(vm, a, "sleep", 1, |vm: &mut Vm, c: NativeCall| -> JsResult<Value> {
        let ms = vm.to_number(&c.arg(0))?;
        std::thread::sleep(Duration::from_secs_f64(ms.clamp(0.0, 10_000.0) / 1000.0));
        Ok(Value::Undefined)
    });
    realm::method(vm, a, "monotonicNow", 0, |_: &mut Vm, _: NativeCall| -> JsResult<Value> {
        let ms = hub().map(|h| h.epoch.elapsed().as_secs_f64() * 1000.0).unwrap_or(0.0);
        Ok(Value::Number(ms.floor()))
    });
}

/// The main agent's `$262.agent`.
fn install_agent_main(vm: &mut Vm, o: bb_js::vm::heap::ObjRef) {
    let a = vm.new_object();
    realm::value(vm, o, "agent", Value::Object(a));
    agent_common(vm, a);
    realm::method(vm, a, "start", 1, |vm: &mut Vm, c: NativeCall| -> JsResult<Value> {
        let src = String::from_utf16_lossy(&vm.to_string(&c.arg(0))?);
        let Some(h) = hub() else { return Ok(Value::Undefined) };
        let (tx, rx) = mpsc::channel::<Broadcast>();
        if let Ok(mut agents) = h.agents.lock() {
            agents.push(tx);
        }
        let _ = std::thread::Builder::new().stack_size(64 * 1024 * 1024).spawn(move || run_agent(h, src, rx));
        Ok(Value::Undefined)
    });
    realm::method(vm, a, "broadcast", 2, |vm: &mut Vm, c: NativeCall| -> JsResult<Value> {
        let Some(block) = vm.shared_block(&c.arg(0)) else {
            return vm.type_error("broadcast needs a SharedArrayBuffer");
        };
        let id = vm.to_number(&c.arg(1))?;
        let Some(h) = hub() else { return Ok(Value::Undefined) };
        let agents: Vec<mpsc::Sender<Broadcast>> = h.agents.lock().map(|a| a.clone()).unwrap_or_default();
        let (ack, acks) = mpsc::channel();
        let mut sent = 0;
        for t in &agents {
            let m = Broadcast { block: block.clone(), id, ack: ack.clone() };
            if t.send(m).is_ok() {
                sent += 1;
            }
        }
        // Block until every agent has received it.
        for _ in 0..sent {
            if acks.recv_timeout(Duration::from_secs(10)).is_err() {
                break;
            }
        }
        Ok(Value::Undefined)
    });
    realm::method(vm, a, "getReport", 0, |_: &mut Vm, _: NativeCall| -> JsResult<Value> {
        let r = hub().and_then(|h| h.reports.lock().ok().and_then(|mut q| q.pop_front()));
        Ok(r.map(|s| Value::str(&s)).unwrap_or(Value::Null))
    });
}

/// An agent: its own VM on this thread, then its broadcasts until the hub goes away.
fn run_agent(h: Arc<Hub>, src: String, rx: mpsc::Receiver<Broadcast>) {
    AGENT.with(|a| *a.borrow_mut() = Some((h, None)));
    let mut vm = Vm::new();
    vm.fuel = Some(fuel());
    let o = vm.new_object();
    let g = vm.realm.global;
    realm::value(&mut vm, g, "$262", Value::Object(o));
    let a = vm.new_object();
    realm::value(&mut vm, o, "agent", Value::Object(a));
    agent_common(&mut vm, a);
    realm::method(&mut vm, a, "receiveBroadcast", 1, |vm: &mut Vm, c: NativeCall| -> JsResult<Value> {
        let i = vm.roots.len();
        vm.roots.push(c.arg(0));
        AGENT.with(|a| {
            if let Some(x) = a.borrow_mut().as_mut() {
                x.1 = Some(i);
            }
        });
        Ok(Value::Undefined)
    });
    realm::method(&mut vm, a, "report", 1, |vm: &mut Vm, c: NativeCall| -> JsResult<Value> {
        let s = String::from_utf16_lossy(&vm.to_string(&c.arg(0))?);
        if let Some(h) = hub() {
            if let Ok(mut q) = h.reports.lock() {
                q.push_back(s);
            }
        }
        Ok(Value::Undefined)
    });
    realm::method(&mut vm, a, "leaving", 0, |_: &mut Vm, _: NativeCall| Ok(Value::Undefined));
    if vm.run_script(&src).is_err() {
        return;
    }
    vm.run_jobs();
    while let Ok(m) = rx.recv_timeout(Duration::from_secs(30)) {
        let _ = m.ack.send(());
        let Some(cb) = AGENT.with(|a| a.borrow().as_ref().and_then(|x| x.1)) else { continue };
        let f = vm.roots[cb].clone();
        let sab = vm.new_shared_array_buffer(m.block, None);
        let _ = vm.call(&f, Value::Undefined, vec![sab, Value::Number(m.id)]);
        vm.run_jobs();
        let start = Instant::now();
        while vm.has_pending_waits() && !vm.terminated && start.elapsed() < Duration::from_secs(10) {
            vm.wait_for_events(Duration::from_millis(20));
        }
    }
}

fn run_variant(
    m: &Meta,
    path: &Path,
    src: &str,
    strict: bool,
    module: bool,
    cache: &mut HashMap<String, String>,
) -> Result<(), String> {
    let mut vm = Vm::new();
    install_262(&mut vm);
    HUB.with(|h| {
        *h.borrow_mut() = Some(Arc::new(Hub {
            agents: Mutex::new(Vec::new()),
            reports: Mutex::new(Default::default()),
            epoch: Instant::now(),
        }))
    });
    {
        let g = vm.realm.global;
        if let Ok(Value::Object(o)) = vm.get(g, &bb_js::vm::PropKey::named("$262")) {
            install_agent_main(&mut vm, o);
        }
    }
    // `<module source>`: a module with a module source (source-phase-imports tests).
    let _ = vm.register_module_source("<module source>", "ModuleSource");
    vm.loader = Some(Box::new(|spec: &str, referrer: Option<&str>| {
        if spec == "<module source>" {
            return Ok((spec.to_string(), Vec::new()));
        }
        let p = resolve_specifier(spec, referrer);
        let src = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        Ok((p.to_string_lossy().into_owned(), src))
    }));
    vm.can_block = !m.flags.iter().any(|f| f == "CanBlockIsFalse");
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
            vm.fuel = Some(fuel());
            if let Err(e) = vm.run_script(&h) {
                return Err(format!("harness {f} threw {}", vm.describe(&e)));
            }
        }
    }
    let code = if strict { format!("\"use strict\";\n{src}") } else { src.to_string() };
    vm.fuel = Some(fuel());
    // The same normalized form the loader produces, so self-imports find this module.
    let key = resolve_specifier(&path.to_string_lossy(), None).to_string_lossy().into_owned();
    let r = if module {
        // The module's evaluation promise settles after the jobs run.
        match vm.run_module(&key, &code) {
            Ok(p) => {
                vm.run_jobs();
                match vm.promise_result(&p) {
                    Some(Err(e)) => Err(e),
                    _ => Ok(Value::Undefined),
                }
            }
            Err(e) => Err(e),
        }
    } else {
        vm.run_script_at(&code, Some(&key))
    };
    if r.is_ok() && !module {
        vm.run_jobs();
    }
    // The event loop keeps going while Atomics.waitAsync promises are pending.
    let start = Instant::now();
    while vm.has_pending_waits() && !vm.terminated && start.elapsed() < Duration::from_secs(10) {
        vm.wait_for_events(Duration::from_millis(20));
    }
    HUB.with(|h| *h.borrow_mut() = None);
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

fn run(path: &Path, src: &str, cache: &mut HashMap<String, String>) -> Result<(), String> {
    let m = meta(src);
    let has = |f: &str| m.flags.iter().any(|x| x == f);
    if has("module") {
        return run_variant(&m, path, src, false, true, cache);
    }
    let variants: &[bool] = if has("raw") || has("noStrict") {
        &[false]
    } else if has("onlyStrict") {
        &[true]
    } else {
        &[false, true]
    };
    for &strict in variants {
        run_variant(&m, path, src, strict, false, cache)
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
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            run(f, &src, &mut cache)
                        }));
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

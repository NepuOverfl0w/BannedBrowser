//! WPT testharness.js tests run in bb-webapi (third_party/wpt, fetched by
//! scripts/fetch-spec-tests.sh): each test page loads at http://web-platform.test/<path>,
//! its scripts come from the checkout, and our testharnessreport.js collects the results.
//! A virtual clock jumps to the next timer, so timeouts cost nothing.
//!
//! `WPT_FILTER=dom/nodes/Node-` narrows the run; `cargo test -p bb-webapi --test wpt --
//! --nocapture` prints per-directory numbers; failures go to target/wpt-failures.txt.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use bb_url::Url;
use bb_webapi::{Embedder, Window};

/// The directories run (and counted).
const DIRS: &[&str] = &[
    "dom/nodes",
    "dom/collections",
    "dom/events",
    "dom/lists",
    "dom/abort",
    "dom/ranges",
    "dom/traversal",
    "domparsing",
    "FileAPI",
    "xhr/formdata",
    "workers",
    "html/webappapis/structured-clone",
    "webmessaging",
    "html/canvas/element",
    "url",
    "encoding",
    "fetch/api/headers",
    "fetch/api/request",
    "fetch/api/response",
    "fetch/api/body",
    "css/geometry",
    "custom-elements",
    "streams",
    "shadow-dom",
    "html/semantics/interactive-elements/the-details-element",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/wpt")
}

const REPORT: &str = r#"
setup({ output: false, explicit_timeout: false });
add_completion_callback(function (tests, status) {
  self.__wpt_results = JSON.stringify({
    status: status.status,
    message: status.message || "",
    tests: tests.map(function (t) { return { name: t.name, status: t.status, message: t.message || "" }; })
  });
});
"#;

/// The clock: real time since the test started, plus the virtual time skipped while idle.
struct TestEmbedder {
    clock: Rc<Cell<f64>>,
    start: std::time::Instant,
}

impl Embedder for TestEmbedder {
    fn fetch_script(&self, url: &Url) -> Result<String, String> {
        let path = url.pathname();
        if path == "/resources/testharnessreport.js" {
            return Ok(REPORT.to_string());
        }
        let path = if path == "/resources/WebIDLParser.js" {
            "/resources/webidl2/lib/webidl2.js".to_string()
        } else {
            path
        };
        let file = root().join(path.trim_start_matches('/'));
        std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))
    }
    fn fetch_sync(&self, req: bb_webapi::net::HttpRequest) -> Result<bb_webapi::net::HttpResponse, String> {
        Ok(serve(req))
    }
    fn now_ms(&self) -> f64 {
        self.clock.get() + self.start.elapsed().as_secs_f64() * 1000.0
    }
    fn worker_net(&self) -> Option<std::sync::Arc<dyn bb_webapi::worker::WorkerNet>> {
        Some(std::sync::Arc::new(FileNet))
    }
    fn nested_browsing(&self) -> bool {
        true
    }
    /// Images decode from the checkout (as a browser would have loaded them).
    fn image_bitmap(&self, url: &Url) -> Option<(u32, u32, Vec<u8>)> {
        let req = bb_webapi::net::HttpRequest {
            method: "GET".into(),
            url: url.clone(),
            headers: Vec::new(),
            body: None,
        };
        let r = serve(req);
        if r.status != 200 {
            return None;
        }
        let img = bb_image::decode(&r.body).ok()?;
        Some((img.width, img.height, img.pixels().to_vec()))
    }
}

/// Workers' requests, answered the same way.
struct FileNet;

impl bb_webapi::worker::WorkerNet for FileNet {
    fn fetch(&self, req: bb_webapi::net::HttpRequest) -> Result<bb_webapi::net::HttpResponse, String> {
        Ok(serve(req))
    }
}

/// Requests are answered from the checkout (as wptserve would serve the files), with the
/// wrapper wptserve generates for an `.any.js` test's dedicated-worker variant.
fn serve(req: bb_webapi::net::HttpRequest) -> bb_webapi::net::HttpResponse {
    let mut path = req.url.pathname();
    // wptserve's alias for the WebIDL parser idlharness.js loads.
    if path == "/resources/WebIDLParser.js" {
        path = "/resources/webidl2/lib/webidl2.js".to_string();
    }
    let generated = path.strip_suffix(".any.worker.js").map(|base| worker_wrapper(&format!("{base}.any.js")));
    let file = root().join(path.trim_start_matches('/'));
    let (status, status_text, body) = match generated {
        Some(Some(src)) => (200, "OK", src.into_bytes()),
        _ => match std::fs::read(&file) {
            Ok(b) => (200, "OK", b),
            Err(_) => (404, "Not Found", b"not found".to_vec()),
        },
    };
    let ct = match file.extension().and_then(|e| e.to_str()) {
        Some("json") => "application/json",
        Some("js") => "text/javascript",
        Some("html") => "text/html",
        Some("xml") => "application/xml",
        Some("xhtml") => "application/xhtml+xml",
        Some("svg") => "image/svg+xml",
        Some("txt") => "text/plain",
        _ => "application/octet-stream",
    };
    bb_webapi::net::HttpResponse {
        url: req.url,
        status,
        status_text: status_text.to_string(),
        headers: vec![("content-type".into(), ct.into())],
        body,
    }
}

/// The `.any.worker.js` wptserve makes for `.any.js` test `rel` (URL path): testharness,
/// the META scripts, the test, done().
fn worker_wrapper(rel: &str) -> Option<String> {
    let src = std::fs::read_to_string(root().join(rel.trim_start_matches('/'))).ok()?;
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let mut out = String::from(
        "self.GLOBAL = { isWindow: () => false, isWorker: () => true, isShadowRealm: () => false };\n\
         importScripts(\"/resources/testharness.js\");\n",
    );
    for s in meta_scripts(&src) {
        out.push_str(&format!("importScripts(\"{s}\");\n"));
    }
    out.push_str(&format!("importScripts(\"{name}\");\ndone();\n"));
    Some(out)
}

/// The `// META: script=` lines of a test.
fn meta_scripts(src: &str) -> Vec<String> {
    meta(src, "script=")
}

fn meta(src: &str, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in src.lines() {
        let Some(m) = line.strip_prefix("// META:") else {
            if !line.trim().is_empty() && !line.starts_with("//") {
                break;
            }
            continue;
        };
        if let Some(s) = m.trim().strip_prefix(key) {
            out.push(s.trim().to_string());
        }
    }
    out
}

/// Whether an `.any.js` test runs in dedicated workers (its `// META: global=` list, by
/// default window and dedicatedworker).
fn any_in_worker(src: &str) -> bool {
    let g = meta(src, "global=");
    g.is_empty()
        || g.iter().flat_map(|l| l.split(',')).any(|x| matches!(x.trim(), "dedicatedworker" | "worker"))
}

fn any_in_window(src: &str) -> bool {
    let g = meta(src, "global=");
    g.is_empty() || g.iter().flat_map(|l| l.split(',')).any(|x| x.trim() == "window")
}

/// The page for a test file: HTML as is; `.window.js` / `.any.js` wrapped as WPT serves them;
/// `.worker.js` and an `.any.js`'s worker variant (`#worker`) run in a dedicated worker.
fn page_for(rel: &str, src: &str) -> String {
    if rel.ends_with(".html") || rel.ends_with(".htm") {
        return src.to_string();
    }
    let worker_script = if let Some(base) = rel.strip_suffix(".any.js#worker") {
        Some(format!("{}.any.worker.js", base.rsplit('/').next().unwrap_or(base)))
    } else if rel.ends_with(".worker.js") {
        Some(rel.rsplit('/').next().unwrap_or(rel).to_string())
    } else {
        None
    };
    if let Some(w) = worker_script {
        return format!(
            "<!doctype html>\n<meta charset=utf-8>\n<script src=\"/resources/testharness.js\"></script>\n\
             <script src=\"/resources/testharnessreport.js\"></script>\n<div id=log></div>\n\
             <script>fetch_tests_from_worker(new Worker(\"{w}\"));</script>\n"
        );
    }
    let mut extra = String::new();
    for line in src.lines() {
        let Some(m) = line.strip_prefix("// META:") else {
            if !line.trim().is_empty() && !line.starts_with("//") {
                break;
            }
            continue;
        };
        if let Some(s) = m.trim().strip_prefix("script=") {
            extra.push_str(&format!("<script src=\"{}\"></script>\n", s.trim()));
        }
    }
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let global = if rel.ends_with(".any.js") {
        "<script>self.GLOBAL = { isWindow: () => true, isWorker: () => false, isShadowRealm: () => false };</script>\n"
    } else {
        ""
    };
    format!(
        "<!doctype html>\n<meta charset=utf-8>\n{global}<script src=\"/resources/testharness.js\"></script>\n\
         <script src=\"/resources/testharnessreport.js\"></script>\n{extra}<div id=log></div>\n<script src=\"{name}\"></script>\n"
    )
}

/// Run one test file: (subtests passed, subtests, harness error).
fn run_file(rel: &str) -> (usize, usize, Vec<String>) {
    let file_rel = rel.strip_suffix("#worker").unwrap_or(rel);
    let file = root().join(file_rel);
    let Ok(src) = std::fs::read_to_string(&file) else { return (0, 1, vec![format!("{rel}: unreadable")]) };
    let page_rel = match file_rel.strip_suffix(".any.js") {
        Some(b) if rel.ends_with("#worker") => format!("{b}.any.worker.html"),
        _ => {
            file_rel.strip_suffix(".js").map(|r| format!("{r}.html")).unwrap_or_else(|| file_rel.to_string())
        }
    };
    let url = Url::parse(&format!("http://web-platform.test/{page_rel}")).expect("test URL");
    let clock = Rc::new(Cell::new(0.0));
    let emb = Rc::new(TestEmbedder { clock: clock.clone(), start: std::time::Instant::now() });
    let mut w = Window::new(url, emb.clone());
    w.load_html(&page_for(rel, &src));
    w.fire_load();
    let mut results = None;
    let real_start = std::time::Instant::now();
    for _ in 0..200_000 {
        w.run_tasks();
        if let Ok(bb_js::Value::String(s)) = w.vm.run_script("self.__wpt_results || ''") {
            let s = bb_js::vm::to_rust(&s);
            if !s.is_empty() {
                results = Some(s);
                break;
            }
        }
        // Workers run in real time: while they live, wait for them rather than skip ahead.
        if w.has_live_workers() && real_start.elapsed() < std::time::Duration::from_secs(20) {
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        }
        // Work is waiting (messages, tasks): run it before any time passes.
        if w.has_queued_tasks() {
            continue;
        }
        // Animation frames come every 16 ms (unless a timer is due sooner).
        if w.has_animation_frames() {
            let now = emb.now_ms();
            let step = w.next_timer_due().map_or(16.0, |t| (t - now).clamp(0.0, 16.0));
            clock.set(clock.get() + step);
            w.run_animation_frames();
            if emb.now_ms() > 60_000.0 {
                break;
            }
            continue;
        }
        // Idle: jump to the next timer (or give up after a minute of virtual time).
        match w.next_timer_due() {
            Some(t) if t < 60_000.0 => {
                let now = emb.now_ms();
                if t > now {
                    clock.set(clock.get() + (t - now));
                }
            }
            _ => {
                if w.has_live_workers() && real_start.elapsed() < std::time::Duration::from_secs(20) {
                    // Workers run in real time: wait for them.
                    std::thread::sleep(std::time::Duration::from_millis(2));
                } else if !w.has_tasks() {
                    break;
                }
            }
        }
    }
    let Some(json) = results else {
        let mut msgs = vec![format!("{rel}: no results (harness never completed)")];
        for e in w.host.errors.borrow().iter().take(3) {
            msgs.push(format!("{rel}:   error: {e}"));
        }
        if std::env::var("WPT_VERBOSE").is_ok() {
            for l in w.take_console().into_iter().take(10) {
                msgs.push(format!("{rel}:   console: {l}"));
            }
        }
        return (0, 1, msgs);
    };
    // Parse the JSON with the engine itself.
    let key = "self.__wpt_parsed = JSON.parse(self.__wpt_results); self.__wpt_parsed.tests.length";
    let n = match w.vm.run_script(key) {
        Ok(bb_js::Value::Number(n)) => n as usize,
        _ => return (0, 1, vec![format!("{rel}: unparsable results {json}")]),
    };
    let mut passed = 0;
    let mut fails = Vec::new();
    if std::env::var("WPT_VERBOSE").is_ok() {
        for l in w.take_console().into_iter().take(10) {
            fails.push(format!("{rel}:   console: {l}"));
        }
    }
    let status = w.vm.run_script("self.__wpt_parsed.status + ' ' + self.__wpt_parsed.message");
    if let Ok(bb_js::Value::String(s)) = status {
        let s = bb_js::vm::to_rust(&s);
        if !s.starts_with("0 ") {
            fails.push(format!("{rel}: harness status {s}"));
        }
    }
    for i in 0..n {
        let q = format!(
            "var t = self.__wpt_parsed.tests[{i}]; t.status + '\\u0001' + t.name + '\\u0001' + t.message"
        );
        if let Ok(bb_js::Value::String(s)) = w.vm.run_script(&q) {
            let s = bb_js::vm::to_rust(&s);
            let mut parts = s.splitn(3, '\u{1}');
            let (st, name, msg) =
                (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            if st == "0" {
                passed += 1;
            } else {
                fails.push(format!("{rel}: [{st}] {name}: {}", msg.replace('\n', " ")));
            }
        }
    }
    if n == 0 {
        return (0, 1, fails);
    }
    (passed, n, fails)
}

/// Whether an HTML test is a reftest (`<link rel=match>` / `mismatch`).
fn is_reftest(p: &Path) -> bool {
    let Ok(src) = std::fs::read_to_string(p) else { return false };
    let s = src.to_ascii_lowercase();
    (s.contains("rel=match")
        || s.contains("rel=\"match\"")
        || s.contains("rel='match'")
        || s.contains("rel=\"mismatch\""))
        && !s.contains("testharness.js")
}

fn collect(dir: &Path, rel: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        let r = format!("{rel}/{name}");
        if p.is_dir() {
            // Not run yet: pages in legacy encodings (the runner reads pages as UTF-8) and
            // streams (no ReadableStream yet).
            let skipped = name.starts_with("legacy-mb-") || (rel == "encoding" && name == "streams");
            if !skipped
                && !matches!(
                    name.as_str(),
                    "resources" | "support" | "crashtests" | "tools" | "non-automated"
                )
            {
                collect(&p, &r, out);
            }
        } else if (name.ends_with(".html")
            || name.ends_with(".window.js")
            || name.ends_with(".any.js")
            || name.ends_with(".worker.js"))
            && !name.contains("-manual")
            && !name.contains(".https.")
            && !name.contains(".sub.")
            && !name.contains("-ref.")
            && !name.contains("-expected.")
        {
            if name.ends_with(".any.js") {
                let src = std::fs::read_to_string(&p).unwrap_or_default();
                if any_in_window(&src) {
                    out.push(r.clone());
                }
                if any_in_worker(&src) {
                    out.push(format!("{r}#worker"));
                }
            } else if name.ends_with(".html") && is_reftest(&p) {
                // Reftests compare renderings; this runner only runs testharness tests.
            } else {
                out.push(r);
            }
        }
    }
}

#[test]
fn wpt() {
    if !root().join("resources/testharness.js").is_file() {
        eprintln!("third_party/wpt/resources missing: run scripts/fetch-spec-tests.sh");
        return;
    }
    let mut files = Vec::new();
    for d in DIRS {
        collect(&root().join(d), d, &mut files);
    }
    files.sort();
    if let Ok(f) = std::env::var("WPT_FILTER") {
        files.retain(|x| x.contains(&f));
    }
    let next = AtomicUsize::new(0);
    let totals = Mutex::new((0usize, 0usize, 0usize, Vec::<String>::new(), Vec::<String>::new()));
    let per_dir = Mutex::new(std::collections::BTreeMap::<String, (usize, usize)>::new());
    // Panics are counted (and fail the run); WPT_VERBOSE shows them.
    if std::env::var("WPT_VERBOSE").is_err() {
        std::panic::set_hook(Box::new(|_| {}));
    }
    let threads = std::env::var("WPT_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8));
    std::thread::scope(|s| {
        for _ in 0..threads {
            std::thread::Builder::new()
                .stack_size(64 * 1024 * 1024)
                .spawn_scoped(s, || loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    let Some(rel) = files.get(i) else { break };
                    if std::env::var("WPT_VERBOSE").is_ok() {
                        eprintln!("wpt: {rel}");
                    }
                    let r = std::panic::catch_unwind(|| run_file(rel));
                    let mut t = totals.lock().unwrap();
                    let dir = rel.rsplit_once('/').map_or("", |x| x.0).to_string();
                    match r {
                        Ok((p, n, fails)) => {
                            t.0 += p;
                            t.1 += n;
                            if p == n {
                                t.2 += 1;
                            }
                            t.3.extend(fails);
                            let mut pd = per_dir.lock().unwrap();
                            let e = pd.entry(dir).or_default();
                            e.0 += p;
                            e.1 += n;
                        }
                        Err(_) => {
                            t.1 += 1;
                            t.4.push(rel.clone());
                        }
                    }
                })
                .expect("thread");
        }
    });
    let (passed, total, files_ok, mut fails, panics) = totals.into_inner().unwrap();
    println!("WPT: {passed}/{total} subtests pass; {files_ok}/{} files fully", files.len());
    for (d, (p, n)) in per_dir.into_inner().unwrap() {
        println!("  {d}: {p}/{n}");
    }
    fails.sort();
    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/wpt-failures.txt");
    let _ = std::fs::write(&out, fails.join("\n"));
    assert!(panics.is_empty(), "panicked on: {panics:?}");
}

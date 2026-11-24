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
    "url",
    "encoding",
    "fetch/api/headers",
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
        let file = root().join(path.trim_start_matches('/'));
        std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))
    }
    /// Requests are answered from the checkout (as wptserve would serve the files).
    fn fetch_sync(&self, req: bb_webapi::net::HttpRequest) -> Result<bb_webapi::net::HttpResponse, String> {
        let path = req.url.pathname();
        let file = root().join(path.trim_start_matches('/'));
        let (status, status_text, body) = match std::fs::read(&file) {
            Ok(b) => (200, "OK", b),
            Err(_) => (404, "Not Found", b"not found".to_vec()),
        };
        let ct = match file.extension().and_then(|e| e.to_str()) {
            Some("json") => "application/json",
            Some("js") => "text/javascript",
            Some("html") => "text/html",
            Some("xml") => "application/xml",
            Some("txt") => "text/plain",
            _ => "application/octet-stream",
        };
        Ok(bb_webapi::net::HttpResponse {
            url: req.url,
            status,
            status_text: status_text.to_string(),
            headers: vec![("content-type".into(), ct.into())],
            body,
        })
    }
    fn now_ms(&self) -> f64 {
        self.clock.get() + self.start.elapsed().as_secs_f64() * 1000.0
    }
}

/// The page for a test file: HTML as is; `.window.js` / `.any.js` wrapped as WPT serves them.
fn page_for(rel: &str, src: &str) -> String {
    if rel.ends_with(".html") || rel.ends_with(".htm") {
        return src.to_string();
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
    let file = root().join(rel);
    let Ok(src) = std::fs::read_to_string(&file) else { return (0, 1, vec![format!("{rel}: unreadable")]) };
    let page_rel = rel.strip_suffix(".js").map(|r| format!("{r}.html")).unwrap_or_else(|| rel.to_string());
    let url = Url::parse(&format!("http://web-platform.test/{page_rel}")).expect("test URL");
    let clock = Rc::new(Cell::new(0.0));
    let emb = Rc::new(TestEmbedder { clock: clock.clone(), start: std::time::Instant::now() });
    let mut w = Window::new(url, emb.clone());
    w.load_html(&page_for(rel, &src));
    w.fire_load();
    let mut results = None;
    for _ in 0..20_000 {
        w.run_tasks();
        if let Ok(bb_js::Value::String(s)) = w.vm.run_script("self.__wpt_results || ''") {
            let s = bb_js::vm::to_rust(&s);
            if !s.is_empty() {
                results = Some(s);
                break;
            }
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
                if !w.has_tasks() {
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
            if !skipped && !matches!(name.as_str(), "resources" | "support" | "crashtests" | "tools") {
                collect(&p, &r, out);
            }
        } else if (name.ends_with(".html") || name.ends_with(".window.js") || name.ends_with(".any.js"))
            && !name.contains("-manual")
            && !name.contains(".https.")
            && !name.contains(".sub.")
            && !name.contains("-ref.")
        {
            out.push(r);
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

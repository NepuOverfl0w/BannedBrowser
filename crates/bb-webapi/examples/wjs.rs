//! Run a script in a bb-webapi window (dev tool): `wjs FILE` or `wjs -e CODE`.
use std::rc::Rc;
struct E(std::time::Instant);
impl bb_webapi::Embedder for E {
    fn fetch_script(&self, url: &bb_url::Url) -> Result<String, String> {
        std::fs::read_to_string(url.pathname()).map_err(|e| e.to_string())
    }
    fn now_ms(&self) -> f64 {
        self.0.elapsed().as_secs_f64() * 1000.0
    }
}
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let src = if a.get(1).map(String::as_str) == Some("-e") {
        a.get(2).cloned().unwrap_or_default()
    } else {
        std::fs::read_to_string(a.get(1).expect("usage: wjs FILE | -e CODE")).expect("read")
    };
    let url = bb_url::Url::parse("http://example.test/").expect("url");
    let mut w = bb_webapi::Window::new(url, Rc::new(E(std::time::Instant::now())));
    w.load_html("<!doctype html><title>wjs</title><body>");
    w.run_script(&src);
    while w.run_tasks() {}
    for l in w.take_console() {
        println!("{l}");
    }
}

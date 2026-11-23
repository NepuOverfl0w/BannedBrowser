//! Dev check: create and drop windows, printing the process's resident size.
use std::rc::Rc;
struct E;
impl bb_webapi::Embedder for E {
    fn fetch_script(&self, _: &bb_url::Url) -> Result<String, String> {
        Err("none".into())
    }
    fn now_ms(&self) -> f64 {
        0.0
    }
}
fn rss() -> String {
    std::fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("VmRSS"))
        .unwrap_or("")
        .to_string()
}
fn main() {
    for i in 0..60 {
        let url = bb_url::Url::parse("http://example.test/").expect("url");
        let mut w = bb_webapi::Window::new(url, Rc::new(E));
        w.load_html("<!doctype html><p>x</p><script>for (var i=0;i<1000;i++) document.body.appendChild(document.createElement('div'));</script>");
        drop(w);
        if i % 10 == 0 {
            println!("{i}: {}", rss());
        }
    }
}

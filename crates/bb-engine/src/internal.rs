//! Internal `about:` pages and error pages (HTML/CSS rendered by our own engine).

const STYLE: &str = include_str!("../../../resources/pages/style.css");
const NEWTAB: &str = include_str!("../../../resources/pages/newtab.html");
const ABOUT_BB: &str = include_str!("../../../resources/pages/bannedbrowser.html");
const ABOUT_ABOUT: &str = include_str!("../../../resources/pages/about.html");
const NETERROR: &str = include_str!("../../../resources/pages/neterror.html");
const SNAG: &str = include_str!("../../../resources/pages/snag.html");

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// `about:` URL handler for bb-net: returns (content type, body).
pub fn about_handler(path: &str) -> Option<(String, Vec<u8>)> {
    let html = |s: String| Some(("text/html; charset=utf-8".to_string(), s.into_bytes()));
    match path {
        "style.css" => Some(("text/css".into(), STYLE.as_bytes().to_vec())),
        "newtab" | "home" => html(NEWTAB.to_string()),
        "bannedbrowser" => html(ABOUT_BB.replace("{{VERSION}}", env!("CARGO_PKG_VERSION"))),
        "about" => html(ABOUT_ABOUT.to_string()),
        "blank" => html(String::new()),
        _ => None,
    }
}

/// Network / load error page.
pub fn error_page_html(title: &str, message: &str, url: &str, detail: &str) -> String {
    NETERROR
        .replace("{{TITLE}}", &esc(title))
        .replace("{{MESSAGE}}", &esc(message))
        .replace("{{URL}}", &esc(url))
        .replace("{{DETAIL}}", &esc(detail))
}

/// "BannedBrowser hit a snag" page for internal errors.
pub fn snag_page_html(url: &str, detail: &str) -> String {
    SNAG.replace("{{URL}}", &esc(url)).replace("{{DETAIL}}", &esc(detail))
}

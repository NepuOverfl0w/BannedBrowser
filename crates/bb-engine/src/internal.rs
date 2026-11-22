//! Internal `about:` pages and error pages (HTML/CSS rendered by our own engine).

const STYLE: &str = include_str!("../../../resources/pages/style.css");
const NEWTAB: &str = include_str!("../../../resources/pages/newtab.html");
const ABOUT_BB: &str = include_str!("../../../resources/pages/bannedbrowser.html");
const ABOUT_ABOUT: &str = include_str!("../../../resources/pages/about.html");
const NETERROR: &str = include_str!("../../../resources/pages/neterror.html");
const CERTERROR: &str = include_str!("../../../resources/pages/certerror.html");
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

/// The certificate error page: "Go Back (Recommended)", and under "Advanced…" the reason, the
/// error code and "Accept the Risk and Continue" (a `bb-action:` link carrying the session
/// token, see [`crate::session_token`]).
pub fn cert_error_page_html(problem: &bb_net::CertificateProblem) -> String {
    CERTERROR
        .replace("{{HOST}}", &esc(&problem.host))
        .replace("{{REASON}}", &esc(&problem.reason))
        .replace("{{CODE}}", &esc(problem.code))
        .replace("{{TOKEN}}", crate::session_token())
}

/// "BannedBrowser hit a snag" page for internal errors.
pub fn snag_page_html(url: &str, detail: &str) -> String {
    SNAG.replace("{{URL}}", &esc(url)).replace("{{DETAIL}}", &esc(detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cert_error_page_has_reason_code_token_and_escapes() {
        let p = bb_net::CertificateProblem {
            host: "evil<host>.example".into(),
            der: vec![1, 2, 3],
            code: "CERT_EXPIRED",
            reason: "the certificate expired on 2026-07-01".into(),
        };
        let html = cert_error_page_html(&p);
        assert!(html.contains("evil&lt;host&gt;.example"));
        assert!(!html.contains("evil<host>"));
        assert!(html.contains("the certificate expired on 2026-07-01"));
        assert!(html.contains("Error code: CERT_EXPIRED"));
        assert!(html.contains(&format!("bb-action:accept-certificate?token={}", crate::session_token())));
        assert!(html.contains("bb-action:go-back"));
        assert_eq!(crate::session_token().len(), 32);
        assert_eq!(crate::session_token(), crate::session_token(), "stable for the session");
    }
}

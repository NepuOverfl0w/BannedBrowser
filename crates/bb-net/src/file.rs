//! `file:` URLs: read local files; directories get a generated index page.

use crate::{mime, NetError, NetErrorKind, NetResult, Response};
use bb_url::Url;

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

pub(crate) fn fetch(url: &Url) -> NetResult<Response> {
    let path = url.to_file_path().ok_or_else(|| NetError::new(NetErrorKind::InvalidUrl, "not a file URL"))?;
    let meta = std::fs::metadata(&path)
        .map_err(|e| NetError::new(NetErrorKind::FileNotFound, format!("{path}: {e}")))?;
    if meta.is_dir() {
        let mut entries: Vec<(String, bool)> = std::fs::read_dir(&path)
            .map_err(|e| NetError::new(NetErrorKind::Io, e.to_string()))?
            .filter_map(|e| e.ok())
            .map(|e| {
                let dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                (e.file_name().to_string_lossy().into_owned(), dir)
            })
            .collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        let mut html = format!(
            "<!DOCTYPE html><html><head><meta charset=utf-8><title>Index of {p}</title>\
             <style>body{{font-family:sans-serif;margin:2em}}li{{line-height:1.6}}</style></head>\
             <body><h1>Index of {p}</h1><ul><li><a href=\"../\">../</a></li>",
            p = escape(&path)
        );
        for (name, dir) in entries {
            let slash = if dir { "/" } else { "" };
            let href = bb_url::percent::utf8_percent_encode(&name, bb_url::percent::EncodeSet::Component);
            html.push_str(&format!("<li><a href=\"{href}{slash}\">{}{slash}</a></li>", escape(&name)));
        }
        html.push_str("</ul></body></html>");
        // Ensure relative links resolve inside the directory.
        let mut u = url.clone();
        if !u.pathname().ends_with('/') {
            if let Ok(nu) = Url::parse(&format!("{}/", url.href_without_fragment())) {
                u = nu;
            }
        }
        return Ok(Response::synthetic(u, 200, "text/html; charset=utf-8", html.into_bytes()));
    }
    let body = std::fs::read(&path).map_err(|e| NetError::new(NetErrorKind::Io, e.to_string()))?;
    let mut ct = mime::from_extension(&path);
    if ct == "application/octet-stream" {
        ct = mime::sniff(&body);
    }
    Ok(Response::synthetic(url.clone(), 200, ct, body))
}

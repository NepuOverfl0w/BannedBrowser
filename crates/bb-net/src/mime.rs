//! Minimal MIME type parsing (MIME Sniffing §4.4) and sniffing (§7).

/// A parsed `Content-Type` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentType {
    /// Lowercased `type/subtype`.
    pub essence: String,
    /// The `charset` parameter, if any (unquoted, original case).
    pub charset: Option<String>,
    pub params: Vec<(String, String)>,
}

impl ContentType {
    pub fn is_html(&self) -> bool {
        self.essence == "text/html" || self.essence == "application/xhtml+xml"
    }
    pub fn is_image(&self) -> bool {
        self.essence.starts_with("image/")
    }
    pub fn is_text(&self) -> bool {
        self.essence.starts_with("text/") || self.essence == "application/json"
    }
}

/// Parse a `Content-Type` header value leniently.
pub fn parse_content_type(value: &str) -> ContentType {
    let mut parts = value.split(';');
    let essence = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let mut params = Vec::new();
    let mut charset = None;
    for p in parts {
        let Some((k, v)) = p.split_once('=') else { continue };
        let k = k.trim().to_ascii_lowercase();
        let v = v.trim().trim_matches('"').to_string();
        if k == "charset" && charset.is_none() {
            charset = Some(v.clone());
        }
        params.push((k, v));
    }
    ContentType { essence, charset, params }
}

/// Sniff a MIME type from leading bytes when none was given (MIME Sniffing §7.1, subset).
pub fn sniff(bytes: &[u8]) -> &'static str {
    let b = bytes;
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "image/png";
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg";
    }
    if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return "image/webp";
    }
    if b.starts_with(b"BM") {
        return "image/bmp";
    }
    if b.starts_with(&[0, 0, 1, 0]) {
        return "image/x-icon";
    }
    if b.starts_with(b"%PDF-") {
        return "application/pdf";
    }
    // Skip whitespace and look for an HTML-ish tag.
    let start = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let head: Vec<u8> = b[start..b.len().min(start + 512)].to_ascii_lowercase();
    for pat in [
        &b"<!doctype html"[..],
        b"<html",
        b"<head",
        b"<body",
        b"<script",
        b"<table",
        b"<title",
        b"<!--",
        b"<p",
        b"<h1",
        b"<div",
        b"<font",
        b"<br",
        b"<a ",
        b"<b>",
        b"<center",
        b"<frameset",
        b"<iframe",
        b"<style",
    ] {
        if head.starts_with(pat) {
            return "text/html";
        }
    }
    if b.iter().take(1024).any(|&c| c < 9 || (c > 13 && c < 32 && c != 27)) {
        return "application/octet-stream";
    }
    "text/plain"
}

/// Guess a MIME type from a file extension (for `file:` URLs).
pub fn from_extension(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" | "shtml" | "xhtml" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "txt" | "text" | "md" | "rs" | "toml" | "log" => "text/plain",
        "png" => "image/png",
        "gif" => "image/gif",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse() {
        let ct = parse_content_type("Text/HTML; Charset=\"Shift_JIS\"; q=1");
        assert_eq!(ct.essence, "text/html");
        assert_eq!(ct.charset.as_deref(), Some("Shift_JIS"));
        assert!(ct.is_html());
    }
    #[test]
    fn sniffing() {
        assert_eq!(sniff(b"  <HTML><body>"), "text/html");
        assert_eq!(sniff(b"GIF89a...."), "image/gif");
        assert_eq!(sniff(b"hello world"), "text/plain");
    }
}

//! HTML §15 Rendering: presentational hints from legacy attributes
//! (`bgcolor`, `<font>`, `align`, `width`, `border`, `cellpadding`, …).
//!
//! Hints are expressed as CSS text and parsed as author-level declarations
//! with zero specificity, ordered before all author rules.

use bb_css::values::parse_legacy_color;
use bb_css::Declaration;
use bb_dom::{Document, Namespace, NodeId};
use std::fmt::Write;

/// HTML §2.3.4.4 "rules for parsing dimension values" → CSS length text.
pub fn parse_dimension(v: &str) -> Option<String> {
    let s = v.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = &s[digits.len()..];
    let mut value: f64 = digits.parse().ok()?;
    let mut rest_chars = rest.chars().peekable();
    if rest_chars.peek() == Some(&'.') {
        rest_chars.next();
        let frac: String = rest_chars.clone().take_while(|c| c.is_ascii_digit()).collect();
        if !frac.is_empty() {
            value += format!("0.{frac}").parse::<f64>().unwrap_or(0.0);
            for _ in 0..frac.len() {
                rest_chars.next();
            }
        }
    }
    if rest_chars.peek() == Some(&'%') {
        return Some(format!("{value}%"));
    }
    Some(format!("{value}px"))
}

/// Non-zero dimension values (used for width/height attributes).
fn nonzero_dimension(v: &str) -> Option<String> {
    let d = parse_dimension(v)?;
    if d == "0px" || d == "0%" {
        None
    } else {
        Some(d)
    }
}

fn parse_uint(v: &str) -> Option<u32> {
    let s = v.trim();
    let d: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    d.parse().ok()
}

fn color_css(v: &str) -> Option<String> {
    let c = parse_legacy_color(v)?;
    Some(format!("rgb({} {} {})", c.r, c.g, c.b))
}

/// HTML §15.3.4 `<font size>` → CSS keyword.
fn font_size_from_attr(v: &str) -> Option<&'static str> {
    let s = v.trim();
    let (mode, digits) = match s.chars().next()? {
        '+' => (1, &s[1..]),
        '-' => (-1, &s[1..]),
        _ => (0, s),
    };
    let d: String = digits.chars().take_while(|c| c.is_ascii_digit()).collect();
    let n: i32 = d.parse().ok()?;
    let mut value = match mode {
        1 => 3 + n,
        -1 => 3 - n,
        _ => n,
    };
    value = value.clamp(1, 7);
    Some(match value {
        1 => "x-small",
        2 => "small",
        3 => "medium",
        4 => "large",
        5 => "x-large",
        6 => "xx-large",
        _ => "xxx-large",
    })
}

fn align_css(v: &str) -> Option<&'static str> {
    Some(match v.trim().to_ascii_lowercase().as_str() {
        "left" => "-webkit-left",
        "right" => "-webkit-right",
        "center" | "middle" => "-webkit-center",
        "justify" => "justify",
        _ => return None,
    })
}

fn valign_css(v: &str) -> Option<&'static str> {
    Some(match v.trim().to_ascii_lowercase().as_str() {
        "top" => "top",
        "middle" | "center" => "middle",
        "bottom" => "bottom",
        "baseline" => "baseline",
        _ => return None,
    })
}

fn nearest_table(doc: &Document, node: NodeId) -> Option<NodeId> {
    doc.ancestors(node).find(|&a| doc.is_html(a, "table"))
}

fn css_url(v: &str) -> String {
    format!("url(\"{}\")", v.trim().replace('\\', "\\\\").replace('"', "\\\""))
}

/// Compute the presentational hint declarations for `node`.
pub fn presentational_hints(doc: &Document, node: NodeId) -> Vec<Declaration> {
    presentational_hints_in(doc, node, (None, None))
}

/// Like [`presentational_hints`] for a document shown in a frame whose
/// container has `marginwidth`/`marginheight` (HTML §15.3.3).
pub fn presentational_hints_in(
    doc: &Document,
    node: NodeId,
    container_margins: (Option<f32>, Option<f32>),
) -> Vec<Declaration> {
    let Some(e) = doc.element(node) else { return Vec::new() };
    if e.ns != Namespace::Html {
        return Vec::new();
    }
    let a = |n: &str| e.attr(n);
    let mut css = String::new();
    let mut add = |k: &str, v: &str| {
        let _ = write!(css, "{k}:{v};");
    };
    let name = e.name.as_str();

    // Common: align on block containers (HTML §15.3.3).
    if matches!(
        name,
        "div"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "caption"
            | "legend"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
    ) {
        if let Some(al) = a("align").and_then(align_css) {
            add("text-align", al);
        }
    }
    if matches!(name, "body" | "table" | "td" | "th" | "tr" | "thead" | "tbody" | "tfoot" | "marquee") {
        if let Some(c) = a("bgcolor").and_then(color_css) {
            add("background-color", &c);
        }
    }
    if matches!(name, "body" | "table" | "td" | "th" | "tr" | "thead" | "tbody" | "tfoot") {
        if let Some(bg) = a("background").filter(|v| !v.trim().is_empty()) {
            add("background-image", &css_url(bg));
        }
    }
    match name {
        "body" => {
            if let Some(c) = a("text").and_then(color_css) {
                add("color", &c);
            }
            // HTML §15.3.3: the frame container's margins win over the body's own.
            let margin = |n: &str| a(n).and_then(parse_dimension);
            let (cw, ch) = (
                container_margins.0.map(|v| format!("{v}px")),
                container_margins.1.map(|v| format!("{v}px")),
            );
            if let Some(m) = ch.clone().or_else(|| margin("marginheight")).or_else(|| margin("topmargin")) {
                add("margin-top", &m);
            }
            if let Some(m) = ch.or_else(|| margin("marginheight")).or_else(|| margin("bottommargin")) {
                add("margin-bottom", &m);
            }
            if let Some(m) = cw.clone().or_else(|| margin("marginwidth")).or_else(|| margin("leftmargin")) {
                add("margin-left", &m);
            }
            if let Some(m) = cw.or_else(|| margin("marginwidth")).or_else(|| margin("rightmargin")) {
                add("margin-right", &m);
            }
        }
        "a" if e.has_attr("href") => {
            // body link/vlink attributes color links (HTML §15.3.4).
            if let Some(body) = doc.body() {
                let visited = doc.node(node).flags.contains(bb_dom::NodeFlags::VISITED);
                let attr = if visited { "vlink" } else { "link" };
                if let Some(c) = doc.attr(body, attr).and_then(color_css) {
                    add("color", &c);
                }
            }
        }
        "font" => {
            if let Some(c) = a("color").and_then(color_css) {
                add("color", &c);
            }
            if let Some(f) = a("face").filter(|f| !f.trim().is_empty()) {
                let fam: Vec<String> = f
                    .split(',')
                    .map(|x| x.trim())
                    .filter(|x| !x.is_empty())
                    .map(|x| format!("\"{}\"", x.replace('"', "")))
                    .collect();
                add("font-family", &fam.join(","));
            }
            if let Some(sz) = a("size").and_then(font_size_from_attr) {
                add("font-size", sz);
            }
        }
        "basefont" => {}
        "table" => {
            if let Some(w) = a("width").and_then(nonzero_dimension) {
                add("width", &w);
            }
            if let Some(h) = a("height").and_then(nonzero_dimension) {
                add("height", &h);
            }
            match a("align").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
                Some("left") => add("float", "left"),
                Some("right") => add("float", "right"),
                Some("center") => {
                    add("margin-left", "auto");
                    add("margin-right", "auto");
                }
                _ => {}
            }
            if let Some(cs) = a("cellspacing").and_then(parse_uint) {
                add("border-spacing", &format!("{cs}px"));
            }
            // HTML §15.3.10: table border attribute.
            if let Some(b) = a("border") {
                let w = if b.trim().is_empty() { 1 } else { parse_uint(b).unwrap_or(1) };
                if w > 0 {
                    add("border-width", &format!("{w}px"));
                    add("border-style", "outset");
                    let c = a("bordercolor").and_then(color_css).unwrap_or_else(|| "gray".into());
                    add("border-color", &c);
                }
            } else if let Some(c) = a("bordercolor").and_then(color_css) {
                add("border-color", &c);
            }
            if let Some(frame) = a("frame") {
                let _ = frame; // TODO(HTML §15.3.10): frame/rules attributes
            }
        }
        "td" | "th" => {
            if let Some(w) = a("width").and_then(nonzero_dimension) {
                add("width", &w);
            }
            if let Some(h) = a("height").and_then(nonzero_dimension) {
                add("height", &h);
            }
            if let Some(v) = a("valign").and_then(valign_css) {
                add("vertical-align", v);
            }
            if e.has_attr("nowrap") {
                add("white-space", "nowrap");
            }
            if let Some(t) = nearest_table(doc, node) {
                if let Some(p) = doc.attr(t, "cellpadding").and_then(parse_uint) {
                    add("padding", &format!("{p}px"));
                }
                if let Some(b) = doc.attr(t, "border") {
                    let w = if b.trim().is_empty() { 1 } else { parse_uint(b).unwrap_or(1) };
                    if w > 0 {
                        add("border-width", "1px");
                        add("border-style", "inset");
                        let c =
                            doc.attr(t, "bordercolor").and_then(color_css).unwrap_or_else(|| "gray".into());
                        add("border-color", &c);
                    }
                }
                // Cells inherit the row/table valign when they have none.
                if !e.has_attr("valign") {
                    let row = doc.parent(node).filter(|&p| doc.is_html(p, "tr"));
                    if let Some(v) = row.and_then(|r| doc.attr(r, "valign")).and_then(valign_css) {
                        add("vertical-align", v);
                    }
                }
                if !e.has_attr("align") {
                    let row = doc.parent(node).filter(|&p| doc.is_html(p, "tr"));
                    if let Some(al) = row.and_then(|r| doc.attr(r, "align")).and_then(align_css) {
                        add("text-align", al);
                    }
                }
            }
        }
        "tr" => {
            if let Some(h) = a("height").and_then(nonzero_dimension) {
                add("height", &h);
            }
        }
        "col" | "colgroup" => {
            if let Some(w) = a("width").and_then(nonzero_dimension) {
                add("width", &w);
            }
        }
        "img" | "object" | "embed" | "iframe" | "video" | "canvas" | "input" => {
            if name != "input" || a("type").is_some_and(|t| t.eq_ignore_ascii_case("image")) {
                if let Some(w) = a("width").and_then(parse_dimension) {
                    add("width", &w);
                }
                if let Some(h) = a("height").and_then(parse_dimension) {
                    add("height", &h);
                }
            }
            if let Some(hs) = a("hspace").and_then(parse_dimension) {
                add("margin-left", &hs);
                add("margin-right", &hs);
            }
            if let Some(vs) = a("vspace").and_then(parse_dimension) {
                add("margin-top", &vs);
                add("margin-bottom", &vs);
            }
            if name == "img" || name == "object" {
                if let Some(b) = a("border").and_then(parse_uint) {
                    add("border", &format!("{b}px solid"));
                }
            }
            if name == "input" {
                if let Some(sz) = a("size").and_then(parse_uint) {
                    let text_like = a("type").is_none_or(|t| {
                        matches!(
                            t.to_ascii_lowercase().as_str(),
                            "text" | "search" | "url" | "tel" | "email" | "password" | ""
                        )
                    });
                    if text_like && sz > 0 {
                        add("width", &format!("{}ch", sz));
                    }
                }
            }
            if name == "iframe"
                && a("frameborder").is_some_and(|v| v.trim() == "0" || v.eq_ignore_ascii_case("no"))
            {
                add("border", "none");
            }
            match a("align").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
                Some("left") => add("float", "left"),
                Some("right") => add("float", "right"),
                Some("top") => add("vertical-align", "top"),
                Some("middle") | Some("absmiddle") | Some("center") => add("vertical-align", "middle"),
                Some("bottom") | Some("baseline") => add("vertical-align", "baseline"),
                Some("texttop") => add("vertical-align", "text-top"),
                Some("absbottom") => add("vertical-align", "bottom"),
                _ => {}
            }
        }
        "hr" => {
            match a("align").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
                Some("left") => {
                    add("margin-left", "0");
                    add("margin-right", "auto");
                }
                Some("right") => {
                    add("margin-left", "auto");
                    add("margin-right", "0");
                }
                _ => {}
            }
            if let Some(w) = a("width").and_then(nonzero_dimension) {
                add("width", &w);
            }
            let color = a("color").and_then(color_css);
            if let Some(c) = &color {
                add("color", c);
                add("background-color", c);
                add("border-color", c);
            }
            let size = a("size").and_then(parse_uint);
            if e.has_attr("noshade") || color.is_some() {
                add("border-style", "solid");
                if color.is_none() {
                    add("border-color", "gray");
                    add("background-color", "gray");
                }
            }
            if let Some(sz) = size {
                if sz == 1 {
                    add("border-bottom-width", "0");
                } else {
                    add("height", &format!("{}px", sz.saturating_sub(2)));
                }
            }
        }
        "br" => match a("clear").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("left") => add("clear", "left"),
            Some("right") => add("clear", "right"),
            Some("all") | Some("both") => add("clear", "both"),
            _ => {}
        },
        "ul" | "ol" | "li" => {
            if let Some(t) = a("type") {
                let ty = match t.trim() {
                    "1" => Some("decimal"),
                    "a" => Some("lower-alpha"),
                    "A" => Some("upper-alpha"),
                    "i" => Some("lower-roman"),
                    "I" => Some("upper-roman"),
                    other => match other.to_ascii_lowercase().as_str() {
                        "disc" => Some("disc"),
                        "circle" | "round" => Some("circle"),
                        "square" => Some("square"),
                        "none" => Some("none"),
                        _ => None,
                    },
                };
                if let Some(ty) = ty {
                    add("list-style-type", ty);
                }
            }
        }
        "pre" | "listing" | "xmp" => {
            if e.has_attr("wrap") {
                add("white-space", "pre-wrap");
            }
        }
        "textarea" => {
            if let Some(c) = a("cols").and_then(parse_uint) {
                add("width", &format!("{}ch", c.max(1)));
            }
            if let Some(r) = a("rows").and_then(parse_uint) {
                add("height", &format!("{}em", r.max(1) as f32 * 1.2));
            }
        }
        _ => {}
    }
    if css.is_empty() {
        return Vec::new();
    }
    bb_css::parse_declaration_list(&css)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions() {
        assert_eq!(parse_dimension("100").as_deref(), Some("100px"));
        assert_eq!(parse_dimension(" 50% ").as_deref(), Some("50%"));
        assert_eq!(parse_dimension("12.5px").as_deref(), Some("12.5px"));
        assert_eq!(parse_dimension("abc"), None);
        assert_eq!(font_size_from_attr("+1"), Some("large"));
        assert_eq!(font_size_from_attr("-1"), Some("small"));
        assert_eq!(font_size_from_attr("6"), Some("xx-large"));
        assert_eq!(font_size_from_attr("99"), Some("xxx-large"));
    }

    #[test]
    fn hints_from_markup() {
        let doc = bb_html::parse_document(
            "<body bgcolor=black text=lime><font size=6 color=maroon face='Arial, Helvetica'>x</font>\
             <table border=2 cellpadding=3 width=532><tr><td bgcolor=black>y</table>",
        );
        let body = doc.body().unwrap();
        let names: Vec<String> = presentational_hints(&doc, body).into_iter().map(|d| d.name).collect();
        assert_eq!(names, vec!["background-color", "color"]);
        let font = doc.elements_by_tag("font")[0];
        assert_eq!(presentational_hints(&doc, font).len(), 3);
        let td = doc.elements_by_tag("td")[0];
        let tdn: Vec<String> = presentational_hints(&doc, td).into_iter().map(|d| d.name).collect();
        assert!(tdn.contains(&"padding".to_string()) && tdn.contains(&"border-style".to_string()));
    }
}

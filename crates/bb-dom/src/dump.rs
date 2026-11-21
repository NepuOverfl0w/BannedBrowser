//! Tree dumps: the html5lib-tests tree format and an HTML serializer.

use crate::{Document, Namespace, NodeData, NodeId};

/// Dump the children of `root` in html5lib-tests "#document" format
/// (each line prefixed by `| `).
pub fn dump_html5lib(doc: &Document, root: NodeId) -> String {
    let mut out = String::new();
    for c in doc.children(root) {
        dump_node(doc, c, 0, &mut out);
    }
    out
}

fn indent(out: &mut String, depth: usize) {
    out.push_str("| ");
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn dump_node(doc: &Document, id: NodeId, depth: usize, out: &mut String) {
    indent(out, depth);
    match &doc.node(id).data {
        NodeData::Document | NodeData::DocumentFragment => out.push_str("#document\n"),
        NodeData::Doctype { name, public_id, system_id } => {
            if public_id.is_empty() && system_id.is_empty() {
                out.push_str(&format!("<!DOCTYPE {name}>\n"));
            } else {
                out.push_str(&format!("<!DOCTYPE {name} \"{public_id}\" \"{system_id}\">\n"));
            }
        }
        NodeData::Text(t) => out.push_str(&format!("\"{t}\"\n")),
        NodeData::Comment(t) => out.push_str(&format!("<!-- {t} -->\n")),
        NodeData::Element(e) => {
            match e.ns {
                Namespace::Svg => out.push_str(&format!("<svg {}>\n", e.name)),
                Namespace::MathMl => out.push_str(&format!("<math {}>\n", e.name)),
                _ => out.push_str(&format!("<{}>\n", e.name)),
            }
            let mut attrs: Vec<(String, &str)> = e
                .attrs
                .iter()
                .map(|a| {
                    let name = match a.ns {
                        Namespace::XLink => format!("xlink {}", a.name),
                        Namespace::Xml => format!("xml {}", a.name),
                        Namespace::Xmlns => format!("xmlns {}", a.name),
                        _ => a.name.clone(),
                    };
                    (name, a.value.as_str())
                })
                .collect();
            attrs.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, value) in attrs {
                indent(out, depth + 1);
                out.push_str(&format!("{name}=\"{value}\"\n"));
            }
            if let Some(frag) = e.template_contents {
                indent(out, depth + 1);
                out.push_str("content\n");
                for c in doc.children(frag) {
                    dump_node(doc, c, depth + 2, out);
                }
            }
        }
    }
    for c in doc.children(id) {
        dump_node(doc, c, depth + 1, out);
    }
}

const VOID: &[&str] = &[
    "area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "img", "input", "keygen",
    "link", "meta", "param", "source", "track", "wbr",
];

fn escape(s: &str, attr: bool, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '\u{A0}' => out.push_str("&nbsp;"),
            '"' if attr => out.push_str("&quot;"),
            '<' if !attr => out.push_str("&lt;"),
            '>' if !attr => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

/// Serialize `id` (and descendants) as HTML (HTML §13.3, simplified).
pub fn serialize_html(doc: &Document, id: NodeId) -> String {
    let mut out = String::new();
    ser(doc, id, &mut out);
    out
}

fn ser(doc: &Document, id: NodeId, out: &mut String) {
    match &doc.node(id).data {
        NodeData::Document | NodeData::DocumentFragment => {
            for c in doc.children(id) {
                ser(doc, c, out);
            }
        }
        NodeData::Doctype { name, .. } => out.push_str(&format!("<!DOCTYPE {name}>")),
        NodeData::Comment(t) => out.push_str(&format!("<!--{t}-->")),
        NodeData::Text(t) => {
            let raw = doc.parent(id).and_then(|p| doc.element(p)).is_some_and(|e| {
                e.ns == Namespace::Html
                    && matches!(
                        e.name.as_str(),
                        "style" | "script" | "xmp" | "iframe" | "noembed" | "noframes" | "plaintext"
                    )
            });
            if raw {
                out.push_str(t);
            } else {
                escape(t, false, out);
            }
        }
        NodeData::Element(e) => {
            out.push('<');
            out.push_str(&e.name);
            for a in &e.attrs {
                out.push(' ');
                if let Some(p) = &a.prefix {
                    out.push_str(p);
                    out.push(':');
                }
                out.push_str(&a.name);
                out.push_str("=\"");
                escape(&a.value, true, out);
                out.push('"');
            }
            out.push('>');
            if e.ns == Namespace::Html && VOID.contains(&e.name.as_str()) {
                return;
            }
            let kids = e.template_contents.unwrap_or(id);
            for c in doc.children(kids) {
                ser(doc, c, out);
            }
            out.push_str("</");
            out.push_str(&e.name);
            out.push('>');
        }
    }
}

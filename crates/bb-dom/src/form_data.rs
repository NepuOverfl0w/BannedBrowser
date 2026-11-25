//! Form data (HTML §4.10.21): constructing a form's entry list (§4.10.21.4) and the three
//! encodings (`application/x-www-form-urlencoded`, `multipart/form-data`, `text/plain`).
//! Form submission (bb-engine) and FormData (bb-webapi) share them.

use encoding_rs::Encoding;

use crate::{Document, InputType, NodeId};

/// One entry's value in an entry list (HTML §4.10.21.4): a string or a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryValue {
    Text(String),
    /// A file: its name, type (MIME type, may be empty) and contents.
    File {
        name: String,
        content_type: String,
        bytes: Vec<u8>,
    },
}

impl EntryValue {
    /// An empty file named `name` (what a file input with no file submits).
    pub fn empty_file(name: &str) -> Self {
        EntryValue::File { name: name.to_string(), content_type: String::new(), bytes: Vec::new() }
    }
}

/// Whether `el` is a button (§4.10.2): submit, reset, button and image inputs, `<button>`.
pub fn is_button(doc: &Document, el: NodeId) -> bool {
    doc.is_html(el, "button")
        || (doc.is_html(el, "input")
            && matches!(
                doc.input_type(el),
                InputType::Submit | InputType::Reset | InputType::Button | InputType::Image
            ))
}

/// §4.10.10 an option's value: its `value` attribute, else its text with whitespace stripped
/// and collapsed.
pub fn option_value(doc: &Document, option: NodeId) -> String {
    match doc.attr(option, "value") {
        Some(v) => v.to_string(),
        None => option_label(doc, option),
    }
}

/// An option's label as shown: its `label` attribute if non-empty, else its collapsed text.
pub fn option_label(doc: &Document, option: NodeId) -> String {
    if let Some(l) = doc.attr(option, "label").filter(|l| !l.is_empty()) {
        return l.to_string();
    }
    doc.text_content(option).split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

/// §4.10.21.4 "constructing the entry list". `image_point` is where an image button
/// submitter was clicked (CSS px relative to the image).
pub fn entry_list(
    doc: &Document,
    form: NodeId,
    submitter: Option<NodeId>,
    image_point: Option<(i32, i32)>,
    encoding: &'static Encoding,
) -> Vec<(String, EntryValue)> {
    let mut out = Vec::new();
    for el in doc.form_controls(form) {
        // §4.13.7.4 a form-associated custom element's entry construction.
        if let Some(sub) = doc.face_submission(el) {
            if doc.is_disabled(el) || doc.ancestors(el).any(|a| doc.is_html(a, "datalist")) {
                continue;
            }
            match sub {
                crate::forms::FaceSubmission::None => {}
                crate::forms::FaceSubmission::Value(v) => {
                    if let Some(name) = doc.attr(el, "name").filter(|n| !n.is_empty()) {
                        out.push((name.to_string(), v.clone()));
                    }
                }
                crate::forms::FaceSubmission::Entries(list) => out.extend(list.iter().cloned()),
            }
            continue;
        }
        let submittable = doc.is_html(el, "button")
            || doc.is_html(el, "input")
            || doc.is_html(el, "select")
            || doc.is_html(el, "textarea");
        if !submittable
            || doc.ancestors(el).any(|a| doc.is_html(a, "datalist"))
            || doc.is_disabled(el)
            || (is_button(doc, el) && Some(el) != submitter)
        {
            continue;
        }
        let ty = doc.is_html(el, "input").then(|| doc.input_type(el));
        if matches!(ty, Some(InputType::Checkbox) | Some(InputType::Radio)) && !doc.checkedness(el) {
            continue;
        }
        if ty == Some(InputType::Image) {
            let name = doc.attr(el, "name").unwrap_or("");
            let prefix = if name.is_empty() { String::new() } else { format!("{name}.") };
            let (x, y) = image_point.unwrap_or((0, 0));
            out.push((format!("{prefix}x"), EntryValue::Text(x.to_string())));
            out.push((format!("{prefix}y"), EntryValue::Text(y.to_string())));
            continue;
        }
        let Some(name) = doc.attr(el, "name").filter(|n| !n.is_empty()) else { continue };
        let name = name.to_string();
        if doc.is_html(el, "select") {
            for o in doc.list_of_options(el) {
                if doc.option_selectedness(o) && !doc.option_disabled(o) {
                    out.push((name.clone(), EntryValue::Text(option_value(doc, o))));
                }
            }
        } else if ty == Some(InputType::File) {
            out.push((name, EntryValue::empty_file("")));
        } else if ty == Some(InputType::Hidden)
            && name.eq_ignore_ascii_case("_charset_")
            && doc.attr(el, "value").is_none()
        {
            out.push((name, EntryValue::Text(encoding.name().to_string())));
        } else {
            out.push((name, EntryValue::Text(doc.control_value(el))));
        }
        // dirname: the element's directionality.
        let dir_capable = doc.is_html(el, "textarea")
            || matches!(ty, Some(InputType::Text) | Some(InputType::Search) | Some(InputType::Hidden));
        if dir_capable {
            if let Some(dirname) = doc.attr(el, "dirname").filter(|d| !d.is_empty()) {
                let rtl = std::iter::once(el)
                    .chain(doc.ancestors(el))
                    .find_map(|a| doc.attr(a, "dir"))
                    .is_some_and(|d| d.eq_ignore_ascii_case("rtl"));
                out.push((dirname.to_string(), EntryValue::Text(if rtl { "rtl" } else { "ltr" }.into())));
            }
        }
    }
    out
}

/// Line breaks become CR LF (§4.10.21.4 "convert to a list of name-value pairs").
fn crlf(s: &str) -> String {
    crate::normalize_newlines(s).replace('\n', "\r\n")
}

/// Encode text in `enc`; characters it can't represent become HTML numeric character
/// references (what encoding_rs does for HTML forms).
fn encode(s: &str, enc: &'static Encoding) -> Vec<u8> {
    enc.encode(s).0.into_owned()
}

/// URL §5.2 application/x-www-form-urlencoded byte serializer.
fn urlencode_bytes(bytes: &[u8], out: &mut String) {
    for &b in bytes {
        match b {
            b' ' => out.push('+'),
            b'*' | b'-' | b'.' | b'_' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
}

/// URL §5.2 application/x-www-form-urlencoded serializer, with the form's encoding.
pub fn urlencode(entries: &[(String, EntryValue)], enc: &'static Encoding) -> String {
    let mut out = String::new();
    for (i, (name, value)) in entries.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        let v = match value {
            EntryValue::Text(t) => crlf(t),
            EntryValue::File { name, .. } => name.clone(),
        };
        urlencode_bytes(&encode(&crlf(name), enc), &mut out);
        out.push('=');
        urlencode_bytes(&encode(&v, enc), &mut out);
    }
    out
}

/// §4.10.21.9 text/plain encoding.
pub fn text_plain(entries: &[(String, EntryValue)], enc: &'static Encoding) -> Vec<u8> {
    let mut s = String::new();
    for (name, value) in entries {
        let v = match value {
            EntryValue::Text(t) => crlf(t),
            EntryValue::File { name, .. } => name.clone(),
        };
        s.push_str(&crlf(name));
        s.push('=');
        s.push_str(&v);
        s.push_str("\r\n");
    }
    encode(&s, enc)
}

/// §4.10.21.8 multipart/form-data encoding (RFC 7578). Names and filenames escape `"`, CR and
/// LF as `%22`, `%0D` and `%0A`.
pub fn multipart(entries: &[(String, EntryValue)], enc: &'static Encoding, boundary: &str) -> Vec<u8> {
    let esc = |s: &str| crlf(s).replace('"', "%22").replace('\r', "%0D").replace('\n', "%0A");
    let mut body = Vec::new();
    for (name, value) in entries {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match value {
            EntryValue::Text(t) => {
                body.extend_from_slice(b"Content-Disposition: form-data; name=\"");
                body.extend_from_slice(&encode(&esc(name), enc));
                body.extend_from_slice(b"\"\r\n\r\n");
                body.extend_from_slice(&encode(&crlf(t), enc));
            }
            EntryValue::File { name: file, content_type, bytes } => {
                body.extend_from_slice(b"Content-Disposition: form-data; name=\"");
                body.extend_from_slice(&encode(&esc(name), enc));
                body.extend_from_slice(b"\"; filename=\"");
                body.extend_from_slice(&encode(&esc(file), enc));
                let ty = if content_type.is_empty() { "application/octet-stream" } else { content_type };
                body.extend_from_slice(format!("\"\r\nContent-Type: {ty}\r\n\r\n").as_bytes());
                body.extend_from_slice(bytes);
            }
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

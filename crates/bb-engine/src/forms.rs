//! Form submission (HTML §4.10.21): constructing the entry list, the three encodings
//! (`application/x-www-form-urlencoded`, `multipart/form-data`, `text/plain`), and turning the
//! form's method, action and target into a navigation request.
//!
//! Not implemented yet: constraint validation beyond `required` (no validation message
//! bubble), file uploads (a file input submits an empty file), `method="dialog"`, and the
//! `submit`/`formdata` events (no JS).

use bb_dom::{Document, InputType, NodeId};
use bb_url::Url;
use encoding_rs::Encoding;

/// A POST body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostBody {
    pub content_type: String,
    pub body: Vec<u8>,
}

/// A navigation: a URL, and for form POSTs a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavRequest {
    pub url: Url,
    pub post: Option<PostBody>,
}

impl NavRequest {
    pub fn get(url: Url) -> Self {
        NavRequest { url, post: None }
    }
}

impl From<Url> for NavRequest {
    fn from(url: Url) -> Self {
        NavRequest::get(url)
    }
}

/// A form submission ready to navigate: the request and the target browsing context name
/// (as for links: empty, `_self`, `_blank`, `_top`, `_parent` or a frame name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormSubmission {
    pub request: NavRequest,
    pub target: String,
}

/// One entry of the entry list (§4.10.21.4); file inputs contribute empty files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryValue {
    Text(String),
    /// A file with this name (always empty: file uploads aren't implemented).
    File(String),
}

/// Whether `el` is a submit button (§4.10.2): `<button>` in the Submit Button state (the
/// default), `<input type=submit>` and `<input type=image>`.
pub fn is_submit_button(doc: &Document, el: NodeId) -> bool {
    if doc.is_html(el, "button") {
        let ty = doc.attr(el, "type").map(|t| t.trim().to_ascii_lowercase());
        return !matches!(ty.as_deref(), Some("button") | Some("reset"));
    }
    doc.is_html(el, "input") && matches!(doc.input_type(el), InputType::Submit | InputType::Image)
}

/// Whether `el` is a reset button: `<button type=reset>` or `<input type=reset>`.
pub fn is_reset_button(doc: &Document, el: NodeId) -> bool {
    if doc.is_html(el, "button") {
        return doc.attr(el, "type").is_some_and(|t| t.trim().eq_ignore_ascii_case("reset"));
    }
    doc.is_html(el, "input") && doc.input_type(el) == InputType::Reset
}

fn is_button(doc: &Document, el: NodeId) -> bool {
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

/// The encoding used for submission (§4.10.21.6 "pick an encoding for the form"): the first
/// label in `accept-charset` we know, else the document's; its output encoding (UTF-16 and
/// replacement become UTF-8).
pub fn form_encoding(doc: &Document, form: NodeId, doc_encoding: &'static Encoding) -> &'static Encoding {
    let picked = doc
        .attr(form, "accept-charset")
        .and_then(|a| a.split_ascii_whitespace().find_map(|l| Encoding::for_label(l.as_bytes())))
        .unwrap_or(doc_encoding);
    picked.output_encoding()
}

/// The element is a `required` control whose value is missing (§4.10.5.3.4, §4.10.7,
/// §4.10.11): the only constraint we check before submitting.
pub fn value_missing(doc: &Document, el: NodeId) -> bool {
    if doc.attr(el, "required").is_none() || doc.is_disabled(el) || doc.attr(el, "readonly").is_some() {
        return false;
    }
    if doc.is_html(el, "textarea") {
        return doc.control_value(el).is_empty();
    }
    if doc.is_html(el, "select") {
        return !doc
            .list_of_options(el)
            .into_iter()
            .any(|o| doc.option_selectedness(o) && !option_value(doc, o).is_empty());
    }
    if !doc.is_html(el, "input") {
        return false;
    }
    match doc.input_type(el) {
        InputType::Checkbox => !doc.checkedness(el),
        InputType::Radio => !doc.radio_group(el).into_iter().any(|r| doc.checkedness(r)),
        InputType::File => true,
        t if t.is_text_entry() => doc.control_value(el).is_empty(),
        _ => false,
    }
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
            out.push((name, EntryValue::File(String::new())));
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
    bb_dom::normalize_newlines(s).replace('\n', "\r\n")
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
            EntryValue::File(f) => f.clone(),
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
            EntryValue::File(f) => f.clone(),
        };
        s.push_str(&crlf(name));
        s.push('=');
        s.push_str(&v);
        s.push_str("\r\n");
    }
    encode(&s, enc)
}

/// A multipart boundary (RFC 7578 §4.1: must not occur in the body; random, 128 bits).
pub fn new_boundary() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut s = String::from("----BannedBrowserFormBoundary");
    for i in 0..2u8 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u8(i);
        s.push_str(&format!("{:016x}", h.finish()));
    }
    s
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
            EntryValue::File(f) => {
                body.extend_from_slice(b"Content-Disposition: form-data; name=\"");
                body.extend_from_slice(&encode(&esc(name), enc));
                body.extend_from_slice(b"\"; filename=\"");
                body.extend_from_slice(&encode(&esc(f), enc));
                body.extend_from_slice(b"\"\r\nContent-Type: application/octet-stream\r\n\r\n");
            }
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

/// §4.10.21.3 "submit": the request for submitting `form` with `submitter` (the form itself
/// for implicit submission). `None` when nothing should be navigated (an unparseable action,
/// an unsupported scheme or `method=dialog`).
pub fn submission(
    doc: &Document,
    form: NodeId,
    submitter: Option<NodeId>,
    image_point: Option<(i32, i32)>,
    document_url: &Url,
    base_url: &Url,
    doc_encoding: &'static Encoding,
) -> Option<FormSubmission> {
    // Attributes come from the submitter's formaction/formmethod/... when it has them.
    let from = |attr: &str, form_attr: &str| -> Option<String> {
        submitter
            .filter(|&s| is_submit_button(doc, s))
            .and_then(|s| doc.attr(s, attr))
            .or_else(|| doc.attr(form, form_attr))
            .map(str::to_string)
    };
    let enc = form_encoding(doc, form, doc_encoding);
    let entries = entry_list(doc, form, submitter, image_point, enc);
    // Step 13-14: an empty action is the document's URL.
    let action = from("formaction", "action").unwrap_or_default();
    let mut url =
        if action.trim().is_empty() { document_url.clone() } else { base_url.join(action.trim()).ok()? };
    let method = from("formmethod", "method").map(|m| m.trim().to_ascii_lowercase()).unwrap_or_default();
    let method = match method.as_str() {
        "post" => "post",
        "dialog" => return None,
        _ => "get",
    };
    let enctype = from("formenctype", "enctype").map(|m| m.trim().to_ascii_lowercase()).unwrap_or_default();
    let target = from("formtarget", "target")
        .or_else(|| {
            doc.elements_by_tag("base").into_iter().find_map(|b| doc.attr(b, "target").map(str::to_string))
        })
        .unwrap_or_default()
        .trim()
        .to_string();
    let request = match (url.scheme(), method) {
        ("http" | "https" | "data" | "file", "get") => {
            // "Mutate action URL": the query becomes the entry list (the fragment stays).
            url.set_query_encoded(Some(&urlencode(&entries, enc)));
            NavRequest { url, post: None }
        }
        ("http" | "https", "post") => {
            let post = match enctype.as_str() {
                "multipart/form-data" => {
                    let boundary = new_boundary();
                    PostBody {
                        content_type: format!("multipart/form-data; boundary={boundary}"),
                        body: multipart(&entries, enc, &boundary),
                    }
                }
                "text/plain" => {
                    PostBody { content_type: "text/plain".into(), body: text_plain(&entries, enc) }
                }
                _ => PostBody {
                    content_type: "application/x-www-form-urlencoded".into(),
                    body: urlencode(&entries, enc).into_bytes(),
                },
            };
            NavRequest { url, post: Some(post) }
        }
        // data: POST navigates to the action as is; mailto:, javascript: and others aren't
        // supported (javascript: needs the JS engine; mailto: an external mail client).
        ("data", "post") => NavRequest { url, post: None },
        _ => return None,
    };
    Some(FormSubmission { request, target })
}

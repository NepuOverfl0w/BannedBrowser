//! Form submission (HTML §4.10.21): constructing the entry list, the three encodings
//! (`application/x-www-form-urlencoded`, `multipart/form-data`, `text/plain`), and turning the
//! form's method, action and target into a navigation request.
//!
//! Constraint validation is bb-dom's (`validity.rs`); the page checks it before submitting.
//! Not implemented yet: file uploads (a file input submits an empty file) and
//! `method="dialog"`.

use bb_dom::{Document, InputType, NodeId};

pub use bb_dom::form_data::{
    entry_list, is_button, multipart, option_label, option_value, text_plain, urlencode, EntryValue,
};
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
    /// How the load uses the HTTP cache (its document, subresources and frames): reload
    /// revalidates, a hard reload bypasses, Back/Forward prefers the cache.
    pub cache_mode: bb_net::cache::CacheMode,
}

impl NavRequest {
    pub fn get(url: Url) -> Self {
        NavRequest { url, post: None, cache_mode: Default::default() }
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
            NavRequest::get(url)
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
            NavRequest { url, post: Some(post), cache_mode: Default::default() }
        }
        // data: POST navigates to the action as is; mailto:, javascript: and others aren't
        // supported (javascript: needs the JS engine; mailto: an external mail client).
        ("data", "post") => NavRequest::get(url),
        _ => return None,
    };
    Some(FormSubmission { request, target })
}

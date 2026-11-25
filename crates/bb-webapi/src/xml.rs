//! DOMParser and XMLSerializer (HTML §8.5 "DOM parsing and serialization APIs"), and the
//! XML side of markup getters and setters: in XML documents innerHTML and outerHTML use the
//! XML serialization (require well-formed) and the XML fragment parsing algorithm.

use bb_dom::{Document, Namespace, NodeId};
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::exception::throw;
use crate::host::{arg_node, host, js, rstr, this_of, with_doc, with_doc_mut, wrap, Host, NodeRef};
use crate::idl::op;

type R = JsResult<Value>;

/// The namespace of the element a parse error document has (HTML §8.5.1 step 5).
pub const PARSERERROR_NS: &str = "http://www.mozilla.org/newlayout/xml/parsererror.xml";

/// Parse `text` as an XML document into a new document of type `ty`; a parse error gives a
/// document with a `parsererror` element instead.
pub fn parse_xml_document(h: &Host, text: &str, ty: &str, url: &str) -> u32 {
    install(h, xml_or_error(text), ty, url)
}

/// `text` parsed as XML, or a document with a `parsererror` element if it isn't well formed.
pub fn xml_or_error(text: &str) -> Document {
    match bb_xml::parse(text) {
        Ok(d) => d,
        Err(e) => {
            let mut d = Document::new();
            let root = d.create_element(Namespace::from_url(PARSERERROR_NS), "parsererror", Vec::new());
            let msg = d.create_text(&format!("XML Parsing Error: {}", e));
            d.append_child(root, msg);
            d.append_child(Document::ROOT, root);
            d
        }
    }
}

fn install(h: &Host, mut doc: Document, ty: &str, url: &str) -> u32 {
    doc.url = url.to_string();
    let idx = crate::dom::new_document(h, ty);
    if let Ok(mut docs) = h.docs.try_borrow_mut() {
        if let Some(slot) = docs.get_mut(idx as usize) {
            *slot = doc;
        }
    }
    idx
}

pub fn dom_parser_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("DOMParser requires 'new'") };
    let h = host(vm)?;
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => p,
        _ => h.proto(C::DOMParser),
    };
    Ok(Value::Object(vm.new_host_object(Some(proto), C::DOMParser as u32, 0, false)))
}

/// HTML §8.4.2 `Document.parseHTMLUnsafe(html)`: a new HTML document, declarative shadow
/// roots allowed (scripts don't run: it has no browsing context).
pub fn setup_document_statics(vm: &mut Vm, ctor: ObjRef) {
    op(vm, ctor, "parseHTMLUnsafe", 1, |vm, c| {
        let text = rstr(vm, &c.arg(0))?;
        let h = host(vm)?;
        let idx = install(&h, bb_html::parse_document_with_shadow_roots(&text), "text/html", "about:blank");
        mark_scripts_started(&h, idx);
        wrap(vm, &h, NodeRef { doc: idx, id: Document::ROOT })
    });
}

pub fn setup_dom_parser(vm: &mut Vm, proto: ObjRef) {
    // HTML §8.5.1 parseFromString(string, type).
    op(vm, proto, "parseFromString", 2, |vm, c| {
        this_of(vm, &c.this, C::DOMParser)?;
        let text = rstr(vm, &c.arg(0))?;
        let ty = rstr(vm, &c.arg(1))?;
        if !matches!(
            ty.as_str(),
            "text/html" | "text/xml" | "application/xml" | "application/xhtml+xml" | "image/svg+xml"
        ) {
            return vm.type_error(&format!("'{ty}' is not a type DOMParser supports"));
        }
        let h = host(vm)?;
        let url = h.docs.borrow().first().map(|d| d.url.clone()).unwrap_or_default();
        let idx = if ty == "text/html" {
            // Scripting is disabled for the new document: no scripts run, <noscript> parses.
            install(&h, bb_html::parse_document(&text), "text/html", &url)
        } else {
            parse_xml_document(&h, &text, &ty, &url)
        };
        mark_scripts_started(&h, idx);
        wrap(vm, &h, NodeRef { doc: idx, id: Document::ROOT })
    });
}

pub fn xml_serializer_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("XMLSerializer requires 'new'") };
    let h = host(vm)?;
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => p,
        _ => h.proto(C::XMLSerializer),
    };
    Ok(Value::Object(vm.new_host_object(Some(proto), C::XMLSerializer as u32, 0, false)))
}

pub fn setup_xml_serializer(vm: &mut Vm, proto: ObjRef) {
    // DOM Parsing serializeToString(root): the XML serialization, require well-formed unset.
    op(vm, proto, "serializeToString", 1, |vm, c| {
        this_of(vm, &c.this, C::XMLSerializer)?;
        if crate::attr::is_attr(vm, &c.arg(0)) {
            // An Attr serializes as nothing: attributes are serialized with their element.
            return Ok(js(""));
        }
        let n = arg_node(vm, &c.arg(0), "the root")?;
        let h = host(vm)?;
        let s = with_doc(vm, &h, n.doc, |d| bb_xml::serialize::serialize(d, n.id, false))?;
        match s {
            Ok(s) => Ok(js(&s)),
            Err(e) => throw(vm, "InvalidStateError", &e.0),
        }
    });
}

/// Scripts of a document parsed with scripting disabled never run, even when moved into
/// the window's document: they are marked "already started".
pub fn mark_scripts_started(h: &Host, idx: u32) {
    let scripts: Vec<u64> = {
        let Ok(docs) = h.docs.try_borrow() else { return };
        let Some(d) = docs.get(idx as usize) else { return };
        d.shadow_including_descendants(Document::ROOT)
            .into_iter()
            .filter(|&x| d.element(x).is_some_and(|e| e.name == "script"))
            .map(|x| NodeRef { doc: idx, id: x }.pack())
            .collect()
    };
    h.started_scripts.borrow_mut().extend(scripts);
}

/// The fragment serializing algorithm for an XML document's node: its children (or the node
/// itself, for outerHTML) as XML, require well-formed set.
pub fn serialize_markup(vm: &mut Vm, h: &Host, n: NodeRef, children: bool) -> JsResult<String> {
    let r = with_doc(vm, h, n.doc, |d| {
        if children {
            bb_xml::serialize::serialize_children(d, n.id, true)
        } else {
            bb_xml::serialize::serialize(d, n.id, true)
        }
    })?;
    match r {
        Ok(s) => Ok(s),
        Err(e) => throw(vm, "InvalidStateError", &e.0),
    }
}

/// The namespace bindings in scope at element `n` (outermost first), as the XML fragment
/// parsing algorithm declares them on its context element (HTML §13.4 step 2).
fn bindings(d: &Document, n: NodeId) -> Vec<(Option<String>, String)> {
    let mut chain: Vec<NodeId> = std::iter::once(n).chain(d.ancestors(n)).collect();
    chain.reverse();
    let mut out = Vec::new();
    for x in chain {
        let Some(e) = d.element(x) else { continue };
        if e.ns != Namespace::None {
            out.push((e.prefix.clone(), e.ns.url().to_string()));
        }
        for a in &e.attrs {
            if a.ns.url() == bb_xml::XMLNS_NS {
                let p = if a.prefix.is_none() { None } else { Some(a.name.clone()) };
                out.push((p, a.value.clone()));
            }
        }
    }
    // The context element's own namespace is the default when it has no prefix.
    if let Some(e) = d.element(n) {
        if e.prefix.is_none() {
            out.push((None, if e.ns == Namespace::None { String::new() } else { e.ns.url().to_string() }));
        }
    }
    out
}

/// HTML §13.4 "XML fragment parsing algorithm" with context element `context`: the parsed
/// nodes, in a new DocumentFragment of document `doc`.
pub fn parse_fragment(vm: &mut Vm, h: &Host, doc: u32, context: NodeRef, markup: &str) -> JsResult<NodeRef> {
    let scope = with_doc(vm, h, context.doc, |d| bindings(d, context.id))?;
    // A default namespace bound to "" means no default namespace.
    let scope: Vec<(Option<String>, String)> =
        scope.into_iter().filter(|(p, u)| p.is_some() || !u.is_empty()).collect();
    let src = match bb_xml::parse_fragment(markup, &scope) {
        Ok(d) => d,
        Err(e) => return throw(vm, "SyntaxError", &format!("the markup isn't well-formed XML: {e}")),
    };
    let frag = with_doc_mut(vm, h, doc, |d| {
        let frag = d.create_fragment();
        let kids: Vec<NodeId> = src.children(Document::ROOT).collect();
        for k in kids {
            let (copy, _) = d.import_node(&src, k, true);
            d.append_child(frag, copy);
        }
        frag
    })?;
    // Scripts in it never run.
    let scripts: Vec<u64> = with_doc(vm, h, doc, |d| {
        d.descendants(frag)
            .filter(|&x| d.element(x).is_some_and(|e| e.name == "script"))
            .map(|x| NodeRef { doc, id: x }.pack())
            .collect()
    })?;
    h.started_scripts.borrow_mut().extend(scripts);
    Ok(NodeRef { doc, id: frag })
}

/// Whether node `n`'s document is an XML document (not HTML).
pub fn in_xml_document(h: &Host, n: NodeRef) -> bool {
    !crate::dom::is_html_doc(h, n.doc)
}

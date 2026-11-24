//! DOM Standard interfaces: Node (§4.4), Document (§4.5), DocumentType (§4.6),
//! DocumentFragment (§4.7), Element (§4.9), CharacterData (§4.10), Text (§4.11),
//! ProcessingInstruction (§4.13), Comment (§4.14), DOMImplementation (§4.5.1), and the
//! ParentNode / ChildNode / NonDocumentTypeChildNode / NonElementParentNode mixins (§4.2.6–8).

use std::rc::Rc;

use bb_dom::{Attribute, Document, Namespace, NodeData, NodeId};
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::collections::{collection, CollKind};
use crate::exception::throw;
use crate::host::*;
use crate::idl::{attr, constant, op};
use crate::tree;

type R = JsResult<Value>;

fn this(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, NodeRef)> {
    this_node(vm, &c.this, C::Node)
}

fn node_rel(vm: &mut Vm, c: &NativeCall, f: fn(&Document, NodeId) -> Option<NodeId>) -> R {
    let (h, n) = this(vm, c)?;
    let r = with_doc(vm, &h, n.doc, |d| f(d, n.id))?;
    wrap_opt(vm, &h, r.map(|x| n.with(x)))
}

// ---- names ----

/// DOM §4.9 "valid element local name".
pub fn valid_element_local_name(n: &str) -> bool {
    let mut cs = n.chars();
    let Some(first) = cs.next() else { return false };
    if first.is_ascii_alphabetic() {
        return !n.chars().any(|c| c.is_ascii_whitespace() || matches!(c, '\0' | '/' | '>'));
    }
    if !(first == ':' || first == '_' || first as u32 >= 0x80) {
        return false;
    }
    cs.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '_') || c as u32 >= 0x80)
}

/// DOM §4.9 "valid attribute local name".
pub fn valid_attribute_local_name(n: &str) -> bool {
    !n.is_empty() && !n.chars().any(|c| c.is_ascii_whitespace() || matches!(c, '\0' | '/' | '>' | '='))
}

/// DOM §4.9 "valid namespace prefix".
fn valid_namespace_prefix(n: &str) -> bool {
    !n.is_empty() && !n.chars().any(|c| c.is_ascii_whitespace() || matches!(c, '\0' | '/' | '>'))
}

/// DOM §4.9 "valid doctype name".
pub fn valid_doctype_name(n: &str) -> bool {
    !n.chars().any(|c| c.is_ascii_whitespace() || matches!(c, '\0' | '>'))
}

/// DOM §1.4 "validate and extract": (namespace, prefix, local name).
pub fn validate_and_extract(
    vm: &mut Vm,
    ns: Option<String>,
    qname: &str,
    element: bool,
) -> JsResult<(Namespace, Option<String>, String)> {
    let ns = ns.filter(|n| !n.is_empty());
    let (prefix, local) = match qname.split_once(':') {
        Some((p, l)) => {
            if !valid_namespace_prefix(p) {
                return throw(vm, "InvalidCharacterError", "invalid namespace prefix");
            }
            (Some(p.to_string()), l.to_string())
        }
        None => (None, qname.to_string()),
    };
    let ok = if element { valid_element_local_name(&local) } else { valid_attribute_local_name(&local) };
    if !ok {
        return throw(vm, "InvalidCharacterError", "invalid name");
    }
    let xml = Namespace::Xml.url();
    let xmlns = Namespace::Xmlns.url();
    if prefix.is_some() && ns.is_none() {
        return throw(vm, "NamespaceError", "a prefix needs a namespace");
    }
    if prefix.as_deref() == Some("xml") && ns.as_deref() != Some(xml) {
        return throw(vm, "NamespaceError", "the xml prefix is for the XML namespace");
    }
    if (qname == "xmlns" || prefix.as_deref() == Some("xmlns")) && ns.as_deref() != Some(xmlns) {
        return throw(vm, "NamespaceError", "xmlns is for the XMLNS namespace");
    }
    if ns.as_deref() == Some(xmlns) && qname != "xmlns" && prefix.as_deref() != Some("xmlns") {
        return throw(vm, "NamespaceError", "the XMLNS namespace needs the xmlns prefix");
    }
    Ok((Namespace::from_url(ns.as_deref().unwrap_or("")), prefix, local))
}

fn ns_value(ns: Namespace) -> Value {
    if ns == Namespace::None {
        Value::Null
    } else {
        js(ns.url())
    }
}

/// Whether document `d` is an HTML document (DOM §4.5).
pub fn is_html_doc(h: &Host, d: u32) -> bool {
    h.doc_types.borrow().get(d as usize).map(String::as_str) == Some("text/html")
}

/// Element.tagName / Node.nodeName of an element (DOM §4.9 "HTML-uppercased qualified name").
fn tag_name(h: &Host, doc: u32, e: &bb_dom::ElementData) -> String {
    let q = match &e.prefix {
        Some(p) => format!("{p}:{}", e.name),
        None => e.name.clone(),
    };
    if e.ns == Namespace::Html && is_html_doc(h, doc) {
        q.to_ascii_uppercase()
    } else {
        q
    }
}

pub fn node_type(d: &NodeData) -> u16 {
    match d {
        NodeData::Element(_) => 1,
        NodeData::Text(_) => 3,
        NodeData::ProcessingInstruction { .. } => 7,
        NodeData::Comment(_) => 8,
        NodeData::Document => 9,
        NodeData::Doctype { .. } => 10,
        NodeData::DocumentFragment => 11,
    }
}

/// The data of a CharacterData node.
pub fn char_data(d: &Document, id: NodeId) -> Option<String> {
    match &d.node(id).data {
        NodeData::Text(t) | NodeData::Comment(t) | NodeData::ProcessingInstruction { data: t, .. } => {
            Some(t.clone())
        }
        _ => None,
    }
}

/// Descendant text, for textContent (DOM §4.4 "descendant text content").
fn descendant_text(d: &Document, id: NodeId) -> String {
    let mut s = String::new();
    for x in d.descendants(id) {
        if let NodeData::Text(t) = &d.node(x).data {
            s.push_str(t);
        }
    }
    s
}

/// The root of `id` (DOM §4.2.1).
fn root_of(d: &Document, id: NodeId) -> NodeId {
    let mut r = id;
    while let Some(p) = d.parent(r) {
        r = p;
    }
    r
}

// ---- Node ----

pub fn setup_node(vm: &mut Vm, proto: ObjRef, ctor: ObjRef) {
    for (n, v) in [
        ("ELEMENT_NODE", 1),
        ("ATTRIBUTE_NODE", 2),
        ("TEXT_NODE", 3),
        ("CDATA_SECTION_NODE", 4),
        ("ENTITY_REFERENCE_NODE", 5),
        ("ENTITY_NODE", 6),
        ("PROCESSING_INSTRUCTION_NODE", 7),
        ("COMMENT_NODE", 8),
        ("DOCUMENT_NODE", 9),
        ("DOCUMENT_TYPE_NODE", 10),
        ("DOCUMENT_FRAGMENT_NODE", 11),
        ("NOTATION_NODE", 12),
        ("DOCUMENT_POSITION_DISCONNECTED", 1),
        ("DOCUMENT_POSITION_PRECEDING", 2),
        ("DOCUMENT_POSITION_FOLLOWING", 4),
        ("DOCUMENT_POSITION_CONTAINS", 8),
        ("DOCUMENT_POSITION_CONTAINED_BY", 16),
        ("DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC", 32),
    ] {
        constant(vm, ctor, n, Value::Number(v as f64));
        constant(vm, proto, n, Value::Number(v as f64));
    }
    attr(
        vm,
        proto,
        "nodeType",
        |vm, c| {
            if crate::attr::is_attr(vm, &c.this) {
                return Ok(Value::Number(2.0));
            }
            let (h, n) = this(vm, &c)?;
            let t = with_doc(vm, &h, n.doc, |d| node_type(&d.node(n.id).data))?;
            Ok(Value::Number(t as f64))
        },
        None,
    );
    attr(
        vm,
        proto,
        "nodeName",
        |vm, c| {
            if let Some(a) = crate::attr::attr_of(vm, &c.this) {
                return Ok(js(&crate::attr::qualified(&a)));
            }
            let (h, n) = this(vm, &c)?;
            let s = with_doc(vm, &h, n.doc, |d| match &d.node(n.id).data {
                NodeData::Element(e) => tag_name(&h, n.doc, e),
                NodeData::Text(_) => "#text".into(),
                NodeData::Comment(_) => "#comment".into(),
                NodeData::Document => "#document".into(),
                NodeData::DocumentFragment => "#document-fragment".into(),
                NodeData::Doctype { name, .. } => name.clone(),
                NodeData::ProcessingInstruction { target, .. } => target.clone(),
            })?;
            Ok(js(&s))
        },
        None,
    );
    attr(
        vm,
        proto,
        "baseURI",
        |vm, c| {
            let h = host(vm)?;
            this_of(vm, &c.this, C::Node)?;
            let u = h.url.borrow().href();
            Ok(js(&u))
        },
        None,
    );
    attr(
        vm,
        proto,
        "isConnected",
        |vm, c| {
            if crate::attr::is_attr(vm, &c.this) {
                return Ok(Value::Bool(false));
            }
            let (h, n) = this(vm, &c)?;
            let r = with_doc(vm, &h, n.doc, |d| root_of(d, n.id) == Document::ROOT)?;
            Ok(Value::Bool(r))
        },
        None,
    );
    attr(
        vm,
        proto,
        "ownerDocument",
        |vm, c| {
            if let Some(a) = crate::attr::attr_of(vm, &c.this) {
                let h = host(vm)?;
                return wrap(vm, &h, NodeRef { doc: a.doc, id: Document::ROOT });
            }
            let (h, n) = this(vm, &c)?;
            if n.id == Document::ROOT {
                return Ok(Value::Null);
            }
            wrap(vm, &h, n.with(Document::ROOT))
        },
        None,
    );
    op(vm, proto, "getRootNode", 0, |vm, c| {
        if crate::attr::is_attr(vm, &c.this) {
            return Ok(c.this.clone());
        }
        node_rel(vm, &c, |d, id| Some(root_of(d, id)))
    });
    attr(
        vm,
        proto,
        "parentNode",
        |vm, c| {
            if crate::attr::is_attr(vm, &c.this) {
                return Ok(Value::Null);
            }
            node_rel(vm, &c, |d, id| d.parent(id))
        },
        None,
    );
    attr(
        vm,
        proto,
        "parentElement",
        |vm, c| {
            if crate::attr::is_attr(vm, &c.this) {
                return Ok(Value::Null);
            }
            node_rel(vm, &c, |d, id| d.parent(id).filter(|&p| d.node(p).is_element()))
        },
        None,
    );
    op(vm, proto, "hasChildNodes", 0, |vm, c| {
        if crate::attr::is_attr(vm, &c.this) {
            return Ok(Value::Bool(false));
        }
        let (h, n) = this(vm, &c)?;
        let r = with_doc(vm, &h, n.doc, |d| d.first_child(n.id).is_some())?;
        Ok(Value::Bool(r))
    });
    attr(
        vm,
        proto,
        "childNodes",
        |vm, c| {
            if crate::attr::is_attr(vm, &c.this) {
                let h = host(vm)?;
                return collection(vm, &h, CollKind::Static(Vec::new()), None);
            }
            let (h, n) = this(vm, &c)?;
            collection(vm, &h, CollKind::ChildNodes(n), Some((0, n.pack())))
        },
        None,
    );
    attr(vm, proto, "firstChild", |vm, c| sibling(vm, &c, Document::first_child), None);
    attr(vm, proto, "lastChild", |vm, c| sibling(vm, &c, Document::last_child), None);
    attr(vm, proto, "previousSibling", |vm, c| sibling(vm, &c, Document::prev_sibling), None);
    attr(vm, proto, "nextSibling", |vm, c| sibling(vm, &c, Document::next_sibling), None);
    attr(vm, proto, "nodeValue", node_value_get, Some(node_value_set));
    attr(vm, proto, "textContent", text_content_get, Some(text_content_set));
    op(vm, proto, "normalize", 0, |vm, c| {
        if crate::attr::is_attr(vm, &c.this) {
            return Ok(Value::Undefined);
        }
        let (h, n) = this(vm, &c)?;
        normalize(vm, &h, n)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "cloneNode", 0, |vm, c| {
        let deep = vm.to_boolean(&c.arg(0));
        if crate::attr::is_attr(vm, &c.this) {
            return crate::attr::clone_attr(vm, &c.this);
        }
        let (h, n) = this(vm, &c)?;
        let r = clone_node(vm, &h, n, deep)?;
        wrap(vm, &h, r)
    });
    op(vm, proto, "isEqualNode", 1, |vm, c| {
        let other = c.arg(0);
        if other.is_nullish() {
            this_of(vm, &c.this, C::Node)?;
            return Ok(Value::Bool(false));
        }
        if crate::attr::is_attr(vm, &c.this) || crate::attr::is_attr(vm, &other) {
            return Ok(Value::Bool(crate::attr::attrs_equal(vm, &c.this, &other)));
        }
        let (h, n) = this(vm, &c)?;
        let o = arg_node(vm, &other, "the argument")?;
        let docs = h.docs.borrow();
        let r = equal_nodes(&docs[n.doc as usize], n.id, &docs[o.doc as usize], o.id);
        Ok(Value::Bool(r))
    });
    op(vm, proto, "isSameNode", 1, |vm, c| {
        this_of(vm, &c.this, C::Node)?;
        Ok(Value::Bool(matches!((&c.this, &c.arg(0)), (Value::Object(a), Value::Object(b)) if a == b)))
    });
    op(vm, proto, "compareDocumentPosition", 1, compare_document_position);
    op(vm, proto, "contains", 1, |vm, c| {
        let other = c.arg(0);
        if other.is_nullish() {
            this_of(vm, &c.this, C::Node)?;
            return Ok(Value::Bool(false));
        }
        if crate::attr::is_attr(vm, &c.this) || crate::attr::is_attr(vm, &other) {
            let same = matches!((&c.this, &other), (Value::Object(a), Value::Object(b)) if a == b);
            return Ok(Value::Bool(same));
        }
        let (h, n) = this(vm, &c)?;
        let o = arg_node(vm, &other, "the argument")?;
        if o.doc != n.doc {
            return Ok(Value::Bool(false));
        }
        let r = with_doc(vm, &h, n.doc, |d| {
            let mut cur = Some(o.id);
            while let Some(x) = cur {
                if x == n.id {
                    return true;
                }
                cur = d.parent(x);
            }
            false
        })?;
        Ok(Value::Bool(r))
    });
    op(vm, proto, "lookupPrefix", 1, |vm, c| {
        let ns = rstr_opt(vm, &c.arg(0), true)?.filter(|s| !s.is_empty());
        let (h, n) = this(vm, &c)?;
        let Some(ns) = ns else { return Ok(Value::Null) };
        let r = with_doc(vm, &h, n.doc, |d| {
            let start = namespace_element(d, n.id)?;
            for a in std::iter::once(start).chain(d.ancestors(start)) {
                let e = d.element(a)?;
                if e.ns.url() == ns && e.prefix.is_some() {
                    return e.prefix.clone();
                }
                for at in &e.attrs {
                    if at.prefix.as_deref() == Some("xmlns") && at.value == ns {
                        return Some(at.name.clone());
                    }
                }
            }
            None
        })?;
        Ok(r.map_or(Value::Null, |s| js(&s)))
    });
    op(vm, proto, "lookupNamespaceURI", 1, |vm, c| {
        let p = rstr_opt(vm, &c.arg(0), true)?.filter(|s| !s.is_empty());
        let (h, n) = this(vm, &c)?;
        let r = with_doc(vm, &h, n.doc, |d| locate_namespace(d, n.id, p.as_deref()))?;
        Ok(r.map_or(Value::Null, |s| js(&s)))
    });
    op(vm, proto, "isDefaultNamespace", 1, |vm, c| {
        let ns = rstr_opt(vm, &c.arg(0), true)?.filter(|s| !s.is_empty());
        let (h, n) = this(vm, &c)?;
        let r = with_doc(vm, &h, n.doc, |d| locate_namespace(d, n.id, None))?;
        Ok(Value::Bool(r == ns))
    });
    op(vm, proto, "insertBefore", 2, |vm, c| {
        let (h, parent) = this(vm, &c)?;
        let node = arg_node(vm, &c.arg(0), "parameter 1")?;
        let child = match c.arg(1) {
            Value::Null => None,
            v => Some(arg_node(vm, &v, "parameter 2")?),
        };
        tree::pre_insert(vm, &h, node, parent, child)?;
        Ok(c.arg(0))
    });
    op(vm, proto, "appendChild", 1, |vm, c| {
        let (h, parent) = this(vm, &c)?;
        let node = arg_node(vm, &c.arg(0), "parameter 1")?;
        tree::append(vm, &h, node, parent)?;
        Ok(c.arg(0))
    });
    op(vm, proto, "replaceChild", 2, |vm, c| {
        let (h, parent) = this(vm, &c)?;
        let node = arg_node(vm, &c.arg(0), "parameter 1")?;
        let child = arg_node(vm, &c.arg(1), "parameter 2")?;
        tree::replace(vm, &h, child, node, parent)?;
        Ok(c.arg(1))
    });
    op(vm, proto, "removeChild", 1, |vm, c| {
        let (h, parent) = this(vm, &c)?;
        let child = arg_node(vm, &c.arg(0), "parameter 1")?;
        tree::pre_remove(vm, &h, child, parent)?;
        Ok(c.arg(0))
    });
}

/// A tree relation, null for attribute nodes (which are never in a tree).
fn sibling(vm: &mut Vm, c: &NativeCall, f: fn(&Document, NodeId) -> Option<NodeId>) -> R {
    if crate::attr::is_attr(vm, &c.this) {
        return Ok(Value::Null);
    }
    node_rel(vm, c, f)
}

fn node_value_get(vm: &mut Vm, c: NativeCall) -> R {
    if let Some(a) = crate::attr::attr_of(vm, &c.this) {
        let v = crate::attr::value(vm, &a)?;
        return Ok(js(&v));
    }
    let (h, n) = this(vm, &c)?;
    let r = with_doc(vm, &h, n.doc, |d| char_data(d, n.id))?;
    Ok(r.map_or(Value::Null, |s| js(&s)))
}

fn node_value_set(vm: &mut Vm, c: NativeCall) -> R {
    let v = rstr_null_empty(vm, &c.arg(0))?;
    if crate::attr::is_attr(vm, &c.this) {
        crate::attr::set_value(vm, &c.this, &v)?;
        return Ok(Value::Undefined);
    }
    let (h, n) = this(vm, &c)?;
    let is_cd = with_doc(vm, &h, n.doc, |d| char_data(d, n.id).is_some())?;
    if is_cd {
        crate::mutation::set_data(vm, &h, n, &v)?;
    }
    Ok(Value::Undefined)
}

pub fn text_content_get(vm: &mut Vm, c: NativeCall) -> R {
    if let Some(a) = crate::attr::attr_of(vm, &c.this) {
        let v = crate::attr::value(vm, &a)?;
        return Ok(js(&v));
    }
    let (h, n) = this(vm, &c)?;
    let r = with_doc(vm, &h, n.doc, |d| match &d.node(n.id).data {
        NodeData::Element(_) | NodeData::DocumentFragment => Some(descendant_text(d, n.id)),
        NodeData::Document | NodeData::Doctype { .. } => None,
        _ => char_data(d, n.id),
    })?;
    Ok(r.map_or(Value::Null, |s| js(&s)))
}

pub fn text_content_set(vm: &mut Vm, c: NativeCall) -> R {
    let v = rstr_null_empty(vm, &c.arg(0))?;
    if crate::attr::is_attr(vm, &c.this) {
        crate::attr::set_value(vm, &c.this, &v)?;
        return Ok(Value::Undefined);
    }
    let (h, n) = this(vm, &c)?;
    let kind = with_doc(vm, &h, n.doc, |d| match d.node(n.id).data {
        NodeData::Element(_) | NodeData::DocumentFragment => 1,
        NodeData::Document | NodeData::Doctype { .. } => 0,
        _ => 2,
    })?;
    match kind {
        1 => {
            // DOM §4.4 "string replace all".
            let node = if v.is_empty() {
                None
            } else {
                Some(n.with(with_doc_mut(vm, &h, n.doc, |d| d.create_text(&v))?))
            };
            tree::replace_all(vm, &h, node, n)?;
        }
        2 => crate::mutation::set_data(vm, &h, n, &v)?,
        _ => {}
    }
    Ok(Value::Undefined)
}

/// Node.normalize (DOM §4.4).
fn normalize(vm: &mut Vm, h: &Host, root: NodeRef) -> JsResult<()> {
    let texts: Vec<NodeId> = with_doc(vm, h, root.doc, |d| {
        d.descendants(root.id)
            .filter(|&x| x != root.id && matches!(d.node(x).data, NodeData::Text(_)))
            .collect()
    })?;
    for t in texts {
        let node = root.with(t);
        // Merged into an earlier node (removed) meanwhile?
        let inside = with_doc(vm, h, root.doc, |d| d.ancestors(t).any(|a| a == root.id))?;
        if !inside {
            continue;
        }
        // Steps 1–2.
        let mut len = with_doc(vm, h, root.doc, |d| crate::range::node_len(d, t))?;
        if len == 0 {
            tree::remove(vm, h, node, false)?;
            continue;
        }
        // Steps 3–4.
        let (data, following) = with_doc(vm, h, root.doc, |d| {
            let mut s = String::new();
            let mut nodes = Vec::new();
            let mut cur = d.next_sibling(t);
            while let Some(x) = cur {
                let Some(text) = d.node(x).as_text() else { break };
                s.push_str(text);
                nodes.push(x);
                cur = d.next_sibling(x);
            }
            (s, nodes)
        })?;
        if following.is_empty() {
            continue;
        }
        crate::mutation::replace_data(vm, h, node, len, 0, &data)?;
        // Steps 5–6: live ranges move into the merged node.
        for &x in &following {
            let (parent, i, xl) = with_doc(vm, h, root.doc, |d| {
                (d.parent(x), crate::range::index(d, x), crate::range::node_len(d, x))
            })?;
            if let Some(p) = parent {
                crate::range::merging(h, node, root.with(x), root.with(p), i, len);
            }
            len += xl;
        }
        // Step 7.
        for x in following {
            tree::remove(vm, h, root.with(x), false)?;
        }
    }
    Ok(())
}

/// DOM §4.4 "clone a node".
pub fn clone_node(vm: &mut Vm, h: &Host, n: NodeRef, deep: bool) -> JsResult<NodeRef> {
    let is_doc = n.id == Document::ROOT;
    if is_doc {
        // A new document of the same type, with copies of the children.
        let ty = h.doc_types.borrow().get(n.doc as usize).cloned().unwrap_or_default();
        let nd = new_document(h, &ty);
        if deep {
            let mut docs = h.docs.borrow_mut();
            let kids: Vec<NodeId> = docs[n.doc as usize].children(Document::ROOT).collect();
            let (url, quirks) = (docs[n.doc as usize].url.clone(), docs[n.doc as usize].quirks_mode);
            for k in kids {
                let (src, dst) = split2(&mut docs, n.doc as usize, nd as usize);
                let (copy, _) = dst.import_node(src, k, true);
                dst.append_child(Document::ROOT, copy);
            }
            docs[nd as usize].url = url;
            docs[nd as usize].quirks_mode = quirks;
        }
        return Ok(NodeRef { doc: nd, id: Document::ROOT });
    }
    let id = with_doc_mut(vm, h, n.doc, |d| d.clone_node(n.id, deep))?;
    Ok(n.with(id))
}

fn split2(docs: &mut [Document], a: usize, b: usize) -> (&Document, &mut Document) {
    if a < b {
        let (l, r) = docs.split_at_mut(b);
        (&l[a], &mut r[0])
    } else {
        let (l, r) = docs.split_at_mut(a);
        (&r[0], &mut l[b])
    }
}

/// A new empty document of content type `ty` (DOM §4.5); returns its index.
pub fn new_document(h: &Host, ty: &str) -> u32 {
    let mut docs = h.docs.borrow_mut();
    docs.push(Document::new());
    h.doc_types.borrow_mut().push(ty.to_string());
    docs.len() as u32 - 1
}

/// DOM §4.4 "equals".
fn equal_nodes(a: &Document, x: NodeId, b: &Document, y: NodeId) -> bool {
    let same = match (&a.node(x).data, &b.node(y).data) {
        (NodeData::Document, NodeData::Document)
        | (NodeData::DocumentFragment, NodeData::DocumentFragment) => true,
        (
            NodeData::Doctype { name: n1, public_id: p1, system_id: s1 },
            NodeData::Doctype { name: n2, public_id: p2, system_id: s2 },
        ) => n1 == n2 && p1 == p2 && s1 == s2,
        (NodeData::Element(e1), NodeData::Element(e2)) => {
            e1.ns == e2.ns
                && e1.prefix == e2.prefix
                && e1.name == e2.name
                && e1.attrs.len() == e2.attrs.len()
                && e1.attrs.iter().all(|a1| {
                    e2.attrs.iter().any(|a2| a1.ns == a2.ns && a1.name == a2.name && a1.value == a2.value)
                })
        }
        (NodeData::Text(t1), NodeData::Text(t2)) | (NodeData::Comment(t1), NodeData::Comment(t2)) => t1 == t2,
        (
            NodeData::ProcessingInstruction { target: t1, data: d1 },
            NodeData::ProcessingInstruction { target: t2, data: d2 },
        ) => t1 == t2 && d1 == d2,
        _ => false,
    };
    if !same {
        return false;
    }
    let k1: Vec<NodeId> = a.children(x).collect();
    let k2: Vec<NodeId> = b.children(y).collect();
    k1.len() == k2.len() && k1.iter().zip(&k2).all(|(&p, &q)| equal_nodes(a, p, b, q))
}

fn compare_document_position(vm: &mut Vm, c: NativeCall) -> R {
    const DISCONNECTED: u16 = 1;
    const PRECEDING: u16 = 2;
    const FOLLOWING: u16 = 4;
    const CONTAINS: u16 = 8;
    const CONTAINED_BY: u16 = 16;
    const IMPL: u16 = 32;
    let other = c.arg(0);
    if crate::attr::is_attr(vm, &c.this) || crate::attr::is_attr(vm, &other) {
        return crate::attr::compare_position(vm, &c.this, &other);
    }
    let (h, n) = this(vm, &c)?;
    let o = arg_node(vm, &other, "parameter 1")?;
    if n == o {
        return Ok(Value::Number(0.0));
    }
    let disconnected =
        |first: bool| Value::Number((DISCONNECTED | IMPL | if first { PRECEDING } else { FOLLOWING }) as f64);
    if n.doc != o.doc {
        return Ok(disconnected(o.doc < n.doc));
    }
    let r = with_doc(vm, &h, n.doc, |d| {
        let (rn, ro) = (root_of(d, n.id), root_of(d, o.id));
        if rn != ro {
            return disconnected(ro.0 < rn.0);
        }
        // `o` an ancestor of `n`?
        if d.ancestors(n.id).any(|a| a == o.id) {
            return Value::Number((CONTAINS | PRECEDING) as f64);
        }
        if d.ancestors(o.id).any(|a| a == n.id) {
            return Value::Number((CONTAINED_BY | FOLLOWING) as f64);
        }
        // Tree order: walk from the root.
        for x in d.descendants(rn) {
            if x == o.id {
                return Value::Number(PRECEDING as f64);
            }
            if x == n.id {
                return Value::Number(FOLLOWING as f64);
            }
        }
        Value::Number(FOLLOWING as f64)
    })?;
    Ok(r)
}

/// The element namespace lookups start from (DOM §4.4 "locate a namespace prefix").
fn namespace_element(d: &Document, id: NodeId) -> Option<NodeId> {
    match &d.node(id).data {
        NodeData::Element(_) => Some(id),
        NodeData::Document => d.document_element(),
        NodeData::Doctype { .. } | NodeData::DocumentFragment => None,
        _ => d.parent(id).filter(|&p| d.node(p).is_element()),
    }
}

/// DOM §4.4 "locate a namespace".
fn locate_namespace(d: &Document, id: NodeId, prefix: Option<&str>) -> Option<String> {
    let el = namespace_element(d, id)?;
    for a in std::iter::once(el).chain(d.ancestors(el)) {
        let Some(e) = d.element(a) else { continue };
        if prefix == Some("xml") {
            return Some(Namespace::Xml.url().into());
        }
        if prefix == Some("xmlns") {
            return Some(Namespace::Xmlns.url().into());
        }
        if e.ns != Namespace::None && e.prefix.as_deref() == prefix {
            return Some(e.ns.url().to_string());
        }
        for at in &e.attrs {
            let hit = match prefix {
                Some(p) => at.ns == Namespace::Xmlns && at.prefix.as_deref() == Some("xmlns") && at.name == p,
                None => at.ns == Namespace::Xmlns && at.prefix.is_none() && at.name == "xmlns",
            };
            if hit {
                return (!at.value.is_empty()).then(|| at.value.clone());
            }
        }
    }
    None
}

// ---- mixins ----

/// ParentNode (DOM §4.2.6) on Document, DocumentFragment and Element.
fn setup_parent_node(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "children",
        |vm, c| {
            let (h, n) = this(vm, &c)?;
            collection(vm, &h, CollKind::Children(n), Some((1, n.pack())))
        },
        None,
    );
    attr(
        vm,
        proto,
        "firstElementChild",
        |vm, c| node_rel(vm, &c, |d, id| d.element_children(id).next()),
        None,
    );
    attr(
        vm,
        proto,
        "lastElementChild",
        |vm, c| node_rel(vm, &c, |d, id| d.element_children(id).last()),
        None,
    );
    attr(
        vm,
        proto,
        "childElementCount",
        |vm, c| {
            let (h, n) = this(vm, &c)?;
            let k = with_doc(vm, &h, n.doc, |d| d.element_children(n.id).count())?;
            Ok(Value::Number(k as f64))
        },
        None,
    );
    op(vm, proto, "prepend", 0, |vm, c| {
        let (h, n) = this(vm, &c)?;
        let node = tree::nodes_into_node(vm, &h, n.doc, &c.args)?;
        let first = with_doc(vm, &h, n.doc, |d| d.first_child(n.id))?;
        tree::pre_insert(vm, &h, node, n, first.map(|f| n.with(f)))?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "append", 0, |vm, c| {
        let (h, n) = this(vm, &c)?;
        let node = tree::nodes_into_node(vm, &h, n.doc, &c.args)?;
        tree::append(vm, &h, node, n)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "replaceChildren", 0, |vm, c| {
        let (h, n) = this(vm, &c)?;
        let node = tree::nodes_into_node(vm, &h, n.doc, &c.args)?;
        // Ensure pre-insertion validity (DOM §4.2.6 replaceChildren step 2), then replace all.
        tree::check_pre_insert(vm, &h, node, n, None)?;
        tree::replace_all(vm, &h, Some(node), n)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "querySelector", 1, |vm, c| {
        let sel = rstr(vm, &c.arg(0))?;
        let (h, n) = this(vm, &c)?;
        let r = query(vm, &h, n, &sel, true)?;
        wrap_opt(vm, &h, r.first().copied())
    });
    op(vm, proto, "querySelectorAll", 1, |vm, c| {
        let sel = rstr(vm, &c.arg(0))?;
        let (h, n) = this(vm, &c)?;
        let r = query(vm, &h, n, &sel, false)?;
        collection(vm, &h, CollKind::Static(r), None)
    });
}

/// querySelector(All) (DOM §4.2.6 "scope-match a selectors string").
fn query(vm: &mut Vm, h: &Host, n: NodeRef, sel: &str, first: bool) -> JsResult<Vec<NodeRef>> {
    let cvs = bb_css::parser::parse_component_values(sel);
    let Some(list) = bb_css::selectors::parse_selector_list(&cvs) else {
        return throw(vm, "SyntaxError", &format!("'{sel}' is not a valid selector"));
    };
    with_doc(vm, h, n.doc, |d| {
        let mut out = Vec::new();
        for x in d.descendants(n.id) {
            if x != n.id
                && d.node(x).is_element()
                && bb_css::selectors::matches_any(&list, bb_style::DomElement { doc: d, id: x })
            {
                out.push(n.with(x));
                if first {
                    break;
                }
            }
        }
        out
    })
}

/// Element.matches / closest.
fn matches_selector(vm: &mut Vm, h: &Host, n: NodeRef, sel: &str) -> JsResult<bool> {
    let cvs = bb_css::parser::parse_component_values(sel);
    let Some(list) = bb_css::selectors::parse_selector_list(&cvs) else {
        return throw(vm, "SyntaxError", &format!("'{sel}' is not a valid selector"));
    };
    with_doc(vm, h, n.doc, |d| {
        bb_css::selectors::matches_any(&list, bb_style::DomElement { doc: d, id: n.id })
    })
}

/// ChildNode (DOM §4.2.8) on Element, CharacterData and DocumentType.
fn setup_child_node(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "before", 0, |vm, c| {
        let (h, n) = this(vm, &c)?;
        let Some(parent) = with_doc(vm, &h, n.doc, |d| d.parent(n.id))? else { return Ok(Value::Undefined) };
        let excluded = nodes_in(vm, &c.args);
        let mut prev = with_doc(vm, &h, n.doc, |d| {
            let mut p = d.prev_sibling(n.id);
            while let Some(x) = p {
                if !excluded.contains(&n.with(x)) {
                    break;
                }
                p = d.prev_sibling(x);
            }
            p
        })?;
        let node = tree::nodes_into_node(vm, &h, n.doc, &c.args)?;
        let reference = match prev.take() {
            None => with_doc(vm, &h, n.doc, |d| d.first_child(parent))?,
            Some(p) => with_doc(vm, &h, n.doc, |d| d.next_sibling(p))?,
        };
        tree::pre_insert(vm, &h, node, n.with(parent), reference.map(|r| n.with(r)))?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "after", 0, |vm, c| {
        let (h, n) = this(vm, &c)?;
        let Some(parent) = with_doc(vm, &h, n.doc, |d| d.parent(n.id))? else { return Ok(Value::Undefined) };
        let excluded = nodes_in(vm, &c.args);
        let next = with_doc(vm, &h, n.doc, |d| {
            let mut p = d.next_sibling(n.id);
            while let Some(x) = p {
                if !excluded.contains(&n.with(x)) {
                    break;
                }
                p = d.next_sibling(x);
            }
            p
        })?;
        let node = tree::nodes_into_node(vm, &h, n.doc, &c.args)?;
        tree::pre_insert(vm, &h, node, n.with(parent), next.map(|r| n.with(r)))?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "replaceWith", 0, |vm, c| {
        let (h, n) = this(vm, &c)?;
        let Some(parent) = with_doc(vm, &h, n.doc, |d| d.parent(n.id))? else { return Ok(Value::Undefined) };
        let excluded = nodes_in(vm, &c.args);
        let next = with_doc(vm, &h, n.doc, |d| {
            let mut p = d.next_sibling(n.id);
            while let Some(x) = p {
                if !excluded.contains(&n.with(x)) {
                    break;
                }
                p = d.next_sibling(x);
            }
            p
        })?;
        let node = tree::nodes_into_node(vm, &h, n.doc, &c.args)?;
        let still_child = with_doc(vm, &h, n.doc, |d| d.parent(n.id) == Some(parent))?;
        if still_child {
            tree::replace(vm, &h, n, node, n.with(parent))?;
        } else {
            tree::pre_insert(vm, &h, node, n.with(parent), next.map(|r| n.with(r)))?;
        }
        Ok(Value::Undefined)
    });
    op(vm, proto, "remove", 0, |vm, c| {
        let (h, n) = this(vm, &c)?;
        tree::remove(vm, &h, n, false)?;
        Ok(Value::Undefined)
    });
}

fn nodes_in(vm: &Vm, args: &[Value]) -> Vec<NodeRef> {
    args.iter().filter_map(|a| node_of(vm, a)).collect()
}

/// NonDocumentTypeChildNode (DOM §4.2.7) on Element and CharacterData.
fn setup_non_doctype_child(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "previousElementSibling",
        |vm, c| {
            node_rel(vm, &c, |d, id| {
                let mut p = d.prev_sibling(id);
                while let Some(x) = p {
                    if d.node(x).is_element() {
                        return Some(x);
                    }
                    p = d.prev_sibling(x);
                }
                None
            })
        },
        None,
    );
    attr(
        vm,
        proto,
        "nextElementSibling",
        |vm, c| {
            node_rel(vm, &c, |d, id| {
                let mut p = d.next_sibling(id);
                while let Some(x) = p {
                    if d.node(x).is_element() {
                        return Some(x);
                    }
                    p = d.next_sibling(x);
                }
                None
            })
        },
        None,
    );
}

/// NonElementParentNode (DOM §4.2.4) on Document and DocumentFragment.
fn setup_get_element_by_id(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "getElementById", 1, |vm, c| {
        let id = rstr(vm, &c.arg(0))?;
        let (h, n) = this(vm, &c)?;
        let r = with_doc(vm, &h, n.doc, |d| {
            d.descendants(n.id).find(|&x| x != n.id && d.element(x).and_then(|e| e.id()) == Some(id.as_str()))
        })?;
        wrap_opt(vm, &h, r.map(|x| n.with(x)))
    });
}

/// getElementsByTagName / NS / ClassName on Document and Element.
fn setup_get_elements(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "getElementsByTagName", 1, |vm, c| {
        let q = rstr(vm, &c.arg(0))?;
        let (h, n) = this(vm, &c)?;
        collection(vm, &h, CollKind::TagName(n, q), None)
    });
    op(vm, proto, "getElementsByTagNameNS", 2, |vm, c| {
        let ns = rstr_opt(vm, &c.arg(0), false)?;
        let local = rstr(vm, &c.arg(1))?;
        let (h, n) = this(vm, &c)?;
        let ns = match ns.as_deref() {
            Some("*") => None,
            Some("") | None => Some(None),
            Some(s) => Some(Some(s.to_string())),
        };
        collection(vm, &h, CollKind::TagNameNs(n, ns, local), None)
    });
    op(vm, proto, "getElementsByClassName", 1, |vm, c| {
        let names = rstr(vm, &c.arg(0))?;
        let (h, n) = this(vm, &c)?;
        let set: Vec<String> = names.split_ascii_whitespace().map(str::to_string).collect();
        collection(vm, &h, CollKind::ClassName(n, set), None)
    });
}

// ---- Document ----

pub fn document_ctor(vm: &mut Vm, c: NativeCall) -> R {
    if c.new_target.is_none() {
        return vm.type_error("Document requires 'new'");
    }
    let h = host(vm)?;
    let d = new_document(&h, "");
    let url = h.url.borrow().href();
    h.docs.borrow_mut()[d as usize].url = url;
    wrap(vm, &h, NodeRef { doc: d, id: Document::ROOT })
}

fn doc_this(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, NodeRef)> {
    this_node(vm, &c.this, C::Document)
}

pub fn setup_document(vm: &mut Vm, proto: ObjRef) {
    setup_parent_node(vm, proto);
    crate::range::setup_document(vm, proto);
    crate::traversal::setup_document(vm, proto);
    crate::selection::setup_document(vm, proto);
    setup_get_element_by_id(vm, proto);
    setup_get_elements(vm, proto);
    attr(
        vm,
        proto,
        "implementation",
        |vm, c| {
            let (h, n) = doc_this(vm, &c)?;
            if let Some(o) = h.coll_cache.borrow().get(&(9, n.pack())) {
                return Ok(Value::Object(*o));
            }
            let o = vm.new_host_object(
                Some(h.proto(C::DOMImplementation)),
                C::DOMImplementation as u32,
                n.doc as u64,
                false,
            );
            h.coll_cache.borrow_mut().insert((9, n.pack()), o);
            Ok(Value::Object(o))
        },
        None,
    );
    for name in ["URL", "documentURI"] {
        attr(
            vm,
            proto,
            name,
            |vm, c| {
                let (h, n) = doc_this(vm, &c)?;
                let u = with_doc(vm, &h, n.doc, |d| d.url.clone())?;
                Ok(js(&u))
            },
            None,
        );
    }
    attr(
        vm,
        proto,
        "compatMode",
        |vm, c| {
            let (h, n) = doc_this(vm, &c)?;
            let q = with_doc(vm, &h, n.doc, |d| d.quirks_mode == bb_dom::QuirksMode::Quirks)?;
            Ok(js(if q { "BackCompat" } else { "CSS1Compat" }))
        },
        None,
    );
    for name in ["characterSet", "charset", "inputEncoding"] {
        attr(
            vm,
            proto,
            name,
            |vm, c| {
                doc_this(vm, &c)?;
                Ok(js("UTF-8"))
            },
            None,
        );
    }
    attr(
        vm,
        proto,
        "contentType",
        |vm, c| {
            let (h, n) = doc_this(vm, &c)?;
            let t = h.doc_types.borrow().get(n.doc as usize).cloned().unwrap_or_default();
            Ok(js(if t.is_empty() { "application/xml" } else { &t }))
        },
        None,
    );
    attr(
        vm,
        proto,
        "doctype",
        |vm, c| {
            node_rel(vm, &c, |d, id| {
                d.children(id).find(|&k| matches!(d.node(k).data, NodeData::Doctype { .. }))
            })
        },
        None,
    );
    attr(vm, proto, "documentElement", |vm, c| node_rel(vm, &c, |d, _| d.document_element()), None);
    attr(vm, proto, "head", |vm, c| node_rel(vm, &c, |d, _| d.head()), None);
    attr(vm, proto, "body", |vm, c| node_rel(vm, &c, |d, _| d.body()), Some(body_set));
    attr(
        vm,
        proto,
        "title",
        |vm, c| {
            let (h, n) = doc_this(vm, &c)?;
            let t = with_doc(vm, &h, n.doc, |d| d.title())?;
            Ok(js(&t))
        },
        Some(title_set),
    );
    attr(
        vm,
        proto,
        "readyState",
        |vm, c| {
            let (h, n) = doc_this(vm, &c)?;
            Ok(js(if n.doc == 0 { h.ready_state.get().as_str() } else { "complete" }))
        },
        None,
    );
    attr(
        vm,
        proto,
        "defaultView",
        |vm, c| {
            let (h, n) = doc_this(vm, &c)?;
            Ok(match (n.doc, h.window.get()) {
                (0, Some(w)) => Value::Object(w),
                _ => Value::Null,
            })
        },
        None,
    );
    attr(
        vm,
        proto,
        "currentScript",
        |vm, c| {
            let (h, n) = doc_this(vm, &c)?;
            Ok(if n.doc == 0 { h.current_script.borrow().clone() } else { Value::Null })
        },
        None,
    );
    attr(
        vm,
        proto,
        "referrer",
        |vm, c| {
            doc_this(vm, &c)?;
            Ok(js(""))
        },
        None,
    );
    attr(
        vm,
        proto,
        "activeElement",
        |vm, c| node_rel(vm, &c, |d, _| d.body().or_else(|| d.document_element())),
        None,
    );
    op(vm, proto, "hasFocus", 0, |vm, c| {
        doc_this(vm, &c)?;
        Ok(Value::Bool(true))
    });
    op(vm, proto, "createElement", 1, |vm, c| {
        let name = rstr(vm, &c.arg(0))?;
        let (h, n) = doc_this(vm, &c)?;
        if !valid_element_local_name(&name) {
            return throw(vm, "InvalidCharacterError", &format!("'{name}' is not a valid element name"));
        }
        let html = is_html_doc(&h, n.doc);
        let name = if html { name.to_ascii_lowercase() } else { name };
        let ty = h.doc_types.borrow().get(n.doc as usize).cloned().unwrap_or_default();
        let ns = if html || ty == "application/xhtml+xml" { Namespace::Html } else { Namespace::None };
        let id = with_doc_mut(vm, &h, n.doc, |d| d.create_element(ns, &name, Vec::new()))?;
        wrap(vm, &h, n.with(id))
    });
    op(vm, proto, "createElementNS", 2, |vm, c| {
        let ns = rstr_opt(vm, &c.arg(0), false)?;
        let q = rstr(vm, &c.arg(1))?;
        let (h, n) = doc_this(vm, &c)?;
        let (ns, prefix, local) = validate_and_extract(vm, ns, &q, true)?;
        let id = with_doc_mut(vm, &h, n.doc, |d| {
            let id = d.create_element(ns, &local, Vec::new());
            if let Some(e) = d.element_mut(id) {
                e.prefix = prefix;
            }
            id
        })?;
        wrap(vm, &h, n.with(id))
    });
    op(vm, proto, "createDocumentFragment", 0, |vm, c| {
        let (h, n) = doc_this(vm, &c)?;
        let id = with_doc_mut(vm, &h, n.doc, |d| d.create_fragment())?;
        wrap(vm, &h, n.with(id))
    });
    op(vm, proto, "createTextNode", 1, |vm, c| {
        let s = rstr(vm, &c.arg(0))?;
        let (h, n) = doc_this(vm, &c)?;
        let id = with_doc_mut(vm, &h, n.doc, |d| d.create_text(&s))?;
        wrap(vm, &h, n.with(id))
    });
    op(vm, proto, "createCDATASection", 1, |vm, c| {
        let s = rstr(vm, &c.arg(0))?;
        let (h, n) = doc_this(vm, &c)?;
        if is_html_doc(&h, n.doc) {
            return throw(vm, "NotSupportedError", "HTML documents have no CDATA sections");
        }
        if s.contains("]]>") {
            return throw(vm, "InvalidCharacterError", "the data contains ']]>'");
        }
        let id = with_doc_mut(vm, &h, n.doc, |d| d.create_text(&s))?;
        wrap(vm, &h, n.with(id))
    });
    op(vm, proto, "createComment", 1, |vm, c| {
        let s = rstr(vm, &c.arg(0))?;
        let (h, n) = doc_this(vm, &c)?;
        let id = with_doc_mut(vm, &h, n.doc, |d| d.create_comment(&s))?;
        wrap(vm, &h, n.with(id))
    });
    op(vm, proto, "createProcessingInstruction", 2, |vm, c| {
        let target = rstr(vm, &c.arg(0))?;
        let data = rstr(vm, &c.arg(1))?;
        let (h, n) = doc_this(vm, &c)?;
        if !valid_pi_target(&target) {
            return throw(vm, "InvalidCharacterError", "invalid processing instruction target");
        }
        if data.contains("?>") {
            return throw(vm, "InvalidCharacterError", "the data contains '?>'");
        }
        let id = with_doc_mut(vm, &h, n.doc, |d| d.create_processing_instruction(&target, &data))?;
        wrap(vm, &h, n.with(id))
    });
    op(vm, proto, "importNode", 1, |vm, c| {
        let (h, n) = doc_this(vm, &c)?;
        let node = arg_node(vm, &c.arg(0), "parameter 1")?;
        // The second argument is `deep` (boolean) or ImportNodeOptions.
        let deep = match c.arg(1) {
            Value::Object(o) if !vm.is_callable(&Value::Object(o)) => {
                let v = vm.get(o, &PropKey::lit("selfOnly"))?;
                !vm.to_boolean(&v)
            }
            v => vm.to_boolean(&v),
        };
        let is_doc = node.id == Document::ROOT;
        if is_doc {
            return throw(vm, "NotSupportedError", "documents can't be imported");
        }
        let copy = clone_node(vm, &h, node, deep)?;
        let r = tree::adopt(vm, &h, copy, n.doc)?;
        wrap(vm, &h, r)
    });
    op(vm, proto, "adoptNode", 1, |vm, c| {
        let (h, n) = doc_this(vm, &c)?;
        if crate::attr::is_attr(vm, &c.arg(0)) {
            crate::attr::adopt_attr(vm, &c.arg(0), n.doc)?;
            return Ok(c.arg(0));
        }
        let node = arg_node(vm, &c.arg(0), "parameter 1")?;
        if node.id == Document::ROOT {
            return throw(vm, "NotSupportedError", "documents can't be adopted");
        }
        tree::adopt(vm, &h, node, n.doc)?;
        Ok(c.arg(0))
    });
    op(vm, proto, "createAttribute", 1, |vm, c| {
        let name = rstr(vm, &c.arg(0))?;
        let (h, n) = doc_this(vm, &c)?;
        if !valid_attribute_local_name(&name) {
            return throw(vm, "InvalidCharacterError", "invalid attribute name");
        }
        let name = if is_html_doc(&h, n.doc) { name.to_ascii_lowercase() } else { name };
        crate::attr::new_detached(vm, &h, n.doc, Namespace::None, None, &name)
    });
    op(vm, proto, "createAttributeNS", 2, |vm, c| {
        let ns = rstr_opt(vm, &c.arg(0), false)?;
        let q = rstr(vm, &c.arg(1))?;
        let (h, n) = doc_this(vm, &c)?;
        let (ns, prefix, local) = validate_and_extract(vm, ns, &q, false)?;
        crate::attr::new_detached(vm, &h, n.doc, ns, prefix, &local)
    });
    op(vm, proto, "createEvent", 1, crate::events::create_event);
    crate::css::setup_document_geometry(vm, proto);
    crate::storage::setup_document_cookie(vm, proto);
    crate::events::setup_event_handlers(vm, proto);
    op(vm, proto, "write", 0, |vm, c| crate::loader::document_write(vm, &c, false));
    op(vm, proto, "writeln", 0, |vm, c| crate::loader::document_write(vm, &c, true));
    op(vm, proto, "open", 0, |vm, c| {
        doc_this(vm, &c)?;
        Ok(c.this.clone())
    });
    op(vm, proto, "close", 0, |vm, c| {
        doc_this(vm, &c)?;
        Ok(Value::Undefined)
    });
}

/// DOM §4.13 PI target: a valid XML Name (approximated by the element-name rule).
fn valid_pi_target(t: &str) -> bool {
    valid_element_local_name(t) && !t.contains(':')
        || t.chars().all(|c| c.is_alphanumeric() || "_-.".contains(c))
            && !t.is_empty()
            && !t.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '.')
}

fn body_set(vm: &mut Vm, c: NativeCall) -> R {
    let (h, n) = doc_this(vm, &c)?;
    let new = arg_node(vm, &c.arg(0), "the new body")?;
    let ok = with_doc(vm, &h, new.doc, |d| d.is_html(new.id, "body") || d.is_html(new.id, "frameset"))?;
    if !ok {
        return throw(vm, "HierarchyRequestError", "the body must be a body or frameset element");
    }
    let (old, root) = with_doc(vm, &h, n.doc, |d| (d.body(), d.document_element()))?;
    if old.map(|o| n.with(o)) == Some(new) {
        return Ok(Value::Undefined);
    }
    match (old, root) {
        (Some(o), _) => {
            let parent = with_doc(vm, &h, n.doc, |d| d.parent(o))?.unwrap_or(Document::ROOT);
            tree::replace(vm, &h, n.with(o), new, n.with(parent))?;
        }
        (None, Some(r)) => {
            tree::append(vm, &h, new, n.with(r))?;
        }
        (None, None) => return throw(vm, "HierarchyRequestError", "the document has no root element"),
    }
    Ok(Value::Undefined)
}

fn title_set(vm: &mut Vm, c: NativeCall) -> R {
    let v = rstr(vm, &c.arg(0))?;
    let (h, n) = doc_this(vm, &c)?;
    let (title, head) = with_doc(vm, &h, n.doc, |d| {
        (d.descendants(Document::ROOT).find(|&x| d.is_html(x, "title")), d.head())
    })?;
    let t = match title {
        Some(t) => n.with(t),
        None => {
            let Some(head) = head else { return Ok(Value::Undefined) };
            let t = n.with(with_doc_mut(vm, &h, n.doc, |d| d.create_html_element("title"))?);
            tree::append(vm, &h, t, n.with(head))?;
            t
        }
    };
    // DOM §4.4 "string replace all".
    let node =
        if v.is_empty() { None } else { Some(n.with(with_doc_mut(vm, &h, n.doc, |d| d.create_text(&v))?)) };
    tree::replace_all(vm, &h, node, t)?;
    Ok(Value::Undefined)
}

// ---- DOMImplementation ----

pub fn setup_implementation(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "createDocumentType", 3, |vm, c| {
        let name = rstr(vm, &c.arg(0))?;
        let public = rstr(vm, &c.arg(1))?;
        let system = rstr(vm, &c.arg(2))?;
        let (_, d) = this_of(vm, &c.this, C::DOMImplementation)?;
        if !valid_doctype_name(&name) {
            return throw(vm, "InvalidCharacterError", "invalid doctype name");
        }
        let h = host(vm)?;
        let id = with_doc_mut(vm, &h, d as u32, |doc| doc.create_doctype(&name, &public, &system))?;
        wrap(vm, &h, NodeRef { doc: d as u32, id })
    });
    op(vm, proto, "createDocument", 2, |vm, c| {
        let ns = rstr_opt(vm, &c.arg(0), false)?;
        let q = rstr_null_empty(vm, &c.arg(1))?;
        let doctype = match c.arg(2) {
            Value::Undefined | Value::Null => None,
            v => Some(arg_node(vm, &v, "parameter 3")?),
        };
        let (_, d) = this_of(vm, &c.this, C::DOMImplementation)?;
        let h = host(vm)?;
        let ty = match ns.as_deref() {
            Some("http://www.w3.org/1999/xhtml") => "application/xhtml+xml",
            Some("http://www.w3.org/2000/svg") => "image/svg+xml",
            _ => "application/xml",
        };
        let element = if q.is_empty() { None } else { Some(validate_and_extract(vm, ns, &q, true)?) };
        let nd = new_document(&h, ty);
        let url = with_doc(vm, &h, d as u32, |x| x.url.clone())?;
        h.docs.borrow_mut()[nd as usize].url = url;
        let root = NodeRef { doc: nd, id: Document::ROOT };
        if let Some(dt) = doctype {
            tree::append(vm, &h, dt, root)?;
        }
        if let Some((ns, prefix, local)) = element {
            let id = with_doc_mut(vm, &h, nd, |doc| {
                let id = doc.create_element(ns, &local, Vec::new());
                if let Some(e) = doc.element_mut(id) {
                    e.prefix = prefix;
                }
                id
            })?;
            tree::append(vm, &h, root.with(id), root)?;
        }
        wrap(vm, &h, root)
    });
    op(vm, proto, "createHTMLDocument", 0, |vm, c| {
        let title = match c.arg(0) {
            Value::Undefined => None,
            v => Some(rstr(vm, &v)?),
        };
        let (_, d) = this_of(vm, &c.this, C::DOMImplementation)?;
        let h = host(vm)?;
        let nd = new_document(&h, "text/html");
        let url = with_doc(vm, &h, d as u32, |x| x.url.clone())?;
        with_doc_mut(vm, &h, nd, |doc| {
            doc.url = url;
            let dt = doc.create_doctype("html", "", "");
            doc.append_child(Document::ROOT, dt);
            let html = doc.create_html_element("html");
            doc.append_child(Document::ROOT, html);
            let head = doc.create_html_element("head");
            doc.append_child(html, head);
            if let Some(t) = title {
                let te = doc.create_html_element("title");
                doc.append_child(head, te);
                let tx = doc.create_text(&t);
                doc.append_child(te, tx);
            }
            let body = doc.create_html_element("body");
            doc.append_child(html, body);
        })?;
        wrap(vm, &h, NodeRef { doc: nd, id: Document::ROOT })
    });
    op(vm, proto, "hasFeature", 0, |vm, c| {
        this_of(vm, &c.this, C::DOMImplementation)?;
        Ok(Value::Bool(true))
    });
}

// ---- DocumentType, DocumentFragment ----

pub fn setup_doctype(vm: &mut Vm, proto: ObjRef) {
    setup_child_node(vm, proto);
    fn field(vm: &mut Vm, c: &NativeCall, i: usize) -> R {
        let (h, n) = this_node(vm, &c.this, C::DocumentType)?;
        let s = with_doc(vm, &h, n.doc, |d| match &d.node(n.id).data {
            NodeData::Doctype { name, public_id, system_id } => [name, public_id, system_id][i].clone(),
            _ => String::new(),
        })?;
        Ok(js(&s))
    }
    attr(vm, proto, "name", |vm, c| field(vm, &c, 0), None);
    attr(vm, proto, "publicId", |vm, c| field(vm, &c, 1), None);
    attr(vm, proto, "systemId", |vm, c| field(vm, &c, 2), None);
}

pub fn fragment_ctor(vm: &mut Vm, c: NativeCall) -> R {
    if c.new_target.is_none() {
        return vm.type_error("DocumentFragment requires 'new'");
    }
    let h = host(vm)?;
    let id = with_doc_mut(vm, &h, 0, |d| d.create_fragment())?;
    wrap(vm, &h, NodeRef { doc: 0, id })
}

pub fn setup_fragment(vm: &mut Vm, proto: ObjRef) {
    setup_parent_node(vm, proto);
    setup_get_element_by_id(vm, proto);
}

// ---- CharacterData, Text, Comment, ProcessingInstruction ----

fn cd_this(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, NodeRef, Vec<u16>)> {
    let (h, n) = this_node(vm, &c.this, C::CharacterData)?;
    let data = with_doc(vm, &h, n.doc, |d| char_data(d, n.id).unwrap_or_default())?;
    Ok((h, n, data.encode_utf16().collect()))
}

pub fn setup_chardata(vm: &mut Vm, proto: ObjRef) {
    setup_child_node(vm, proto);
    setup_non_doctype_child(vm, proto);
    attr(
        vm,
        proto,
        "data",
        |vm, c| {
            let (_, _, d) = cd_this(vm, &c)?;
            Ok(Value::String(d.into()))
        },
        Some(|vm, c| {
            let v = rstr_null_empty(vm, &c.arg(0))?;
            let (h, n, _) = cd_this(vm, &c)?;
            crate::mutation::set_data(vm, &h, n, &v)?;
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "length",
        |vm, c| {
            let (_, _, d) = cd_this(vm, &c)?;
            Ok(Value::Number(d.len() as f64))
        },
        None,
    );
    op(vm, proto, "substringData", 2, |vm, c| {
        let off = to_ulong(vm, &c.arg(0))?;
        let count = to_ulong(vm, &c.arg(1))?;
        let (_, _, d) = cd_this(vm, &c)?;
        if off as usize > d.len() {
            return throw(vm, "IndexSizeError", "the offset is past the end of the data");
        }
        let end = (off as usize).saturating_add(count as usize).min(d.len());
        Ok(Value::String(d[off as usize..end].to_vec().into()))
    });
    op(vm, proto, "appendData", 1, |vm, c| {
        let s = rstr(vm, &c.arg(0))?;
        let (h, n, d) = cd_this(vm, &c)?;
        let len = d.len() as u32;
        crate::mutation::replace_data(vm, &h, n, len, 0, &s)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "insertData", 2, |vm, c| {
        let off = to_ulong(vm, &c.arg(0))?;
        let s = rstr(vm, &c.arg(1))?;
        let (h, n, _) = cd_this(vm, &c)?;
        crate::mutation::replace_data(vm, &h, n, off, 0, &s)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "deleteData", 2, |vm, c| {
        let off = to_ulong(vm, &c.arg(0))?;
        let count = to_ulong(vm, &c.arg(1))?;
        let (h, n, _) = cd_this(vm, &c)?;
        crate::mutation::replace_data(vm, &h, n, off, count, "")?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "replaceData", 3, |vm, c| {
        let off = to_ulong(vm, &c.arg(0))?;
        let count = to_ulong(vm, &c.arg(1))?;
        let s = rstr(vm, &c.arg(2))?;
        let (h, n, _) = cd_this(vm, &c)?;
        crate::mutation::replace_data(vm, &h, n, off, count, &s)?;
        Ok(Value::Undefined)
    });
}

fn new_char_node(vm: &mut Vm, c: &NativeCall, comment: bool) -> R {
    if c.new_target.is_none() {
        return vm.type_error("requires 'new'");
    }
    let s = match c.arg(0) {
        Value::Undefined => String::new(),
        v => rstr(vm, &v)?,
    };
    let h = host(vm)?;
    let id = with_doc_mut(vm, &h, 0, |d| if comment { d.create_comment(&s) } else { d.create_text(&s) })?;
    wrap(vm, &h, NodeRef { doc: 0, id })
}

pub fn text_ctor(vm: &mut Vm, c: NativeCall) -> R {
    new_char_node(vm, &c, false)
}

pub fn comment_ctor(vm: &mut Vm, c: NativeCall) -> R {
    new_char_node(vm, &c, true)
}

/// DOM §4.11 "split a Text node" `n` at `offset`; returns the new node.
pub fn split_text(vm: &mut Vm, h: &Host, n: NodeRef, offset: u32) -> JsResult<NodeRef> {
    let data = crate::mutation::data16(vm, h, n)?;
    // Steps 1–4.
    let len = data.len() as u32;
    if offset > len {
        return throw(vm, "IndexSizeError", "the offset is past the end of the data");
    }
    let count = len - offset;
    let tail = String::from_utf16_lossy(&data[offset as usize..]);
    // Steps 5–7.
    let new = n.with(with_doc_mut(vm, h, n.doc, |d| d.create_text(&tail))?);
    let parent = with_doc(vm, h, n.doc, |d| {
        d.parent(n.id).map(|p| (p, crate::range::index(d, n.id), d.next_sibling(n.id)))
    })?;
    if let Some((p, i, next)) = parent {
        let p = n.with(p);
        tree::insert(vm, h, new, p, next, false)?;
        crate::range::split(h, n, new, offset, p, i);
    }
    // Step 8.
    crate::mutation::replace_data(vm, h, n, offset, count, "")?;
    Ok(new)
}

pub fn setup_text(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "splitText", 1, |vm, c| {
        let off = to_ulong(vm, &c.arg(0))?;
        let (h, n) = this_node(vm, &c.this, C::Text)?;
        let new = split_text(vm, &h, n, off)?;
        wrap(vm, &h, new)
    });
    attr(
        vm,
        proto,
        "wholeText",
        |vm, c| {
            let (h, n) = this_node(vm, &c.this, C::Text)?;
            let s = with_doc(vm, &h, n.doc, |d| {
                let mut first = n.id;
                while let Some(p) = d.prev_sibling(first).filter(|&p| d.node(p).as_text().is_some()) {
                    first = p;
                }
                let mut s = String::new();
                let mut cur = Some(first);
                while let Some(x) = cur {
                    match d.node(x).as_text() {
                        Some(t) => s.push_str(t),
                        None => break,
                    }
                    cur = d.next_sibling(x);
                }
                s
            })?;
            Ok(js(&s))
        },
        None,
    );
}

pub fn setup_pi(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "target",
        |vm, c| {
            let (h, n) = this_node(vm, &c.this, C::ProcessingInstruction)?;
            let s = with_doc(vm, &h, n.doc, |d| match &d.node(n.id).data {
                NodeData::ProcessingInstruction { target, .. } => target.clone(),
                _ => String::new(),
            })?;
            Ok(js(&s))
        },
        None,
    );
}

// ---- Element ----

fn el_this(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, NodeRef)> {
    this_node(vm, &c.this, C::Element)
}

/// The qualified name of an attribute.
fn attr_qname(a: &Attribute) -> String {
    match &a.prefix {
        Some(p) => format!("{p}:{}", a.name),
        None => a.name.clone(),
    }
}

/// The name an attribute lookup by qualified name uses (lowercased for HTML elements in
/// HTML documents, DOM §4.9 "get an attribute by name").
fn lookup_name(h: &Host, n: NodeRef, d: &Document, q: &str) -> String {
    if d.element(n.id).is_some_and(|e| e.ns == Namespace::Html) && is_html_doc(h, n.doc) {
        q.to_ascii_lowercase()
    } else {
        q.to_string()
    }
}

/// DOM §4.9 "get an attribute by name": its index.
pub fn attr_index_by_name(h: &Host, n: NodeRef, d: &Document, q: &str) -> Option<usize> {
    let q = lookup_name(h, n, d, q);
    d.element(n.id)?.attrs.iter().position(|a| attr_qname(a) == q)
}

/// Set an attribute's value on the element's data, telling the document about plain ones.
pub fn set_attr_raw(
    d: &mut Document,
    id: NodeId,
    ns: Namespace,
    prefix: Option<String>,
    local: &str,
    value: &str,
) {
    if ns == Namespace::None {
        d.set_attr(id, local, value);
        return;
    }
    if let Some(e) = d.element_mut(id) {
        match e.attrs.iter_mut().find(|a| a.ns == ns && a.name == local) {
            Some(a) => a.value = value.to_string(),
            None => e.attrs.push(Attribute { ns, prefix, name: local.to_string(), value: value.to_string() }),
        }
    }
    d.mark_style_dirty(id);
}

pub fn setup_element(vm: &mut Vm, proto: ObjRef) {
    setup_parent_node(vm, proto);
    setup_child_node(vm, proto);
    setup_non_doctype_child(vm, proto);
    setup_get_elements(vm, proto);
    attr(
        vm,
        proto,
        "namespaceURI",
        |vm, c| {
            let (h, n) = el_this(vm, &c)?;
            let ns = with_doc(vm, &h, n.doc, |d| d.element(n.id).map_or(Namespace::None, |e| e.ns))?;
            Ok(ns_value(ns))
        },
        None,
    );
    attr(
        vm,
        proto,
        "prefix",
        |vm, c| {
            let (h, n) = el_this(vm, &c)?;
            let p = with_doc(vm, &h, n.doc, |d| d.element(n.id).and_then(|e| e.prefix.clone()))?;
            Ok(p.map_or(Value::Null, |p| js(&p)))
        },
        None,
    );
    attr(
        vm,
        proto,
        "localName",
        |vm, c| {
            let (h, n) = el_this(vm, &c)?;
            let s = with_doc(vm, &h, n.doc, |d| d.element(n.id).map(|e| e.name.clone()).unwrap_or_default())?;
            Ok(js(&s))
        },
        None,
    );
    attr(
        vm,
        proto,
        "tagName",
        |vm, c| {
            let (h, n) = el_this(vm, &c)?;
            let s = with_doc(vm, &h, n.doc, |d| {
                d.element(n.id).map(|e| tag_name(&h, n.doc, e)).unwrap_or_default()
            })?;
            Ok(js(&s))
        },
        None,
    );
    crate::html::reflect(vm, proto, "id", "id", crate::html::Reflect::Str);
    crate::html::reflect(vm, proto, "className", "class", crate::html::Reflect::Str);
    crate::html::reflect(vm, proto, "slot", "slot", crate::html::Reflect::Str);
    attr(
        vm,
        proto,
        "classList",
        |vm, c| {
            let (h, n) = el_this(vm, &c)?;
            collection(vm, &h, CollKind::ClassList(n), Some((2, n.pack())))
        },
        Some(|vm, c| {
            // [PutForwards=value]
            let list = vm.get(
                match &c.this {
                    Value::Object(o) => *o,
                    _ => return vm.type_error("Illegal invocation"),
                },
                &PropKey::lit("classList"),
            )?;
            if let Value::Object(l) = list {
                vm.set(l, PropKey::lit("value"), c.arg(0), Value::Object(l))?;
            }
            Ok(Value::Undefined)
        }),
    );
    op(vm, proto, "hasAttributes", 0, |vm, c| {
        let (h, n) = el_this(vm, &c)?;
        let r = with_doc(vm, &h, n.doc, |d| d.element(n.id).is_some_and(|e| !e.attrs.is_empty()))?;
        Ok(Value::Bool(r))
    });
    attr(
        vm,
        proto,
        "attributes",
        |vm, c| {
            let (h, n) = el_this(vm, &c)?;
            collection(vm, &h, CollKind::Attributes(n), Some((3, n.pack())))
        },
        None,
    );
    op(vm, proto, "getAttributeNames", 0, |vm, c| {
        let (h, n) = el_this(vm, &c)?;
        let names = with_doc(vm, &h, n.doc, |d| {
            d.element(n.id).map(|e| e.attrs.iter().map(attr_qname).collect::<Vec<_>>())
        })?
        .unwrap_or_default();
        let vals = names.iter().map(|s| js(s)).collect();
        Ok(Value::Object(vm.new_array(vals)))
    });
    op(vm, proto, "getAttribute", 1, |vm, c| {
        let q = rstr(vm, &c.arg(0))?;
        let (h, n) = el_this(vm, &c)?;
        let r = with_doc(vm, &h, n.doc, |d| {
            attr_index_by_name(&h, n, d, &q).and_then(|i| d.element(n.id).map(|e| e.attrs[i].value.clone()))
        })?;
        Ok(r.map_or(Value::Null, |s| js(&s)))
    });
    op(vm, proto, "getAttributeNS", 2, |vm, c| {
        let ns = rstr_opt(vm, &c.arg(0), false)?.unwrap_or_default();
        let local = rstr(vm, &c.arg(1))?;
        let (h, n) = el_this(vm, &c)?;
        let ns = Namespace::from_url(&ns);
        let r = with_doc(vm, &h, n.doc, |d| {
            d.element(n.id)
                .and_then(|e| e.attrs.iter().find(|a| a.ns == ns && a.name == local).map(|a| a.value.clone()))
        })?;
        Ok(r.map_or(Value::Null, |s| js(&s)))
    });
    op(vm, proto, "setAttribute", 2, |vm, c| {
        let q = rstr(vm, &c.arg(0))?;
        let v = rstr(vm, &c.arg(1))?;
        let (h, n) = el_this(vm, &c)?;
        if !valid_attribute_local_name(&q) {
            return throw(vm, "InvalidCharacterError", &format!("'{q}' is not a valid attribute name"));
        }
        let target = with_doc(vm, &h, n.doc, |d| match attr_index_by_name(&h, n, d, &q) {
            Some(i) => {
                d.element(n.id).map(|e| (e.attrs[i].ns, e.attrs[i].prefix.clone(), e.attrs[i].name.clone()))
            }
            None => Some((Namespace::None, None, lookup_name(&h, n, d, &q))),
        })?;
        if let Some((ns, prefix, local)) = target {
            crate::mutation::set_attr(vm, &h, n, ns, prefix, &local, &v)?;
        }
        Ok(Value::Undefined)
    });
    op(vm, proto, "setAttributeNS", 3, |vm, c| {
        let ns = rstr_opt(vm, &c.arg(0), false)?;
        let q = rstr(vm, &c.arg(1))?;
        let v = rstr(vm, &c.arg(2))?;
        let (h, n) = el_this(vm, &c)?;
        let (ns, prefix, local) = validate_and_extract(vm, ns, &q, false)?;
        crate::mutation::set_attr(vm, &h, n, ns, prefix, &local, &v)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "removeAttribute", 1, |vm, c| {
        let q = rstr(vm, &c.arg(0))?;
        let (h, n) = el_this(vm, &c)?;
        let i = with_doc(vm, &h, n.doc, |d| attr_index_by_name(&h, n, d, &q))?;
        if let Some(i) = i {
            crate::mutation::remove_attr_at(vm, &h, n, i)?;
        }
        Ok(Value::Undefined)
    });
    op(vm, proto, "removeAttributeNS", 2, |vm, c| {
        let ns = rstr_opt(vm, &c.arg(0), false)?.unwrap_or_default();
        let local = rstr(vm, &c.arg(1))?;
        let (h, n) = el_this(vm, &c)?;
        crate::mutation::remove_attr(vm, &h, n, Namespace::from_url(&ns), &local)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "toggleAttribute", 1, |vm, c| {
        let q = rstr(vm, &c.arg(0))?;
        let force = match c.arg(1) {
            Value::Undefined => None,
            v => Some(vm.to_boolean(&v)),
        };
        let (h, n) = el_this(vm, &c)?;
        if !valid_attribute_local_name(&q) {
            return throw(vm, "InvalidCharacterError", &format!("'{q}' is not a valid attribute name"));
        }
        let (i, name) =
            with_doc(vm, &h, n.doc, |d| (attr_index_by_name(&h, n, d, &q), lookup_name(&h, n, d, &q)))?;
        let r = match i {
            None => {
                if force == Some(false) {
                    false
                } else {
                    crate::mutation::set_attr(vm, &h, n, Namespace::None, None, &name, "")?;
                    true
                }
            }
            Some(i) => {
                if force == Some(true) {
                    true
                } else {
                    crate::mutation::remove_attr_at(vm, &h, n, i)?;
                    false
                }
            }
        };
        Ok(Value::Bool(r))
    });
    op(vm, proto, "hasAttribute", 1, |vm, c| {
        let q = rstr(vm, &c.arg(0))?;
        let (h, n) = el_this(vm, &c)?;
        let r = with_doc(vm, &h, n.doc, |d| attr_index_by_name(&h, n, d, &q).is_some())?;
        Ok(Value::Bool(r))
    });
    op(vm, proto, "hasAttributeNS", 2, |vm, c| {
        let ns = rstr_opt(vm, &c.arg(0), false)?.unwrap_or_default();
        let local = rstr(vm, &c.arg(1))?;
        let (h, n) = el_this(vm, &c)?;
        let ns = Namespace::from_url(&ns);
        let r = with_doc(vm, &h, n.doc, |d| {
            d.element(n.id).is_some_and(|e| e.attrs.iter().any(|a| a.ns == ns && a.name == local))
        })?;
        Ok(Value::Bool(r))
    });
    crate::attr::setup_element_attr_nodes(vm, proto);
    op(vm, proto, "closest", 1, |vm, c| {
        let sel = rstr(vm, &c.arg(0))?;
        let (h, n) = el_this(vm, &c)?;
        // Validate once.
        matches_selector(vm, &h, n, &sel)?;
        let chain = with_doc(vm, &h, n.doc, |d| {
            std::iter::once(n.id)
                .chain(d.ancestors(n.id))
                .filter(|&x| d.node(x).is_element())
                .collect::<Vec<_>>()
        })?;
        for x in chain {
            if matches_selector(vm, &h, n.with(x), &sel)? {
                return wrap(vm, &h, n.with(x));
            }
        }
        Ok(Value::Null)
    });
    for name in ["matches", "webkitMatchesSelector"] {
        op(vm, proto, name, 1, |vm, c| {
            let sel = rstr(vm, &c.arg(0))?;
            let (h, n) = el_this(vm, &c)?;
            Ok(Value::Bool(matches_selector(vm, &h, n, &sel)?))
        });
    }
    op(vm, proto, "insertAdjacentElement", 2, |vm, c| {
        let w = rstr(vm, &c.arg(0))?;
        let (h, n) = el_this(vm, &c)?;
        let el = match crate::host::node_of(vm, &c.arg(1)) {
            Some(e) if with_doc(vm, &h, e.doc, |d| d.node(e.id).is_element())? => e,
            _ => return vm.type_error("parameter 2 is not an Element"),
        };
        let r = insert_adjacent(vm, &h, n, &w, el)?;
        wrap_opt(vm, &h, r)
    });
    op(vm, proto, "insertAdjacentText", 2, |vm, c| {
        let w = rstr(vm, &c.arg(0))?;
        let data = rstr(vm, &c.arg(1))?;
        let (h, n) = el_this(vm, &c)?;
        let t = with_doc_mut(vm, &h, n.doc, |d| d.create_text(&data))?;
        insert_adjacent(vm, &h, n, &w, n.with(t))?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "insertAdjacentHTML", 2, crate::html::insert_adjacent_html);
    attr(vm, proto, "innerHTML", crate::html::inner_html_get, Some(crate::html::inner_html_set));
    attr(vm, proto, "outerHTML", crate::html::outer_html_get, Some(crate::html::outer_html_set));
}

/// DOM §4.9 "insert adjacent".
pub fn insert_adjacent(
    vm: &mut Vm,
    h: &Host,
    el: NodeRef,
    wher: &str,
    node: NodeRef,
) -> JsResult<Option<NodeRef>> {
    let (parent, next, first) =
        with_doc(vm, h, el.doc, |d| (d.parent(el.id), d.next_sibling(el.id), d.first_child(el.id)))?;
    match wher.to_ascii_lowercase().as_str() {
        "beforebegin" => {
            let Some(p) = parent else { return Ok(None) };
            tree::pre_insert(vm, h, node, el.with(p), Some(el)).map(Some)
        }
        "afterbegin" => tree::pre_insert(vm, h, node, el, first.map(|f| el.with(f))).map(Some),
        "beforeend" => tree::pre_insert(vm, h, node, el, None).map(Some),
        "afterend" => {
            let Some(p) = parent else { return Ok(None) };
            tree::pre_insert(vm, h, node, el.with(p), next.map(|x| el.with(x))).map(Some)
        }
        _ => throw(vm, "SyntaxError", "the position must be beforebegin, afterbegin, beforeend or afterend"),
    }
}

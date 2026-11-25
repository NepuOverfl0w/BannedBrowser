//! HTML element interfaces (HTML §3.2.8 HTMLElement and the element sections): reflected
//! content attributes (HTML §2.6.1), innerHTML / outerHTML / insertAdjacentHTML (DOM
//! Parsing and Serialization, HTML §8.5), and `dataset` (HTML §3.2.6.6 DOMStringMap).

use bb_dom::{Document, Namespace, NodeData, NodeId};
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::object::PropDesc;
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::collections::{collection, CollKind};
use crate::exception::throw;
use crate::host::*;
use crate::idl::{attr, attr_with, callee_slots, op};
use crate::tree;

/// How a reflected attribute converts (HTML §2.6.1).
#[derive(Clone, Copy)]
pub enum Reflect {
    Str,
    Bool,
    /// `long` with a default.
    Long(i32),
    /// `unsigned long` with a default.
    ULong(u32),
    /// A URL resolved against the document's base URL.
    Url,
}

fn reflect_kind(slots: &[Value]) -> Reflect {
    let n = match slots.get(1) {
        Some(Value::Number(n)) => *n as i64,
        _ => 0,
    };
    let def = match slots.get(2) {
        Some(Value::Number(n)) => *n,
        _ => 0.0,
    };
    match n {
        1 => Reflect::Bool,
        2 => Reflect::Long(def as i32),
        3 => Reflect::ULong(def as u32),
        4 => Reflect::Url,
        _ => Reflect::Str,
    }
}

/// Define IDL attribute `idl` reflecting content attribute `content`.
pub fn reflect(vm: &mut Vm, proto: ObjRef, idl: &str, content: &str, kind: Reflect) {
    let (tag, def) = match kind {
        Reflect::Str => (0.0, 0.0),
        Reflect::Bool => (1.0, 0.0),
        Reflect::Long(d) => (2.0, d as f64),
        Reflect::ULong(d) => (3.0, d as f64),
        Reflect::Url => (4.0, 0.0),
    };
    attr_with(
        vm,
        proto,
        idl,
        reflect_get,
        Some(reflect_set),
        vec![js(content), Value::Number(tag), Value::Number(def)],
    );
}

fn reflect_get(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let slots = callee_slots(vm, c.callee);
    let name = match slots.first() {
        Some(v) => rstr(vm, v)?,
        None => return Ok(Value::Undefined),
    };
    let kind = reflect_kind(&slots);
    let (h, n) = this_node(vm, &c.this, C::Element)?;
    let v = with_doc(vm, &h, n.doc, |d| d.attr(n.id, &name).map(str::to_string))?;
    Ok(match kind {
        Reflect::Str => js(v.as_deref().unwrap_or("")),
        Reflect::Bool => Value::Bool(v.is_some()),
        Reflect::Long(d) => Value::Number(
            v.and_then(|s| parse_int(&s)).filter(|x| i32::try_from(*x).is_ok()).unwrap_or(d as i64) as f64,
        ),
        Reflect::ULong(d) => Value::Number(
            v.and_then(|s| parse_int(&s)).filter(|x| (0..=2147483647).contains(x)).unwrap_or(d as i64) as f64,
        ),
        Reflect::Url => match v {
            None => js(""),
            Some(s) => {
                let base = base_url(&h, n.doc);
                match bb_url::Url::parse_with_base(&s, Some(&base)) {
                    Ok(u) => js(&u.href()),
                    Err(_) => js(&s),
                }
            }
        },
    })
}

fn reflect_set(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let slots = callee_slots(vm, c.callee);
    let name = match slots.first() {
        Some(v) => rstr(vm, v)?,
        None => return Ok(Value::Undefined),
    };
    let kind = reflect_kind(&slots);
    let v = match kind {
        Reflect::Bool => {
            if vm.to_boolean(&c.arg(0)) {
                Some(String::new())
            } else {
                None
            }
        }
        Reflect::Long(_) => Some(vm.to_int32(&c.arg(0))?.to_string()),
        Reflect::ULong(d) => {
            let x = vm.to_uint32(&c.arg(0))?;
            Some(if x > 2147483647 { d } else { x }.to_string())
        }
        _ => Some(rstr(vm, &c.arg(0))?),
    };
    let (h, n) = this_node(vm, &c.this, C::Element)?;
    crate::mutation::set_or_remove(vm, &h, n, &name, v.as_deref())?;
    Ok(Value::Undefined)
}

/// HTML §2.3.4.1 "rules for parsing integers".
pub fn parse_int(s: &str) -> Option<i64> {
    let t = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let digits: &str = &t[..t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len())];
    if digits.is_empty() {
        return None;
    }
    let v: i64 = digits.parse().ok()?;
    Some(if neg { -v } else { v })
}

// ---- HTMLElement ----

pub fn setup_html_element(vm: &mut Vm, proto: ObjRef) {
    for (idl, content) in [("title", "title"), ("lang", "lang"), ("accessKey", "accesskey")] {
        reflect(vm, proto, idl, content, Reflect::Str);
    }
    reflect(vm, proto, "hidden", "hidden", Reflect::Bool);
    reflect(vm, proto, "inert", "inert", Reflect::Bool);
    reflect(vm, proto, "autofocus", "autofocus", Reflect::Bool);
    reflect(vm, proto, "nonce", "nonce", Reflect::Str);
    attr(
        vm,
        proto,
        "dir",
        |vm, c| {
            let (h, n) = this_node(vm, &c.this, C::HTMLElement)?;
            let v = with_doc(vm, &h, n.doc, |d| d.attr(n.id, "dir").unwrap_or("").to_ascii_lowercase())?;
            Ok(js(if matches!(v.as_str(), "ltr" | "rtl" | "auto") { &v } else { "" }))
        },
        Some(|vm, c| {
            let v = rstr(vm, &c.arg(0))?;
            let (h, n) = this_node(vm, &c.this, C::HTMLElement)?;
            crate::mutation::set_or_remove(vm, &h, n, "dir", Some(&v))?;
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "tabIndex",
        |vm, c| {
            let (h, n) = this_node(vm, &c.this, C::HTMLElement)?;
            let (v, focusable) = with_doc(vm, &h, n.doc, |d| {
                let v = d.attr(n.id, "tabindex").and_then(parse_int);
                let f = d.element(n.id).is_some_and(|e| {
                    matches!(
                        e.name.as_str(),
                        "a" | "button" | "input" | "select" | "textarea" | "iframe" | "summary"
                    )
                });
                (v, f)
            })?;
            Ok(Value::Number(
                v.filter(|x| i32::try_from(*x).is_ok()).unwrap_or(if focusable { 0 } else { -1 }) as f64,
            ))
        },
        Some(|vm, c| {
            let v = vm.to_int32(&c.arg(0))?;
            let (h, n) = this_node(vm, &c.this, C::HTMLElement)?;
            crate::mutation::set_or_remove(vm, &h, n, "tabindex", Some(&v.to_string()))?;
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "dataset",
        |vm, c| {
            let (h, n) = this_node(vm, &c.this, C::HTMLElement)?;
            collection(vm, &h, CollKind::Dataset(n), Some((4, n.pack())))
        },
        None,
    );
    attr(vm, proto, "innerText", inner_text_get, Some(inner_text_set));
    attr(vm, proto, "outerText", inner_text_get, Some(outer_text_set));
    op(vm, proto, "click", 0, |vm, c| {
        let (h, n) = this_node(vm, &c.this, C::HTMLElement)?;
        // HTML §6.5.3 click(): a disabled form control gets none.
        let disabled = with_doc(vm, &h, n.doc, |d| {
            ["button", "input", "select", "textarea"].iter().any(|t| d.is_html(n.id, t))
                && d.is_disabled(n.id)
        })?;
        if disabled {
            return Ok(Value::Undefined);
        }
        crate::events::fire_synthetic_click(vm, &h, n)?;
        Ok(Value::Undefined)
    });
    crate::focus::setup_element(vm, proto);
    crate::events::setup_event_handlers(vm, proto);
    crate::css::setup_inline_style(vm, proto);
    if let Ok(h) = host(vm) {
        let ep = h.proto(C::Element);
        crate::css::setup_element_geometry(vm, ep, proto);
    }
}

/// innerText without layout: the text content, with `<br>` as a line break (HTML §3.2.7,
/// approximated: no CSS-dependent spacing).
fn inner_text_get(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let (h, n) = this_node(vm, &c.this, C::HTMLElement)?;
    let s = with_doc(vm, &h, n.doc, |d| {
        let mut s = String::new();
        for x in d.descendants(n.id) {
            match &d.node(x).data {
                NodeData::Text(t) => {
                    let in_hidden = d.ancestors(x).take_while(|&a| a != n.id).any(|a| {
                        d.element(a).is_some_and(|e| {
                            matches!(e.name.as_str(), "script" | "style" | "template" | "head")
                        })
                    });
                    if !in_hidden {
                        s.push_str(t);
                    }
                }
                NodeData::Element(e) if e.name == "br" => s.push('\n'),
                _ => {}
            }
        }
        s
    })?;
    Ok(js(&s))
}

/// HTML §3.2.7 "rendered text fragment": text with `<br>` for line breaks.
fn text_fragment(d: &mut Document, text: &str) -> NodeId {
    let frag = d.create_fragment();
    let mut first = true;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if !first {
            let br = d.create_html_element("br");
            d.append_child(frag, br);
        }
        first = false;
        if !line.is_empty() {
            let t = d.create_text(line);
            d.append_child(frag, t);
        }
    }
    frag
}

fn inner_text_set(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let v = rstr_null_empty(vm, &c.arg(0))?;
    let (h, n) = this_node(vm, &c.this, C::HTMLElement)?;
    let frag = with_doc_mut(vm, &h, n.doc, |d| text_fragment(d, &v))?;
    tree::replace_all(vm, &h, Some(n.with(frag)), n)?;
    Ok(Value::Undefined)
}

fn outer_text_set(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let v = rstr_null_empty(vm, &c.arg(0))?;
    let (h, n) = this_node(vm, &c.this, C::HTMLElement)?;
    let Some(parent) = with_doc(vm, &h, n.doc, |d| d.parent(n.id))? else {
        return throw(vm, "NoModificationAllowedError", "the element has no parent");
    };
    let frag = with_doc_mut(vm, &h, n.doc, |d| text_fragment(d, &v))?;
    tree::replace(vm, &h, n, n.with(frag), n.with(parent))?;
    Ok(Value::Undefined)
}

// ---- serialization and fragment parsing ----

/// HTML §13.3 "serialize the children" of `id` (a template's contents for a template).
fn serialize_children(d: &Document, id: NodeId) -> String {
    let src = d.template_of(id).unwrap_or(id);
    d.children(src).map(|k| bb_dom::serialize_html(d, k)).collect()
}

pub fn inner_html_get(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let (h, n) = this_node(vm, &c.this, C::Node)?;
    if crate::xml::in_xml_document(&h, n) {
        return Ok(js(&crate::xml::serialize_markup(vm, &h, n, true)?));
    }
    let s = with_doc(vm, &h, n.doc, |d| serialize_children(d, n.id))?;
    Ok(js(&s))
}

pub fn outer_html_get(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let (h, n) = this_node(vm, &c.this, C::Element)?;
    if crate::xml::in_xml_document(&h, n) {
        return Ok(js(&crate::xml::serialize_markup(vm, &h, n, false)?));
    }
    let s = with_doc(vm, &h, n.doc, |d| bb_dom::serialize_html(d, n.id))?;
    Ok(js(&s))
}

/// HTML §13.4 fragment parsing with `context`, imported into document `doc` as a fragment.
/// Scripts in it never run (they are marked "already started").
pub fn parse_fragment_into(
    vm: &mut Vm,
    h: &Host,
    doc: u32,
    context: NodeRef,
    markup: &str,
) -> JsResult<NodeRef> {
    parse_fragment_scripts(vm, h, doc, context, markup, false, false)
}

/// [`parse_fragment_into`] with declarative shadow roots allowed (HTML §8.4.1 "unsafely set
/// HTML").
pub fn parse_fragment_shadow(
    vm: &mut Vm,
    h: &Host,
    doc: u32,
    context: NodeRef,
    markup: &str,
) -> JsResult<NodeRef> {
    parse_fragment_scripts(vm, h, doc, context, markup, false, true)
}

/// Fragment parsing; `scripts_run` leaves the scripts able to run when inserted.
fn parse_fragment_scripts(
    vm: &mut Vm,
    h: &Host,
    doc: u32,
    context: NodeRef,
    markup: &str,
    scripts_run: bool,
    shadow_roots: bool,
) -> JsResult<NodeRef> {
    // XML documents use the XML fragment parsing algorithm (HTML §13.4).
    if !crate::dom::is_html_doc(h, doc) {
        return crate::xml::parse_fragment(vm, h, doc, context, markup);
    }
    let (ns, name) = with_doc(vm, h, context.doc, |d| {
        d.element(context.id).map_or((Namespace::Html, "body".to_string()), |e| (e.ns, e.name.clone()))
    })?;
    let (src, root) =
        bb_html::tree_builder::TreeBuilder::parse_fragment_with(markup, ns, &name, shadow_roots);
    let before = with_doc(vm, h, doc, |d| d.len())?;
    let (frag, pairs) = with_doc_mut(vm, h, doc, |d| {
        let frag = d.create_fragment();
        let mut all = Vec::new();
        let kids: Vec<NodeId> = src.children(root).collect();
        for k in kids {
            let (copy, pairs) = d.import_node(&src, k, true);
            d.append_child(frag, copy);
            all.extend(pairs);
        }
        (frag, all)
    })?;
    let scripts: Vec<u64> = with_doc(vm, h, doc, |d| {
        pairs
            .iter()
            .filter(|(_, new)| d.is_html(*new, "script"))
            .map(|(_, new)| NodeRef { doc, id: *new }.pack())
            .collect()
    })?;
    if !scripts_run {
        h.started_scripts.borrow_mut().extend(scripts);
    }
    // The parser appended their attributes: details elements' steps run.
    crate::details::parsed(vm, h, doc, before);
    Ok(NodeRef { doc, id: frag })
}

/// Range.createContextualFragment (HTML "DOM parsing and serialization APIs"): parse
/// `markup` in the context of `node` (the range's start node). Unlike innerHTML, its
/// scripts run when the fragment is inserted.
pub fn create_contextual_fragment(vm: &mut Vm, h: &Host, node: NodeRef, markup: &str) -> JsResult<NodeRef> {
    // Steps 1–5: the context element.
    let element = with_doc(vm, h, node.doc, |d| match &d.node(node.id).data {
        NodeData::Element(_) => Some(node.id),
        NodeData::Text(_) | NodeData::Comment(_) => d.parent(node.id).filter(|&p| d.element(p).is_some()),
        _ => None,
    })?;
    let is_html_root = match element {
        Some(e) => {
            with_doc(vm, h, node.doc, |d| crate::dom::is_html_doc(h, node.doc) && d.is_html(e, "html"))?
        }
        None => true,
    };
    let context = match element {
        Some(e) if !is_html_root => node.with(e),
        _ => node.with(with_doc_mut(vm, h, node.doc, |d| d.create_html_element("body"))?),
    };
    parse_fragment_scripts(vm, h, node.doc, context, markup, true, false)
}

pub fn inner_html_set(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let v = rstr_null_empty(vm, &c.arg(0))?;
    let (h, n) = this_node(vm, &c.this, C::Node)?;
    let frag = parse_fragment_into(vm, &h, n.doc, n, &v)?;
    let target = with_doc(vm, &h, n.doc, |d| d.template_of(n.id))?.map_or(n, |t| n.with(t));
    tree::replace_all(vm, &h, Some(frag), target)?;
    Ok(Value::Undefined)
}

pub fn outer_html_set(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let v = rstr_null_empty(vm, &c.arg(0))?;
    let (h, n) = this_node(vm, &c.this, C::Element)?;
    let Some(parent) = with_doc(vm, &h, n.doc, |d| d.parent(n.id))? else { return Ok(Value::Undefined) };
    let parent_kind = with_doc(vm, &h, n.doc, |d| match d.node(parent).data {
        NodeData::Document => 0,
        NodeData::DocumentFragment => 1,
        _ => 2,
    })?;
    if parent_kind == 0 {
        return throw(vm, "NoModificationAllowedError", "can't replace the document element this way");
    }
    let context = if parent_kind == 1 {
        let body = with_doc_mut(vm, &h, n.doc, |d| d.create_html_element("body"))?;
        n.with(body)
    } else {
        n.with(parent)
    };
    let frag = parse_fragment_into(vm, &h, n.doc, context, &v)?;
    tree::replace(vm, &h, n, frag, n.with(parent))?;
    Ok(Value::Undefined)
}

pub fn insert_adjacent_html(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let w = rstr(vm, &c.arg(0))?.to_ascii_lowercase();
    let markup = rstr(vm, &c.arg(1))?;
    let (h, n) = this_node(vm, &c.this, C::Element)?;
    let parent = with_doc(vm, &h, n.doc, |d| d.parent(n.id))?;
    let context = match w.as_str() {
        "beforebegin" | "afterend" => {
            let ok = parent.is_some_and(|p| p != Document::ROOT)
                && with_doc(vm, &h, n.doc, |d| {
                    parent.is_some_and(|p| !matches!(d.node(p).data, NodeData::Document))
                })?;
            if !ok {
                return throw(vm, "NoModificationAllowedError", "the element has no parent element");
            }
            n.with(parent.unwrap_or(Document::ROOT))
        }
        "afterbegin" | "beforeend" => n,
        _ => {
            return throw(
                vm,
                "SyntaxError",
                "the position must be beforebegin, afterbegin, beforeend or afterend",
            )
        }
    };
    let frag = parse_fragment_into(vm, &h, n.doc, context, &markup)?;
    crate::dom::insert_adjacent(vm, &h, n, &w, frag)?;
    Ok(Value::Undefined)
}

// ---- dataset ----

fn dataset_element(kind: &CollKind) -> Option<NodeRef> {
    match kind {
        CollKind::Dataset(n) => Some(*n),
        _ => None,
    }
}

/// The supported property names of a DOMStringMap and their values (HTML §3.2.6.6).
pub fn dataset_names(h: &Host, kind: &CollKind) -> Vec<(String, String)> {
    let Some(n) = dataset_element(kind) else { return Vec::new() };
    let docs = h.docs.borrow();
    let Some(e) = docs.get(n.doc as usize).and_then(|d| d.element(n.id)) else { return Vec::new() };
    let mut out = Vec::new();
    for a in &e.attrs {
        if a.ns != Namespace::None
            || !a.name.starts_with("data-")
            || a.name.chars().any(|c| c.is_ascii_uppercase())
        {
            continue;
        }
        let mut name = String::new();
        let mut cs = a.name["data-".len()..].chars().peekable();
        while let Some(c) = cs.next() {
            if c == '-' {
                if let Some(&nx) = cs.peek() {
                    if nx.is_ascii_lowercase() {
                        name.push(nx.to_ascii_uppercase());
                        cs.next();
                        continue;
                    }
                }
            }
            name.push(c);
        }
        out.push((name, a.value.clone()));
    }
    out
}

/// A dataset name as its content attribute (HTML §3.2.6.6 setter steps 1–4).
fn data_attr_name(vm: &mut Vm, name: &str) -> JsResult<String> {
    let cs: Vec<char> = name.chars().collect();
    if cs.windows(2).any(|w| w[0] == '-' && w[1].is_ascii_lowercase()) {
        return throw(vm, "SyntaxError", "a dataset name can't contain '-' followed by a lowercase letter");
    }
    let mut out = String::from("data-");
    for c in cs {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    if !crate::dom::valid_attribute_local_name(&out) {
        return throw(vm, "InvalidCharacterError", "not a valid attribute name");
    }
    Ok(out)
}

pub fn dataset_set(vm: &mut Vm, h: &Host, kind: &CollKind, name: &str, v: &str) -> JsResult<bool> {
    let Some(n) = dataset_element(kind) else { return Ok(false) };
    let a = data_attr_name(vm, name)?;
    crate::mutation::set_or_remove(vm, h, n, &a, Some(v))?;
    Ok(true)
}

pub fn dataset_define(
    vm: &mut Vm,
    h: &Host,
    kind: &CollKind,
    k: &PropKey,
    d: &PropDesc,
) -> JsResult<Option<bool>> {
    if matches!(k, PropKey::Sym(_)) {
        return Ok(None);
    }
    if d.get.is_some() || d.set.is_some() {
        return Ok(Some(false));
    }
    let v = match &d.value {
        Some(v) => rstr(vm, v)?,
        None => return Ok(Some(false)),
    };
    dataset_set(vm, h, kind, &k.display(), &v).map(Some)
}

pub fn dataset_delete(vm: &mut Vm, h: &Host, kind: &CollKind, k: &PropKey) -> JsResult<Option<bool>> {
    if matches!(k, PropKey::Sym(_)) {
        return Ok(None);
    }
    let Some(n) = dataset_element(kind) else { return Ok(None) };
    let name = k.display();
    if !dataset_names(h, kind).iter().any(|x| x.0 == name) {
        return Ok(None);
    }
    let Ok(a) = data_attr_name(vm, &name) else { return Ok(Some(true)) };
    crate::mutation::set_or_remove(vm, h, n, &a, None)?;
    Ok(Some(true))
}

// ---- HTMLHyperlinkElementUtils (HTML §4.6.4) ----

/// The element's URL: its href parsed against the document's base (None if it has none or
/// it doesn't parse).
fn hyperlink_url(
    vm: &mut Vm,
    c: &NativeCall,
) -> JsResult<(std::rc::Rc<Host>, NodeRef, Option<bb_url::Url>, Option<String>)> {
    let (h, n) = this_node(vm, &c.this, C::HTMLElement)?;
    let href = with_doc(vm, &h, n.doc, |d| d.attr(n.id, "href").map(str::to_string))?;
    let base = base_url(&h, n.doc);
    let url = href.as_deref().and_then(|v| bb_url::Url::parse_with_base(v, Some(&base)).ok());
    Ok((h, n, url, href))
}

fn hyperlink_get(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let part = match callee_slots(vm, c.callee).first() {
        Some(v) => rstr(vm, v)?,
        None => String::new(),
    };
    let (_, _, url, href) = hyperlink_url(vm, &c)?;
    let Some(u) = url else {
        // No URL: href is the attribute as is, origin and the parts are empty.
        return Ok(js(if part == "href" { href.as_deref().unwrap_or("") } else { "" }));
    };
    let s = match part.as_str() {
        "href" => u.href(),
        "origin" => u.origin().serialize(),
        "protocol" => u.protocol(),
        "username" => u.username().to_string(),
        "password" => u.password().to_string(),
        "host" => u.host_with_port(),
        "hostname" => u.hostname(),
        "port" => u.port().map(|p| p.to_string()).unwrap_or_default(),
        "pathname" => u.pathname(),
        "search" => u.search(),
        "hash" => u.hash(),
        _ => String::new(),
    };
    Ok(js(&s))
}

fn hyperlink_set(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let part = match callee_slots(vm, c.callee).first() {
        Some(v) => rstr(vm, v)?,
        None => String::new(),
    };
    let v = rstr(vm, &c.arg(0))?;
    let (h, n, url, _) = hyperlink_url(vm, &c)?;
    if part == "href" {
        crate::mutation::set_or_remove(vm, &h, n, "href", Some(&v))?;
        return Ok(Value::Undefined);
    }
    // Without a URL (or with an opaque path, for most parts) nothing changes.
    let Some(mut u) = url else { return Ok(Value::Undefined) };
    match part.as_str() {
        "protocol" => u.set_protocol(&v),
        "username" => u.set_username(&v),
        "password" => u.set_password(&v),
        "host" => u.set_host(&v),
        "hostname" => u.set_hostname(&v),
        "port" => u.set_port(&v),
        "pathname" => u.set_pathname(&v),
        "search" => u.set_search(&v),
        "hash" => u.set_hash(&v),
        _ => return Ok(Value::Undefined),
    }
    let href = u.href();
    crate::mutation::set_or_remove(vm, &h, n, "href", Some(&href))?;
    Ok(Value::Undefined)
}

fn setup_hyperlink_utils(vm: &mut Vm, proto: ObjRef) {
    for part in
        ["href", "protocol", "username", "password", "host", "hostname", "port", "pathname", "search", "hash"]
    {
        attr_with(vm, proto, part, hyperlink_get, Some(hyperlink_set), vec![js(part)]);
    }
    attr_with(vm, proto, "origin", hyperlink_get, None, vec![js("origin")]);
    let f = bb_js::vm::realm::native_with(vm, "toString", 0, hyperlink_get, false, vec![js("href")]);
    vm.heap.obj_mut(proto).props.insert(
        PropKey::lit("toString"),
        bb_js::vm::heap::Prop {
            slot: bb_js::vm::heap::Slot::Data(Value::Object(f)),
            writable: true,
            enumerable: true,
            configurable: true,
        },
    );
}

// ---- element interfaces ----

pub fn setup_element_interface(vm: &mut Vm, c: C, proto: ObjRef) {
    use Reflect::*;
    let s = |vm: &mut Vm, names: &[&str]| {
        for n in names {
            reflect(vm, proto, n, &n.to_ascii_lowercase(), Str);
        }
    };
    match c {
        C::HTMLAnchorElement | C::HTMLAreaElement => {
            setup_hyperlink_utils(vm, proto);
            s(vm, &["target", "download", "ping", "rel", "hreflang", "type", "referrerPolicy"]);
            if c == C::HTMLAreaElement {
                s(vm, &["alt", "coords", "shape"]);
            }
            attr(vm, proto, "text", crate::dom::text_content_get, None);
        }
        C::HTMLImageElement => {
            reflect(vm, proto, "src", "src", Url);
            s(
                vm,
                &[
                    "alt",
                    "srcset",
                    "sizes",
                    "crossOrigin",
                    "useMap",
                    "referrerPolicy",
                    "decoding",
                    "loading",
                    "name",
                    "align",
                    "border",
                ],
            );
            reflect(vm, proto, "isMap", "ismap", Bool);
            reflect(vm, proto, "width", "width", ULong(0));
            reflect(vm, proto, "height", "height", ULong(0));
        }
        C::HTMLScriptElement => {
            reflect(vm, proto, "src", "src", Url);
            s(vm, &["type", "charset", "crossOrigin", "integrity", "referrerPolicy", "event"]);
            reflect(vm, proto, "htmlFor", "for", Str);
            reflect(vm, proto, "async", "async", Bool);
            reflect(vm, proto, "defer", "defer", Bool);
            reflect(vm, proto, "noModule", "nomodule", Bool);
            attr(vm, proto, "text", crate::dom::text_content_get, Some(crate::dom::text_content_set));
        }
        C::HTMLLinkElement => {
            reflect(vm, proto, "href", "href", Url);
            s(
                vm,
                &[
                    "rel",
                    "media",
                    "hreflang",
                    "type",
                    "as",
                    "crossOrigin",
                    "integrity",
                    "referrerPolicy",
                    "sizes",
                ],
            );
            reflect(vm, proto, "disabled", "disabled", Bool);
        }
        C::HTMLInputElement => {
            s(
                vm,
                &[
                    "name",
                    "accept",
                    "alt",
                    "autocomplete",
                    "dirName",
                    "max",
                    "min",
                    "pattern",
                    "placeholder",
                    "step",
                    "formAction",
                    "formEnctype",
                    "formMethod",
                    "formTarget",
                ],
            );
            reflect(vm, proto, "src", "src", Url);
            for b in ["disabled", "required", "readOnly", "multiple", "autofocus", "formNoValidate"] {
                reflect(vm, proto, b, &b.to_ascii_lowercase(), Bool);
            }
            reflect(vm, proto, "defaultChecked", "checked", Bool);
            reflect(vm, proto, "defaultValue", "value", Str);
            reflect(vm, proto, "size", "size", ULong(20));
            reflect(vm, proto, "maxLength", "maxlength", Long(-1));
            reflect(vm, proto, "minLength", "minlength", Long(-1));
            crate::forms::setup_input(vm, proto);
        }
        C::HTMLButtonElement => {
            s(vm, &["name", "value", "formAction", "formEnctype", "formMethod", "formTarget"]);
            reflect(vm, proto, "disabled", "disabled", Bool);
            crate::forms::setup_button(vm, proto);
        }
        C::HTMLSelectElement => {
            reflect(vm, proto, "name", "name", Str);
            reflect(vm, proto, "disabled", "disabled", Bool);
            reflect(vm, proto, "multiple", "multiple", Bool);
            reflect(vm, proto, "required", "required", Bool);
            reflect(vm, proto, "size", "size", ULong(0));
            crate::forms::setup_select(vm, proto);
        }
        C::HTMLTextAreaElement => {
            s(vm, &["name", "placeholder", "wrap", "autocomplete", "dirName"]);
            for b in ["disabled", "required", "readOnly"] {
                reflect(vm, proto, b, &b.to_ascii_lowercase(), Bool);
            }
            reflect(vm, proto, "rows", "rows", ULong(2));
            reflect(vm, proto, "cols", "cols", ULong(20));
            reflect(vm, proto, "maxLength", "maxlength", Long(-1));
            crate::forms::setup_textarea(vm, proto);
        }
        C::HTMLOptionElement => {
            reflect(vm, proto, "disabled", "disabled", Bool);
            reflect(vm, proto, "label", "label", Str);
            reflect(vm, proto, "defaultSelected", "selected", Bool);
            crate::forms::setup_option(vm, proto);
        }
        C::HTMLOptGroupElement => {
            reflect(vm, proto, "disabled", "disabled", Bool);
            reflect(vm, proto, "label", "label", Str);
        }
        C::HTMLFormElement => {
            s(vm, &["acceptCharset", "name", "rel", "target", "autocomplete"]);
            reflect(vm, proto, "action", "action", Url);
            reflect(vm, proto, "noValidate", "novalidate", Bool);
            crate::forms::setup_form(vm, proto);
        }
        C::HTMLLabelElement => {
            reflect(vm, proto, "htmlFor", "for", Str);
            crate::forms::setup_label(vm, proto);
        }
        C::HTMLFieldSetElement => {
            reflect(vm, proto, "disabled", "disabled", Bool);
            reflect(vm, proto, "name", "name", Str);
            crate::forms::setup_fieldset(vm, proto);
        }
        C::HTMLIFrameElement | C::HTMLFrameElement => {
            attr(vm, proto, "contentWindow", crate::frames::content_window, None);
            attr(vm, proto, "contentDocument", crate::frames::content_document, None);
            reflect(vm, proto, "src", "src", Url);
            s(
                vm,
                &[
                    "srcdoc",
                    "name",
                    "allow",
                    "width",
                    "height",
                    "referrerPolicy",
                    "loading",
                    "scrolling",
                    "frameBorder",
                ],
            );
            reflect(vm, proto, "allowFullscreen", "allowfullscreen", Bool);
        }
        C::HTMLEmbedElement => {
            reflect(vm, proto, "src", "src", Url);
            s(vm, &["type", "width", "height"]);
        }
        C::HTMLObjectElement => {
            reflect(vm, proto, "data", "data", Url);
            s(vm, &["type", "name", "useMap", "width", "height"]);
            crate::forms::setup_validation(vm, proto);
        }
        C::HTMLMediaElement => {
            reflect(vm, proto, "src", "src", Url);
            s(vm, &["crossOrigin", "preload"]);
            for b in ["autoplay", "loop", "controls"] {
                reflect(vm, proto, b, b, Bool);
            }
            reflect(vm, proto, "defaultMuted", "muted", Bool);
        }
        C::HTMLVideoElement => {
            reflect(vm, proto, "poster", "poster", Url);
            reflect(vm, proto, "width", "width", ULong(0));
            reflect(vm, proto, "height", "height", ULong(0));
            reflect(vm, proto, "playsInline", "playsinline", Bool);
        }
        C::HTMLSourceElement => {
            reflect(vm, proto, "src", "src", Url);
            s(vm, &["type", "srcset", "sizes", "media"]);
        }
        C::HTMLTrackElement => {
            reflect(vm, proto, "src", "src", Url);
            s(vm, &["kind", "srclang", "label"]);
            reflect(vm, proto, "default", "default", Bool);
        }
        C::HTMLMetaElement => s(vm, &["name", "content", "media", "scheme"]),
        C::HTMLStyleElement => {
            s(vm, &["media", "type"]);
            reflect(vm, proto, "disabled", "disabled", Bool);
        }
        C::HTMLBaseElement => {
            reflect(vm, proto, "href", "href", Url);
            reflect(vm, proto, "target", "target", Str);
        }
        C::HTMLQuoteElement | C::HTMLModElement => {
            reflect(vm, proto, "cite", "cite", Url);
            if c == C::HTMLModElement {
                reflect(vm, proto, "dateTime", "datetime", Str);
            }
        }
        C::HTMLOListElement => {
            reflect(vm, proto, "reversed", "reversed", Bool);
            reflect(vm, proto, "start", "start", Long(1));
            reflect(vm, proto, "type", "type", Str);
            reflect(vm, proto, "compact", "compact", Bool);
        }
        C::HTMLUListElement | C::HTMLDListElement | C::HTMLMenuElement | C::HTMLDirectoryElement => {
            reflect(vm, proto, "compact", "compact", Bool);
        }
        C::HTMLLIElement => {
            reflect(vm, proto, "value", "value", Long(0));
            reflect(vm, proto, "type", "type", Str);
        }
        C::HTMLTableCellElement => {
            reflect(vm, proto, "colSpan", "colspan", ULong(1));
            reflect(vm, proto, "rowSpan", "rowspan", ULong(1));
            s(
                vm,
                &[
                    "headers", "scope", "abbr", "align", "axis", "height", "width", "ch", "chOff", "vAlign",
                    "bgColor",
                ],
            );
            reflect(vm, proto, "noWrap", "nowrap", Bool);
        }
        C::HTMLTableColElement => reflect(vm, proto, "span", "span", ULong(1)),
        C::HTMLTableElement => s(
            vm,
            &[
                "align",
                "border",
                "frame",
                "rules",
                "summary",
                "width",
                "bgColor",
                "cellPadding",
                "cellSpacing",
            ],
        ),
        C::HTMLDetailsElement => {
            reflect(vm, proto, "open", "open", Bool);
            reflect(vm, proto, "name", "name", Str);
        }
        C::HTMLDialogElement => reflect(vm, proto, "open", "open", Bool),
        C::HTMLDataElement => reflect(vm, proto, "value", "value", Str),
        C::HTMLTimeElement => reflect(vm, proto, "dateTime", "datetime", Str),
        C::HTMLProgressElement | C::HTMLMeterElement => {}
        C::HTMLOutputElement => {
            reflect(vm, proto, "name", "name", Str);
            crate::forms::setup_validation(vm, proto);
        }
        C::HTMLCanvasElement => {
            reflect(vm, proto, "width", "width", ULong(300));
            reflect(vm, proto, "height", "height", ULong(150));
            crate::canvas::setup_canvas_element(vm, proto);
        }
        C::HTMLTemplateElement => {
            attr(
                vm,
                proto,
                "content",
                |vm, c| {
                    let (h, n) = this_node(vm, &c.this, C::HTMLTemplateElement)?;
                    let t = with_doc(vm, &h, n.doc, |d| d.template_of(n.id))?;
                    wrap_opt(vm, &h, t.map(|t| n.with(t)))
                },
                None,
            );
            // HTML §4.12.3: shadowRootMode reflects shadowrootmode, limited to known values.
            attr(
                vm,
                proto,
                "shadowRootMode",
                |vm, c| {
                    let (h, n) = this_node(vm, &c.this, C::HTMLTemplateElement)?;
                    let v = with_doc(vm, &h, n.doc, |d| {
                        d.attr(n.id, "shadowrootmode").map(str::to_ascii_lowercase)
                    })?;
                    Ok(js(match v.as_deref() {
                        Some("open") => "open",
                        Some("closed") => "closed",
                        _ => "",
                    }))
                },
                Some(|vm, c| {
                    let v = rstr(vm, &c.arg(0))?;
                    let (h, n) = this_node(vm, &c.this, C::HTMLTemplateElement)?;
                    crate::mutation::set_or_remove(vm, &h, n, "shadowrootmode", Some(&v))?;
                    Ok(Value::Undefined)
                }),
            );
            attr(
                vm,
                proto,
                "shadowRootSlotAssignment",
                |vm, c| {
                    let (h, n) = this_node(vm, &c.this, C::HTMLTemplateElement)?;
                    let manual = with_doc(vm, &h, n.doc, |d| {
                        d.attr(n.id, "shadowrootslotassignment")
                            .is_some_and(|v| v.eq_ignore_ascii_case("manual"))
                    })?;
                    Ok(js(if manual { "manual" } else { "named" }))
                },
                Some(|vm, c| {
                    let v = rstr(vm, &c.arg(0))?;
                    let (h, n) = this_node(vm, &c.this, C::HTMLTemplateElement)?;
                    crate::mutation::set_or_remove(vm, &h, n, "shadowrootslotassignment", Some(&v))?;
                    Ok(Value::Undefined)
                }),
            );
            reflect(vm, proto, "shadowRootDelegatesFocus", "shadowrootdelegatesfocus", Bool);
            reflect(vm, proto, "shadowRootClonable", "shadowrootclonable", Bool);
            reflect(vm, proto, "shadowRootSerializable", "shadowrootserializable", Bool);
        }
        C::HTMLHtmlElement => reflect(vm, proto, "version", "version", Str),
        C::HTMLBodyElement => s(vm, &["text", "link", "vLink", "aLink", "bgColor", "background"]),
        C::HTMLHeadingElement
        | C::HTMLParagraphElement
        | C::HTMLDivElement
        | C::HTMLTableCaptionElement
        | C::HTMLLegendElement => reflect(vm, proto, "align", "align", Str),
        C::HTMLHRElement => s(vm, &["align", "color", "size", "width"]),
        C::HTMLPreElement => reflect(vm, proto, "width", "width", Long(0)),
        C::HTMLBRElement => reflect(vm, proto, "clear", "clear", Str),
        C::HTMLFontElement => s(vm, &["color", "face", "size"]),
        C::HTMLParamElement => s(vm, &["name", "value", "type", "valueType"]),
        C::HTMLMapElement => reflect(vm, proto, "name", "name", Str),
        C::HTMLFrameSetElement => s(vm, &["cols", "rows"]),
        _ => {}
    }
}

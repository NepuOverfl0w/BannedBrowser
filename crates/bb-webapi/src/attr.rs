//! Attr (DOM §4.9.2) and NamedNodeMap (DOM §4.9.1).
//!
//! An Attr's payload indexes `Host::attrs`. While attached it reads and writes its
//! element's attribute (found by namespace and local name); `Host::attr_ids` keeps one Attr
//! per attached attribute, so `getAttributeNode` returns the same object each time.

use std::rc::Rc;

use bb_dom::{Attribute, Namespace};
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::{JsResult, Value, Vm};

use crate::class::{self, C};
use crate::collections::CollKind;
use crate::exception::throw;
use crate::host::*;
use crate::idl::{attr, op};

fn key(n: NodeRef, ns: Namespace, local: &str) -> (u64, String, String) {
    (n.pack(), ns.url().to_string(), local.to_string())
}

pub fn is_attr(vm: &Vm, v: &Value) -> bool {
    vm.host_data(v).is_some_and(|(c, _)| class::implements(c, C::Attr))
}

/// The record of an Attr object.
pub fn attr_of(vm: &mut Vm, v: &Value) -> Option<AttrRec> {
    let (c, id) = vm.host_data(v)?;
    if !class::implements(c, C::Attr) {
        return None;
    }
    let h = vm.host::<Host>()?;
    let r = h.attrs.borrow().get(id as usize).cloned();
    r
}

fn attr_id(vm: &mut Vm, v: &Value) -> JsResult<u64> {
    match vm.host_data(v) {
        Some((c, id)) if class::implements(c, C::Attr) => Ok(id),
        _ => vm.type_error("not an Attr"),
    }
}

pub fn qualified(a: &AttrRec) -> String {
    match &a.prefix {
        Some(p) => format!("{p}:{}", a.local),
        None => a.local.clone(),
    }
}

/// The current value of an Attr.
pub fn value(vm: &mut Vm, a: &AttrRec) -> JsResult<String> {
    let Some(o) = a.owner else { return Ok(a.value.clone()) };
    let h = host(vm)?;
    with_doc(vm, &h, o.doc, |d| {
        d.element(o.id)
            .and_then(|e| e.attrs.iter().find(|x| x.ns == a.ns && x.name == a.local))
            .map(|x| x.value.clone())
            .unwrap_or_default()
    })
}

pub fn set_value(vm: &mut Vm, this: &Value, v: &str) -> JsResult<()> {
    let id = attr_id(vm, this)?;
    let h = host(vm)?;
    let rec = h.attrs.borrow().get(id as usize).cloned();
    let Some(a) = rec else { return Ok(()) };
    match a.owner {
        Some(o) => {
            with_doc_mut(vm, &h, o.doc, |d| {
                crate::dom::set_attr_raw(d, o.id, a.ns, a.prefix.clone(), &a.local, v)
            })?;
        }
        None => {
            if let Some(r) = h.attrs.borrow_mut().get_mut(id as usize) {
                r.value = v.to_string();
            }
        }
    }
    Ok(())
}

/// The Attr object for an element's attribute (one per attribute while attached).
pub fn attr_node(vm: &mut Vm, h: &Host, n: NodeRef, a: &Attribute) -> JsResult<Value> {
    let k = key(n, a.ns, &a.name);
    let existing = h.attr_ids.borrow().get(&k).copied();
    let id = match existing {
        Some(id) => id,
        None => {
            let mut attrs = h.attrs.borrow_mut();
            attrs.push(AttrRec {
                owner: Some(n),
                ns: a.ns,
                prefix: a.prefix.clone(),
                local: a.name.clone(),
                value: String::new(),
                doc: n.doc,
            });
            let id = attrs.len() as u64 - 1;
            h.attr_ids.borrow_mut().insert(k, id);
            id
        }
    };
    Ok(Value::Object(attr_object(vm, h, id)))
}

fn attr_object(vm: &mut Vm, h: &Host, id: u64) -> ObjRef {
    // Attr objects are cached like node wrappers (keyed apart from nodes by the top bit).
    let wk = (1u64 << 63) | id;
    if let Some(o) = h.wrappers.borrow().get(&wk) {
        return *o;
    }
    let o = vm.new_host_object(Some(h.proto(C::Attr)), C::Attr as u32, id, false);
    h.wrappers.borrow_mut().insert(wk, o);
    o
}

/// After an element loses attribute `a`: its Attr (if any) keeps the value, detached.
pub fn detach_attr(h: &Host, n: NodeRef, a: &Attribute) {
    let k = key(n, a.ns, &a.name);
    if let Some(id) = h.attr_ids.borrow_mut().remove(&k) {
        if let Some(r) = h.attrs.borrow_mut().get_mut(id as usize) {
            r.owner = None;
            r.value = a.value.clone();
        }
    }
}

pub fn new_detached(
    vm: &mut Vm,
    h: &Host,
    doc: u32,
    ns: Namespace,
    prefix: Option<String>,
    local: &str,
) -> JsResult<Value> {
    let id = {
        let mut attrs = h.attrs.borrow_mut();
        attrs.push(AttrRec { owner: None, ns, prefix, local: local.to_string(), value: String::new(), doc });
        attrs.len() as u64 - 1
    };
    Ok(Value::Object(attr_object(vm, h, id)))
}

pub fn clone_attr(vm: &mut Vm, this: &Value) -> JsResult<Value> {
    let Some(a) = attr_of(vm, this) else { return vm.type_error("not an Attr") };
    let v = value(vm, &a)?;
    let h = host(vm)?;
    let o = new_detached(vm, &h, a.doc, a.ns, a.prefix.clone(), &a.local)?;
    set_value(vm, &o, &v)?;
    Ok(o)
}

pub fn attrs_equal(vm: &mut Vm, a: &Value, b: &Value) -> bool {
    let (Some(x), Some(y)) = (attr_of(vm, a), attr_of(vm, b)) else { return false };
    let (vx, vy) = (value(vm, &x).unwrap_or_default(), value(vm, &y).unwrap_or_default());
    x.ns == y.ns && x.local == y.local && vx == vy
}

pub fn adopt_attr(vm: &mut Vm, v: &Value, doc: u32) -> JsResult<()> {
    let id = attr_id(vm, v)?;
    let h = host(vm)?;
    let a = h.attrs.borrow().get(id as usize).cloned();
    if let Some(a) = a {
        if let Some(o) = a.owner {
            // Removing it from its element first (DOM §4.5 "adopt" step 3).
            let removed = with_doc_mut(vm, &h, o.doc, |d| {
                let e = d.element_mut(o.id)?;
                let i = e.attrs.iter().position(|x| x.ns == a.ns && x.name == a.local)?;
                Some(e.attrs.remove(i))
            })?;
            if let Some(r) = removed {
                detach_attr(&h, o, &r);
            }
        }
        if let Some(r) = h.attrs.borrow_mut().get_mut(id as usize) {
            r.doc = doc;
        }
    }
    Ok(())
}

/// Node.compareDocumentPosition with Attrs involved (DOM §4.4 steps 3–6, simplified to
/// the element order).
pub fn compare_position(vm: &mut Vm, this: &Value, other: &Value) -> JsResult<Value> {
    let same = matches!((this, other), (Value::Object(a), Value::Object(b)) if a == b);
    if same {
        return Ok(Value::Number(0.0));
    }
    let owner = |vm: &mut Vm, v: &Value| -> JsResult<Option<NodeRef>> {
        Ok(match attr_of(vm, v) {
            Some(a) => a.owner,
            None => Some(arg_node(vm, v, "the argument")?),
        })
    };
    let (a, b) = (owner(vm, this)?, owner(vm, other)?);
    match (a, b) {
        (Some(x), Some(y)) if x == y => {
            // Two attributes of one element: implementation-specific, consistent order.
            Ok(Value::Number((32 | 2) as f64))
        }
        _ => Ok(Value::Number((1 | 32 | 4) as f64)),
    }
}

// ---- Attr interface ----

fn this_attr(vm: &mut Vm, c: &NativeCall) -> JsResult<AttrRec> {
    match attr_of(vm, &c.this) {
        Some(a) => Ok(a),
        None => vm.type_error("Illegal invocation: not an Attr"),
    }
}

pub fn setup_attr(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "namespaceURI",
        |vm, c| {
            let a = this_attr(vm, &c)?;
            Ok(if a.ns == Namespace::None { Value::Null } else { js(a.ns.url()) })
        },
        None,
    );
    attr(
        vm,
        proto,
        "prefix",
        |vm, c| {
            let a = this_attr(vm, &c)?;
            Ok(a.prefix.map_or(Value::Null, |p| js(&p)))
        },
        None,
    );
    attr(vm, proto, "localName", |vm, c| Ok(js(&this_attr(vm, &c)?.local)), None);
    attr(vm, proto, "name", |vm, c| Ok(js(&qualified(&this_attr(vm, &c)?))), None);
    attr(
        vm,
        proto,
        "value",
        |vm, c| {
            let a = this_attr(vm, &c)?;
            Ok(js(&value(vm, &a)?))
        },
        Some(|vm, c| {
            let v = rstr(vm, &c.arg(0))?;
            set_value(vm, &c.this, &v)?;
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "ownerElement",
        |vm, c| {
            let a = this_attr(vm, &c)?;
            let h = host(vm)?;
            wrap_opt(vm, &h, a.owner)
        },
        None,
    );
    attr(
        vm,
        proto,
        "specified",
        |vm, c| {
            this_attr(vm, &c)?;
            Ok(Value::Bool(true))
        },
        None,
    );
}

/// Element's Attr-node methods (DOM §4.9).
pub fn setup_element_attr_nodes(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "getAttributeNode", 1, |vm, c| {
        let q = rstr(vm, &c.arg(0))?;
        let (h, n) = this_node(vm, &c.this, C::Element)?;
        let a = with_doc(vm, &h, n.doc, |d| {
            crate::dom::attr_index_by_name(&h, n, d, &q)
                .and_then(|i| d.element(n.id).map(|e| e.attrs[i].clone()))
        })?;
        match a {
            Some(a) => attr_node(vm, &h, n, &a),
            None => Ok(Value::Null),
        }
    });
    op(vm, proto, "getAttributeNodeNS", 2, |vm, c| {
        let ns = rstr_opt(vm, &c.arg(0), false)?.unwrap_or_default();
        let local = rstr(vm, &c.arg(1))?;
        let (h, n) = this_node(vm, &c.this, C::Element)?;
        let ns = Namespace::from_url(&ns);
        let a = with_doc(vm, &h, n.doc, |d| {
            d.element(n.id).and_then(|e| e.attrs.iter().find(|a| a.ns == ns && a.name == local).cloned())
        })?;
        match a {
            Some(a) => attr_node(vm, &h, n, &a),
            None => Ok(Value::Null),
        }
    });
    for name in ["setAttributeNode", "setAttributeNodeNS"] {
        op(vm, proto, name, 1, |vm, c| {
            let (h, n) = this_node(vm, &c.this, C::Element)?;
            set_attribute_node(vm, &h, n, &c.arg(0))
        });
    }
    op(vm, proto, "removeAttributeNode", 1, |vm, c| {
        let (h, n) = this_node(vm, &c.this, C::Element)?;
        let Some(a) = attr_of(vm, &c.arg(0)) else { return vm.type_error("parameter 1 is not an Attr") };
        if a.owner != Some(n) {
            return throw(vm, "NotFoundError", "the attribute is not this element's");
        }
        let removed = with_doc_mut(vm, &h, n.doc, |d| {
            let e = d.element_mut(n.id)?;
            let i = e.attrs.iter().position(|x| x.ns == a.ns && x.name == a.local)?;
            Some(e.attrs.remove(i))
        })?;
        if let Some(r) = removed {
            detach_attr(&h, n, &r);
        }
        Ok(c.arg(0))
    });
}

/// DOM §4.9 "set an attribute": returns the Attr it replaced (or null).
fn set_attribute_node(vm: &mut Vm, h: &Rc<Host>, n: NodeRef, v: &Value) -> JsResult<Value> {
    let id = attr_id(vm, v)?;
    let Some(a) = h.attrs.borrow().get(id as usize).cloned() else { return Ok(Value::Null) };
    if let Some(o) = a.owner {
        if o != n {
            return throw(vm, "InUseAttributeError", "the attribute belongs to another element");
        }
        return Ok(v.clone());
    }
    let old = with_doc(vm, h, n.doc, |d| {
        d.element(n.id).and_then(|e| e.attrs.iter().find(|x| x.ns == a.ns && x.name == a.local).cloned())
    })?;
    let old_obj = match &old {
        Some(o) => attr_node(vm, h, n, o)?,
        None => Value::Null,
    };
    if let Some(o) = &old {
        detach_attr(h, n, o);
    }
    with_doc_mut(vm, h, n.doc, |d| {
        crate::dom::set_attr_raw(d, n.id, a.ns, a.prefix.clone(), &a.local, &a.value)
    })?;
    if let Some(r) = h.attrs.borrow_mut().get_mut(id as usize) {
        r.owner = Some(n);
        r.doc = n.doc;
    }
    h.attr_ids.borrow_mut().insert(key(n, a.ns, &a.local), id);
    Ok(old_obj)
}

// ---- NamedNodeMap ----

fn map_element(kind: &CollKind) -> Option<NodeRef> {
    match kind {
        CollKind::Attributes(n) => Some(*n),
        _ => None,
    }
}

pub fn map_len(h: &Host, kind: &CollKind) -> usize {
    let Some(n) = map_element(kind) else { return 0 };
    let docs = h.docs.borrow();
    docs.get(n.doc as usize).and_then(|d| d.element(n.id)).map_or(0, |e| e.attrs.len())
}

fn attr_at(h: &Host, n: NodeRef, i: usize) -> Option<Attribute> {
    let docs = h.docs.borrow();
    docs.get(n.doc as usize).and_then(|d| d.element(n.id)).and_then(|e| e.attrs.get(i).cloned())
}

pub fn map_item(vm: &mut Vm, h: &Host, kind: &CollKind, i: usize) -> JsResult<Option<Value>> {
    let Some(n) = map_element(kind) else { return Ok(None) };
    match attr_at(h, n, i) {
        Some(a) => Ok(Some(attr_node(vm, h, n, &a)?)),
        None => Ok(None),
    }
}

pub fn map_names(vm: &mut Vm, h: &Host, kind: &CollKind) -> JsResult<Vec<(String, Value)>> {
    let Some(n) = map_element(kind) else { return Ok(Vec::new()) };
    let (attrs, html) = {
        let docs = h.docs.borrow();
        let e = docs.get(n.doc as usize).and_then(|d| d.element(n.id)).cloned();
        let html = e.as_ref().is_some_and(|e| e.ns == Namespace::Html) && crate::dom::is_html_doc(h, n.doc);
        (e.map(|e| e.attrs).unwrap_or_default(), html)
    };
    let mut out: Vec<(String, Value)> = Vec::new();
    for a in attrs {
        let q = match &a.prefix {
            Some(p) => format!("{p}:{}", a.name),
            None => a.name.clone(),
        };
        // HTML elements in HTML documents skip names with uppercase letters (DOM §4.9.1).
        if html && q.chars().any(|c| c.is_ascii_uppercase()) {
            continue;
        }
        if out.iter().any(|x| x.0 == q) {
            continue;
        }
        let v = attr_node(vm, h, n, &a)?;
        out.push((q, v));
    }
    Ok(out)
}

fn map_this(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, NodeRef)> {
    let (_, id) = this_of(vm, &c.this, C::NamedNodeMap)?;
    let h = host(vm)?;
    let k = h.collections.borrow().get(id as usize).cloned();
    match k.as_ref().and_then(map_element) {
        Some(n) => Ok((h, n)),
        None => vm.type_error("Illegal invocation"),
    }
}

pub fn setup_named_node_map(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "length",
        |vm, c| {
            let (h, n) = map_this(vm, &c)?;
            Ok(Value::Number(map_len(&h, &CollKind::Attributes(n)) as f64))
        },
        None,
    );
    op(vm, proto, "item", 1, |vm, c| {
        let i = to_ulong(vm, &c.arg(0))?;
        let (h, n) = map_this(vm, &c)?;
        Ok(map_item(vm, &h, &CollKind::Attributes(n), i as usize)?.unwrap_or(Value::Null))
    });
    op(vm, proto, "getNamedItem", 1, |vm, c| {
        let q = rstr(vm, &c.arg(0))?;
        let (h, n) = map_this(vm, &c)?;
        let a = with_doc(vm, &h, n.doc, |d| {
            crate::dom::attr_index_by_name(&h, n, d, &q)
                .and_then(|i| d.element(n.id).map(|e| e.attrs[i].clone()))
        })?;
        match a {
            Some(a) => attr_node(vm, &h, n, &a),
            None => Ok(Value::Null),
        }
    });
    op(vm, proto, "getNamedItemNS", 2, |vm, c| {
        let ns = rstr_opt(vm, &c.arg(0), false)?.unwrap_or_default();
        let local = rstr(vm, &c.arg(1))?;
        let (h, n) = map_this(vm, &c)?;
        let ns = Namespace::from_url(&ns);
        let a = with_doc(vm, &h, n.doc, |d| {
            d.element(n.id).and_then(|e| e.attrs.iter().find(|a| a.ns == ns && a.name == local).cloned())
        })?;
        match a {
            Some(a) => attr_node(vm, &h, n, &a),
            None => Ok(Value::Null),
        }
    });
    for name in ["setNamedItem", "setNamedItemNS"] {
        op(vm, proto, name, 1, |vm, c| {
            let (h, n) = map_this(vm, &c)?;
            set_attribute_node(vm, &h, n, &c.arg(0))
        });
    }
    op(vm, proto, "removeNamedItem", 1, |vm, c| {
        let q = rstr(vm, &c.arg(0))?;
        let (h, n) = map_this(vm, &c)?;
        let a = with_doc(vm, &h, n.doc, |d| {
            crate::dom::attr_index_by_name(&h, n, d, &q)
                .and_then(|i| d.element(n.id).map(|e| e.attrs[i].clone()))
        })?;
        let Some(a) = a else { return throw(vm, "NotFoundError", "no such attribute") };
        let obj = attr_node(vm, &h, n, &a)?;
        with_doc_mut(vm, &h, n.doc, |d| {
            if let Some(e) = d.element_mut(n.id) {
                e.attrs.retain(|x| !(x.ns == a.ns && x.name == a.name));
            }
            d.mark_style_dirty(n.id);
        })?;
        detach_attr(&h, n, &a);
        Ok(obj)
    });
    op(vm, proto, "removeNamedItemNS", 2, |vm, c| {
        let ns = rstr_opt(vm, &c.arg(0), false)?.unwrap_or_default();
        let local = rstr(vm, &c.arg(1))?;
        let (h, n) = map_this(vm, &c)?;
        let ns = Namespace::from_url(&ns);
        let a = with_doc(vm, &h, n.doc, |d| {
            d.element(n.id).and_then(|e| e.attrs.iter().find(|a| a.ns == ns && a.name == local).cloned())
        })?;
        let Some(a) = a else { return throw(vm, "NotFoundError", "no such attribute") };
        let obj = attr_node(vm, &h, n, &a)?;
        with_doc_mut(vm, &h, n.doc, |d| {
            if let Some(e) = d.element_mut(n.id) {
                e.attrs.retain(|x| !(x.ns == a.ns && x.name == a.name));
            }
            d.mark_style_dirty(n.id);
        })?;
        detach_attr(&h, n, &a);
        Ok(obj)
    });
}

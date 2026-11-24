//! Traversal (DOM §6): NodeIterator, TreeWalker and the NodeFilter callback interface.
//!
//! A traverser's state lives in `Host::traversers`; its filter is slot 0 of its object.

use std::rc::Rc;

use bb_dom::{Document, NodeId};
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::{realm, PropKey};
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::exception::throw;
use crate::host::{arg_node, host, this_of, with_doc, wrap, Host, NodeRef};
use crate::idl::{attr, op};

type R = JsResult<Value>;

const ACCEPT: u16 = 1;
const REJECT: u16 = 2;
const SKIP: u16 = 3;

/// A NodeIterator's or TreeWalker's state. For a walker `reference` is its current node.
#[derive(Clone, Copy, Debug)]
pub struct Traverser {
    pub root: NodeRef,
    pub reference: NodeRef,
    pub pointer_before: bool,
    pub what_to_show: u32,
    pub active: bool,
    pub walker: bool,
}

/// DOM §6.1 "NodeIterator pre-removing steps", for each iterator over `node`'s document.
pub fn pre_removing(h: &Host, d: &Document, node: NodeRef) {
    let Ok(mut ts) = h.traversers.try_borrow_mut() else { return };
    for t in ts.iter_mut().filter(|t| !t.walker && t.root.doc == node.doc) {
        // Step 1 (the removed node is an inclusive ancestor of the root: the iterator's
        // whole collection goes with it, as browsers and WPT have it).
        let incl_anc = |x: NodeId| node.id == x || d.ancestors(x).any(|a| a == node.id);
        if t.reference.doc != node.doc || !incl_anc(t.reference.id) || incl_anc(t.root.id) {
            continue;
        }
        // Step 2.
        if t.pointer_before {
            let mut next = None;
            let mut cur = node.id;
            loop {
                if cur == t.root.id {
                    break;
                }
                if let Some(s) = d.next_sibling(cur) {
                    next = Some(s);
                    break;
                }
                match d.parent(cur) {
                    Some(p) => cur = p,
                    None => break,
                }
            }
            // It must be an inclusive descendant of the root.
            let next = next.filter(|&n| n == t.root.id || d.ancestors(n).any(|a| a == t.root.id));
            if let Some(n) = next {
                t.reference = node.with(n);
                continue;
            }
            t.pointer_before = false;
        }
        // Step 3.
        t.reference = match d.prev_sibling(node.id) {
            None => match d.parent(node.id) {
                Some(p) => node.with(p),
                None => continue,
            },
            Some(mut s) => {
                while let Some(l) = d.last_child(s) {
                    s = l;
                }
                node.with(s)
            }
        };
    }
}

/// Nodes moved to other ids (adopted into another document's arena).
pub fn remap(h: &Host, pairs: &[(u64, u64)]) {
    let map: std::collections::HashMap<u64, u64> = pairs.iter().copied().collect();
    let Ok(mut ts) = h.traversers.try_borrow_mut() else { return };
    for t in ts.iter_mut() {
        for n in [&mut t.root, &mut t.reference] {
            if let Some(x) = map.get(&n.pack()) {
                *n = NodeRef::unpack(*x);
            }
        }
    }
}

fn this(vm: &mut Vm, c: &NativeCall, iface: C) -> JsResult<(Rc<Host>, usize, Traverser, ObjRef)> {
    let (_, id) = this_of(vm, &c.this, iface)?;
    let Value::Object(o) = c.this else { return vm.type_error("Illegal invocation") };
    let h = host(vm)?;
    let t = h.traversers.borrow().get(id as usize).copied();
    match t {
        Some(t) => Ok((h, id as usize, t, o)),
        None => vm.type_error("Illegal invocation: a stale traverser"),
    }
}

fn update(h: &Host, id: usize, f: impl FnOnce(&mut Traverser)) {
    if let Some(t) = h.traversers.borrow_mut().get_mut(id) {
        f(t);
    }
}

fn get(h: &Host, id: usize) -> Option<Traverser> {
    h.traversers.borrow().get(id).copied()
}

/// DOM §6 "filter" `node` with traverser `id` (whose object is `obj`).
fn filter(vm: &mut Vm, h: &Host, id: usize, obj: ObjRef, node: NodeRef) -> JsResult<u16> {
    let Some(t) = get(h, id) else { return Ok(REJECT) };
    // Step 1.
    if t.active {
        return throw(vm, "InvalidStateError", "the filter is already running");
    }
    // Steps 2–4.
    let ty = with_doc(vm, h, node.doc, |d| crate::dom::node_type_at(d, node.id))?;
    if t.what_to_show & (1 << (ty - 1)) == 0 {
        return Ok(SKIP);
    }
    let f = vm.host_obj(obj).and_then(|o| o.slots.first().cloned()).unwrap_or(Value::Null);
    if f.is_nullish() {
        return Ok(ACCEPT);
    }
    // Steps 5–8: WebIDL "call a user object's operation" acceptNode.
    update(h, id, |t| t.active = true);
    let r = (|| {
        let arg = wrap(vm, h, node)?;
        let v = if vm.is_callable(&f) {
            vm.call(&f, Value::Undefined, vec![arg])?
        } else {
            let Value::Object(fo) = &f else { return vm.type_error("the filter is not an object") };
            let m = vm.get(*fo, &PropKey::lit("acceptNode"))?;
            if !vm.is_callable(&m) {
                return vm.type_error("the filter's acceptNode is not a function");
            }
            vm.call(&m, f.clone(), vec![arg])?
        };
        Ok((vm.to_uint32(&v)? & 0xffff) as u16)
    })();
    update(h, id, |t| t.active = false);
    r
}

fn first_child(d: &Document, n: NodeRef) -> Option<NodeRef> {
    d.first_child(n.id).map(|x| n.with(x))
}
fn last_child(d: &Document, n: NodeRef) -> Option<NodeRef> {
    d.last_child(n.id).map(|x| n.with(x))
}
fn next_sibling(d: &Document, n: NodeRef) -> Option<NodeRef> {
    d.next_sibling(n.id).map(|x| n.with(x))
}
fn prev_sibling(d: &Document, n: NodeRef) -> Option<NodeRef> {
    d.prev_sibling(n.id).map(|x| n.with(x))
}
fn parent(d: &Document, n: NodeRef) -> Option<NodeRef> {
    d.parent(n.id).map(|x| n.with(x))
}

/// The node before `n` in tree order among `root`'s inclusive descendants.
fn preceding(d: &Document, n: NodeId, root: NodeId) -> Option<NodeId> {
    if n == root {
        return None;
    }
    match d.prev_sibling(n) {
        Some(mut s) => {
            while let Some(l) = d.last_child(s) {
                s = l;
            }
            Some(s)
        }
        None => d.parent(n),
    }
}

/// The filter argument: a NodeFilter callback interface value, or null.
fn filter_arg(vm: &mut Vm, v: &Value) -> JsResult<Value> {
    match v {
        Value::Undefined | Value::Null => Ok(Value::Null),
        Value::Object(_) => Ok(v.clone()),
        _ => vm.type_error("the filter must be an object"),
    }
}

fn create(vm: &mut Vm, c: &NativeCall, walker: bool) -> R {
    let (h, _) = crate::host::this_node(vm, &c.this, C::Document)?;
    let root = arg_node(vm, &c.arg(0), "the root")?;
    let what = match c.arg(1) {
        Value::Undefined => 0xffff_ffff,
        v => vm.to_uint32(&v)?,
    };
    let f = filter_arg(vm, &c.arg(2))?;
    let t =
        Traverser { root, reference: root, pointer_before: true, what_to_show: what, active: false, walker };
    let id = {
        let mut ts = h.traversers.borrow_mut();
        ts.push(t);
        ts.len() as u64 - 1
    };
    let class = if walker { C::TreeWalker } else { C::NodeIterator };
    let o = vm.new_host_object(Some(h.proto(class)), class as u32, id, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = vec![f];
    }
    Ok(Value::Object(o))
}

/// Document.createNodeIterator and createTreeWalker (DOM §4.5).
pub fn setup_document(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "createNodeIterator", 1, |vm, c| create(vm, &c, false));
    op(vm, proto, "createTreeWalker", 1, |vm, c| create(vm, &c, true));
}

/// The common attributes: root, whatToShow, filter.
fn common(vm: &mut Vm, proto: ObjRef, iface: C) {
    let root: bb_js::vm::heap::NativeFn = if iface == C::TreeWalker {
        |vm, c| {
            let (h, _, t, _) = this(vm, &c, C::TreeWalker)?;
            wrap(vm, &h, t.root)
        }
    } else {
        |vm, c| {
            let (h, _, t, _) = this(vm, &c, C::NodeIterator)?;
            wrap(vm, &h, t.root)
        }
    };
    attr(vm, proto, "root", root, None);
    let what: bb_js::vm::heap::NativeFn = if iface == C::TreeWalker {
        |vm, c| Ok(Value::Number(this(vm, &c, C::TreeWalker)?.2.what_to_show as f64))
    } else {
        |vm, c| Ok(Value::Number(this(vm, &c, C::NodeIterator)?.2.what_to_show as f64))
    };
    attr(vm, proto, "whatToShow", what, None);
    let filter: bb_js::vm::heap::NativeFn = if iface == C::TreeWalker {
        |vm, c| {
            let (_, _, _, o) = this(vm, &c, C::TreeWalker)?;
            Ok(vm.host_obj(o).and_then(|o| o.slots.first().cloned()).unwrap_or(Value::Null))
        }
    } else {
        |vm, c| {
            let (_, _, _, o) = this(vm, &c, C::NodeIterator)?;
            Ok(vm.host_obj(o).and_then(|o| o.slots.first().cloned()).unwrap_or(Value::Null))
        }
    };
    attr(vm, proto, "filter", filter, None);
}

// ---- NodeIterator (DOM §6.1) ----

/// DOM §6.1 "traverse" (next or previous).
fn traverse(vm: &mut Vm, c: &NativeCall, next: bool) -> R {
    let (h, id, t, o) = this(vm, c, C::NodeIterator)?;
    let mut node = t.reference;
    let mut before = t.pointer_before;
    loop {
        if next {
            if !before {
                let n = with_doc(vm, &h, node.doc, |d| d.next_in_preorder(node.id, t.root.id))?;
                let Some(n) = n else { return Ok(Value::Null) };
                node = node.with(n);
            } else {
                before = false;
            }
        } else if before {
            let n = with_doc(vm, &h, node.doc, |d| preceding(d, node.id, t.root.id))?;
            let Some(n) = n else { return Ok(Value::Null) };
            node = node.with(n);
        } else {
            before = true;
        }
        if filter(vm, &h, id, o, node)? == ACCEPT {
            break;
        }
    }
    update(&h, id, |t| {
        t.reference = node;
        t.pointer_before = before;
    });
    wrap(vm, &h, node)
}

pub fn setup_node_iterator(vm: &mut Vm, proto: ObjRef) {
    common(vm, proto, C::NodeIterator);
    attr(
        vm,
        proto,
        "referenceNode",
        |vm, c| {
            let (h, _, t, _) = this(vm, &c, C::NodeIterator)?;
            wrap(vm, &h, t.reference)
        },
        None,
    );
    attr(
        vm,
        proto,
        "pointerBeforeReferenceNode",
        |vm, c| Ok(Value::Bool(this(vm, &c, C::NodeIterator)?.2.pointer_before)),
        None,
    );
    op(vm, proto, "nextNode", 0, |vm, c| traverse(vm, &c, true));
    op(vm, proto, "previousNode", 0, |vm, c| traverse(vm, &c, false));
    op(vm, proto, "detach", 0, |vm, c| {
        this(vm, &c, C::NodeIterator)?;
        Ok(Value::Undefined)
    });
}

// ---- TreeWalker (DOM §6.2) ----

fn accept_current(vm: &mut Vm, h: &Host, id: usize, node: NodeRef) -> R {
    update(h, id, |t| t.reference = node);
    wrap(vm, h, node)
}

/// DOM §6.2 "traverse children" (first or last).
fn traverse_children(vm: &mut Vm, c: &NativeCall, first: bool) -> R {
    let (h, id, t, o) = this(vm, c, C::TreeWalker)?;
    let cur = t.reference;
    let mut node =
        with_doc(vm, &h, cur.doc, |d| if first { first_child(d, cur) } else { last_child(d, cur) })?;
    while let Some(n) = node {
        let r = filter(vm, &h, id, o, n)?;
        if r == ACCEPT {
            return accept_current(vm, &h, id, n);
        }
        if r == SKIP {
            let child =
                with_doc(vm, &h, n.doc, |d| if first { first_child(d, n) } else { last_child(d, n) })?;
            if child.is_some() {
                node = child;
                continue;
            }
        }
        let mut m = Some(n);
        node = None;
        while let Some(x) = m {
            let sib =
                with_doc(vm, &h, x.doc, |d| if first { next_sibling(d, x) } else { prev_sibling(d, x) })?;
            if sib.is_some() {
                node = sib;
                break;
            }
            let p = with_doc(vm, &h, x.doc, |d| parent(d, x))?;
            match p {
                None => return Ok(Value::Null),
                Some(p) if p == t.root || p == cur => return Ok(Value::Null),
                Some(p) => m = Some(p),
            }
        }
    }
    Ok(Value::Null)
}

/// DOM §6.2 "traverse siblings" (next or previous).
fn traverse_siblings(vm: &mut Vm, c: &NativeCall, next: bool) -> R {
    let (h, id, t, o) = this(vm, c, C::TreeWalker)?;
    let mut node = t.reference;
    if node == t.root {
        return Ok(Value::Null);
    }
    loop {
        let mut sib =
            with_doc(vm, &h, node.doc, |d| if next { next_sibling(d, node) } else { prev_sibling(d, node) })?;
        while let Some(s) = sib {
            node = s;
            let r = filter(vm, &h, id, o, node)?;
            if r == ACCEPT {
                return accept_current(vm, &h, id, node);
            }
            sib = with_doc(
                vm,
                &h,
                node.doc,
                |d| if next { first_child(d, node) } else { last_child(d, node) },
            )?;
            if r == REJECT || sib.is_none() {
                sib = with_doc(vm, &h, node.doc, |d| {
                    if next {
                        next_sibling(d, node)
                    } else {
                        prev_sibling(d, node)
                    }
                })?;
            }
        }
        let p = with_doc(vm, &h, node.doc, |d| parent(d, node))?;
        match p {
            None => return Ok(Value::Null),
            Some(p) if p == t.root => return Ok(Value::Null),
            Some(p) => node = p,
        }
        if filter(vm, &h, id, o, node)? == ACCEPT {
            return Ok(Value::Null);
        }
    }
}

pub fn setup_tree_walker(vm: &mut Vm, proto: ObjRef) {
    common(vm, proto, C::TreeWalker);
    attr(
        vm,
        proto,
        "currentNode",
        |vm, c| {
            let (h, _, t, _) = this(vm, &c, C::TreeWalker)?;
            wrap(vm, &h, t.reference)
        },
        Some(|vm, c| {
            let (h, id, _, _) = this(vm, &c, C::TreeWalker)?;
            let n = arg_node(vm, &c.arg(0), "the current node")?;
            update(&h, id, |t| t.reference = n);
            Ok(Value::Undefined)
        }),
    );
    op(vm, proto, "parentNode", 0, |vm, c| {
        let (h, id, t, o) = this(vm, &c, C::TreeWalker)?;
        let mut node = Some(t.reference);
        while let Some(n) = node {
            if n == t.root {
                break;
            }
            node = with_doc(vm, &h, n.doc, |d| parent(d, n))?;
            if let Some(p) = node {
                if filter(vm, &h, id, o, p)? == ACCEPT {
                    return accept_current(vm, &h, id, p);
                }
            }
        }
        Ok(Value::Null)
    });
    op(vm, proto, "firstChild", 0, |vm, c| traverse_children(vm, &c, true));
    op(vm, proto, "lastChild", 0, |vm, c| traverse_children(vm, &c, false));
    op(vm, proto, "nextSibling", 0, |vm, c| traverse_siblings(vm, &c, true));
    op(vm, proto, "previousSibling", 0, |vm, c| traverse_siblings(vm, &c, false));
    op(vm, proto, "previousNode", 0, |vm, c| {
        let (h, id, t, o) = this(vm, &c, C::TreeWalker)?;
        let mut node = t.reference;
        while node != t.root {
            let mut sib = with_doc(vm, &h, node.doc, |d| prev_sibling(d, node))?;
            while let Some(s) = sib {
                node = s;
                let mut r = filter(vm, &h, id, o, node)?;
                loop {
                    if r == REJECT {
                        break;
                    }
                    let Some(l) = with_doc(vm, &h, node.doc, |d| last_child(d, node))? else { break };
                    node = l;
                    r = filter(vm, &h, id, o, node)?;
                }
                if r == ACCEPT {
                    return accept_current(vm, &h, id, node);
                }
                sib = with_doc(vm, &h, node.doc, |d| prev_sibling(d, node))?;
            }
            let p = with_doc(vm, &h, node.doc, |d| parent(d, node))?;
            let Some(p) = p.filter(|_| node != t.root) else { return Ok(Value::Null) };
            node = p;
            if filter(vm, &h, id, o, node)? == ACCEPT {
                return accept_current(vm, &h, id, node);
            }
        }
        Ok(Value::Null)
    });
    op(vm, proto, "nextNode", 0, |vm, c| {
        let (h, id, t, o) = this(vm, &c, C::TreeWalker)?;
        let mut node = t.reference;
        let mut r = ACCEPT;
        loop {
            while r != REJECT {
                let Some(ch) = with_doc(vm, &h, node.doc, |d| first_child(d, node))? else { break };
                node = ch;
                r = filter(vm, &h, id, o, node)?;
                if r == ACCEPT {
                    return accept_current(vm, &h, id, node);
                }
            }
            let mut temp = Some(node);
            let mut found = false;
            while let Some(x) = temp {
                if x == t.root {
                    return Ok(Value::Null);
                }
                if let Some(s) = with_doc(vm, &h, x.doc, |d| next_sibling(d, x))? {
                    node = s;
                    found = true;
                    break;
                }
                temp = with_doc(vm, &h, x.doc, |d| parent(d, x))?;
            }
            // The current node was outside the root's tree: nothing follows within it.
            if !found {
                return Ok(Value::Null);
            }
            r = filter(vm, &h, id, o, node)?;
            if r == ACCEPT {
                return accept_current(vm, &h, id, node);
            }
        }
    });
}

// ---- NodeFilter (DOM §6.3) ----

/// The NodeFilter "legacy callback interface object" (WebIDL): a function that throws,
/// carrying the constants.
pub fn install_node_filter(vm: &mut Vm) {
    let f = realm::native(vm, "NodeFilter", 0, |vm, _| vm.type_error("NodeFilter is not a constructor"));
    let consts: [(&str, f64); 16] = [
        ("FILTER_ACCEPT", 1.0),
        ("FILTER_REJECT", 2.0),
        ("FILTER_SKIP", 3.0),
        ("SHOW_ALL", 4294967295.0),
        ("SHOW_ELEMENT", 0x1 as f64),
        ("SHOW_ATTRIBUTE", 0x2 as f64),
        ("SHOW_TEXT", 0x4 as f64),
        ("SHOW_CDATA_SECTION", 0x8 as f64),
        ("SHOW_ENTITY_REFERENCE", 0x10 as f64),
        ("SHOW_ENTITY", 0x20 as f64),
        ("SHOW_PROCESSING_INSTRUCTION", 0x40 as f64),
        ("SHOW_COMMENT", 0x80 as f64),
        ("SHOW_DOCUMENT", 0x100 as f64),
        ("SHOW_DOCUMENT_TYPE", 0x200 as f64),
        ("SHOW_DOCUMENT_FRAGMENT", 0x400 as f64),
        ("SHOW_NOTATION", 0x800 as f64),
    ];
    for (k, v) in consts {
        crate::idl::constant(vm, f, k, Value::Number(v));
    }
    let g = vm.realm.global;
    realm::value(vm, g, "NodeFilter", Value::Object(f));
}

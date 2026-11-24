//! Ranges (DOM §5): AbstractRange, StaticRange and live Range objects, and the steps the
//! mutation algorithms run to keep live ranges' boundary points valid (DOM §4.2.3 insert
//! and remove, §4.10 replace data, §4.11 split a Text node, §4.4 normalize).

use std::cmp::Ordering;
use std::rc::Rc;

use bb_dom::{Document, NodeData, NodeId};
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::exception::throw;
use crate::host::{
    arg_node, dict_get, host, js, this_of, to_ulong, utf16_len, with_doc, wrap, Host, NodeRef,
};
use crate::idl::{attr, constant, op};

type R = JsResult<Value>;

/// A boundary point (DOM §5.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Bp {
    pub node: NodeRef,
    pub offset: u32,
}

/// A range's boundary points; `live` for Range objects (StaticRanges aren't updated).
#[derive(Clone, Copy, Debug)]
pub struct RangeData {
    pub start: Bp,
    pub end: Bp,
    pub live: bool,
}

// ---- tree helpers ----

/// DOM §4.2 node length.
pub fn node_len(d: &Document, id: NodeId) -> u32 {
    match &d.node(id).data {
        NodeData::Doctype { .. } => 0,
        NodeData::Text(t) | NodeData::Comment(t) | NodeData::ProcessingInstruction { data: t, .. } => {
            utf16_len(t) as u32
        }
        _ => d.children(id).count() as u32,
    }
}

/// DOM §4.2.1 index.
pub fn index(d: &Document, id: NodeId) -> u32 {
    let mut i = 0;
    let mut cur = d.prev_sibling(id);
    while let Some(p) = cur {
        i += 1;
        cur = d.prev_sibling(p);
    }
    i
}

pub fn root(d: &Document, id: NodeId) -> NodeId {
    d.ancestors(id).last().unwrap_or(id)
}

fn incl_ancestor(d: &Document, a: NodeId, b: NodeId) -> bool {
    a == b || d.ancestors(b).any(|x| x == a)
}

fn is_char_data(d: &Document, id: NodeId) -> bool {
    matches!(
        d.node(id).data,
        NodeData::Text(_) | NodeData::Comment(_) | NodeData::ProcessingInstruction { .. }
    )
}

fn same_root(d: &Document, a: NodeRef, b: NodeRef) -> bool {
    a.doc == b.doc && root(d, a.id) == root(d, b.id)
}

/// Whether `a` precedes `b` in tree order (same root, a != b).
fn precedes(d: &Document, a: NodeId, b: NodeId) -> bool {
    let chain = |x: NodeId| {
        let mut v: Vec<NodeId> = d.ancestors(x).collect();
        v.reverse();
        v.push(x);
        v
    };
    let (ca, cb) = (chain(a), chain(b));
    let mut i = 0;
    while i < ca.len() && i < cb.len() && ca[i] == cb[i] {
        i += 1;
    }
    if i == ca.len() {
        return true; // a is an ancestor of b
    }
    if i == cb.len() {
        return false;
    }
    index(d, ca[i]) < index(d, cb[i])
}

/// DOM §5.2 "position of a boundary point" a relative to b (same root).
pub fn position(d: &Document, a: Bp, b: Bp) -> Ordering {
    if a.node == b.node {
        return a.offset.cmp(&b.offset);
    }
    if precedes(d, b.node.id, a.node.id) {
        return position(d, b, a).reverse();
    }
    if incl_ancestor(d, a.node.id, b.node.id) {
        let mut child = b.node.id;
        while let Some(p) = d.parent(child) {
            if p == a.node.id {
                break;
            }
            child = p;
        }
        if index(d, child) < a.offset {
            return Ordering::Greater;
        }
    }
    Ordering::Less
}

impl RangeData {
    fn collapsed(&self) -> bool {
        self.start == self.end
    }

    /// DOM §5.5 contained.
    fn contains(&self, d: &Document, n: NodeRef) -> bool {
        same_root(d, n, self.start.node)
            && position(d, Bp { node: n, offset: 0 }, self.start) == Ordering::Greater
            && position(d, Bp { node: n, offset: node_len(d, n.id) }, self.end) == Ordering::Less
    }

    /// DOM §5.5 partially contained.
    fn partially_contains(&self, d: &Document, n: NodeRef) -> bool {
        n.doc == self.start.node.doc
            && (incl_ancestor(d, n.id, self.start.node.id) != incl_ancestor(d, n.id, self.end.node.id))
    }
}

// ---- keeping live ranges valid ----

fn each_bp(h: &Host, mut f: impl FnMut(&mut Bp)) {
    let Ok(mut ranges) = h.ranges.try_borrow_mut() else { return };
    for r in ranges.iter_mut().filter(|r| r.live) {
        f(&mut r.start);
        f(&mut r.end);
    }
}

/// DOM §4.10 "replace data" steps 8–11.
pub fn replaced_data(h: &Host, n: NodeRef, offset: u32, count: u32, len: u32) {
    each_bp(h, |bp| {
        if bp.node != n {
            return;
        }
        if bp.offset > offset && bp.offset <= offset + count {
            bp.offset = offset;
        } else if bp.offset > offset + count {
            bp.offset = bp.offset + len - count;
        }
    });
}

/// DOM §4.2.3 "insert" step 5: `count` nodes go in before the child at `child_index`.
pub fn inserting(h: &Host, parent: NodeRef, child_index: u32, count: u32) {
    each_bp(h, |bp| {
        if bp.node == parent && bp.offset > child_index {
            bp.offset += count;
        }
    });
}

/// DOM §4.2.3 "remove" steps 4–7 (before `node`, child `index` of `parent`, goes).
pub fn removing(h: &Host, d: &Document, node: NodeRef, parent: NodeRef, index: u32) {
    each_bp(h, |bp| {
        if bp.node.doc == node.doc && incl_ancestor(d, node.id, bp.node.id) {
            *bp = Bp { node: parent, offset: index };
        }
    });
    each_bp(h, |bp| {
        if bp.node == parent && bp.offset > index {
            bp.offset -= 1;
        }
    });
}

/// DOM §4.11 "split a Text node" steps 7.2–7.5.
pub fn split(h: &Host, node: NodeRef, new: NodeRef, offset: u32, parent: NodeRef, node_index: u32) {
    each_bp(h, |bp| {
        if bp.node == node && bp.offset > offset {
            bp.node = new;
            bp.offset -= offset;
        }
    });
    each_bp(h, |bp| {
        if bp.node == parent && bp.offset == node_index + 1 {
            bp.offset += 1;
        }
    });
}

/// DOM §4.4 normalize step 6.6: Text node `cur` (child `cur_index` of `parent`) merges into
/// `node`, whose data was `len` long before it.
pub fn merging(h: &Host, node: NodeRef, cur: NodeRef, parent: NodeRef, cur_index: u32, len: u32) {
    each_bp(h, |bp| {
        if bp.node == cur {
            bp.node = node;
            bp.offset += len;
        }
    });
    each_bp(h, |bp| {
        if bp.node == parent && bp.offset == cur_index {
            bp.node = node;
            bp.offset = len;
        }
    });
}

/// Nodes moved to other ids (adopted into another document's arena).
pub fn remap(h: &Host, pairs: &[(u64, u64)]) {
    let map: std::collections::HashMap<u64, u64> = pairs.iter().copied().collect();
    let Ok(mut ranges) = h.ranges.try_borrow_mut() else { return };
    for r in ranges.iter_mut() {
        for bp in [&mut r.start, &mut r.end] {
            if let Some(n) = map.get(&bp.node.pack()) {
                bp.node = NodeRef::unpack(*n);
            }
        }
    }
}

// ---- objects ----

pub fn new_range(vm: &mut Vm, h: &Host, data: RangeData, proto: Option<ObjRef>) -> R {
    let class = if data.live { C::Range } else { C::StaticRange };
    let id = {
        let mut rs = h.ranges.borrow_mut();
        rs.push(data);
        rs.len() as u64 - 1
    };
    let proto = proto.unwrap_or_else(|| h.proto(class));
    Ok(Value::Object(vm.new_host_object(Some(proto), class as u32, id, false)))
}

fn range_this(vm: &mut Vm, this: &Value, iface: C) -> JsResult<(Rc<Host>, usize, RangeData)> {
    let (_, id) = this_of(vm, this, iface)?;
    let h = host(vm)?;
    let r = h.ranges.borrow().get(id as usize).copied();
    match r {
        Some(r) => Ok((h, id as usize, r)),
        None => vm.type_error("Illegal invocation: a stale range"),
    }
}

pub fn store(h: &Host, id: usize, r: RangeData) {
    if let Some(x) = h.ranges.borrow_mut().get_mut(id) {
        *x = r;
    }
}

pub fn get(h: &Host, id: usize) -> Option<RangeData> {
    h.ranges.borrow().get(id).copied()
}

pub fn range_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("Range requires 'new'") };
    let h = host(vm)?;
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => Some(p),
        _ => None,
    };
    let doc = NodeRef { doc: 0, id: Document::ROOT };
    let bp = Bp { node: doc, offset: 0 };
    new_range(vm, &h, RangeData { start: bp, end: bp, live: true }, proto)
}

/// A container argument: a Node that is not an Attr (attribute nodes have no place in the
/// node arenas; they can't be boundary points here).
fn container(vm: &mut Vm, v: &Value) -> JsResult<NodeRef> {
    if crate::attr::is_attr(vm, v) {
        return throw(vm, "InvalidNodeTypeError", "an attribute can't be a boundary point here");
    }
    arg_node(vm, v, "the node")
}

pub fn static_range_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("StaticRange requires 'new'") };
    let init = c.arg(0);
    if !matches!(init, Value::Object(_)) {
        return vm.type_error("StaticRange needs a StaticRangeInit");
    }
    // Members in lexicographic order; all required.
    let mut vals = Vec::with_capacity(4);
    for k in ["endContainer", "endOffset", "startContainer", "startOffset"] {
        let v = dict_get(vm, &init, k)?;
        if matches!(v, Value::Undefined) {
            return vm.type_error(&format!("StaticRangeInit's {k} is required"));
        }
        vals.push(v);
    }
    let is_doctype_or_attr = |vm: &mut Vm, v: &Value| -> JsResult<bool> {
        if crate::attr::is_attr(vm, v) {
            return Ok(true);
        }
        let Some(n) = crate::host::node_of(vm, v) else { return vm.type_error("a container is not a Node") };
        let h = host(vm)?;
        with_doc(vm, &h, n.doc, |d| matches!(d.node(n.id).data, NodeData::Doctype { .. }))
    };
    let end_c = vals[0].clone();
    if crate::host::node_of(vm, &end_c).is_none() && !crate::attr::is_attr(vm, &end_c) {
        return vm.type_error("endContainer is not a Node");
    }
    let end_o = to_ulong(vm, &vals[1])?;
    let start_c = vals[2].clone();
    if crate::host::node_of(vm, &start_c).is_none() && !crate::attr::is_attr(vm, &start_c) {
        return vm.type_error("startContainer is not a Node");
    }
    let start_o = to_ulong(vm, &vals[3])?;
    if is_doctype_or_attr(vm, &start_c)? || is_doctype_or_attr(vm, &end_c)? {
        return throw(
            vm,
            "InvalidNodeTypeError",
            "a StaticRange can't have a doctype or attribute container",
        );
    }
    let (Some(s), Some(e)) = (crate::host::node_of(vm, &start_c), crate::host::node_of(vm, &end_c)) else {
        return vm.type_error("a container is not a Node");
    };
    let h = host(vm)?;
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => Some(p),
        _ => None,
    };
    let data =
        RangeData { start: Bp { node: s, offset: start_o }, end: Bp { node: e, offset: end_o }, live: false };
    new_range(vm, &h, data, proto)
}

pub fn setup_abstract_range(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "startContainer",
        |vm, c| {
            let (h, _, r) = range_this(vm, &c.this, C::AbstractRange)?;
            wrap(vm, &h, r.start.node)
        },
        None,
    );
    attr(
        vm,
        proto,
        "startOffset",
        |vm, c| {
            let (_, _, r) = range_this(vm, &c.this, C::AbstractRange)?;
            Ok(Value::Number(r.start.offset as f64))
        },
        None,
    );
    attr(
        vm,
        proto,
        "endContainer",
        |vm, c| {
            let (h, _, r) = range_this(vm, &c.this, C::AbstractRange)?;
            wrap(vm, &h, r.end.node)
        },
        None,
    );
    attr(
        vm,
        proto,
        "endOffset",
        |vm, c| {
            let (_, _, r) = range_this(vm, &c.this, C::AbstractRange)?;
            Ok(Value::Number(r.end.offset as f64))
        },
        None,
    );
    attr(
        vm,
        proto,
        "collapsed",
        |vm, c| {
            let (_, _, r) = range_this(vm, &c.this, C::AbstractRange)?;
            Ok(Value::Bool(r.collapsed()))
        },
        None,
    );
}

/// DOM §5.5 "set the start or end".
fn set_boundary(vm: &mut Vm, c: &NativeCall, start: bool) -> R {
    let (h, id, mut r) = range_this(vm, &c.this, C::Range)?;
    let node = container(vm, &c.arg(0))?;
    let offset = to_ulong(vm, &c.arg(1))?;
    set_bp(vm, &h, id, &mut r, node, offset, start)?;
    Ok(Value::Undefined)
}

fn set_bp(
    vm: &mut Vm,
    h: &Host,
    id: usize,
    r: &mut RangeData,
    node: NodeRef,
    offset: u32,
    start: bool,
) -> JsResult<()> {
    let (doctype, len) = with_doc(vm, h, node.doc, |d| {
        (matches!(d.node(node.id).data, NodeData::Doctype { .. }), node_len(d, node.id))
    })?;
    if doctype {
        return throw(vm, "InvalidNodeTypeError", "a doctype can't be a boundary point");
    }
    if offset > len {
        return throw(vm, "IndexSizeError", "the offset is past the node's length");
    }
    let bp = Bp { node, offset };
    with_doc(vm, h, node.doc, |d| {
        if start {
            if !same_root(d, r.start.node, node) || position(d, bp, r.end) == Ordering::Greater {
                r.end = bp;
            }
            r.start = bp;
        } else {
            if !same_root(d, r.start.node, node) || position(d, bp, r.start) == Ordering::Less {
                r.start = bp;
            }
            r.end = bp;
        }
    })?;
    store(h, id, *r);
    Ok(())
}

/// setStartBefore & co.: a boundary point next to `node` in its parent.
fn set_beside(vm: &mut Vm, c: &NativeCall, start: bool, after: bool) -> R {
    let (h, id, mut r) = range_this(vm, &c.this, C::Range)?;
    let node = container(vm, &c.arg(0))?;
    let p = with_doc(vm, &h, node.doc, |d| d.parent(node.id).map(|p| (p, index(d, node.id))))?;
    let Some((p, i)) = p else { return throw(vm, "InvalidNodeTypeError", "the node has no parent") };
    set_bp(vm, &h, id, &mut r, node.with(p), i + after as u32, start)?;
    Ok(Value::Undefined)
}

/// DOM §5.5 "select" `node` within range `id`.
fn select(vm: &mut Vm, h: &Host, id: usize, node: NodeRef) -> JsResult<()> {
    let p = with_doc(vm, h, node.doc, |d| d.parent(node.id).map(|p| (p, index(d, node.id))))?;
    let Some((p, i)) = p else { return throw(vm, "InvalidNodeTypeError", "the node has no parent") };
    let Some(mut r) = get(h, id) else { return Ok(()) };
    r.start = Bp { node: node.with(p), offset: i };
    r.end = Bp { node: node.with(p), offset: i + 1 };
    store(h, id, r);
    Ok(())
}

fn char_data(d: &Document, id: NodeId) -> Vec<u16> {
    crate::dom::char_data(d, id).unwrap_or_default().encode_utf16().collect()
}

fn substring(d: &Document, id: NodeId, from: u32, to: u32) -> String {
    let s = char_data(d, id);
    let (a, b) = ((from as usize).min(s.len()), (to as usize).min(s.len()));
    String::from_utf16_lossy(&s[a..b.max(a)])
}

fn new_fragment(vm: &mut Vm, h: &Host, doc: u32) -> JsResult<NodeRef> {
    let id = crate::host::with_doc_mut(vm, h, doc, |d| d.create_fragment())?;
    Ok(NodeRef { doc, id })
}

/// Clone character data node `n` with `data` as its data.
fn clone_with_data(vm: &mut Vm, h: &Host, n: NodeRef, data: &str) -> JsResult<NodeRef> {
    let c = crate::dom::clone_node(vm, h, n, false)?;
    crate::host::with_doc_mut(vm, h, c.doc, |d| d.set_text(c.id, data))?;
    Ok(c)
}

/// What extracting/cloning works with (DOM §5.5 "extract", "clone the contents", steps 3–12).
struct Plan {
    common: NodeRef,
    first_partial: Option<NodeRef>,
    last_partial: Option<NodeRef>,
    contained: Vec<NodeRef>,
    new_bp: Bp,
}

fn plan(vm: &mut Vm, h: &Host, r: &RangeData) -> JsResult<Plan> {
    let (s, e) = (r.start, r.end);
    let p = with_doc(vm, h, s.node.doc, |d| {
        let mut common = s.node.id;
        while !incl_ancestor(d, common, e.node.id) {
            match d.parent(common) {
                Some(p) => common = p,
                None => break,
            }
        }
        let kids: Vec<NodeRef> = d.children(common).map(|k| s.node.with(k)).collect();
        let first_partial = if incl_ancestor(d, s.node.id, e.node.id) {
            None
        } else {
            kids.iter().copied().find(|&k| r.partially_contains(d, k))
        };
        let last_partial = if incl_ancestor(d, e.node.id, s.node.id) {
            None
        } else {
            kids.iter().rev().copied().find(|&k| r.partially_contains(d, k))
        };
        let contained: Vec<NodeRef> = kids.iter().copied().filter(|&k| r.contains(d, k)).collect();
        let doctype = contained.iter().any(|k| matches!(d.node(k.id).data, NodeData::Doctype { .. }));
        let new_bp = if incl_ancestor(d, s.node.id, e.node.id) {
            s
        } else {
            let mut reference = s.node.id;
            while let Some(p) = d.parent(reference) {
                if incl_ancestor(d, p, e.node.id) {
                    break;
                }
                reference = p;
            }
            match d.parent(reference) {
                Some(p) => Bp { node: s.node.with(p), offset: index(d, reference) + 1 },
                None => s,
            }
        };
        (Plan { common: s.node.with(common), first_partial, last_partial, contained, new_bp }, doctype)
    })?;
    if p.1 {
        return throw(vm, "HierarchyRequestError", "the range contains a doctype");
    }
    Ok(p.0)
}

/// DOM §5.5 "extract" the range `r` (and collapse range `id`, when given, to where it was).
fn extract(vm: &mut Vm, h: &Host, r: RangeData, id: Option<usize>) -> JsResult<NodeRef> {
    let (s, e) = (r.start, r.end);
    let frag = new_fragment(vm, h, s.node.doc)?;
    if r.collapsed() {
        return Ok(frag);
    }
    let same_cd = s.node == e.node && with_doc(vm, h, s.node.doc, |d| is_char_data(d, s.node.id))?;
    if same_cd {
        let data = with_doc(vm, h, s.node.doc, |d| substring(d, s.node.id, s.offset, e.offset))?;
        let clone = clone_with_data(vm, h, s.node, &data)?;
        crate::tree::append(vm, h, clone, frag)?;
        crate::mutation::replace_data(vm, h, s.node, s.offset, e.offset - s.offset, "")?;
        return Ok(frag);
    }
    let p = plan(vm, h, &r)?;
    let _ = p.common;
    if let Some(first) = p.first_partial {
        if with_doc(vm, h, first.doc, |d| is_char_data(d, first.id))? {
            let (data, len) = with_doc(vm, h, s.node.doc, |d| {
                let len = node_len(d, s.node.id);
                (substring(d, s.node.id, s.offset, len), len)
            })?;
            let clone = clone_with_data(vm, h, s.node, &data)?;
            crate::tree::append(vm, h, clone, frag)?;
            crate::mutation::replace_data(vm, h, s.node, s.offset, len - s.offset, "")?;
        } else {
            let clone = crate::dom::clone_node(vm, h, first, false)?;
            crate::tree::append(vm, h, clone, frag)?;
            let len = with_doc(vm, h, first.doc, |d| node_len(d, first.id))?;
            let sub = RangeData { start: s, end: Bp { node: first, offset: len }, live: false };
            let subfrag = extract(vm, h, sub, None)?;
            crate::tree::append(vm, h, subfrag, clone)?;
        }
    }
    for k in &p.contained {
        crate::tree::append(vm, h, *k, frag)?;
    }
    if let Some(last) = p.last_partial {
        if with_doc(vm, h, last.doc, |d| is_char_data(d, last.id))? {
            let data = with_doc(vm, h, e.node.doc, |d| substring(d, e.node.id, 0, e.offset))?;
            let clone = clone_with_data(vm, h, e.node, &data)?;
            crate::tree::append(vm, h, clone, frag)?;
            crate::mutation::replace_data(vm, h, e.node, 0, e.offset, "")?;
        } else {
            let clone = crate::dom::clone_node(vm, h, last, false)?;
            crate::tree::append(vm, h, clone, frag)?;
            let sub = RangeData { start: Bp { node: last, offset: 0 }, end: e, live: false };
            let subfrag = extract(vm, h, sub, None)?;
            crate::tree::append(vm, h, subfrag, clone)?;
        }
    }
    if let Some(id) = id {
        if let Some(mut cur) = get(h, id) {
            cur.start = p.new_bp;
            cur.end = p.new_bp;
            store(h, id, cur);
        }
    }
    Ok(frag)
}

/// DOM §5.5 "clone the contents" of the range `r`.
fn clone_contents(vm: &mut Vm, h: &Host, r: RangeData) -> JsResult<NodeRef> {
    let (s, e) = (r.start, r.end);
    let frag = new_fragment(vm, h, s.node.doc)?;
    if r.collapsed() {
        return Ok(frag);
    }
    let same_cd = s.node == e.node && with_doc(vm, h, s.node.doc, |d| is_char_data(d, s.node.id))?;
    if same_cd {
        let data = with_doc(vm, h, s.node.doc, |d| substring(d, s.node.id, s.offset, e.offset))?;
        let clone = clone_with_data(vm, h, s.node, &data)?;
        crate::tree::append(vm, h, clone, frag)?;
        return Ok(frag);
    }
    let p = plan(vm, h, &r)?;
    if let Some(first) = p.first_partial {
        if with_doc(vm, h, first.doc, |d| is_char_data(d, first.id))? {
            let data = with_doc(vm, h, s.node.doc, |d| {
                let len = node_len(d, s.node.id);
                substring(d, s.node.id, s.offset, len)
            })?;
            let clone = clone_with_data(vm, h, s.node, &data)?;
            crate::tree::append(vm, h, clone, frag)?;
        } else {
            let clone = crate::dom::clone_node(vm, h, first, false)?;
            crate::tree::append(vm, h, clone, frag)?;
            let len = with_doc(vm, h, first.doc, |d| node_len(d, first.id))?;
            let sub = RangeData { start: s, end: Bp { node: first, offset: len }, live: false };
            let subfrag = clone_contents(vm, h, sub)?;
            crate::tree::append(vm, h, subfrag, clone)?;
        }
    }
    for k in &p.contained {
        let clone = crate::dom::clone_node(vm, h, *k, true)?;
        crate::tree::append(vm, h, clone, frag)?;
    }
    if let Some(last) = p.last_partial {
        if with_doc(vm, h, last.doc, |d| is_char_data(d, last.id))? {
            let data = with_doc(vm, h, e.node.doc, |d| substring(d, e.node.id, 0, e.offset))?;
            let clone = clone_with_data(vm, h, e.node, &data)?;
            crate::tree::append(vm, h, clone, frag)?;
        } else {
            let clone = crate::dom::clone_node(vm, h, last, false)?;
            crate::tree::append(vm, h, clone, frag)?;
            let sub = RangeData { start: Bp { node: last, offset: 0 }, end: e, live: false };
            let subfrag = clone_contents(vm, h, sub)?;
            crate::tree::append(vm, h, subfrag, clone)?;
        }
    }
    Ok(frag)
}

/// DOM §5.5 "insert" `node` into range `id`.
fn insert(vm: &mut Vm, h: &Host, id: usize, node: NodeRef) -> JsResult<()> {
    let Some(r) = get(h, id) else { return Ok(()) };
    let s = r.start;
    let (bad, is_text) = with_doc(vm, h, s.node.doc, |d| {
        let data = &d.node(s.node.id).data;
        let is_text = matches!(data, NodeData::Text(_));
        let bad = matches!(data, NodeData::ProcessingInstruction { .. } | NodeData::Comment(_))
            || (is_text && d.parent(s.node.id).is_none());
        (bad, is_text)
    })?;
    if bad || s.node == node {
        return throw(vm, "HierarchyRequestError", "the node can't be inserted at the range's start");
    }
    // Steps 3–5.
    let mut reference: Option<NodeRef> = if is_text {
        Some(s.node)
    } else {
        with_doc(vm, h, s.node.doc, |d| d.children(s.node.id).nth(s.offset as usize))?.map(|k| s.node.with(k))
    };
    let parent = match reference {
        None => s.node,
        Some(rf) => match with_doc(vm, h, rf.doc, |d| d.parent(rf.id))? {
            Some(p) => rf.with(p),
            None => return throw(vm, "HierarchyRequestError", "the node can't be inserted here"),
        },
    };
    // Step 6.
    crate::tree::check_pre_insert(vm, h, node, parent, reference)?;
    // Step 7.
    if is_text {
        reference = Some(crate::dom::split_text(vm, h, s.node, s.offset)?);
    }
    // Step 8.
    if reference == Some(node) {
        reference = with_doc(vm, h, node.doc, |d| d.next_sibling(node.id))?.map(|x| node.with(x));
    }
    // Step 9.
    let has_parent = with_doc(vm, h, node.doc, |d| d.parent(node.id).is_some())?;
    if has_parent {
        crate::tree::remove(vm, h, node, false)?;
    }
    // Steps 10–11.
    let mut new_offset = match reference {
        None => with_doc(vm, h, parent.doc, |d| node_len(d, parent.id))?,
        Some(rf) => with_doc(vm, h, rf.doc, |d| index(d, rf.id))?,
    };
    new_offset += with_doc(vm, h, node.doc, |d| match d.node(node.id).data {
        NodeData::DocumentFragment => node_len(d, node.id),
        _ => 1,
    })?;
    // Step 12.
    crate::tree::pre_insert(vm, h, node, parent, reference)?;
    // Step 13.
    if let Some(mut cur) = get(h, id) {
        if cur.collapsed() {
            cur.end = Bp { node: parent, offset: new_offset };
            store(h, id, cur);
        }
    }
    Ok(())
}

fn node_arg_and_root(vm: &mut Vm, h: &Host, r: &RangeData, v: &Value) -> JsResult<(NodeRef, bool)> {
    let n = container(vm, v)?;
    let same = with_doc(vm, h, n.doc, |d| same_root(d, n, r.start.node))?;
    Ok((n, same))
}

/// Point checks for isPointInRange and comparePoint (steps 2–3).
fn check_point(vm: &mut Vm, h: &Host, n: NodeRef, offset: u32) -> JsResult<()> {
    let (doctype, len) = with_doc(vm, h, n.doc, |d| {
        (matches!(d.node(n.id).data, NodeData::Doctype { .. }), node_len(d, n.id))
    })?;
    if doctype {
        return throw(vm, "InvalidNodeTypeError", "a doctype can't be a boundary point");
    }
    if offset > len {
        return throw(vm, "IndexSizeError", "the offset is past the node's length");
    }
    Ok(())
}

pub fn setup_range(vm: &mut Vm, proto: ObjRef, ctor: ObjRef) {
    // Constants live on the interface object and the prototype (WebIDL §3.7.5).
    for (name, v) in
        [("START_TO_START", 0.0), ("START_TO_END", 1.0), ("END_TO_END", 2.0), ("END_TO_START", 3.0)]
    {
        constant(vm, proto, name, Value::Number(v));
        constant(vm, ctor, name, Value::Number(v));
    }
    attr(
        vm,
        proto,
        "commonAncestorContainer",
        |vm, c| {
            let (h, _, r) = range_this(vm, &c.this, C::Range)?;
            let n = with_doc(vm, &h, r.start.node.doc, |d| {
                let mut cur = r.start.node.id;
                while !incl_ancestor(d, cur, r.end.node.id) {
                    match d.parent(cur) {
                        Some(p) => cur = p,
                        None => break,
                    }
                }
                cur
            })?;
            wrap(vm, &h, r.start.node.with(n))
        },
        None,
    );
    op(vm, proto, "setStart", 2, |vm, c| set_boundary(vm, &c, true));
    op(vm, proto, "setEnd", 2, |vm, c| set_boundary(vm, &c, false));
    op(vm, proto, "setStartBefore", 1, |vm, c| set_beside(vm, &c, true, false));
    op(vm, proto, "setStartAfter", 1, |vm, c| set_beside(vm, &c, true, true));
    op(vm, proto, "setEndBefore", 1, |vm, c| set_beside(vm, &c, false, false));
    op(vm, proto, "setEndAfter", 1, |vm, c| set_beside(vm, &c, false, true));
    op(vm, proto, "collapse", 0, |vm, c| {
        let (h, id, mut r) = range_this(vm, &c.this, C::Range)?;
        if vm.to_boolean(&c.arg(0)) {
            r.end = r.start;
        } else {
            r.start = r.end;
        }
        store(&h, id, r);
        Ok(Value::Undefined)
    });
    op(vm, proto, "selectNode", 1, |vm, c| {
        let (h, id, _) = range_this(vm, &c.this, C::Range)?;
        let n = container(vm, &c.arg(0))?;
        select(vm, &h, id, n)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "selectNodeContents", 1, |vm, c| {
        let (h, id, mut r) = range_this(vm, &c.this, C::Range)?;
        let n = container(vm, &c.arg(0))?;
        let (doctype, len) = with_doc(vm, &h, n.doc, |d| {
            (matches!(d.node(n.id).data, NodeData::Doctype { .. }), node_len(d, n.id))
        })?;
        if doctype {
            return throw(vm, "InvalidNodeTypeError", "a doctype has no contents");
        }
        r.start = Bp { node: n, offset: 0 };
        r.end = Bp { node: n, offset: len };
        store(&h, id, r);
        Ok(Value::Undefined)
    });
    op(vm, proto, "compareBoundaryPoints", 2, |vm, c| {
        let how = vm.to_uint32(&c.arg(0))? & 0xffff;
        let (_, oid) = this_of(vm, &c.arg(1), C::Range)?;
        let (h, _, r) = range_this(vm, &c.this, C::Range)?;
        if how > 3 {
            return throw(vm, "NotSupportedError", "unknown comparison");
        }
        let Some(src) = get(&h, oid as usize) else { return vm.type_error("a stale range") };
        let same = with_doc(vm, &h, r.start.node.doc, |d| same_root(d, r.start.node, src.start.node))?;
        if !same {
            return throw(vm, "WrongDocumentError", "the ranges are in different trees");
        }
        let (a, b) = match how {
            0 => (r.start, src.start),
            1 => (r.end, src.start),
            2 => (r.end, src.end),
            _ => (r.start, src.end),
        };
        let o = with_doc(vm, &h, a.node.doc, |d| position(d, a, b))?;
        Ok(Value::Number(o as i8 as f64))
    });
    op(vm, proto, "deleteContents", 0, |vm, c| {
        let (h, id, r) = range_this(vm, &c.this, C::Range)?;
        delete_contents(vm, &h, id, r)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "extractContents", 0, |vm, c| {
        let (h, id, r) = range_this(vm, &c.this, C::Range)?;
        let f = extract(vm, &h, r, Some(id))?;
        wrap(vm, &h, f)
    });
    op(vm, proto, "cloneContents", 0, |vm, c| {
        let (h, _, r) = range_this(vm, &c.this, C::Range)?;
        let f = clone_contents(vm, &h, r)?;
        wrap(vm, &h, f)
    });
    op(vm, proto, "insertNode", 1, |vm, c| {
        let (h, id, _) = range_this(vm, &c.this, C::Range)?;
        let n = arg_node(vm, &c.arg(0), "the node")?;
        insert(vm, &h, id, n)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "surroundContents", 1, |vm, c| {
        let (h, id, r) = range_this(vm, &c.this, C::Range)?;
        let np = arg_node(vm, &c.arg(0), "the new parent")?;
        // Step 1: a partially contained non-Text node.
        let bad = with_doc(vm, &h, r.start.node.doc, |d| {
            let mut nodes: Vec<NodeId> =
                std::iter::once(r.start.node.id).chain(d.ancestors(r.start.node.id)).collect();
            nodes.extend(std::iter::once(r.end.node.id).chain(d.ancestors(r.end.node.id)));
            nodes.into_iter().any(|x| {
                !matches!(d.node(x).data, NodeData::Text(_)) && r.partially_contains(d, r.start.node.with(x))
            })
        })?;
        if bad {
            return throw(vm, "InvalidStateError", "the range partially contains a non-Text node");
        }
        let kind = with_doc(vm, &h, np.doc, |d| {
            matches!(
                d.node(np.id).data,
                NodeData::Document | NodeData::Doctype { .. } | NodeData::DocumentFragment
            )
        })?;
        if kind {
            return throw(vm, "InvalidNodeTypeError", "that node can't surround the range");
        }
        let frag = extract(vm, &h, r, Some(id))?;
        let has_kids = with_doc(vm, &h, np.doc, |d| d.first_child(np.id).is_some())?;
        if has_kids {
            crate::tree::replace_all(vm, &h, None, np)?;
        }
        insert(vm, &h, id, np)?;
        crate::tree::append(vm, &h, frag, np)?;
        select(vm, &h, id, np)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "cloneRange", 0, |vm, c| {
        let (h, _, r) = range_this(vm, &c.this, C::Range)?;
        new_range(vm, &h, r, None)
    });
    op(vm, proto, "detach", 0, |vm, c| {
        range_this(vm, &c.this, C::Range)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "isPointInRange", 2, |vm, c| {
        let (h, _, r) = range_this(vm, &c.this, C::Range)?;
        let (n, same) = node_arg_and_root(vm, &h, &r, &c.arg(0))?;
        let offset = to_ulong(vm, &c.arg(1))?;
        if !same {
            return Ok(Value::Bool(false));
        }
        check_point(vm, &h, n, offset)?;
        let bp = Bp { node: n, offset };
        let inside = with_doc(vm, &h, n.doc, |d| {
            position(d, bp, r.start) != Ordering::Less && position(d, bp, r.end) != Ordering::Greater
        })?;
        Ok(Value::Bool(inside))
    });
    op(vm, proto, "comparePoint", 2, |vm, c| {
        let (h, _, r) = range_this(vm, &c.this, C::Range)?;
        let (n, same) = node_arg_and_root(vm, &h, &r, &c.arg(0))?;
        let offset = to_ulong(vm, &c.arg(1))?;
        if !same {
            return throw(vm, "WrongDocumentError", "the node is in a different tree");
        }
        check_point(vm, &h, n, offset)?;
        let bp = Bp { node: n, offset };
        let o = with_doc(vm, &h, n.doc, |d| {
            if position(d, bp, r.start) == Ordering::Less {
                -1.0
            } else if position(d, bp, r.end) == Ordering::Greater {
                1.0
            } else {
                0.0
            }
        })?;
        Ok(Value::Number(o))
    });
    op(vm, proto, "intersectsNode", 1, |vm, c| {
        let (h, _, r) = range_this(vm, &c.this, C::Range)?;
        let (n, same) = node_arg_and_root(vm, &h, &r, &c.arg(0))?;
        if !same {
            return Ok(Value::Bool(false));
        }
        let b = with_doc(vm, &h, n.doc, |d| match d.parent(n.id) {
            None => true,
            Some(p) => {
                let i = index(d, n.id);
                let p = n.with(p);
                position(d, Bp { node: p, offset: i }, r.end) == Ordering::Less
                    && position(d, Bp { node: p, offset: i + 1 }, r.start) == Ordering::Greater
            }
        })?;
        Ok(Value::Bool(b))
    });
    op(vm, proto, "toString", 0, |vm, c| {
        let (h, _, r) = range_this(vm, &c.this, C::Range)?;
        let s = with_doc(vm, &h, r.start.node.doc, |d| stringify(d, &r))?;
        Ok(js(&s))
    });
    op(vm, proto, "createContextualFragment", 1, |vm, c| {
        let markup = crate::host::rstr(vm, &c.arg(0))?;
        let (h, _, r) = range_this(vm, &c.this, C::Range)?;
        let f = crate::html::create_contextual_fragment(vm, &h, r.start.node, &markup)?;
        wrap(vm, &h, f)
    });
}

/// DOM §5.5 deleteContents().
pub fn delete_contents(vm: &mut Vm, h: &Host, id: usize, r: RangeData) -> JsResult<()> {
    if r.collapsed() {
        return Ok(());
    }
    let (s, e) = (r.start, r.end);
    let same_cd = s.node == e.node && with_doc(vm, h, s.node.doc, |d| is_char_data(d, s.node.id))?;
    if same_cd {
        return crate::mutation::replace_data(vm, h, s.node, s.offset, e.offset - s.offset, "");
    }
    let (to_remove, new_bp, start_cd, end_cd) = with_doc(vm, h, s.node.doc, |d| {
        let root = root(d, s.node.id);
        let mut out: Vec<NodeRef> = Vec::new();
        for x in d.descendants(root) {
            let n = s.node.with(x);
            if r.contains(d, n) && !d.parent(x).is_some_and(|p| r.contains(d, s.node.with(p))) {
                out.push(n);
            }
        }
        let new_bp = if incl_ancestor(d, s.node.id, e.node.id) {
            s
        } else {
            let mut reference = s.node.id;
            while let Some(p) = d.parent(reference) {
                if incl_ancestor(d, p, e.node.id) {
                    break;
                }
                reference = p;
            }
            match d.parent(reference) {
                Some(p) => Bp { node: s.node.with(p), offset: index(d, reference) + 1 },
                None => s,
            }
        };
        (out, new_bp, is_char_data(d, s.node.id), is_char_data(d, e.node.id))
    })?;
    if start_cd {
        let len = with_doc(vm, h, s.node.doc, |d| node_len(d, s.node.id))?;
        crate::mutation::replace_data(vm, h, s.node, s.offset, len - s.offset, "")?;
    }
    for n in to_remove {
        crate::tree::remove(vm, h, n, false)?;
    }
    if end_cd {
        crate::mutation::replace_data(vm, h, e.node, 0, e.offset, "")?;
    }
    if let Some(mut cur) = get(h, id) {
        cur.start = new_bp;
        cur.end = new_bp;
        store(h, id, cur);
    }
    Ok(())
}

/// DOM §5.5 the stringification behavior.
pub fn stringify(d: &Document, r: &RangeData) -> String {
    let (s, e) = (r.start, r.end);
    let is_text = |id: NodeId| matches!(d.node(id).data, NodeData::Text(_));
    if s.node == e.node && is_text(s.node.id) {
        return substring(d, s.node.id, s.offset, e.offset);
    }
    let mut out = String::new();
    if is_text(s.node.id) {
        out.push_str(&substring(d, s.node.id, s.offset, node_len(d, s.node.id)));
    }
    for x in d.descendants(root(d, s.node.id)) {
        if is_text(x) && r.contains(d, s.node.with(x)) {
            out.push_str(d.node(x).as_text().unwrap_or(""));
        }
    }
    if is_text(e.node.id) {
        out.push_str(&substring(d, e.node.id, 0, e.offset));
    }
    out
}

/// Document.createRange() (DOM §4.5).
pub fn setup_document(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "createRange", 0, |vm, c| {
        let (h, n) = crate::host::this_node(vm, &c.this, C::Document)?;
        let bp = Bp { node: n, offset: 0 };
        new_range(vm, &h, RangeData { start: bp, end: bp, live: true }, None)
    });
}

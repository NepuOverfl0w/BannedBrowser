//! The Selection API (W3C Selection API): the window's document has one selection, which
//! holds at most one live Range object (the very object `addRange` got) and a direction.
//!
//! The browser's own selection (mouse and keyboard selecting text) isn't wired to it yet;
//! this is the script side.

use std::cmp::Ordering;

use bb_dom::{Document, NodeData};
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::exception::throw;
use crate::host::{arg_node, host, js, this_of, to_ulong, with_doc, wrap, Host, NodeRef};
use crate::idl::{attr, op};
use crate::range::{self, Bp, RangeData};

type R = JsResult<Value>;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Direction {
    #[default]
    None,
    Forwards,
    Backwards,
}

/// The window document's selection: its Range object and that object's range id.
#[derive(Default)]
pub struct State {
    pub object: Option<ObjRef>,
    pub range: Option<(ObjRef, usize)>,
    pub direction: Direction,
}

impl State {
    pub fn trace(&self, out: &mut Vec<ObjRef>) {
        out.extend(self.object);
        if let Some((o, _)) = self.range {
            out.push(o);
        }
    }
}

/// The Selection object of the window's document (created on first use).
pub fn selection_object(vm: &mut Vm, h: &Host) -> Value {
    if let Some(o) = h.selection.borrow().object {
        return Value::Object(o);
    }
    let o = vm.new_host_object(Some(h.proto(C::Selection)), C::Selection as u32, 0, false);
    h.selection.borrow_mut().object = Some(o);
    Value::Object(o)
}

fn current(h: &Host) -> Option<(ObjRef, usize, RangeData)> {
    let (o, id) = h.selection.borrow().range?;
    range::get(h, id).map(|r| (o, id, r))
}

/// Whether `n`'s root is the window's document (the selection's document).
fn in_document(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<bool> {
    if n.doc != 0 {
        return Ok(false);
    }
    with_doc(vm, h, 0, |d| range::root(d, n.id) == Document::ROOT)
}

/// Point checks: a doctype is never a boundary point, and the offset fits.
fn check_point(vm: &mut Vm, h: &Host, n: NodeRef, offset: u32) -> JsResult<()> {
    let (doctype, len) = with_doc(vm, h, n.doc, |d| {
        (matches!(d.node(n.id).data, NodeData::Doctype { .. }), range::node_len(d, n.id))
    })?;
    if doctype {
        return throw(vm, "InvalidNodeTypeError", "a doctype can't be a boundary point");
    }
    if offset > len {
        return throw(vm, "IndexSizeError", "the offset is past the node's length");
    }
    Ok(())
}

/// Make a new live range for the selection.
fn set_new_range(vm: &mut Vm, h: &Host, start: Bp, end: Bp, direction: Direction) -> JsResult<()> {
    let v = range::new_range(vm, h, RangeData { start, end, live: true }, None)?;
    let Value::Object(o) = v else { return Ok(()) };
    let id = vm.host_data(&v).map_or(0, |(_, id)| id as usize);
    let mut s = h.selection.borrow_mut();
    s.range = Some((o, id));
    s.direction = direction;
    Ok(())
}

/// Anchor and focus (forwards or directionless: anchor at the start).
fn anchor_focus(h: &Host) -> Option<(Bp, Bp)> {
    let (_, _, r) = current(h)?;
    Some(match h.selection.borrow().direction {
        Direction::Backwards => (r.end, r.start),
        _ => (r.start, r.end),
    })
}

fn this(vm: &mut Vm, c: &NativeCall) -> JsResult<std::rc::Rc<Host>> {
    this_of(vm, &c.this, C::Selection)?;
    host(vm)
}

/// `Document.getSelection()` and `Window.getSelection()`.
pub fn setup_document(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "getSelection", 0, |vm, c| {
        let (h, n) = crate::host::this_node(vm, &c.this, C::Document)?;
        // Only the window's document has a browsing context.
        if n.doc != 0 {
            return Ok(Value::Null);
        }
        Ok(selection_object(vm, &h))
    });
}

pub fn setup_selection(vm: &mut Vm, proto: ObjRef) {
    for (name, focus, offset) in [
        ("anchorNode", false, false),
        ("anchorOffset", false, true),
        ("focusNode", true, false),
        ("focusOffset", true, true),
    ] {
        let f: bb_js::vm::heap::NativeFn = match (focus, offset) {
            (false, false) => |vm, c| point_attr(vm, &c, false, false),
            (false, true) => |vm, c| point_attr(vm, &c, false, true),
            (true, false) => |vm, c| point_attr(vm, &c, true, false),
            (true, true) => |vm, c| point_attr(vm, &c, true, true),
        };
        attr(vm, proto, name, f, None);
    }
    attr(
        vm,
        proto,
        "isCollapsed",
        |vm, c| {
            let h = this(vm, &c)?;
            Ok(Value::Bool(current(&h).is_none_or(|(_, _, r)| r.start == r.end)))
        },
        None,
    );
    attr(
        vm,
        proto,
        "rangeCount",
        |vm, c| {
            let h = this(vm, &c)?;
            Ok(Value::Number(if current(&h).is_some() { 1.0 } else { 0.0 }))
        },
        None,
    );
    attr(
        vm,
        proto,
        "type",
        |vm, c| {
            let h = this(vm, &c)?;
            Ok(js(match current(&h) {
                None => "None",
                Some((_, _, r)) if r.start == r.end => "Caret",
                Some(_) => "Range",
            }))
        },
        None,
    );
    attr(
        vm,
        proto,
        "direction",
        |vm, c| {
            let h = this(vm, &c)?;
            let d = if current(&h).is_some() { h.selection.borrow().direction } else { Direction::None };
            Ok(js(match d {
                Direction::None => "none",
                Direction::Forwards => "forward",
                Direction::Backwards => "backward",
            }))
        },
        None,
    );
    op(vm, proto, "getRangeAt", 1, |vm, c| {
        let i = to_ulong(vm, &c.arg(0))?;
        let h = this(vm, &c)?;
        match current(&h) {
            Some((o, _, _)) if i == 0 => Ok(Value::Object(o)),
            _ => throw(vm, "IndexSizeError", "there is no range at that index"),
        }
    });
    op(vm, proto, "addRange", 1, |vm, c| {
        let (_, id) = this_of(vm, &c.arg(0), C::Range)?;
        let h = this(vm, &c)?;
        let Value::Object(o) = c.arg(0) else { return Ok(Value::Undefined) };
        let Some(r) = range::get(&h, id as usize) else { return Ok(Value::Undefined) };
        // Steps 1–2: a range in another tree, or a selection that already has one, is ignored.
        if !in_document(vm, &h, r.start.node)? || current(&h).is_some() {
            return Ok(Value::Undefined);
        }
        let mut s = h.selection.borrow_mut();
        s.range = Some((o, id as usize));
        s.direction = Direction::Forwards;
        Ok(Value::Undefined)
    });
    op(vm, proto, "removeRange", 1, |vm, c| {
        let (_, id) = this_of(vm, &c.arg(0), C::Range)?;
        let h = this(vm, &c)?;
        if current(&h).is_some_and(|(_, cur, _)| cur == id as usize) {
            h.selection.borrow_mut().range = None;
            return Ok(Value::Undefined);
        }
        throw(vm, "NotFoundError", "that range isn't the selection's")
    });
    for name in ["removeAllRanges", "empty"] {
        op(vm, proto, name, 0, |vm, c| {
            let h = this(vm, &c)?;
            h.selection.borrow_mut().range = None;
            Ok(Value::Undefined)
        });
    }
    for name in ["collapse", "setPosition"] {
        op(vm, proto, name, 1, |vm, c| {
            let h = this(vm, &c)?;
            if c.arg(0).is_nullish() {
                h.selection.borrow_mut().range = None;
                return Ok(Value::Undefined);
            }
            let n = arg_node(vm, &c.arg(0), "the node")?;
            let offset = to_ulong(vm, &c.arg(1))?;
            check_point(vm, &h, n, offset)?;
            if !in_document(vm, &h, n)? {
                return Ok(Value::Undefined);
            }
            let bp = Bp { node: n, offset };
            set_new_range(vm, &h, bp, bp, Direction::Forwards)?;
            Ok(Value::Undefined)
        });
    }
    for (name, start) in [("collapseToStart", true), ("collapseToEnd", false)] {
        let f: bb_js::vm::heap::NativeFn =
            if start { |vm, c| collapse_to(vm, &c, true) } else { |vm, c| collapse_to(vm, &c, false) };
        op(vm, proto, name, 0, f);
    }
    op(vm, proto, "extend", 1, |vm, c| {
        let h = this(vm, &c)?;
        let n = arg_node(vm, &c.arg(0), "the node")?;
        let offset = to_ulong(vm, &c.arg(1))?;
        if !in_document(vm, &h, n)? {
            return Ok(Value::Undefined);
        }
        let Some((anchor, _)) = anchor_focus(&h) else {
            return throw(vm, "InvalidStateError", "the selection is empty");
        };
        check_point(vm, &h, n, offset)?;
        let focus = Bp { node: n, offset };
        let order = with_doc(vm, &h, 0, |d| range::position(d, anchor, focus))?;
        let (s, e, dir) = match order {
            Ordering::Greater => (focus, anchor, Direction::Backwards),
            _ => (anchor, focus, Direction::Forwards),
        };
        set_new_range(vm, &h, s, e, dir)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "setBaseAndExtent", 4, |vm, c| {
        let h = this(vm, &c)?;
        let an = arg_node(vm, &c.arg(0), "the anchor node")?;
        let ao = to_ulong(vm, &c.arg(1))?;
        let fnode = arg_node(vm, &c.arg(2), "the focus node")?;
        let fo = to_ulong(vm, &c.arg(3))?;
        check_point(vm, &h, an, ao)?;
        check_point(vm, &h, fnode, fo)?;
        if !in_document(vm, &h, an)? || !in_document(vm, &h, fnode)? {
            return Ok(Value::Undefined);
        }
        let (anchor, focus) = (Bp { node: an, offset: ao }, Bp { node: fnode, offset: fo });
        let order = with_doc(vm, &h, 0, |d| range::position(d, anchor, focus))?;
        let (s, e, dir) = match order {
            Ordering::Greater => (focus, anchor, Direction::Backwards),
            _ => (anchor, focus, Direction::Forwards),
        };
        set_new_range(vm, &h, s, e, dir)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "selectAllChildren", 1, |vm, c| {
        let h = this(vm, &c)?;
        let n = arg_node(vm, &c.arg(0), "the node")?;
        let (doctype, len) = with_doc(vm, &h, n.doc, |d| {
            (matches!(d.node(n.id).data, NodeData::Doctype { .. }), range::node_len(d, n.id))
        })?;
        if doctype {
            return throw(vm, "InvalidNodeTypeError", "a doctype has no children to select");
        }
        if !in_document(vm, &h, n)? {
            return Ok(Value::Undefined);
        }
        set_new_range(vm, &h, Bp { node: n, offset: 0 }, Bp { node: n, offset: len }, Direction::Forwards)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "deleteFromDocument", 0, |vm, c| {
        let h = this(vm, &c)?;
        if let Some((_, id, r)) = current(&h) {
            range::delete_contents(vm, &h, id, r)?;
        }
        Ok(Value::Undefined)
    });
    op(vm, proto, "containsNode", 1, |vm, c| {
        let h = this(vm, &c)?;
        let n = arg_node(vm, &c.arg(0), "the node")?;
        let partial = vm.to_boolean(&c.arg(1));
        let Some((_, _, r)) = current(&h) else { return Ok(Value::Bool(false)) };
        if !in_document(vm, &h, n)? {
            return Ok(Value::Bool(false));
        }
        let b = with_doc(vm, &h, 0, |d| {
            let len = range::node_len(d, n.id);
            let (first, last) = (Bp { node: n, offset: 0 }, Bp { node: n, offset: len });
            if partial {
                range::position(d, r.start, last) != Ordering::Greater
                    && range::position(d, r.end, first) != Ordering::Less
            } else {
                range::position(d, r.start, first) != Ordering::Greater
                    && range::position(d, r.end, last) != Ordering::Less
            }
        })?;
        Ok(Value::Bool(b))
    });
    op(vm, proto, "toString", 0, |vm, c| {
        let h = this(vm, &c)?;
        let Some((_, _, r)) = current(&h) else { return Ok(js("")) };
        let s = with_doc(vm, &h, r.start.node.doc, |d| range::stringify(d, &r))?;
        Ok(js(&s))
    });
}

fn point_attr(vm: &mut Vm, c: &NativeCall, focus: bool, offset: bool) -> R {
    let h = this(vm, c)?;
    let Some((a, f)) = anchor_focus(&h) else {
        return Ok(if offset { Value::Number(0.0) } else { Value::Null });
    };
    let p = if focus { f } else { a };
    if offset {
        Ok(Value::Number(p.offset as f64))
    } else {
        wrap(vm, &h, p.node)
    }
}

fn collapse_to(vm: &mut Vm, c: &NativeCall, start: bool) -> R {
    let h = this(vm, c)?;
    let Some((_, _, r)) = current(&h) else {
        return throw(vm, "InvalidStateError", "the selection is empty");
    };
    let p = if start { r.start } else { r.end };
    set_new_range(vm, &h, p, p, Direction::Forwards)?;
    Ok(Value::Undefined)
}

/// `Window.getSelection()`.
pub fn setup_window(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "getSelection", 0, |vm, _c| {
        let h = host(vm)?;
        Ok(selection_object(vm, &h))
    });
}

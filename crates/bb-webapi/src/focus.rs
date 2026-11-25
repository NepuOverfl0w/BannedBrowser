//! Focus from scripts (HTML §6.6): `focus()` and `blur()`, `document.activeElement` and
//! `ShadowRoot.activeElement` (retargeted), and the focus update steps' events (blur,
//! focusout, focus, focusin). The focused element is bb-dom's; the embedder hears about
//! changes scripts make ([`Host::focus_request`]) and tells the window about the user's.

use bb_dom::NodeId;
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::host::*;
use crate::idl::{attr, op};

type R = JsResult<Value>;

/// §6.6.4 "focus update steps", simplified to the two elements: `blur` and `focusout` at the
/// old focused element, then `focus` and `focusin` at the new one (FocusEvents whose
/// relatedTarget is the other element).
pub fn update(vm: &mut Vm, h: &Host, doc: u32, old: Option<NodeId>, new: Option<NodeId>) -> JsResult<()> {
    if old == new {
        return Ok(());
    }
    with_doc_mut(vm, h, doc, |d| d.set_focused(new))?;
    h.mark_dirty();
    let wrap_n = |vm: &mut Vm, n: Option<NodeId>| match n {
        Some(id) => wrap(vm, h, NodeRef { doc, id }),
        None => Ok(Value::Null),
    };
    let (o, nw) = (wrap_n(vm, old)?, wrap_n(vm, new)?);
    let fire = |vm: &mut Vm, target: &Value, ty: &str, bubbles: bool, related: &Value| -> JsResult<()> {
        let e = crate::events::trusted_event(vm, h, C::FocusEvent, ty, bubbles, false);
        crate::events::set_field(vm, e, "relatedTarget", related.clone());
        crate::events::dispatch(vm, h, e, target.clone())?;
        Ok(())
    };
    if old.is_some() {
        fire(vm, &o, "blur", false, &nw)?;
        fire(vm, &o, "focusout", true, &nw)?;
    }
    // A blur listener may have moved focus elsewhere already.
    let now = with_doc(vm, h, doc, |d| d.focused())?;
    if new.is_some() && now == new {
        fire(vm, &nw, "focus", false, &o)?;
        fire(vm, &nw, "focusin", true, &o)?;
    }
    Ok(())
}

/// HTMLOrSVGElement.focus() (§6.6.5): the focusing steps for the element (its focus delegate
/// for a delegating shadow host).
fn focus(vm: &mut Vm, c: NativeCall) -> R {
    let (h, n) = this_node(vm, &c.this, C::Element)?;
    let (target, old, connected) =
        with_doc(vm, &h, n.doc, |d| (d.focus_target(n.id), d.focused(), d.is_connected(n.id)))?;
    let Some(t) = target else { return Ok(Value::Undefined) };
    if !connected || Some(t) == old {
        return Ok(Value::Undefined);
    }
    update(vm, &h, n.doc, old, Some(t))?;
    if n.doc == 0 {
        h.focus_request.set(Some(with_doc(vm, &h, 0, |d| d.focused())?));
    }
    Ok(Value::Undefined)
}

/// blur() (§6.6.5): if the element is focused, focus moves to the viewport.
fn blur(vm: &mut Vm, c: NativeCall) -> R {
    let (h, n) = this_node(vm, &c.this, C::Element)?;
    // The element itself, or (a host with delegatesFocus) the focused element inside it.
    let (focused, ours) = with_doc(vm, &h, n.doc, |d| {
        let f = d.focused();
        let delegating =
            d.shadow_root(n.id).and_then(|r| d.shadow_info(r)).is_some_and(|i| i.delegates_focus);
        let ours = f == Some(n.id)
            || (delegating && f.is_some_and(|f| d.is_shadow_including_inclusive_ancestor(n.id, f)));
        (f, ours)
    })?;
    if !ours {
        return Ok(Value::Undefined);
    }
    update(vm, &h, n.doc, focused, None)?;
    if n.doc == 0 {
        h.focus_request.set(Some(None));
    }
    Ok(Value::Undefined)
}

pub fn setup_element(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "focus", 0, focus);
    op(vm, proto, "blur", 0, blur);
}

/// DocumentOrShadowRoot.activeElement (§6.6.6): the focused element retargeted against the
/// document or shadow root; for a document with nothing focused, its body (or document
/// element).
pub fn active_element(vm: &mut Vm, h: &Host, root: NodeRef) -> R {
    let (focused, is_doc, fallback) = with_doc(vm, h, root.doc, |d| {
        let is_doc = root.id == bb_dom::Document::ROOT;
        (d.focused(), is_doc, if is_doc { d.body().or_else(|| d.document_element()) } else { None })
    })?;
    let Some(f) = focused else { return wrap_opt(vm, h, fallback.map(|x| root.with(x))) };
    // Retarget against the root: the shadow host the focus is inside, up to this tree.
    let candidate = with_doc(vm, h, root.doc, |d| {
        let mut n = f;
        loop {
            let r = d.tree_root(n);
            if r == root.id {
                return Some(n);
            }
            match d.shadow_host(r) {
                Some(host) => n = host,
                None => return None,
            }
        }
    })?;
    match candidate {
        Some(x) => wrap(vm, h, root.with(x)),
        None if is_doc => wrap_opt(vm, h, fallback.map(|x| root.with(x))),
        None => Ok(Value::Null),
    }
}

/// The focused element was removed from the document: focus goes to the viewport, without
/// events (§6.6.4 "focus fixup rule").
pub fn removed(vm: &mut Vm, h: &Host, doc: u32) {
    let gone = with_doc(vm, h, doc, |d| d.focused().is_some_and(|f| !d.is_connected(f))).unwrap_or(false);
    if gone {
        let _ = with_doc_mut(vm, h, doc, |d| d.set_focused(None));
        if doc == 0 {
            h.focus_request.set(Some(None));
        }
        h.mark_dirty();
    }
}

pub fn setup_document(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "activeElement",
        |vm, c| {
            let (h, n) = this_node(vm, &c.this, C::Document)?;
            active_element(vm, &h, n)
        },
        None,
    );
}

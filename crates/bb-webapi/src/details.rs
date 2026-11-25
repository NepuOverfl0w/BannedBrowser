//! The `<details>` element's DOM steps (HTML §4.11.1): the `toggle` event (a ToggleEvent
//! queued when `open` changes, coalesced with one still queued) and exclusive accordions
//! (details elements with the same `name` in a tree: opening one closes the others).

use bb_dom::{Document, NodeId};
use bb_js::{JsResult, Vm};

use crate::class::C;
use crate::host::*;

/// Details elements whose toggle task is queued: (old state, new state, task number).
#[derive(Default)]
pub struct Toggles {
    pending: std::collections::HashMap<u64, (bool, bool, u64)>,
    next: u64,
}

/// HTML §4.11.1 "queue a details toggle event task".
fn queue_toggle(h: &Host, n: NodeRef, old_open: bool, new_open: bool) {
    let mut t = h.toggles.borrow_mut();
    t.next += 1;
    let seq = t.next;
    // A task still queued is canceled; its old state is kept.
    let old_open = t.pending.get(&n.pack()).map_or(old_open, |p| p.0);
    t.pending.insert(n.pack(), (old_open, new_open, seq));
    h.tasks.borrow_mut().push_back(Task::Toggle(n, seq));
}

/// Run a queued toggle task (if it wasn't canceled): fire `toggle` at the details element.
pub fn run_toggle(vm: &mut Vm, h: &Host, n: NodeRef, seq: u64) -> JsResult<()> {
    let entry = {
        let mut t = h.toggles.borrow_mut();
        match t.pending.get(&n.pack()) {
            Some(&(o, nw, s)) if s == seq => {
                t.pending.remove(&n.pack());
                Some((o, nw))
            }
            _ => None,
        }
    };
    let Some((old_open, new_open)) = entry else { return Ok(()) };
    let state = |open: bool| js(if open { "open" } else { "closed" });
    let e = crate::events::trusted_event(vm, h, C::ToggleEvent, "toggle", false, false);
    crate::events::set_field(vm, e, "oldState", state(old_open));
    crate::events::set_field(vm, e, "newState", state(new_open));
    let t = wrap(vm, h, n)?;
    crate::events::dispatch(vm, h, e, t)?;
    Ok(())
}

/// The other details elements of `n`'s name group (same tree, same non-empty `name`).
fn group(d: &Document, n: NodeId) -> Vec<NodeId> {
    let Some(name) = d.attr(n, "name").filter(|s| !s.is_empty()) else { return Vec::new() };
    let root = d.tree_root(n);
    d.descendants(root)
        .filter(|&x| x != n && d.is_html(x, "details") && d.attr(x, "name") == Some(name))
        .collect()
}

/// "Ensure details exclusivity by closing other elements if needed": the first other open
/// member of `n`'s group closes.
fn close_others(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<()> {
    let other =
        with_doc(vm, h, n.doc, |d| group(d, n.id).into_iter().find(|&x| d.attr(x, "open").is_some()))?;
    if let Some(o) = other {
        crate::mutation::set_or_remove(vm, h, n.with(o), "open", None)?;
    }
    Ok(())
}

/// "Ensure details exclusivity by closing the given element if needed" (counting the group
/// members `present` accepts).
fn close_self_if_needed(vm: &mut Vm, h: &Host, n: NodeRef, present: &dyn Fn(NodeId) -> bool) -> JsResult<()> {
    let close = with_doc(vm, h, n.doc, |d| {
        d.attr(n.id, "open").is_some()
            && group(d, n.id).iter().any(|&x| present(x) && d.attr(x, "open").is_some())
    })?;
    if close {
        crate::mutation::set_or_remove(vm, h, n, "open", None)?;
    }
    Ok(())
}

/// The details attribute change steps.
pub fn attribute_changed(vm: &mut Vm, h: &Host, n: NodeRef, local: &str, old: Option<&str>) -> JsResult<()> {
    if local != "open" && local != "name" {
        return Ok(());
    }
    let (is_details, now) =
        with_doc(vm, h, n.doc, |d| (d.is_html(n.id, "details"), d.attr(n.id, local).is_some()))?;
    if !is_details {
        return Ok(());
    }
    if local == "name" {
        return close_self_if_needed(vm, h, n, &|_| true);
    }
    match (old.is_some(), now) {
        (true, false) => queue_toggle(h, n, true, false),
        (false, true) => {
            close_others(vm, h, n)?;
            queue_toggle(h, n, false, true);
        }
        _ => {}
    }
    Ok(())
}

/// Details elements the parser made (arena indices from `before` of document `doc`): an
/// `open` attribute queues a toggle event (the parser appended it), and the insertion steps
/// run, in tree order.
pub fn parsed(vm: &mut Vm, h: &Host, doc: u32, before: usize) {
    let found: Vec<(NodeRef, bool)> = {
        let Ok(docs) = h.docs.try_borrow() else { return };
        let Some(d) = docs.get(doc as usize) else { return };
        (before..d.len())
            .map(|i| NodeId(i as u32))
            .filter(|&x| d.is_html(x, "details") && d.parent(x).is_some())
            .map(|x| (NodeRef { doc, id: x }, d.attr(x, "open").is_some()))
            .collect()
    };
    for (n, open) in found {
        if open {
            queue_toggle(h, n, false, true);
        }
        // Only the elements parsed before this one were in the tree when it was inserted.
        let earlier = |x: NodeId| (x.0 as usize) < before || x.0 < n.id.0;
        if let Err(e) = close_self_if_needed(vm, h, n, &earlier) {
            crate::events::report_exception(vm, e);
        }
    }
}

/// The details insertion steps, for the details elements among inserted `nodes` (DOM
/// "insert" runs them as each node goes in, so later nodes' elements don't count yet).
pub fn inserted(vm: &mut Vm, h: &Host, nodes: &[NodeRef]) -> JsResult<()> {
    let Some(first) = nodes.first() else { return Ok(()) };
    let found: Vec<(NodeRef, usize)> = with_doc(vm, h, first.doc, |d| {
        let mut out = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            out.extend(
                d.descendants(n.id)
                    .filter(|&x| d.is_html(x, "details") && d.attr(x, "name").is_some_and(|s| !s.is_empty()))
                    .map(|x| (n.with(x), i)),
            );
        }
        out
    })?;
    if found.is_empty() {
        return Ok(());
    }
    let later_roots: Vec<NodeId> = nodes.iter().map(|n| n.id).collect();
    for (n, i) in found {
        let later = &later_roots[i + 1..];
        let doc = n.doc;
        let not_yet: Vec<NodeId> = with_doc(vm, h, doc, |d| {
            later.iter().flat_map(|&r| d.descendants(r).collect::<Vec<_>>()).collect()
        })?;
        let present = |x: NodeId| !not_yet.contains(&x);
        close_self_if_needed(vm, h, n, &present)?;
    }
    Ok(())
}

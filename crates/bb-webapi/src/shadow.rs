//! Shadow DOM (DOM §4.2.2, §4.8): Element.attachShadow and shadowRoot, the ShadowRoot
//! interface, HTMLSlotElement's assignment API (HTML §4.12.4), the Slottable mixin's
//! assignedSlot, slotchange events (DOM §4.2.2.5 "signal a slot change"), and the HTML
//! serialization and parsing members that know about shadow roots (getHTML, setHTMLUnsafe).
//!
//! The trees themselves are bb-dom's (`attach_shadow`, `find_slot`, …). Slot assignment is
//! computed from the tree; what is kept here is each slot's assigned nodes as last seen, so
//! that a mutation that changes them can signal a slot change.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use bb_dom::{Namespace, NodeData, NodeId, ShadowRootInfo, Shadows, SlotAssignment};
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::exception::throw;
use crate::host::*;
use crate::idl::{attr, op};

type R = JsResult<Value>;

/// Slots' assigned nodes as last computed, the agent's signal slots, and every slot's
/// manually assigned nodes (by packed node reference: a slot and its nodes can be in
/// different documents, and adoption moves nodes to new ids).
#[derive(Default)]
pub struct SlotState {
    assigned: HashMap<u64, Vec<NodeId>>,
    signals: Vec<NodeRef>,
    manual: Vec<(u64, Vec<u64>)>,
}

impl SlotState {
    /// Nodes moved to other ids (adopted into another document's arena).
    pub fn remap(&mut self, pairs: &[(u64, u64)]) -> bool {
        if self.manual.is_empty() && self.assigned.is_empty() {
            return false;
        }
        let map: HashMap<u64, u64> = pairs.iter().copied().collect();
        let fix = |v: &mut u64| {
            if let Some(n) = map.get(v) {
                *v = *n;
            }
        };
        for (s, nodes) in &mut self.manual {
            fix(s);
            nodes.iter_mut().for_each(fix);
        }
        let old = std::mem::take(&mut self.assigned);
        for (mut k, v) in old {
            fix(&mut k);
            self.assigned.insert(k, v);
        }
        !self.manual.is_empty()
    }
}

/// Give document `doc` its part of the manual slot assignments.
pub fn sync_manual(vm: &mut Vm, h: &Host, doc: u32) {
    let list: Vec<(NodeId, Vec<NodeId>)> = h
        .slots
        .borrow()
        .manual
        .iter()
        .filter(|(s, _)| NodeRef::unpack(*s).doc == doc)
        .map(|(s, nodes)| {
            let own = nodes.iter().map(|n| NodeRef::unpack(*n)).filter(|n| n.doc == doc).map(|n| n.id);
            (NodeRef::unpack(*s).id, own.collect())
        })
        .collect();
    let _ = with_doc_mut(vm, h, doc, |d| d.set_manual_assignments(list));
}

/// After adoption moved nodes from `from` to `to`: the assignments follow them.
pub fn adopted(vm: &mut Vm, h: &Host, pairs: &[(u64, u64)], from: u32, to: u32) {
    if h.slots.borrow_mut().remap(pairs) {
        sync_manual(vm, h, from);
        sync_manual(vm, h, to);
    }
}

/// DOM §4.2.2.5 "signal a slot change".
pub fn signal(vm: &mut Vm, h: &Host, slot: NodeRef) {
    {
        let mut st = h.slots.borrow_mut();
        if st.signals.contains(&slot) {
            return;
        }
        st.signals.push(slot);
    }
    crate::mutation::queue_microtask(vm, h);
}

/// Take the signal slots (the mutation observer microtask, DOM §4.3.2 step 4).
pub fn take_signals(h: &Host) -> Vec<NodeRef> {
    std::mem::take(&mut h.slots.borrow_mut().signals)
}

/// DOM §4.2.2.4 "assign slottables for a tree" for the shadow roots `roots` of document
/// `doc`: every slot whose assigned nodes changed gets a slot change signal. Slots that have
/// left the shadow trees (a removed subtree) now have none.
pub fn update(vm: &mut Vm, h: &Host, doc: u32, roots: &[NodeId]) {
    if roots.is_empty() && h.slots.borrow().assigned.is_empty() {
        return;
    }
    let changed = with_doc(vm, h, doc, |d| {
        let mut st = h.slots.borrow_mut();
        let mut changed = Vec::new();
        let mut seen = HashSet::new();
        for &r in roots {
            for (slot, nodes) in d.slot_assignments(r) {
                let k = NodeRef { doc, id: slot }.pack();
                seen.insert(k);
                let old = st.assigned.get(&k).map_or(&[][..], Vec::as_slice);
                if old != nodes.as_slice() {
                    changed.push(NodeRef { doc, id: slot });
                    if nodes.is_empty() {
                        st.assigned.remove(&k);
                    } else {
                        st.assigned.insert(k, nodes);
                    }
                }
            }
        }
        // Slots no longer in a shadow tree, or in one of `roots` but not found there.
        let stale: Vec<u64> = st
            .assigned
            .keys()
            .copied()
            .filter(|k| {
                let n = NodeRef::unpack(*k);
                if n.doc != doc || seen.contains(k) {
                    return false;
                }
                let root = d.tree_root(n.id);
                !d.is_shadow_root(root) || roots.contains(&root) || !d.is_slot(n.id)
            })
            .collect();
        for k in stale {
            st.assigned.remove(&k);
            changed.push(NodeRef::unpack(k));
        }
        changed
    })
    .unwrap_or_default();
    for s in changed {
        signal(vm, h, s);
    }
}

/// A child was inserted into or removed from `parent`: the slots of the trees that can
/// change are reassigned, and a slot whose fallback content changed signals (DOM §4.2.3
/// "insert" steps 7.4–7.5, "remove" steps 12–13).
pub fn tree_changed(vm: &mut Vm, h: &Host, parent: NodeRef) {
    let info = with_doc(vm, h, parent.doc, |d| {
        if !d.has_shadow_trees() {
            return None;
        }
        let mut roots = Vec::new();
        let in_root = d.containing_shadow_root(parent.id);
        roots.extend(in_root);
        roots.extend(d.shadow_root(parent.id));
        let fallback = in_root.is_some() && d.is_slot(parent.id) && d.find_slottables(parent.id).is_empty();
        Some((roots, fallback))
    })
    .ok()
    .flatten();
    match info {
        Some((roots, fallback)) => {
            if fallback {
                signal(vm, h, parent);
            }
            update(vm, h, parent.doc, &roots);
        }
        None => {
            if !h.slots.borrow().assigned.is_empty() {
                update(vm, h, parent.doc, &[]);
            }
        }
    }
}

/// An attribute of element `n` changed: a slottable's `slot` or a slot's `name` (DOM
/// §4.2.2.3 attribute change steps).
pub fn attribute_changed(vm: &mut Vm, h: &Host, n: NodeRef, local: &str) {
    if local != "slot" && local != "name" {
        return;
    }
    let roots = with_doc(vm, h, n.doc, |d| {
        if !d.has_shadow_trees() {
            return Vec::new();
        }
        if local == "slot" {
            d.parent(n.id).and_then(|p| d.shadow_root(p)).into_iter().collect()
        } else if d.is_slot(n.id) {
            d.containing_shadow_root(n.id).into_iter().collect()
        } else {
            Vec::new()
        }
    })
    .unwrap_or_default();
    if !roots.is_empty() {
        update(vm, h, n.doc, &roots);
    }
}

// ---- Element ----

/// HTML §4.2.2.2 "valid shadow host name".
fn valid_shadow_host_name(name: &str) -> bool {
    crate::ce::valid_name(name)
        || matches!(
            name,
            "article"
                | "aside"
                | "blockquote"
                | "body"
                | "div"
                | "footer"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "header"
                | "main"
                | "nav"
                | "p"
                | "section"
                | "span"
        )
}

fn bool_member(vm: &mut Vm, d: &Value, name: &'static str) -> JsResult<bool> {
    let v = dict_get(vm, d, name)?;
    Ok(vm.to_boolean(&v))
}

/// DOM §4.2.2.2 "attach a shadow root" to `n`; returns the shadow root.
pub fn attach(vm: &mut Vm, h: &Host, n: NodeRef, mut info: ShadowRootInfo) -> JsResult<NodeRef> {
    let (ns, local) = with_doc(vm, h, n.doc, |d| d.element(n.id).map(|e| (e.ns, e.name.clone())))?
        .unwrap_or((Namespace::None, String::new()));
    // Steps 1–2.
    if ns != Namespace::Html || !valid_shadow_host_name(&local) {
        return throw(vm, "NotSupportedError", &format!("a {local} element can't have a shadow root"));
    }
    // Step 3.
    let is = crate::ce::is_value(h, n);
    if crate::ce::valid_name(&local) || is.is_some() {
        if let Some(def) = crate::ce::lookup(h, n.doc, ns, &local, is.as_deref()) {
            if h.ce.borrow().defs[def].disable_shadow {
                return throw(vm, "NotSupportedError", "this custom element disables shadow roots");
            }
        }
    }
    // Step 4: a declarative shadow root of the same mode is reused, emptied.
    if let Some(existing) = with_doc(vm, h, n.doc, |d| d.shadow_root(n.id))? {
        let ok = with_doc(vm, h, n.doc, |d| {
            d.shadow_info(existing).is_some_and(|i| i.declarative && i.open == info.open)
        })?;
        if !ok {
            return throw(vm, "NotSupportedError", "the element already has a shadow root");
        }
        let root = n.with(existing);
        crate::tree::replace_all(vm, h, None, root)?;
        with_doc_mut(vm, h, n.doc, |d| {
            if let Some(i) = d.shadow_info_mut(existing) {
                i.declarative = false;
            }
        })?;
        return Ok(root);
    }
    // Step 7.
    if matches!(crate::ce::state(h, n), Some(crate::ce::State::Custom)) || crate::ce::is_precustomized(h, n) {
        info.available_to_element_internals = true;
    }
    let root = with_doc_mut(vm, h, n.doc, |d| d.attach_shadow(n.id, info))?;
    h.mark_dirty();
    Ok(n.with(root))
}

fn attach_shadow(vm: &mut Vm, c: NativeCall) -> R {
    let (h, n) = this_node(vm, &c.this, C::Element)?;
    let init = c.arg(0);
    if !matches!(init, Value::Object(_)) {
        return vm.type_error("attachShadow needs a ShadowRootInit with a mode");
    }
    // ShadowRootInit's members, in lexicographic order (WebIDL §3.2.17).
    let clonable = bool_member(vm, &init, "clonable")?;
    let delegates_focus = bool_member(vm, &init, "delegatesFocus")?;
    let mode = dict_get(vm, &init, "mode")?;
    if mode.is_undefined() {
        return vm.type_error("ShadowRootInit.mode is required");
    }
    let open = match rstr(vm, &mode)?.as_str() {
        "open" => true,
        "closed" => false,
        m => return vm.type_error(&format!("'{m}' is not a valid ShadowRootMode")),
    };
    let serializable = bool_member(vm, &init, "serializable")?;
    let sa = dict_get(vm, &init, "slotAssignment")?;
    let slot_assignment = if sa.is_undefined() {
        SlotAssignment::Named
    } else {
        match rstr(vm, &sa)?.as_str() {
            "named" => SlotAssignment::Named,
            "manual" => SlotAssignment::Manual,
            m => return vm.type_error(&format!("'{m}' is not a valid SlotAssignmentMode")),
        }
    };
    let info = ShadowRootInfo {
        open,
        delegates_focus,
        slot_assignment,
        clonable,
        serializable,
        ..Default::default()
    };
    let root = attach(vm, &h, n, info)?;
    wrap(vm, &h, root)
}

/// Element.shadowRoot: the shadow root, if it is open.
fn shadow_root_get(vm: &mut Vm, c: NativeCall) -> R {
    let (h, n) = this_node(vm, &c.this, C::Element)?;
    let r = with_doc(vm, &h, n.doc, |d| {
        d.shadow_root(n.id).filter(|&r| d.shadow_info(r).is_some_and(|i| i.open))
    })?;
    wrap_opt(vm, &h, r.map(|r| n.with(r)))
}

/// Slottable.assignedSlot (DOM §4.2.9): the slot it is assigned to, in an open shadow root.
fn assigned_slot_get(vm: &mut Vm, c: NativeCall) -> R {
    let (h, n) = this_node(vm, &c.this, C::Node)?;
    let r = with_doc(vm, &h, n.doc, |d| d.find_slot(n.id, true))?;
    wrap_opt(vm, &h, r.map(|r| n.with(r)))
}

/// GetHTMLOptions (HTML §8.4.1): serializableShadowRoots and shadowRoots.
fn html_options(vm: &mut Vm, h: &Host, o: &Value, doc: u32) -> JsResult<(bool, Vec<NodeId>)> {
    let ser = bool_member(vm, o, "serializableShadowRoots")?;
    let v = dict_get(vm, o, "shadowRoots")?;
    let mut roots = Vec::new();
    if !v.is_undefined() {
        for x in vm.iterate_to_vec(&v)? {
            let ok = matches!(vm.host_data(&x), Some((c, _)) if c == C::ShadowRoot as u32);
            if !ok {
                return vm.type_error("shadowRoots must hold ShadowRoots");
            }
            if let Some(n) = node_of(vm, &x) {
                if n.doc == doc {
                    roots.push(n.id);
                }
            }
        }
    }
    let _ = h;
    Ok((ser, roots))
}

/// HTML §13.3 "HTML fragment serialization algorithm" of `n`'s children (and its own
/// shadow root, when it is a host whose root is included).
fn serialize(vm: &mut Vm, h: &Host, n: NodeRef, ser: bool, roots: &[NodeId]) -> JsResult<String> {
    with_doc(vm, h, n.doc, |d| {
        let sh = Shadows { serializable: ser, roots };
        let void = d.element(n.id).is_some_and(|e| e.ns == Namespace::Html && bb_dom::is_void(&e.name));
        if void {
            return String::new();
        }
        let mut out = String::new();
        if let Some(sr) = d.shadow_root(n.id) {
            out.push_str(&bb_dom::serialize_shadow_template(d, sr, &sh));
        }
        out.push_str(&bb_dom::serialize_children_html(d, n.id, &sh));
        out
    })
}

fn get_html(vm: &mut Vm, c: NativeCall) -> R {
    let (h, n) = this_node(vm, &c.this, C::Node)?;
    let (ser, roots) = html_options(vm, &h, &c.arg(0), n.doc)?;
    Ok(js(&serialize(vm, &h, n, ser, &roots)?))
}

/// HTML §8.4.1 "unsafely set HTML": parse with declarative shadow roots allowed, then
/// replace all of `target`'s children.
fn set_html_unsafe(vm: &mut Vm, h: &Host, target: NodeRef, context: NodeRef, html: &str) -> JsResult<()> {
    let frag = crate::html::parse_fragment_shadow(vm, h, target.doc, context, html)?;
    crate::tree::replace_all(vm, h, Some(frag), target)
}

fn element_set_html_unsafe(vm: &mut Vm, c: NativeCall) -> R {
    let html = rstr(vm, &c.arg(0))?;
    let (h, n) = this_node(vm, &c.this, C::Element)?;
    let target = with_doc(vm, &h, n.doc, |d| d.template_of(n.id))?.map_or(n, |t| n.with(t));
    set_html_unsafe(vm, &h, target, n, &html)?;
    Ok(Value::Undefined)
}

pub fn setup_element(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "attachShadow", 1, attach_shadow);
    attr(vm, proto, "shadowRoot", shadow_root_get, None);
    attr(vm, proto, "assignedSlot", assigned_slot_get, None);
    op(vm, proto, "getHTML", 0, get_html);
    op(vm, proto, "setHTMLUnsafe", 1, element_set_html_unsafe);
}

pub fn setup_text(vm: &mut Vm, proto: ObjRef) {
    attr(vm, proto, "assignedSlot", assigned_slot_get, None);
}

// ---- ShadowRoot ----

fn root_this(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, NodeRef, ShadowRootInfo)> {
    let (h, n) = this_node(vm, &c.this, C::ShadowRoot)?;
    let info = with_doc(vm, &h, n.doc, |d| d.shadow_info(n.id).cloned())?;
    match info {
        Some(i) => Ok((h, n, i)),
        None => vm.type_error("Illegal invocation: not a ShadowRoot"),
    }
}

pub fn setup_shadow_root(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "mode",
        |vm, c| {
            let (_, _, i) = root_this(vm, &c)?;
            Ok(js(if i.open { "open" } else { "closed" }))
        },
        None,
    );
    attr(
        vm,
        proto,
        "delegatesFocus",
        |vm, c| {
            let (_, _, i) = root_this(vm, &c)?;
            Ok(Value::Bool(i.delegates_focus))
        },
        None,
    );
    attr(
        vm,
        proto,
        "slotAssignment",
        |vm, c| {
            let (_, _, i) = root_this(vm, &c)?;
            Ok(js(if i.slot_assignment == SlotAssignment::Manual { "manual" } else { "named" }))
        },
        None,
    );
    attr(
        vm,
        proto,
        "clonable",
        |vm, c| {
            let (_, _, i) = root_this(vm, &c)?;
            Ok(Value::Bool(i.clonable))
        },
        None,
    );
    attr(
        vm,
        proto,
        "serializable",
        |vm, c| {
            let (_, _, i) = root_this(vm, &c)?;
            Ok(Value::Bool(i.serializable))
        },
        None,
    );
    attr(
        vm,
        proto,
        "host",
        |vm, c| {
            let (h, n, i) = root_this(vm, &c)?;
            wrap(vm, &h, n.with(i.host))
        },
        None,
    );
    // DocumentOrShadowRoot.activeElement: nothing in a shadow tree is focused (focus is the
    // browser's, on the document's elements).
    attr(
        vm,
        proto,
        "activeElement",
        |vm, c| {
            root_this(vm, &c)?;
            Ok(Value::Null)
        },
        None,
    );
    attr(
        vm,
        proto,
        "innerHTML",
        |vm, c| {
            let (h, n, _) = root_this(vm, &c)?;
            Ok(js(&serialize(vm, &h, n, false, &[])?))
        },
        Some(|vm, c| {
            let v = rstr_null_empty(vm, &c.arg(0))?;
            let (h, n, i) = root_this(vm, &c)?;
            let frag = crate::html::parse_fragment_into(vm, &h, n.doc, n.with(i.host), &v)?;
            crate::tree::replace_all(vm, &h, Some(frag), n)?;
            Ok(Value::Undefined)
        }),
    );
    op(vm, proto, "getHTML", 0, get_html);
    op(vm, proto, "setHTMLUnsafe", 1, |vm, c| {
        let html = rstr(vm, &c.arg(0))?;
        let (h, n, i) = root_this(vm, &c)?;
        set_html_unsafe(vm, &h, n, n.with(i.host), &html)?;
        Ok(Value::Undefined)
    });
    crate::events::setup_named_handlers(vm, proto, &["onslotchange"]);
}

// ---- HTMLSlotElement ----

fn assigned(vm: &mut Vm, c: &NativeCall, elements: bool) -> R {
    let (h, n) = this_node(vm, &c.this, C::HTMLSlotElement)?;
    let flatten = bool_member(vm, &c.arg(0), "flatten")?;
    let nodes = with_doc(vm, &h, n.doc, |d| {
        let list = if flatten { d.find_flattened_slottables(n.id) } else { d.find_slottables(n.id) };
        list.into_iter().filter(|&x| !elements || d.element(x).is_some()).collect::<Vec<_>>()
    })?;
    let mut out = Vec::with_capacity(nodes.len());
    for x in nodes {
        out.push(wrap(vm, &h, n.with(x))?);
    }
    Ok(Value::Object(vm.new_array(out)))
}

pub fn setup_slot(vm: &mut Vm, proto: ObjRef) {
    crate::html::reflect(vm, proto, "name", "name", crate::html::Reflect::Str);
    op(vm, proto, "assignedNodes", 0, |vm, c| assigned(vm, &c, false));
    op(vm, proto, "assignedElements", 0, |vm, c| assigned(vm, &c, true));
    // HTML §4.12.4 assign(...nodes).
    op(vm, proto, "assign", 0, |vm, c| {
        let (h, n) = this_node(vm, &c.this, C::HTMLSlotElement)?;
        let mut nodes: Vec<u64> = Vec::with_capacity(c.args.len());
        for a in &c.args {
            let ok = matches!(vm.host_data(a), Some((k, _)) if crate::class::implements(k, C::Element) || crate::class::implements(k, C::Text));
            let Some(x) = node_of(vm, a).filter(|_| ok) else {
                return vm.type_error("assign takes elements and text nodes");
            };
            if !nodes.contains(&x.pack()) {
                nodes.push(x.pack());
            }
        }
        // Steps 1–4: the nodes leave the slots they were in; this slot's old nodes are
        // unassigned.
        let me = n.pack();
        let mut touched: Vec<NodeRef> = vec![n];
        {
            let mut st = h.slots.borrow_mut();
            for (s, list) in st.manual.iter_mut() {
                if *s == me {
                    touched.extend(list.iter().map(|x| NodeRef::unpack(*x)));
                    list.clear();
                } else if list.iter().any(|x| nodes.contains(x)) {
                    list.retain(|x| !nodes.contains(x));
                    touched.push(NodeRef::unpack(*s));
                }
            }
            st.manual.retain(|(s, l)| *s != me && !l.is_empty());
            touched.extend(nodes.iter().map(|x| NodeRef::unpack(*x)));
            st.manual.push((me, nodes));
        }
        let mut docs: Vec<u32> = touched.iter().map(|t| t.doc).collect();
        docs.sort_unstable();
        docs.dedup();
        for &d in &docs {
            sync_manual(vm, &h, d);
        }
        h.mark_dirty();
        // Step 5: "assign slottables for a tree" for the trees involved.
        for &doc in &docs {
            let roots: Vec<NodeId> = with_doc(vm, &h, doc, |d| {
                let mut r: Vec<NodeId> = Vec::new();
                for t in touched.iter().filter(|t| t.doc == doc) {
                    r.extend(d.containing_shadow_root(t.id));
                    r.extend(d.parent(t.id).and_then(|p| d.shadow_root(p)));
                }
                r.sort_unstable();
                r.dedup();
                r
            })?;
            update(vm, &h, doc, &roots);
        }
        Ok(Value::Undefined)
    });
}

/// Whether node `n` is a shadow root (for the APIs that treat them specially).
pub fn is_shadow_root(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<bool> {
    with_doc(vm, h, n.doc, |d| {
        matches!(d.node(n.id).data, NodeData::DocumentFragment) && d.is_shadow_root(n.id)
    })
}

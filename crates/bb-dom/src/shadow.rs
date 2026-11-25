//! Shadow trees (DOM §4.2.2): shadow roots attached to shadow hosts, slots and slottables
//! (§4.2.2.3–4), and the shadow-including relations built on them (§4.2.2.1).
//!
//! A shadow root is a DocumentFragment node with no parent; this module's maps say which
//! fragments are shadow roots and of which host. Slot assignment is computed from the tree
//! when asked ("find a slot", "find slottables"): after every mutation the spec's stored
//! assignment equals what those algorithms return, so nothing has to be kept in step except
//! the manually assigned nodes of `slotAssignment: "manual"` roots.

use std::collections::HashMap;

use crate::{Document, NodeData, NodeId};

/// A shadow root's slot assignment (DOM §4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SlotAssignment {
    #[default]
    Named,
    Manual,
}

/// A shadow root's state (DOM §4.8 "ShadowRoot").
#[derive(Clone, Debug, Default)]
pub struct ShadowRootInfo {
    pub host: NodeId,
    /// Mode "open" (else "closed").
    pub open: bool,
    pub delegates_focus: bool,
    pub slot_assignment: SlotAssignment,
    pub clonable: bool,
    pub serializable: bool,
    /// Made by the parser from a `<template shadowrootmode>`.
    pub declarative: bool,
    pub available_to_element_internals: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ShadowState {
    roots: HashMap<NodeId, ShadowRootInfo>,
    /// Host → shadow root.
    hosts: HashMap<NodeId, NodeId>,
    /// A slot's manually assigned nodes.
    manual: HashMap<NodeId, Vec<NodeId>>,
    /// A slottable's manual slot assignment.
    manual_slot: HashMap<NodeId, NodeId>,
}

impl Document {
    /// Attach a new, empty shadow root to `host` (DOM §4.2.2.2 "attach a shadow root"; the
    /// caller checks that it may). Returns the root.
    pub fn attach_shadow(&mut self, host: NodeId, mut info: ShadowRootInfo) -> NodeId {
        info.host = host;
        let root = self.create_fragment();
        self.shadow.roots.insert(root, info);
        self.shadow.hosts.insert(host, root);
        self.mark_style_dirty(host);
        self.mark_layout_dirty(host);
        root
    }

    /// Whether any shadow root exists (else the shadow-aware paths can be skipped).
    pub fn has_shadow_trees(&self) -> bool {
        !self.shadow.roots.is_empty()
    }

    /// The shadow root of `host`, if it is a shadow host.
    pub fn shadow_root(&self, host: NodeId) -> Option<NodeId> {
        self.shadow.hosts.get(&host).copied()
    }

    /// The state of shadow root `root` (None if `root` is not one).
    pub fn shadow_info(&self, root: NodeId) -> Option<&ShadowRootInfo> {
        self.shadow.roots.get(&root)
    }

    pub fn shadow_info_mut(&mut self, root: NodeId) -> Option<&mut ShadowRootInfo> {
        self.shadow.roots.get_mut(&root)
    }

    pub fn is_shadow_root(&self, id: NodeId) -> bool {
        self.shadow.roots.contains_key(&id)
    }

    /// The host of shadow root `root`.
    pub fn shadow_host(&self, root: NodeId) -> Option<NodeId> {
        self.shadow.roots.get(&root).map(|s| s.host)
    }

    /// The root of `id` (DOM §4.2.1): the top of its tree, a shadow root for a node in a
    /// shadow tree.
    pub fn tree_root(&self, id: NodeId) -> NodeId {
        let mut r = id;
        while let Some(p) = self.parent(r) {
            r = p;
        }
        r
    }

    /// The shadow root `id` is in, if any.
    pub fn containing_shadow_root(&self, id: NodeId) -> Option<NodeId> {
        if self.shadow.roots.is_empty() {
            return None;
        }
        let r = self.tree_root(id);
        self.is_shadow_root(r).then_some(r)
    }

    /// DOM §4.2.2.1 "shadow-including root".
    pub fn shadow_including_root(&self, id: NodeId) -> NodeId {
        let mut r = self.tree_root(id);
        while let Some(h) = self.shadow_host(r) {
            r = self.tree_root(h);
        }
        r
    }

    /// The parent of `id`, or its host if `id` is a shadow root.
    pub fn parent_or_host(&self, id: NodeId) -> Option<NodeId> {
        self.parent(id).or_else(|| self.shadow_host(id))
    }

    /// DOM §4.2.2.1 "shadow-including inclusive ancestor": whether `a` is `b` or an
    /// ancestor of `b`, crossing from shadow roots to their hosts.
    pub fn is_shadow_including_inclusive_ancestor(&self, a: NodeId, b: NodeId) -> bool {
        let mut cur = Some(b);
        while let Some(c) = cur {
            if c == a {
                return true;
            }
            cur = self.parent_or_host(c);
        }
        false
    }

    /// `id` and its shadow-including descendants, in shadow-including tree order (DOM
    /// §4.2.2.1: a host's shadow tree comes right after the host, before its children).
    pub fn shadow_including_descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        if self.shadow.roots.is_empty() {
            out.extend(self.descendants(id));
            return out;
        }
        // An explicit stack of "visit this node next" entries (no recursion on deep trees).
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            out.push(n);
            let kids: Vec<NodeId> = self.children(n).collect();
            for &k in kids.iter().rev() {
                stack.push(k);
            }
            if let Some(sr) = self.shadow_root(n) {
                stack.push(sr);
            }
        }
        out
    }

    /// Whether `id` is a slot (an HTML `slot` element).
    pub fn is_slot(&self, id: NodeId) -> bool {
        self.is_html(id, "slot")
    }

    /// Whether `id` is a slottable (DOM §4.2.2.3: an element or a Text node).
    pub fn is_slottable(&self, id: NodeId) -> bool {
        matches!(self.node(id).data, NodeData::Element(_) | NodeData::Text(_))
    }

    /// A slot's name (its `name` attribute) or a slottable's (its `slot` attribute).
    fn slot_name(&self, slot: NodeId) -> &str {
        self.attr(slot, "name").unwrap_or("")
    }

    fn slottable_name(&self, s: NodeId) -> &str {
        self.attr(s, "slot").unwrap_or("")
    }

    /// DOM §4.2.2.4 "find a slot" for `slottable` (with `open_only`, only in an open root).
    pub fn find_slot(&self, slottable: NodeId, open_only: bool) -> Option<NodeId> {
        if self.shadow.roots.is_empty() {
            return None;
        }
        let parent = self.parent(slottable)?;
        let shadow = self.shadow_root(parent)?;
        let info = self.shadow_info(shadow)?;
        if open_only && !info.open {
            return None;
        }
        if info.slot_assignment == SlotAssignment::Manual {
            let s = *self.shadow.manual_slot.get(&slottable)?;
            return (self.tree_root(s) == shadow && self.is_slot(s)).then_some(s);
        }
        let name = self.slottable_name(slottable);
        self.descendants(shadow).find(|&x| self.is_slot(x) && self.slot_name(x) == name)
    }

    /// DOM §4.2.2.4 "find slottables" for `slot`: its assigned nodes.
    pub fn find_slottables(&self, slot: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        if self.shadow.roots.is_empty() {
            return out;
        }
        let root = self.tree_root(slot);
        let Some(info) = self.shadow_info(root) else { return out };
        let host = info.host;
        if info.slot_assignment == SlotAssignment::Manual {
            if let Some(list) = self.shadow.manual.get(&slot) {
                out.extend(list.iter().copied().filter(|&x| self.parent(x) == Some(host)));
            }
            return out;
        }
        for c in self.children(host) {
            if self.is_slottable(c) && self.find_slot(c, false) == Some(slot) {
                out.push(c);
            }
        }
        out
    }

    /// DOM §4.2.2.4 "find flattened slottables" for `slot`.
    pub fn find_flattened_slottables(&self, slot: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        if !self.is_shadow_root(self.tree_root(slot)) {
            return out;
        }
        let mut list = self.find_slottables(slot);
        if list.is_empty() {
            list = self.children(slot).filter(|&c| self.is_slottable(c)).collect();
        }
        for n in list {
            if self.is_slot(n) && self.is_shadow_root(self.tree_root(n)) {
                out.extend(self.find_flattened_slottables(n));
            } else {
                out.push(n);
            }
        }
        out
    }

    /// Every shadow root, in creation order.
    pub fn shadow_roots(&self) -> Vec<NodeId> {
        let mut v: Vec<NodeId> = self.shadow.roots.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// The assigned nodes of every slot in shadow root `root`, in tree order (one pass over
    /// the tree and the host's children, instead of "find slottables" per slot).
    pub fn slot_assignments(&self, root: NodeId) -> Vec<(NodeId, Vec<NodeId>)> {
        let Some(info) = self.shadow_info(root) else { return Vec::new() };
        let slots: Vec<NodeId> = self.descendants(root).filter(|&x| self.is_slot(x)).collect();
        if info.slot_assignment == SlotAssignment::Manual {
            return slots.into_iter().map(|s| (s, self.find_slottables(s))).collect();
        }
        let mut first: HashMap<&str, usize> = HashMap::new();
        for (i, &s) in slots.iter().enumerate() {
            first.entry(self.slot_name(s)).or_insert(i);
        }
        let mut out: Vec<(NodeId, Vec<NodeId>)> = slots.iter().map(|&s| (s, Vec::new())).collect();
        for c in self.children(info.host) {
            if self.is_slottable(c) {
                if let Some(&i) = first.get(self.slottable_name(c)) {
                    out[i].1.push(c);
                }
            }
        }
        out
    }

    /// A slot's manually assigned nodes.
    pub fn manually_assigned(&self, slot: NodeId) -> &[NodeId] {
        self.shadow.manual.get(&slot).map_or(&[], Vec::as_slice)
    }

    /// Replace every manual slot assignment (HTMLSlotElement.assign, HTML §4.12.4; the host
    /// keeps the authoritative list, as slots and nodes can be in different documents):
    /// each slot with its manually assigned nodes of this document.
    pub fn set_manual_assignments(&mut self, slots: Vec<(NodeId, Vec<NodeId>)>) {
        let mut dirty: Vec<NodeId> = self.shadow.manual.keys().copied().collect();
        self.shadow.manual.clear();
        self.shadow.manual_slot.clear();
        for (slot, nodes) in slots {
            for &n in &nodes {
                self.shadow.manual_slot.insert(n, slot);
            }
            dirty.push(slot);
            self.shadow.manual.insert(slot, nodes);
        }
        for s in dirty {
            if let Some(h) = self.containing_shadow_root(s).and_then(|r| self.shadow_host(r)) {
                self.mark_style_dirty(h);
                self.mark_layout_dirty(h);
            }
        }
    }

    /// The flat tree children of `id` (CSS Scoping §2.2): a shadow host's are its shadow
    /// root's children, a slot's in a shadow tree are its assigned nodes (or its own children
    /// when it has none); everyone else's are their children.
    pub fn flat_children(&self, id: NodeId) -> Vec<NodeId> {
        if self.shadow.roots.is_empty() {
            return self.children(id).collect();
        }
        if let Some(sr) = self.shadow_root(id) {
            return self.children(sr).collect();
        }
        if self.is_slot(id) && self.is_shadow_root(self.tree_root(id)) {
            let assigned = self.find_slottables(id);
            if !assigned.is_empty() {
                return assigned;
            }
        }
        self.children(id).collect()
    }

    /// The parent of `id` in the flat tree: a slotted node's slot, a shadow tree's top-level
    /// node's host, else the parent; None for a host's child that isn't slotted.
    pub fn flat_parent(&self, id: NodeId) -> Option<NodeId> {
        let p = self.parent(id)?;
        if self.shadow.roots.is_empty() {
            return Some(p);
        }
        if self.shadow_root(p).is_some() {
            return self.find_slot(id, false);
        }
        if let Some(h) = self.shadow_host(p) {
            return Some(h);
        }
        if self.is_slot(p) && self.is_shadow_root(self.tree_root(p)) && !self.find_slottables(p).is_empty() {
            return None;
        }
        Some(p)
    }

    /// Copy shadow root `from` of document `src` (its state, not its children) onto `host`.
    pub(crate) fn copy_shadow_from(&mut self, src: &Document, from: NodeId, host: NodeId) -> Option<NodeId> {
        let info = src.shadow_info(from)?.clone();
        (self.shadow_root(host).is_none()).then(|| self.attach_shadow(host, info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Namespace;

    fn el(d: &mut Document, name: &str) -> NodeId {
        d.create_element(Namespace::Html, name, Vec::new())
    }

    #[test]
    fn slots_and_connectedness() {
        let mut d = Document::new();
        let host = el(&mut d, "div");
        d.append_child(Document::ROOT, host);
        let a = el(&mut d, "span");
        d.set_attr(a, "slot", "x");
        let b = d.create_text("b");
        d.append_child(host, a);
        d.append_child(host, b);
        let sr = d.attach_shadow(host, ShadowRootInfo { open: true, ..Default::default() });
        let named = el(&mut d, "slot");
        d.set_attr(named, "name", "x");
        let default = el(&mut d, "slot");
        d.append_child(sr, named);
        d.append_child(sr, default);
        assert!(d.is_connected(named));
        assert_eq!(d.shadow_including_root(named), Document::ROOT);
        assert_eq!(d.find_slot(a, false), Some(named));
        assert_eq!(d.find_slot(b, false), Some(default));
        assert_eq!(d.find_slottables(default), vec![b]);
        assert_eq!(d.flat_children(host), vec![named, default]);
        assert_eq!(d.flat_children(named), vec![a]);
        assert_eq!(d.flat_parent(a), Some(named));
        assert_eq!(d.shadow_including_descendants(host), vec![host, sr, named, default, a, b]);
        d.detach(host);
        assert!(!d.is_connected(named));
    }

    #[test]
    fn manual_assignment() {
        let mut d = Document::new();
        let host = el(&mut d, "div");
        let a = el(&mut d, "span");
        d.append_child(host, a);
        let sr = d.attach_shadow(
            host,
            ShadowRootInfo { open: true, slot_assignment: SlotAssignment::Manual, ..Default::default() },
        );
        let s1 = el(&mut d, "slot");
        let s2 = el(&mut d, "slot");
        d.append_child(sr, s1);
        d.append_child(sr, s2);
        assert!(d.find_slottables(s1).is_empty());
        d.set_manual_assignments(vec![(s1, vec![a])]);
        assert_eq!(d.find_slottables(s1), vec![a]);
        d.set_manual_assignments(vec![(s2, vec![a])]);
        assert!(d.find_slottables(s1).is_empty());
        assert_eq!(d.find_slot(a, false), Some(s2));
    }
}

//! `<select>`, `<option>` and `<selectedcontent>` state (HTML §4.10.7, §4.10.10, §4.10.17).
//!
//! An option's *selectedness* is DOM state, not an attribute, and a `<selectedcontent>` inside
//! a select's `<button>` mirrors the selected option's content. Both are maintained by DOM
//! hooks: the HTML element insertion, post-connection and removing steps run from
//! [`Document::insert_before`] and [`Document::detach`], the attribute change steps from
//! [`Document::set_attr`] / [`Document::remove_attr`], and the parser calls
//! [`Document::option_children_parsed`] when it pops an `<option>`.
//!
//! "Queue a microtask" is modelled by the per-select *pending selectedcontent update* flag;
//! whoever owns the event loop calls [`Document::run_select_microtasks`] at a microtask
//! checkpoint (the parser does at "stop parsing").

use crate::{Document, NodeData, NodeId};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Clone, Debug, Default)]
pub(crate) struct SelectState {
    /// Number of `option` and `selectedcontent` elements created; the hooks do nothing while
    /// it is zero, so documents without them pay nothing.
    tracked: usize,
    /// Options whose selectedness is true.
    selected: HashSet<NodeId>,
    /// Options whose dirtiness is true (set by user interaction or script; never by parsing).
    dirty: HashSet<NodeId>,
    /// An option's cached nearest ancestor select element.
    option_select: HashMap<NodeId, NodeId>,
    /// Selects with a pending selectedcontent update (an ordered set: deterministic order).
    pending: BTreeSet<NodeId>,
    /// `selectedcontent` elements whose disabled flag is true.
    sc_disabled: HashSet<NodeId>,
    /// A `selectedcontent`'s cached select.
    sc_select: HashMap<NodeId, NodeId>,
    /// Set while a selectedcontent's children are being replaced, so the insertions that does
    /// can't recursively start another update (e.g. an `<option>` cloned from an option that
    /// script gave option children).
    updating: bool,
}

/// HTML §2.3.4.1 rules for parsing non-negative integers.
fn parse_non_negative_integer(s: &str) -> Option<u64> {
    let s = s.trim_start_matches([' ', '\t', '\n', '\x0C', '\r']);
    let s = s.strip_prefix('+').unwrap_or(s);
    let digits: &str = &s[..s.bytes().take_while(u8::is_ascii_digit).count()];
    if digits.is_empty() {
        return None;
    }
    Some(digits.bytes().fold(0u64, |v, d| v.saturating_mul(10).saturating_add(u64::from(d - b'0'))))
}

impl Document {
    /// Called for every new element: an option's selectedness starts as "has a `selected`
    /// attribute" (§4.10.10).
    pub(crate) fn select_element_created(&mut self, id: NodeId) {
        if self.is_html(id, "option") {
            self.select_state.tracked += 1;
            if self.attr(id, "selected").is_some() {
                self.select_state.selected.insert(id);
            }
        } else if self.is_html(id, "selectedcontent") {
            self.select_state.tracked += 1;
        }
    }

    /// An option element's selectedness.
    pub fn option_selectedness(&self, option: NodeId) -> bool {
        self.select_state.selected.contains(&option)
    }

    /// Whether `id` is connected (DOM §4.4: its shadow-including root is the document).
    pub fn is_connected(&self, id: NodeId) -> bool {
        self.shadow_including_root(id) == Document::ROOT
    }

    /// §4.10.10 "get the nearest ancestor select".
    pub fn nearest_ancestor_select(&self, element: NodeId) -> Option<NodeId> {
        let mut optgroup = None;
        for a in self.ancestors(element) {
            if self.is_html(a, "datalist") || self.is_html(a, "hr") || self.is_html(a, "option") {
                return None;
            }
            if self.is_html(a, "optgroup") {
                if optgroup.is_some() {
                    return None;
                }
                optgroup = Some(a);
            }
            if self.is_html(a, "select") {
                return Some(a);
            }
        }
        None
    }

    /// Pre-order successor of `node` within `root`, skipping `node`'s descendants.
    fn next_skipping_children(&self, node: NodeId, root: NodeId) -> Option<NodeId> {
        let mut cur = node;
        loop {
            if cur == root {
                return None;
            }
            if let Some(s) = self.next_sibling(cur) {
                return Some(s);
            }
            cur = self.parent(cur)?;
        }
    }

    /// §4.10.7 "get the list of options" of a select element.
    pub fn list_of_options(&self, select: NodeId) -> Vec<NodeId> {
        let mut options = Vec::new();
        let mut node = self.first_child(select);
        while let Some(n) = node {
            if self.is_html(n, "option") {
                options.push(n);
            }
            let skip = self.is_html(n, "select")
                || self.is_html(n, "hr")
                || self.is_html(n, "option")
                || self.is_html(n, "datalist")
                || (self.is_html(n, "optgroup")
                    && self.ancestors(n).take_while(|&a| a != select).any(|a| self.is_html(a, "optgroup")));
            node =
                if skip { self.next_skipping_children(n, select) } else { self.next_in_preorder(n, select) };
        }
        options
    }

    /// §4.10.10: an option is disabled if it has a `disabled` attribute or is a child of an
    /// `optgroup` that has one.
    pub fn option_disabled(&self, option: NodeId) -> bool {
        self.attr(option, "disabled").is_some()
            || self
                .parent(option)
                .is_some_and(|p| self.is_html(p, "optgroup") && self.attr(p, "disabled").is_some())
    }

    /// §4.10.7 display size.
    fn display_size(&self, select: NodeId) -> u64 {
        match self.attr(select, "size").and_then(parse_non_negative_integer) {
            Some(n) => n,
            None if self.attr(select, "multiple").is_some() => 4,
            None => 1,
        }
    }

    /// §4.10.7 "selectedness setting algorithm". Returns whether anything changed.
    fn selectedness_setting(&mut self, select: NodeId) -> bool {
        if self.attr(select, "multiple").is_some() {
            return false;
        }
        let mut changed = false;
        let mut first_enabled = None;
        let mut last_selected: Option<NodeId> = None;
        for option in self.list_of_options(select) {
            if self.select_state.selected.contains(&option) {
                if let Some(prev) = last_selected {
                    self.select_state.selected.remove(&prev);
                    changed = true;
                }
                last_selected = Some(option);
            }
            if first_enabled.is_none() && !self.option_disabled(option) {
                first_enabled = Some(option);
            }
        }
        if last_selected.is_none() && self.display_size(select) == 1 {
            if let Some(first) = first_enabled {
                self.select_state.selected.insert(first);
                changed = true;
            }
        }
        changed
    }

    /// §4.10.10 "update an option's nearest ancestor select": the selects whose
    /// selectedcontent elements need updating.
    fn update_option_nearest_select(&mut self, option: NodeId) -> Vec<NodeId> {
        let old = self.select_state.option_select.get(&option).copied();
        let new = self.nearest_ancestor_select(option);
        match new {
            Some(s) => self.select_state.option_select.insert(option, s),
            None => self.select_state.option_select.remove(&option),
        };
        if old == new {
            return Vec::new();
        }
        let mut result = Vec::new();
        for select in [old, new].into_iter().flatten() {
            let changed = self.selectedness_setting(select);
            if self.option_selectedness(option) || changed {
                result.push(select);
            }
        }
        result
    }

    fn has_descendant_selectedcontent(&self, select: NodeId) -> bool {
        self.descendants(select).skip(1).any(|d| self.is_html(d, "selectedcontent"))
    }

    /// §4.10.7 "queue an update of a select's descendant selectedcontent elements".
    fn queue_selectedcontent_update(&mut self, select: NodeId) {
        if self.select_state.pending.contains(&select) || !self.has_descendant_selectedcontent(select) {
            return;
        }
        self.select_state.pending.insert(select);
    }

    /// The user picked `option` in `select` (§4.10.7): in a single select it becomes the only
    /// selected option; in a `multiple` select its selectedness toggles. Its dirtiness becomes
    /// true and selectedcontent elements are updated. Returns whether anything changed.
    pub fn user_select_option(&mut self, select: NodeId, option: NodeId) -> bool {
        let options = self.list_of_options(select);
        if !options.contains(&option) || self.option_disabled(option) {
            return false;
        }
        let before: Vec<bool> = options.iter().map(|&o| self.option_selectedness(o)).collect();
        if self.attr(select, "multiple").is_some() {
            if !self.select_state.selected.remove(&option) {
                self.select_state.selected.insert(option);
            }
        } else {
            for &o in &options {
                self.select_state.selected.remove(&o);
            }
            self.select_state.selected.insert(option);
        }
        self.select_state.dirty.insert(option);
        let after: Vec<bool> = options.iter().map(|&o| self.option_selectedness(o)).collect();
        if before == after {
            return false;
        }
        self.generation += 1;
        self.queue_selectedcontent_update(select);
        self.run_select_microtasks();
        true
    }

    /// §4.10.7 the select's reset algorithm: dirtiness false, selectedness = has `selected`,
    /// then the selectedness setting algorithm.
    pub(crate) fn reset_select(&mut self, select: NodeId) {
        for o in self.list_of_options(select) {
            self.select_state.dirty.remove(&o);
            if self.attr(o, "selected").is_some() {
                self.select_state.selected.insert(o);
            } else {
                self.select_state.selected.remove(&o);
            }
        }
        self.selectedness_setting(select);
        self.queue_selectedcontent_update(select);
        self.run_select_microtasks();
    }

    /// Run the queued selectedcontent updates (the microtasks).
    pub fn run_select_microtasks(&mut self) {
        let pending: Vec<NodeId> = self.select_state.pending.iter().copied().collect();
        for select in pending {
            if self.select_state.pending.contains(&select) {
                self.update_select_selectedcontents(select);
            }
        }
    }

    /// §4.10.7 "update a select's descendant selectedcontent elements".
    fn update_select_selectedcontents(&mut self, select: NodeId) {
        self.select_state.pending.remove(&select);
        if self.attr(select, "multiple").is_some() {
            return;
        }
        let targets: Vec<NodeId> = self
            .descendants(select)
            .skip(1)
            .filter(|&d| self.is_html(d, "selectedcontent") && !self.select_state.sc_disabled.contains(&d))
            .collect();
        for sc in targets {
            self.update_selectedcontent(select, sc);
        }
    }

    /// §4.10.17 "update a selectedcontent".
    fn update_selectedcontent(&mut self, select: NodeId, sc: NodeId) {
        if self.select_state.sc_disabled.contains(&sc) || self.select_state.updating {
            return;
        }
        let option =
            self.list_of_options(select).into_iter().find(|&o| self.select_state.selected.contains(&o));
        self.select_state.updating = true;
        // "Replace all": remove the old children, then insert the new ones.
        while let Some(c) = self.first_child(sc) {
            self.detach(c);
        }
        if let Some(option) = option {
            // §4.10.17 "clone an option into a selectedcontent".
            let children: Vec<NodeId> = self.children(option).collect();
            let clones: Vec<NodeId> = children.into_iter().map(|c| self.clone_node(c, true)).collect();
            let mut removed = Vec::new();
            for &clone in &clones {
                for d in self.descendants(clone) {
                    let drop = self.element(d).is_some_and(|e| match e.ns {
                        crate::Namespace::Html => {
                            matches!(e.name.as_str(), "script" | "frame" | "iframe" | "embed" | "object")
                        }
                        crate::Namespace::Svg => e.name == "script",
                        _ => false,
                    });
                    if drop {
                        removed.push(d);
                    }
                }
            }
            let mut kept = Vec::new();
            for clone in clones {
                if removed.contains(&clone) {
                    continue; // a top-level child of the fragment that is removed
                }
                kept.push(clone);
            }
            for r in removed {
                self.detach(r);
            }
            for clone in kept {
                self.append_child(sc, clone);
            }
        }
        self.select_state.updating = false;
    }

    /// §4.10.17 "recalculate a selectedcontent element's disabledness". Returns its select.
    fn recalculate_selectedcontent_disabledness(&mut self, sc: NodeId) -> Option<NodeId> {
        let mut nearest = None;
        let mut disabled = false;
        for a in self.ancestors(sc) {
            if self.is_html(a, "select") {
                if nearest.is_none() {
                    nearest = Some(a);
                    continue;
                }
                disabled = true;
                break;
            }
            if self.is_html(a, "option") || self.is_html(a, "selectedcontent") {
                disabled = true;
                break;
            }
        }
        if disabled {
            self.select_state.sc_disabled.insert(sc);
        } else {
            self.select_state.sc_disabled.remove(&sc);
        }
        match nearest {
            Some(s) => self.select_state.sc_select.insert(sc, s),
            None => self.select_state.sc_select.remove(&sc),
        };
        nearest
    }

    fn tracked_inclusive_descendants(&self, root: NodeId) -> Vec<NodeId> {
        self.descendants(root)
            .filter(|&d| self.is_html(d, "option") || self.is_html(d, "selectedcontent"))
            .collect()
    }

    /// DOM "insert" hooks for `node` and its inclusive descendants: the HTML element insertion
    /// steps, then the post-connection steps (§4.10.10, §4.10.17).
    pub(crate) fn select_after_insert(&mut self, node: NodeId) {
        if self.select_state.tracked == 0 {
            return;
        }
        let nodes = self.tracked_inclusive_descendants(node);
        if nodes.is_empty() {
            return;
        }
        let connected = self.is_connected(node);
        for &n in &nodes {
            if self.is_html(n, "option") {
                let selects = self.update_option_nearest_select(n);
                if connected {
                    for s in selects {
                        self.queue_selectedcontent_update(s);
                    }
                }
            } else {
                let select = self.recalculate_selectedcontent_disabledness(n);
                if !connected || self.select_state.sc_disabled.contains(&n) {
                    continue;
                }
                if let Some(s) = select.filter(|&s| self.attr(s, "multiple").is_none()) {
                    self.queue_selectedcontent_update(s);
                }
            }
        }
        if !connected {
            return;
        }
        for n in nodes {
            let select = if self.is_html(n, "option") {
                self.select_state.option_select.get(&n).copied()
            } else {
                self.select_state.sc_select.get(&n).copied()
            };
            if let Some(s) = select.filter(|s| self.select_state.pending.contains(s)) {
                if !self.select_state.updating {
                    self.update_select_selectedcontents(s);
                }
            }
        }
    }

    /// DOM "remove" hooks for `node` and its inclusive descendants (the removing steps).
    pub(crate) fn select_after_remove(&mut self, node: NodeId) {
        if self.select_state.tracked == 0 {
            return;
        }
        for n in self.tracked_inclusive_descendants(node) {
            if self.is_html(n, "option") {
                for s in self.update_option_nearest_select(n) {
                    self.queue_selectedcontent_update(s);
                }
            } else {
                self.recalculate_selectedcontent_disabledness(n);
            }
        }
    }

    /// The option and select attribute change steps (§4.10.10, §4.10.7).
    pub(crate) fn select_attribute_changed(
        &mut self,
        element: NodeId,
        name: &str,
        old: Option<&str>,
        new: Option<&str>,
    ) {
        if self.is_html(element, "option") {
            if !matches!(name, "selected" | "disabled") || (old.is_some() && new.is_some()) {
                return;
            }
            let mut changed = false;
            if name == "selected" && !self.select_state.dirty.contains(&element) {
                let selected = self.option_selectedness(element);
                if old.is_none() && !selected {
                    self.select_state.selected.insert(element);
                    // §4.10.7: in a select without `multiple`, an option becoming selected
                    // deselects all the others in its list of options.
                    if let Some(select) = self.select_state.option_select.get(&element).copied() {
                        if self.attr(select, "multiple").is_none() {
                            for o in self.list_of_options(select) {
                                if o != element {
                                    self.select_state.selected.remove(&o);
                                }
                            }
                        }
                    }
                    changed = true;
                }
                if new.is_none() && selected {
                    self.select_state.selected.remove(&element);
                    changed = true;
                }
            }
            if let Some(select) = self.select_state.option_select.get(&element).copied() {
                self.reset_select_selectedness(select, changed);
            }
        } else if self.is_html(element, "select") {
            if name == "size" {
                self.reset_select_selectedness(element, false);
            }
            if name != "multiple" || (old.is_some() && new.is_some()) {
                return;
            }
            if new.is_none() {
                // Keep only the first selected option selected.
                let mut seen = false;
                for o in self.list_of_options(element) {
                    if self.select_state.selected.contains(&o) {
                        if seen {
                            self.select_state.selected.remove(&o);
                        }
                        seen = true;
                    }
                }
            }
            self.reset_select_selectedness(element, true);
        }
    }

    /// §4.10.7 "reset a select's selectedness".
    fn reset_select_selectedness(&mut self, select: NodeId, mut changed: bool) {
        if self.selectedness_setting(select) {
            changed = true;
        }
        if changed {
            self.update_select_selectedcontents(select);
        }
    }

    /// §4.10.10 "option HTML element children parsed steps" (run by the parser when it pops an
    /// option off the stack of open elements).
    pub fn option_children_parsed(&mut self, option: NodeId) {
        let Some(select) = self.select_state.option_select.get(&option).copied() else { return };
        if !self.option_selectedness(option) {
            return;
        }
        self.update_select_selectedcontents(select);
    }

    /// DOM §4.4 "clone a node" into this document (`deep` = clone the subtree too). Template
    /// contents are cloned along with a template element (HTML §4.12.3 cloning steps), and a
    /// clonable shadow root along with its host (whatever `deep` is).
    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> NodeId {
        self.clone_node_pairs(id, deep).0
    }

    /// [`Document::clone_node`], also returning the (original, copy) pairs of every node
    /// copied, in shadow-including tree order.
    pub fn clone_node_pairs(&mut self, id: NodeId, deep: bool) -> (NodeId, Vec<(NodeId, NodeId)>) {
        let mut pairs = Vec::new();
        let copy = self.clone_rec(id, deep, &mut pairs);
        (copy, pairs)
    }

    fn clone_rec(&mut self, id: NodeId, deep: bool, pairs: &mut Vec<(NodeId, NodeId)>) -> NodeId {
        let data = self.node(id).data.clone();
        let copy = match data {
            NodeData::Element(e) => {
                let copy = self.create_element(e.ns, &e.name, e.attrs.clone());
                if let Some(el) = self.element_mut(copy) {
                    el.prefix = e.prefix.clone();
                }
                pairs.push((id, copy));
                if deep {
                    if let (Some(from), Some(to)) =
                        (e.template_contents, self.element(copy).and_then(|c| c.template_contents))
                    {
                        let kids: Vec<NodeId> = self.children(from).collect();
                        for k in kids {
                            let c = self.clone_rec(k, true, pairs);
                            self.append_child(to, c);
                        }
                    }
                }
                if let Some(sr) = self.shadow_root(id) {
                    if let Some(info) = self.shadow_info(sr).filter(|i| i.clonable).cloned() {
                        let to = self.attach_shadow(copy, info);
                        pairs.push((sr, to));
                        let kids: Vec<NodeId> = self.children(sr).collect();
                        for k in kids {
                            let c = self.clone_rec(k, true, pairs);
                            self.append_child(to, c);
                        }
                    }
                }
                copy
            }
            other => {
                let cdata = self.is_cdata(id);
                let c = self.alloc(other);
                if cdata {
                    self.set_cdata(c);
                }
                pairs.push((id, c));
                c
            }
        };
        if deep {
            let kids: Vec<NodeId> = self.children(id).collect();
            for k in kids {
                let c = self.clone_rec(k, true, pairs);
                self.append_child(copy, c);
            }
        }
        copy
    }
}

#[cfg(test)]
mod tests {
    use crate::{Attribute, Document, Namespace};

    fn el(d: &mut Document, name: &str, attrs: &[(&str, &str)]) -> crate::NodeId {
        d.create_element(Namespace::Html, name, attrs.iter().map(|(k, v)| Attribute::new(k, v)).collect())
    }

    #[test]
    fn first_enabled_option_becomes_selected() {
        let mut d = Document::new();
        let select = el(&mut d, "select", &[]);
        d.append_child(Document::ROOT, select);
        let a = el(&mut d, "option", &[("disabled", "")]);
        let b = el(&mut d, "option", &[]);
        d.append_child(select, a);
        d.append_child(select, b);
        assert!(!d.option_selectedness(a));
        assert!(d.option_selectedness(b));
        // A later `selected` attribute moves the selection.
        d.set_attr(a, "selected", "");
        assert!(d.option_selectedness(a) && !d.option_selectedness(b));
    }

    #[test]
    fn selectedcontent_mirrors_the_selected_option() {
        let mut d = Document::new();
        let select = el(&mut d, "select", &[]);
        d.append_child(Document::ROOT, select);
        let button = el(&mut d, "button", &[]);
        d.append_child(select, button);
        let sc = el(&mut d, "selectedcontent", &[]);
        d.append_child(button, sc);
        let o = el(&mut d, "option", &[]);
        d.append_child(select, o);
        d.append_text(o, "Hi");
        let script = el(&mut d, "script", &[]);
        d.append_child(o, script);
        d.option_children_parsed(o);
        assert_eq!(d.text_content(sc), "Hi");
        assert!(d.children(sc).all(|c| !d.is_html(c, "script")), "scripts are not cloned");
        // Removing the option clears the mirror.
        d.detach(o);
        d.run_select_microtasks();
        assert_eq!(d.first_child(sc), None);
    }

    #[test]
    fn nested_select_disables_selectedcontent() {
        let mut d = Document::new();
        let outer = el(&mut d, "select", &[]);
        d.append_child(Document::ROOT, outer);
        let inner = el(&mut d, "select", &[]);
        d.append_child(outer, inner);
        let sc = el(&mut d, "selectedcontent", &[]);
        d.append_child(inner, sc);
        let o = el(&mut d, "option", &[]);
        d.append_child(outer, o);
        d.append_text(o, "x");
        d.option_children_parsed(o);
        assert_eq!(d.first_child(sc), None);
    }
}

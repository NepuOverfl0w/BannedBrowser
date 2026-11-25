//! `bb-dom` — an arena-based DOM.
//!
//! Nodes live in a `Vec` inside [`Document`] and refer to each other with
//! [`NodeId`] indices (no `Rc` cycles). Removing a node detaches it; the slot
//! stays allocated so ids remain valid for the document's lifetime.
//!
//! Every mutation bumps [`Document::generation`] and marks dirty flags so the
//! style/layout stages can later do incremental work.

mod dump;
pub mod form_data;
mod forms;
mod select;
mod shadow;
pub use dump::{
    dump_html5lib, is_void, serialize_children_html, serialize_html, serialize_shadow_template, Shadows,
};
pub use forms::{sanitize_value, FaceSubmission, InputType, FORM_ASSOCIATED, LISTED};
pub use shadow::{ShadowRootInfo, SlotAssignment};

/// CR LF and lone CR become LF (HTML's newline normalization).
pub fn normalize_newlines(s: &str) -> String {
    if s.contains('\r') {
        s.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        s.to_string()
    }
}

use bitflags::bitflags;

/// Index of a node in its document's arena.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Default)]
pub struct NodeId(pub u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// XML namespaces the HTML parser knows about (Infra §8).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Namespace {
    None,
    Html,
    Svg,
    MathMl,
    XLink,
    Xml,
    Xmlns,
    /// Any other namespace (scripts can use any URL): an index into the interned URLs.
    Other(u32),
}

/// Interned namespace URLs for `Namespace::Other`. They live for the process (scripts can
/// add new ones; each is a short string).
static OTHER_NAMESPACES: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());

impl Namespace {
    /// The namespace with URL `url` ("" is no namespace).
    pub fn from_url(url: &str) -> Namespace {
        const KNOWN: [Namespace; 7] = [
            Namespace::None,
            Namespace::Html,
            Namespace::Svg,
            Namespace::MathMl,
            Namespace::XLink,
            Namespace::Xml,
            Namespace::Xmlns,
        ];
        if let Some(k) = KNOWN.iter().find(|k| k.url() == url) {
            return *k;
        }
        let mut t = OTHER_NAMESPACES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = t.iter().position(|u| *u == url) {
            return Namespace::Other(i as u32);
        }
        t.push(Box::leak(url.to_string().into_boxed_str()));
        Namespace::Other(t.len() as u32 - 1)
    }

    pub fn url(self) -> &'static str {
        match self {
            Namespace::Other(i) => {
                let t = OTHER_NAMESPACES.lock().unwrap_or_else(|e| e.into_inner());
                t.get(i as usize).copied().unwrap_or("")
            }
            Namespace::None => "",
            Namespace::Html => "http://www.w3.org/1999/xhtml",
            Namespace::Svg => "http://www.w3.org/2000/svg",
            Namespace::MathMl => "http://www.w3.org/1998/Math/MathML",
            Namespace::XLink => "http://www.w3.org/1999/xlink",
            Namespace::Xml => "http://www.w3.org/XML/1998/namespace",
            Namespace::Xmlns => "http://www.w3.org/2000/xmlns/",
        }
    }
}

/// Document quirks mode (DOM §4.5).
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum QuirksMode {
    #[default]
    NoQuirks,
    LimitedQuirks,
    Quirks,
}

/// An element attribute.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Attribute {
    pub ns: Namespace,
    pub prefix: Option<String>,
    /// Local name (lowercase for HTML attributes).
    pub name: String,
    pub value: String,
}

impl Attribute {
    pub fn new(name: &str, value: &str) -> Self {
        Attribute { ns: Namespace::None, prefix: None, name: name.to_string(), value: value.to_string() }
    }
}

/// Element-specific data.
#[derive(Clone, Debug)]
pub struct ElementData {
    pub ns: Namespace,
    /// Local name (lowercase for HTML elements).
    pub name: String,
    pub attrs: Vec<Attribute>,
    /// For `<template>`: the DocumentFragment holding its contents.
    pub template_contents: Option<NodeId>,
    /// The namespace prefix (elements made with `createElementNS("…", "p:x")`).
    pub prefix: Option<String>,
}

impl ElementData {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|a| a.ns == Namespace::None && a.name == name).map(|a| a.value.as_str())
    }

    pub fn has_attr(&self, name: &str) -> bool {
        self.attr(name).is_some()
    }

    pub fn id(&self) -> Option<&str> {
        self.attr("id")
    }

    /// Whitespace-separated class list.
    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attr("class").unwrap_or("").split_ascii_whitespace()
    }

    pub fn has_class(&self, c: &str) -> bool {
        self.classes().any(|x| x == c)
    }

    pub fn is_html(&self, name: &str) -> bool {
        self.ns == Namespace::Html && self.name == name
    }
}

/// The kind-specific payload of a node.
#[derive(Clone, Debug)]
pub enum NodeData {
    Document,
    DocumentFragment,
    Doctype {
        name: String,
        public_id: String,
        system_id: String,
    },
    Element(ElementData),
    Text(String),
    Comment(String),
    /// DOM §4.13 `ProcessingInstruction`: a target name and data (a character-data node).
    ProcessingInstruction {
        target: String,
        data: String,
    },
}

bitflags! {
    /// Per-node damage / state flags.
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
    pub struct NodeFlags: u8 {
        const STYLE_DIRTY = 1;
        const DESCENDANT_STYLE_DIRTY = 2;
        const LAYOUT_DIRTY = 4;
        /// `:hover` matches.
        const HOVER = 8;
        const ACTIVE = 16;
        const FOCUS = 32;
        const VISITED = 64;
        /// A Text node that is a CDATASection (DOM §4.12).
        const CDATA = 128;
    }
}

/// A DOM node.
#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub first_child: Option<NodeId>,
    pub last_child: Option<NodeId>,
    pub prev_sibling: Option<NodeId>,
    pub next_sibling: Option<NodeId>,
    pub data: NodeData,
    pub flags: NodeFlags,
}

impl Node {
    fn new(data: NodeData) -> Self {
        Node {
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
            data,
            flags: NodeFlags::STYLE_DIRTY | NodeFlags::LAYOUT_DIRTY,
        }
    }

    pub fn as_element(&self) -> Option<&ElementData> {
        match &self.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match &self.data {
            NodeData::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn is_element(&self) -> bool {
        matches!(self.data, NodeData::Element(_))
    }
}

/// A document: the node arena plus document-level state.
#[derive(Clone, Debug)]
pub struct Document {
    nodes: Vec<Node>,
    pub quirks_mode: QuirksMode,
    /// The document's URL (string form; the engine owns the parsed `Url`).
    pub url: String,
    /// Incremented on every mutation.
    pub generation: u64,
    /// Option selectedness and `<selectedcontent>` state (see `select.rs`).
    select_state: select::SelectState,
    /// Form control values, checkedness and parser-set form owners (see `forms.rs`).
    form_state: forms::FormState,
    /// Shadow roots and manual slot assignments (see `shadow.rs`).
    shadow: shadow::ShadowState,
    /// Custom element "defined"-ness the host recorded (see [`Document::is_defined`]).
    custom_defined: std::collections::HashMap<NodeId, bool>,
    /// Custom elements' states (ElementInternals.states, CSS `:state()`).
    custom_states: std::collections::HashMap<NodeId, Vec<String>>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// The id of the Document node itself.
    pub const ROOT: NodeId = NodeId(0);

    pub fn new() -> Self {
        Document {
            nodes: vec![Node::new(NodeData::Document)],
            quirks_mode: QuirksMode::NoQuirks,
            url: "about:blank".to_string(),
            generation: 0,
            select_state: Default::default(),
            form_state: Default::default(),
            shadow: Default::default(),
            custom_defined: Default::default(),
            custom_states: Default::default(),
        }
    }

    pub fn root(&self) -> NodeId {
        Self::ROOT
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.generation += 1;
        &mut self.nodes[id.index()]
    }

    fn alloc(&mut self, data: NodeData) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node::new(data));
        id
    }

    pub fn create_element(&mut self, ns: Namespace, name: &str, attrs: Vec<Attribute>) -> NodeId {
        let is_template = ns == Namespace::Html && name == "template";
        let id = self.alloc(NodeData::Element(ElementData {
            ns,
            name: name.to_string(),
            attrs,
            template_contents: None,
            prefix: None,
        }));
        if is_template {
            let frag = self.alloc(NodeData::DocumentFragment);
            if let NodeData::Element(e) = &mut self.nodes[id.index()].data {
                e.template_contents = Some(frag);
            }
        }
        self.select_element_created(id);
        id
    }

    pub fn create_html_element(&mut self, name: &str) -> NodeId {
        self.create_element(Namespace::Html, name, Vec::new())
    }

    pub fn create_text(&mut self, text: &str) -> NodeId {
        self.alloc(NodeData::Text(text.to_string()))
    }

    /// A CDATASection node: a Text node flagged [`NodeFlags::CDATA`].
    pub fn create_cdata(&mut self, text: &str) -> NodeId {
        let id = self.create_text(text);
        self.nodes[id.index()].flags |= NodeFlags::CDATA;
        id
    }

    /// Mark Text node `id` as a CDATASection.
    pub fn set_cdata(&mut self, id: NodeId) {
        self.nodes[id.index()].flags |= NodeFlags::CDATA;
    }

    /// Whether node `id` is a CDATASection.
    pub fn is_cdata(&self, id: NodeId) -> bool {
        self.nodes[id.index()].flags.contains(NodeFlags::CDATA)
    }

    pub fn create_comment(&mut self, text: &str) -> NodeId {
        self.alloc(NodeData::Comment(text.to_string()))
    }

    pub fn create_processing_instruction(&mut self, target: &str, data: &str) -> NodeId {
        self.alloc(NodeData::ProcessingInstruction { target: target.to_string(), data: data.to_string() })
    }

    pub fn create_doctype(&mut self, name: &str, public_id: &str, system_id: &str) -> NodeId {
        self.alloc(NodeData::Doctype {
            name: name.to_string(),
            public_id: public_id.to_string(),
            system_id: system_id.to_string(),
        })
    }

    pub fn create_fragment(&mut self) -> NodeId {
        self.alloc(NodeData::DocumentFragment)
    }

    // ----- tree navigation -----

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).parent
    }

    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).first_child
    }

    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).last_child
    }

    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).next_sibling
    }

    pub fn prev_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).prev_sibling
    }

    /// Iterate over the children of `id`.
    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children { doc: self, next: self.node(id).first_child }
    }

    /// Element children only.
    pub fn element_children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.children(id).filter(move |&c| self.node(c).is_element())
    }

    /// Pre-order traversal of `id` and its descendants (not into template contents).
    pub fn descendants(&self, id: NodeId) -> Descendants<'_> {
        Descendants { doc: self, root: id, next: Some(id) }
    }

    /// Ancestors of `id`, nearest first (excluding `id`).
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(self.parent(id), move |&p| self.parent(p))
    }

    /// Pre-order successor of `id` within `root`.
    pub fn next_in_preorder(&self, id: NodeId, root: NodeId) -> Option<NodeId> {
        if let Some(c) = self.first_child(id) {
            return Some(c);
        }
        let mut cur = id;
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

    pub fn element(&self, id: NodeId) -> Option<&ElementData> {
        self.node(id).as_element()
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut ElementData> {
        self.generation += 1;
        match &mut self.nodes[id.index()].data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }

    /// Whether `id` is an HTML element named `name`.
    pub fn is_html(&self, id: NodeId, name: &str) -> bool {
        self.element(id).is_some_and(|e| e.is_html(name))
    }

    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.element(id).and_then(|e| e.attr(name))
    }

    /// Set (or add) a no-namespace attribute.
    pub fn set_attr(&mut self, id: NodeId, name: &str, value: &str) {
        let old = self.attr(id, name).map(str::to_string);
        if let Some(e) = self.element_mut(id) {
            if let Some(a) = e.attrs.iter_mut().find(|a| a.ns == Namespace::None && a.name == name) {
                a.value = value.to_string();
            } else {
                e.attrs.push(Attribute::new(name, value));
            }
        }
        self.mark_style_dirty(id);
        self.select_attribute_changed(id, name, old.as_deref(), Some(value));
    }

    pub fn remove_attr(&mut self, id: NodeId, name: &str) {
        let old = self.attr(id, name).map(str::to_string);
        if let Some(e) = self.element_mut(id) {
            e.attrs.retain(|a| !(a.ns == Namespace::None && a.name == name));
        }
        self.mark_style_dirty(id);
        if old.is_some() {
            self.select_attribute_changed(id, name, old.as_deref(), None);
        }
    }

    // ----- mutation -----

    /// Detach `id` from its parent (DOM "remove").
    pub fn detach(&mut self, id: NodeId) {
        let (parent, prev, next) = {
            let n = self.node(id);
            (n.parent, n.prev_sibling, n.next_sibling)
        };
        let Some(parent) = parent else { return };
        self.generation += 1;
        match prev {
            Some(p) => self.nodes[p.index()].next_sibling = next,
            None => self.nodes[parent.index()].first_child = next,
        }
        match next {
            Some(n) => self.nodes[n.index()].prev_sibling = prev,
            None => self.nodes[parent.index()].last_child = prev,
        }
        let n = &mut self.nodes[id.index()];
        n.parent = None;
        n.prev_sibling = None;
        n.next_sibling = None;
        self.mark_layout_dirty(parent);
        self.select_after_remove(id);
    }

    /// Append `child` as the last child of `parent` (detaching it first).
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.insert_before(parent, child, None);
    }

    /// Insert `child` into `parent` before `reference` (or at the end).
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, reference: Option<NodeId>) {
        if child == parent || self.ancestors(parent).any(|a| a == child) {
            return; // would create a cycle (DOM "pre-insert" hierarchy check)
        }
        if reference == Some(child) {
            return;
        }
        self.detach(child);
        self.generation += 1;
        let prev = match reference {
            Some(r) => self.nodes[r.index()].prev_sibling,
            None => self.nodes[parent.index()].last_child,
        };
        {
            let c = &mut self.nodes[child.index()];
            c.parent = Some(parent);
            c.prev_sibling = prev;
            c.next_sibling = reference;
        }
        match prev {
            Some(p) => self.nodes[p.index()].next_sibling = Some(child),
            None => self.nodes[parent.index()].first_child = Some(child),
        }
        match reference {
            Some(r) => self.nodes[r.index()].prev_sibling = Some(child),
            None => self.nodes[parent.index()].last_child = Some(child),
        }
        self.mark_style_dirty(child);
        self.mark_layout_dirty(parent);
        self.select_after_insert(child);
    }

    /// A copy of node `id` of document `src` (and its descendants, for `deep`) in this one,
    /// not inserted anywhere. Returns the copy and the (source, copy) pairs of every node
    /// copied, in tree order.
    /// Shadow roots go along (with `deep`), as adopting a node takes its shadow-including
    /// descendants.
    pub fn import_node(&mut self, src: &Document, id: NodeId, deep: bool) -> (NodeId, Vec<(NodeId, NodeId)>) {
        let mut pairs = Vec::new();
        let copy = self.import_rec(src, id, deep, false, &mut pairs);
        (copy, pairs)
    }

    /// Like [`Document::import_node`] with `deep`, but as DOM §4.4 "clone a node" does it:
    /// only clonable shadow roots are copied.
    pub fn import_clone(&mut self, src: &Document, id: NodeId) -> NodeId {
        let mut pairs = Vec::new();
        self.import_rec(src, id, true, true, &mut pairs)
    }

    fn import_rec(
        &mut self,
        src: &Document,
        id: NodeId,
        deep: bool,
        cloning: bool,
        pairs: &mut Vec<(NodeId, NodeId)>,
    ) -> NodeId {
        let copy = self.alloc_copy(src.nodes[id.index()].data.clone());
        if src.is_cdata(id) {
            self.nodes[copy.index()].flags |= NodeFlags::CDATA;
        }
        pairs.push((id, copy));
        if !cloning {
            // Adoption: the host's per-element state goes along.
            if let Some(&d) = src.custom_defined.get(&id) {
                self.custom_defined.insert(copy, d);
            }
            if let Some(s) = src.custom_states.get(&id) {
                self.custom_states.insert(copy, s.clone());
            }
            if let Some(sub) = src.face_submission(id) {
                self.set_form_associated_custom(copy, true);
                self.set_face_submission(copy, sub.clone());
            }
        }
        if deep {
            if let Some(sr) = src.shadow_root(id) {
                if !cloning || src.shadow_info(sr).is_some_and(|i| i.clonable) {
                    if let Some(to) = self.copy_shadow_from(src, sr, copy) {
                        pairs.push((sr, to));
                        let kids: Vec<NodeId> = src.children(sr).collect();
                        for k in kids {
                            let c = self.import_rec(src, k, true, cloning, pairs);
                            self.append_child(to, c);
                        }
                    }
                }
            }
            let kids: Vec<NodeId> = src.children(id).collect();
            for k in kids {
                let c = self.import_rec(src, k, true, cloning, pairs);
                self.append_child(copy, c);
            }
            if let (Some(s), Some(d)) = (src.template_of(id), self.template_of(copy)) {
                let kids: Vec<NodeId> = src.children(s).collect();
                for k in kids {
                    let c = self.import_rec(src, k, true, cloning, pairs);
                    self.append_child(d, c);
                }
            }
        }
        copy
    }

    /// A new node with `data` (a template gets fresh, empty contents).
    fn alloc_copy(&mut self, data: NodeData) -> NodeId {
        match data {
            NodeData::Element(e) => {
                let id = self.create_element(e.ns, &e.name, e.attrs);
                if let Some(pfx) = e.prefix {
                    if let Some(el) = self.element_mut(id) {
                        el.prefix = Some(pfx);
                    }
                }
                id
            }
            NodeData::Document => self.alloc(NodeData::DocumentFragment),
            d => self.alloc(d),
        }
    }

    /// DOM §4.9 "defined" (CSS `:defined`): the element's custom element state is
    /// "uncustomized" or "custom". The host records the states it sets; an element it hasn't
    /// recorded is undefined if it could be a custom element (an HTML element with a valid
    /// custom element name or an `is` attribute), else uncustomized.
    pub fn is_defined(&self, id: NodeId) -> bool {
        if let Some(&d) = self.custom_defined.get(&id) {
            return d;
        }
        let Some(e) = self.element(id) else { return true };
        if e.ns != Namespace::Html {
            return true;
        }
        let n = e.name.as_str();
        let custom = n.starts_with(|c: char| c.is_ascii_lowercase()) && n.contains('-');
        !(custom || e.has_attr("is"))
    }

    /// Record whether element `id` is defined (its custom element state changed).
    pub fn set_defined(&mut self, id: NodeId, defined: bool) {
        if self.custom_defined.insert(id, defined) != Some(defined) {
            self.mark_style_dirty(id);
        }
    }

    /// Set element `id`'s custom states (HTML §4.13.7.7 CustomStateSet).
    pub fn set_custom_states(&mut self, id: NodeId, states: Vec<String>) {
        if states.is_empty() {
            self.custom_states.remove(&id);
        } else {
            self.custom_states.insert(id, states);
        }
        self.mark_style_dirty(id);
    }

    /// Whether element `id` has custom state `state` (CSS `:state()`).
    pub fn has_custom_state(&self, id: NodeId, state: &str) -> bool {
        self.custom_states.get(&id).is_some_and(|s| s.iter().any(|x| x == state))
    }

    /// A `<template>`'s contents fragment.
    pub fn template_of(&self, id: NodeId) -> Option<NodeId> {
        self.element(id).and_then(|e| e.template_contents)
    }

    /// Move all children of `from` to the end of `to`.
    pub fn reparent_children(&mut self, from: NodeId, to: NodeId) {
        while let Some(c) = self.first_child(from) {
            self.append_child(to, c);
        }
    }

    /// Append text to `parent`, merging with a trailing text node.
    pub fn append_text(&mut self, parent: NodeId, text: &str) {
        if let Some(last) = self.last_child(parent) {
            if let NodeData::Text(t) = &mut self.nodes[last.index()].data {
                t.push_str(text);
                self.generation += 1;
                return;
            }
        }
        let t = self.create_text(text);
        self.append_child(parent, t);
    }

    /// Insert text before `reference`, merging with the preceding text node.
    pub fn insert_text_before(&mut self, parent: NodeId, text: &str, reference: NodeId) {
        if let Some(prev) = self.prev_sibling(reference) {
            if let NodeData::Text(t) = &mut self.nodes[prev.index()].data {
                t.push_str(text);
                self.generation += 1;
                return;
            }
        }
        let t = self.create_text(text);
        self.insert_before(parent, t, Some(reference));
    }

    /// Replace the data of a character-data node (text, comment, processing instruction).
    pub fn set_text(&mut self, id: NodeId, text: &str) {
        self.generation += 1;
        match &mut self.nodes[id.index()].data {
            NodeData::Text(t) | NodeData::Comment(t) | NodeData::ProcessingInstruction { data: t, .. } => {
                *t = text.to_string()
            }
            _ => {}
        }
        if let Some(p) = self.parent(id) {
            self.mark_layout_dirty(p);
        }
    }

    pub fn mark_style_dirty(&mut self, id: NodeId) {
        self.nodes[id.index()].flags |= NodeFlags::STYLE_DIRTY;
        let mut cur = self.nodes[id.index()].parent;
        while let Some(p) = cur {
            let n = &mut self.nodes[p.index()];
            if n.flags.contains(NodeFlags::DESCENDANT_STYLE_DIRTY) {
                break;
            }
            n.flags |= NodeFlags::DESCENDANT_STYLE_DIRTY;
            cur = n.parent;
        }
    }

    pub fn mark_layout_dirty(&mut self, id: NodeId) {
        self.nodes[id.index()].flags |= NodeFlags::LAYOUT_DIRTY;
    }

    /// Clear damage flags on every node (after a full restyle/relayout).
    pub fn clear_dirty(&mut self) {
        let mask = NodeFlags::STYLE_DIRTY | NodeFlags::DESCENDANT_STYLE_DIRTY | NodeFlags::LAYOUT_DIRTY;
        for n in &mut self.nodes {
            n.flags.remove(mask);
        }
    }

    pub fn set_flag(&mut self, id: NodeId, flag: NodeFlags, on: bool) {
        let n = &mut self.nodes[id.index()];
        if n.flags.contains(flag) != on {
            n.flags.set(flag, on);
            self.mark_style_dirty(id);
        }
    }

    // ----- queries -----

    /// The document element (`<html>`).
    pub fn document_element(&self) -> Option<NodeId> {
        self.element_children(Self::ROOT).next()
    }

    fn child_html(&self, parent: Option<NodeId>, name: &str) -> Option<NodeId> {
        let parent = parent?;
        self.element_children(parent).find(|&c| self.is_html(c, name))
    }

    pub fn head(&self) -> Option<NodeId> {
        self.child_html(self.document_element(), "head")
    }

    /// The body element: first `<body>` or `<frameset>` child of `<html>`.
    pub fn body(&self) -> Option<NodeId> {
        let html = self.document_element()?;
        self.element_children(html).find(|&c| self.is_html(c, "body") || self.is_html(c, "frameset"))
    }

    /// Concatenated text of all descendant text nodes (DOM `textContent`).
    pub fn text_content(&self, id: NodeId) -> String {
        let mut s = String::new();
        for d in self.descendants(id) {
            if let NodeData::Text(t) = &self.node(d).data {
                s.push_str(t);
            }
        }
        s
    }

    /// The document title (HTML §3.1.3), whitespace-collapsed.
    pub fn title(&self) -> String {
        let t = self.descendants(Self::ROOT).find(|&n| {
            self.element(n)
                .is_some_and(|e| e.name == "title" && matches!(e.ns, Namespace::Html | Namespace::Svg))
        });
        match t {
            Some(t) => self.text_content(t).split_ascii_whitespace().collect::<Vec<_>>().join(" "),
            None => String::new(),
        }
    }

    pub fn get_element_by_id(&self, id: &str) -> Option<NodeId> {
        self.descendants(Self::ROOT).find(|&n| self.element(n).and_then(|e| e.id()) == Some(id))
    }

    /// All HTML elements with the given local name, in tree order.
    pub fn elements_by_tag(&self, name: &str) -> Vec<NodeId> {
        self.descendants(Self::ROOT).filter(|&n| self.is_html(n, name)).collect()
    }
}

/// Iterator over a node's children.
pub struct Children<'a> {
    doc: &'a Document,
    next: Option<NodeId>,
}

impl Iterator for Children<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        let cur = self.next?;
        self.next = self.doc.node(cur).next_sibling;
        Some(cur)
    }
}

/// Pre-order descendant iterator (includes the root).
pub struct Descendants<'a> {
    doc: &'a Document,
    root: NodeId,
    next: Option<NodeId>,
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;
    fn next(&mut self) -> Option<NodeId> {
        let cur = self.next?;
        self.next = self.doc.next_in_preorder(cur, self.root);
        Some(cur)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_mutate() {
        let mut d = Document::new();
        let html = d.create_html_element("html");
        d.append_child(Document::ROOT, html);
        let body = d.create_html_element("body");
        d.append_child(html, body);
        let a = d.create_html_element("p");
        let b = d.create_html_element("div");
        d.append_child(body, a);
        d.append_child(body, b);
        d.append_text(a, "hello ");
        d.append_text(a, "world");
        assert_eq!(d.children(a).count(), 1);
        assert_eq!(d.text_content(body), "hello world");
        d.insert_before(body, b, Some(a));
        assert_eq!(d.children(body).collect::<Vec<_>>(), vec![b, a]);
        d.detach(b);
        assert_eq!(d.children(body).collect::<Vec<_>>(), vec![a]);
        assert_eq!(d.body(), Some(body));
        // Cycles are refused.
        d.append_child(a, body);
        assert_eq!(d.parent(body), Some(html));
        assert_eq!(d.descendants(Document::ROOT).count(), 5);
    }

    #[test]
    fn attributes_and_queries() {
        let mut d = Document::new();
        let e = d.create_element(Namespace::Html, "div", vec![Attribute::new("class", " a  b ")]);
        d.append_child(Document::ROOT, e);
        d.set_attr(e, "id", "x");
        assert_eq!(d.get_element_by_id("x"), Some(e));
        assert!(d.element(e).unwrap().has_class("b"));
        d.remove_attr(e, "id");
        assert_eq!(d.get_element_by_id("x"), None);
    }

    #[test]
    fn template_has_contents() {
        let mut d = Document::new();
        let t = d.create_html_element("template");
        let frag = d.element(t).unwrap().template_contents.unwrap();
        assert!(matches!(d.node(frag).data, NodeData::DocumentFragment));
    }
}

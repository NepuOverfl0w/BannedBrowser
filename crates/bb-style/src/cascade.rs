//! CSS Cascade 4: collecting, ordering and applying declarations.

use crate::computed::{ComputedStyle, Display, Float, Position};
use crate::properties::{self, contains_var, substitute_vars, Ctx};
use bb_css::selectors::{matches, Combinator, Element, PseudoClass, PseudoElement, Selector, Simple};
use bb_css::{Declaration, MediaContext, MediaQueryList, Rule, Stylesheet};
use bb_dom::{Document, Namespace, NodeData, NodeFlags, NodeId, QuirksMode};
use rustc_hash::FxHashMap;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    UserAgent,
    User,
    Author,
}

/// A stylesheet with its origin and the media it applies to.
#[derive(Clone)]
pub struct Sheet {
    pub origin: Origin,
    pub stylesheet: Rc<Stylesheet>,
    /// `<link media>` / `@import` media.
    pub media: MediaQueryList,
    /// The shadow root whose tree the sheet is in (CSS Scoping §3: it styles that tree
    /// only); None for the document's sheets and the UA's.
    pub scope: Option<NodeId>,
}

struct IndexedRule {
    selector: Selector,
    decls: Rc<Vec<Declaration>>,
    origin: Origin,
    order: u32,
    scope: Option<NodeId>,
}

/// `@font-face` rule data for the text layer.
#[derive(Debug, Clone)]
pub struct FontFaceRule {
    pub family: String,
    pub sources: Vec<String>,
    pub weight: u16,
    pub italic: bool,
}

/// A stylesheet set prepared for matching.
pub struct Stylist {
    rules: Vec<IndexedRule>,
    by_id: FxHashMap<String, Vec<usize>>,
    by_class: FxHashMap<String, Vec<usize>>,
    by_tag: FxHashMap<String, Vec<usize>>,
    other: Vec<usize>,
    pub font_faces: Vec<FontFaceRule>,
    pub media: MediaContext,
    /// Pseudo-elements that at least one rule targets.
    pseudo_rules: Vec<PseudoElement>,
    /// Whether any rule is a `:host` or `::slotted()` rule.
    shadow_rules: bool,
    host_rules: Vec<usize>,
    slotted_rules: Vec<usize>,
}

fn flatten_rules(
    rules: &[Rule],
    media: &MediaContext,
    out: &mut Vec<(Rc<Vec<Declaration>>, Vec<Selector>)>,
    faces: &mut Vec<FontFaceRule>,
) {
    for r in rules {
        match r {
            Rule::Style(sr) => out.push((Rc::new(sr.declarations.clone()), sr.selectors.clone())),
            Rule::Media { query, rules } => {
                if query.evaluate(media) {
                    flatten_rules(rules, media, out, faces);
                }
            }
            Rule::Supports { supported, rules } => {
                if *supported {
                    flatten_rules(rules, media, out, faces);
                }
            }
            Rule::Layer(rules) => flatten_rules(rules, media, out, faces),
            Rule::FontFace(decls) => {
                let mut family = None;
                let mut sources = Vec::new();
                let mut weight = 400;
                let mut italic = false;
                for d in decls {
                    match d.name.as_str() {
                        "font-family" => {
                            family = d.value.iter().find_map(|c| match c {
                                bb_css::Cv::Token(bb_css::Token::String(s))
                                | bb_css::Cv::Token(bb_css::Token::Ident(s)) => Some(s.clone()),
                                _ => None,
                            })
                        }
                        "src" => {
                            for cv in &d.value {
                                match cv {
                                    bb_css::Cv::Token(bb_css::Token::Url(u)) => sources.push(u.clone()),
                                    bb_css::Cv::Function(f, args) if f == "url" => {
                                        if let Some(bb_css::Cv::Token(bb_css::Token::String(s))) =
                                            args.iter().find(|c| !c.is_ws())
                                        {
                                            sources.push(s.clone());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        "font-weight" => {
                            if let Some(bb_css::Cv::Token(bb_css::Token::Number { value, .. })) =
                                d.value.iter().find(|c| !c.is_ws())
                            {
                                weight = *value as u16;
                            } else if d.value.iter().any(|c| c.is_ident("bold")) {
                                weight = 700;
                            }
                        }
                        "font-style" => {
                            italic = d.value.iter().any(|c| c.is_ident("italic") || c.is_ident("oblique"))
                        }
                        _ => {}
                    }
                }
                if let Some(family) = family {
                    faces.push(FontFaceRule { family, sources, weight, italic });
                }
            }
            Rule::Import { .. } => {} // resolved by the engine before building the Stylist
        }
    }
}

/// The key used to bucket a selector by its rightmost compound.
fn bucket_key(sel: &Selector) -> (u8, String) {
    let subject = &sel.compounds[0];
    for s in subject {
        if let Simple::Id(id) = s {
            return (0, id.clone());
        }
    }
    for s in subject {
        if let Simple::Class(c) = s {
            return (1, c.clone());
        }
    }
    for s in subject {
        if let Simple::Type(t) = s {
            return (2, t.clone());
        }
    }
    (3, String::new())
}

impl Stylist {
    pub fn new(sheets: &[Sheet], media: MediaContext) -> Stylist {
        let mut st = Stylist {
            rules: Vec::new(),
            by_id: FxHashMap::default(),
            by_class: FxHashMap::default(),
            by_tag: FxHashMap::default(),
            other: Vec::new(),
            font_faces: Vec::new(),
            media,
            pseudo_rules: Vec::new(),
            shadow_rules: false,
            host_rules: Vec::new(),
            slotted_rules: Vec::new(),
        };
        let mut order = 1u32;
        for sheet in sheets {
            if !sheet.media.evaluate(&media) {
                continue;
            }
            let mut flat = Vec::new();
            flatten_rules(&sheet.stylesheet.rules, &media, &mut flat, &mut st.font_faces);
            for (decls, selectors) in flat {
                for sel in selectors {
                    let idx = st.rules.len();
                    if let Some(pe) = sel.pseudo_element {
                        if !st.pseudo_rules.contains(&pe) {
                            st.pseudo_rules.push(pe);
                        }
                    }
                    let (kind, key) = bucket_key(&sel);
                    let host = sel.compounds[0].iter().any(|s| {
                        matches!(s, Simple::Pseudo(PseudoClass::Host(_) | PseudoClass::HostContext(_)))
                    });
                    let slotted = sel.combinators.contains(&Combinator::Slotted);
                    if host || slotted {
                        st.shadow_rules = true;
                    }
                    if host {
                        st.host_rules.push(idx);
                    }
                    if slotted {
                        st.slotted_rules.push(idx);
                    }
                    st.rules.push(IndexedRule {
                        selector: sel,
                        decls: decls.clone(),
                        origin: sheet.origin,
                        order,
                        scope: sheet.scope,
                    });
                    if host || slotted {
                        continue;
                    }
                    let map = match kind {
                        0 => &mut st.by_id,
                        1 => &mut st.by_class,
                        2 => &mut st.by_tag,
                        _ => {
                            st.other.push(idx);
                            continue;
                        }
                    };
                    // Case-insensitive buckets would be needed for quirks mode classes;
                    // we store as-is and also check `other` there (see candidates()).
                    map.entry(key).or_default().push(idx);
                }
                order += 1;
            }
        }
        st
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    fn candidates(&self, el: &DomElement, out: &mut Vec<usize>) {
        out.clear();
        let e = el.data();
        if let Some(id) = e.id() {
            if let Some(v) = self.by_id.get(id) {
                out.extend_from_slice(v);
            }
        }
        for c in e.classes() {
            if let Some(v) = self.by_class.get(c) {
                out.extend_from_slice(v);
            }
        }
        if let Some(v) = self.by_tag.get(e.name.as_str()) {
            out.extend_from_slice(v);
        }
        out.extend_from_slice(&self.other);
        if el.doc.quirks_mode == QuirksMode::Quirks {
            // Class/id selectors match case-insensitively in quirks mode: scan all buckets.
            for (k, v) in self.by_class.iter() {
                if e.classes().any(|c| c.eq_ignore_ascii_case(k) && c != k) {
                    out.extend_from_slice(v);
                }
            }
            if let Some(id) = e.id() {
                for (k, v) in self.by_id.iter() {
                    if id.eq_ignore_ascii_case(k) && id != k {
                        out.extend_from_slice(v);
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
    }
}

/// Selector-matching view of a DOM element, in the context of the rules of tree `scope`
/// (None: the document's).
#[derive(Clone, Copy)]
pub struct DomElement<'a> {
    pub doc: &'a Document,
    pub id: NodeId,
    pub scope: Option<NodeId>,
    /// A shadow host as its own shadow tree's rules see it (CSS Scoping §3.1).
    pub featureless: bool,
}

impl<'a> DomElement<'a> {
    fn data(&self) -> &'a bb_dom::ElementData {
        match &self.doc.node(self.id).data {
            NodeData::Element(e) => e,
            _ => unreachable!("DomElement wraps elements only"),
        }
    }

    /// A plain element of the document tree.
    pub fn new(doc: &'a Document, id: NodeId) -> Self {
        DomElement { doc, id, scope: None, featureless: false }
    }

    fn wrap(&self, id: Option<NodeId>) -> Option<Self> {
        id.map(|id| DomElement { doc: self.doc, id, scope: self.scope, featureless: false })
    }

    fn is_link(&self) -> bool {
        let e = self.data();
        e.ns == Namespace::Html && matches!(e.name.as_str(), "a" | "area" | "link") && e.has_attr("href")
    }
}

impl<'a> Element for DomElement<'a> {
    fn parent_element(&self) -> Option<Self> {
        if self.featureless {
            return None;
        }
        let p = self.doc.parent(self.id)?;
        if self.doc.node(p).is_element() {
            self.wrap(Some(p))
        } else if self.scope == Some(p) {
            // The top of the shadow tree: its host, featureless (CSS Scoping §3.1).
            let host = self.doc.shadow_host(p)?;
            Some(DomElement { doc: self.doc, id: host, scope: self.scope, featureless: true })
        } else {
            None
        }
    }
    fn is_featureless(&self) -> bool {
        self.featureless
    }
    fn as_featured(&self) -> Self {
        DomElement { featureless: false, ..*self }
    }
    fn shadow_including_parent(&self) -> Option<Self> {
        let mut p = self.doc.parent_or_host(self.id)?;
        while !self.doc.node(p).is_element() {
            p = self.doc.parent_or_host(p)?;
        }
        Some(DomElement { doc: self.doc, id: p, scope: None, featureless: false })
    }
    fn assigned_slot(&self) -> Option<Self> {
        // The slot in the scope's tree this element is (after flattening) assigned to.
        let scope = self.scope?;
        let mut s = self.doc.find_slot(self.id, false)?;
        loop {
            if self.doc.tree_root(s) == scope {
                return Some(DomElement { doc: self.doc, id: s, scope: self.scope, featureless: false });
            }
            s = self.doc.find_slot(s, false)?;
        }
    }
    fn prev_sibling_element(&self) -> Option<Self> {
        let mut c = self.doc.prev_sibling(self.id);
        while let Some(n) = c {
            if self.doc.node(n).is_element() {
                return self.wrap(Some(n));
            }
            c = self.doc.prev_sibling(n);
        }
        None
    }
    fn next_sibling_element(&self) -> Option<Self> {
        let mut c = self.doc.next_sibling(self.id);
        while let Some(n) = c {
            if self.doc.node(n).is_element() {
                return self.wrap(Some(n));
            }
            c = self.doc.next_sibling(n);
        }
        None
    }
    fn first_child_element(&self) -> Option<Self> {
        self.wrap(self.doc.element_children(self.id).next())
    }
    fn local_name(&self) -> &str {
        &self.data().name
    }
    fn is_html(&self) -> bool {
        self.data().ns == Namespace::Html
    }
    fn id(&self) -> Option<&str> {
        self.data().id()
    }
    fn has_class(&self, name: &str) -> bool {
        self.data().has_class(name)
    }
    fn attr(&self, name: &str) -> Option<&str> {
        self.data().attr(name)
    }
    fn is_root(&self) -> bool {
        self.doc.parent(self.id) == Some(Document::ROOT)
    }
    fn is_empty(&self) -> bool {
        self.doc.children(self.id).all(|c| match &self.doc.node(c).data {
            NodeData::Element(_) => false,
            NodeData::Text(t) => t.is_empty(),
            _ => true,
        })
    }
    fn quirks(&self) -> bool {
        self.doc.quirks_mode == QuirksMode::Quirks
    }
    fn same(&self, other: &Self) -> bool {
        self.id == other.id && self.featureless == other.featureless
    }
    fn match_state(&self, pc: &PseudoClass) -> bool {
        let flags = self.doc.node(self.id).flags;
        let e = self.data();
        let form_control = (e.ns == Namespace::Html
            && matches!(
                e.name.as_str(),
                "input" | "button" | "select" | "textarea" | "option" | "optgroup" | "fieldset"
            ))
            || self.doc.is_form_associated_custom(self.id);
        match pc {
            PseudoClass::Link => self.is_link() && !flags.contains(NodeFlags::VISITED),
            PseudoClass::Visited => self.is_link() && flags.contains(NodeFlags::VISITED),
            PseudoClass::AnyLink => self.is_link(),
            PseudoClass::Hover => flags.contains(NodeFlags::HOVER),
            PseudoClass::Active => flags.contains(NodeFlags::ACTIVE),
            PseudoClass::Focus | PseudoClass::FocusVisible => flags.contains(NodeFlags::FOCUS),
            // The focused element is a shadow-including descendant (Selectors 4 §11.5).
            PseudoClass::FocusWithin => self
                .doc
                .focused()
                .is_some_and(|f| self.doc.is_shadow_including_inclusive_ancestor(self.id, f)),
            PseudoClass::Checked => {
                (e.is_html("input") && e.has_attr("checked"))
                    || (e.is_html("option") && e.has_attr("selected"))
            }
            // §4.16.3: "actually disabled" (a disabled fieldset's descendants too).
            PseudoClass::Disabled => form_control && self.doc.is_disabled(self.id),
            PseudoClass::Enabled => form_control && !self.doc.is_disabled(self.id),
            PseudoClass::Required => form_control && e.has_attr("required"),
            PseudoClass::Optional => form_control && !e.has_attr("required"),
            PseudoClass::ReadOnly => !(e.is_html("input") || e.is_html("textarea")) || e.has_attr("readonly"),
            PseudoClass::ReadWrite => {
                (e.is_html("input") || e.is_html("textarea")) && !e.has_attr("readonly")
            }
            PseudoClass::PlaceholderShown => {
                (e.is_html("input") || e.is_html("textarea"))
                    && e.has_attr("placeholder")
                    && e.attr("value").is_none_or(|v| v.is_empty())
            }
            PseudoClass::Defined => self.doc.is_defined(self.id),
            PseudoClass::State(s) => self.doc.has_custom_state(self.id, s),
            // HTML §4.16.3: candidates by their validity; a form or fieldset by its controls'.
            PseudoClass::Valid | PseudoClass::Invalid => {
                let d = self.doc;
                let invalid = |x: bb_dom::NodeId| d.is_validation_candidate(x) && !d.is_valid_control(x);
                let state = if e.is_html("form") {
                    Some(d.form_controls(self.id).into_iter().any(invalid))
                } else if e.is_html("fieldset") {
                    Some(d.descendants(self.id).skip(1).any(invalid))
                } else if d.is_validation_candidate(self.id) {
                    Some(!d.is_valid_control(self.id))
                } else {
                    None
                };
                match state {
                    Some(bad) => bad == matches!(pc, PseudoClass::Invalid),
                    None => false,
                }
            }
            PseudoClass::InRange => self.doc.out_of_range(self.id) == Some(false),
            PseudoClass::OutOfRange => self.doc.out_of_range(self.id) == Some(true),
            PseudoClass::Scope => self.is_root(),
            PseudoClass::Default => e.has_attr("checked") || e.has_attr("selected"),
            PseudoClass::Target | PseudoClass::Indeterminate => false,
            _ => false,
        }
    }
}

/// Computed styles for every element of a document.
pub struct StyleMap {
    styles: Vec<Option<Rc<ComputedStyle>>>,
    pseudo: FxHashMap<(NodeId, PseudoKind), Rc<ComputedStyle>>,
    /// The style for the root element's canvas background (CSS 2 §14.2).
    pub canvas_background: bb_css::Rgba,
    pub root_dark: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PseudoKind {
    Before,
    After,
    Marker,
}

impl StyleMap {
    pub fn get(&self, id: NodeId) -> Option<&Rc<ComputedStyle>> {
        self.styles.get(id.index()).and_then(|s| s.as_ref())
    }

    pub fn pseudo(&self, id: NodeId, kind: PseudoKind) -> Option<&Rc<ComputedStyle>> {
        self.pseudo.get(&(id, kind))
    }

    pub fn len(&self) -> usize {
        self.styles.iter().filter(|s| s.is_some()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Inputs to style computation beyond the stylesheets.
#[derive(Debug, Clone, Copy)]
pub struct StyleEnv {
    pub viewport: (f32, f32),
    pub prefers_dark: bool,
    /// Ignore author styles ("View › Page Style › No Style").
    pub no_author_styles: bool,
    /// Minimum font size in px (0 = none).
    pub min_font_size: f32,
    /// `marginwidth`/`marginheight` of the `<frame>`/`<iframe>` holding this
    /// document (HTML §15.3.3), in CSS px.
    pub container_margins: (Option<f32>, Option<f32>),
}

impl Default for StyleEnv {
    fn default() -> Self {
        StyleEnv {
            viewport: (1024.0, 768.0),
            prefers_dark: false,
            no_author_styles: false,
            min_font_size: 0.0,
            container_margins: (None, None),
        }
    }
}

struct Matched<'a> {
    /// (origin and importance, encapsulation context, specificity, order).
    key: (u8, u8, u32, u32),
    decl: &'a Declaration,
}

fn push_rule<'r>(matched: &mut Vec<Matched<'r>>, r: &'r IndexedRule, depth: u8) {
    for d in r.decls.iter() {
        matched.push(Matched {
            key: (
                rank(r.origin, d.important),
                context_key(depth, d.important),
                r.selector.specificity,
                r.order,
            ),
            decl: d,
        });
    }
}

/// How deep shadow tree `scope` is (0: the document tree).
fn tree_depth(doc: &Document, mut scope: Option<NodeId>) -> u8 {
    let mut depth = 0u8;
    while let Some(r) = scope {
        depth = depth.saturating_add(1);
        scope = doc.shadow_host(r).and_then(|h| doc.containing_shadow_root(h));
    }
    depth
}

/// CSS Cascade 4 §6.2 context: for normal declarations the outer (shallower) tree's win,
/// for important ones the inner tree's.
fn context_key(depth: u8, important: bool) -> u8 {
    if important {
        depth
    } else {
        u8::MAX - depth
    }
}

fn rank(origin: Origin, important: bool) -> u8 {
    match (origin, important) {
        (Origin::UserAgent, false) => 0,
        (Origin::User, false) => 1,
        (Origin::Author, false) => 2,
        (Origin::Author, true) => 3,
        (Origin::User, true) => 4,
        (Origin::UserAgent, true) => 5,
    }
}

impl Stylist {
    /// Compute styles for all elements in `doc`.
    pub fn compute(&self, doc: &Document, env: &StyleEnv) -> StyleMap {
        let mut map = StyleMap {
            styles: vec![None; doc.len()],
            pseudo: FxHashMap::default(),
            canvas_background: bb_css::Rgba::WHITE,
            root_dark: false,
        };
        let Some(root) = doc.document_element() else { return map };
        let base = Rc::new(ComputedStyle::initial());
        let mut cand = Vec::new();
        // Iterative pre-order traversal: (node, parent style).
        let mut stack: Vec<(NodeId, Rc<ComputedStyle>, f32)> = vec![(root, base.clone(), 16.0)];
        while let Some((node, parent, root_fs)) = stack.pop() {
            if !doc.node(node).is_element() {
                continue;
            }
            let is_root = node == root;
            let style = self.compute_element(
                doc,
                node,
                &parent,
                if is_root { 16.0 } else { root_fs },
                env,
                &mut cand,
                None,
            );
            let style = Rc::new(style);
            let root_fs = if is_root { style.font_size } else { root_fs };
            if is_root {
                map.root_dark = style.color_scheme_dark;
            }
            if style.display != Display::None {
                for (pe, kind) in [
                    (PseudoElement::Before, PseudoKind::Before),
                    (PseudoElement::After, PseudoKind::After),
                    (PseudoElement::Marker, PseudoKind::Marker),
                ] {
                    if !self.pseudo_rules.contains(&pe) {
                        continue;
                    }
                    let ps = self.compute_element(doc, node, &style, root_fs, env, &mut cand, Some(pe));
                    let has_content = ps.content.as_ref().is_some_and(|c| !c.is_empty());
                    if (pe != PseudoElement::Marker && has_content && ps.display != Display::None)
                        || pe == PseudoElement::Marker
                    {
                        map.pseudo.insert((node, kind), Rc::new(ps));
                    }
                }
                // Children (in the flat tree: CSS Scoping §2.2) in reverse so the stack pops
                // them in order.
                let kids: Vec<NodeId> = doc.flat_children(node);
                for &k in kids.iter().rev() {
                    stack.push((k, style.clone(), root_fs));
                }
            }
            map.styles[node.index()] = Some(style);
        }
        // CSS 2 §14.2: the canvas uses the root's background, or body's if the root has none.
        if let Some(rs) = map.get(root) {
            let mut bg = rs.background_color;
            if bg.a == 0 && rs.background.is_empty() {
                if let Some(body) = doc.body() {
                    if let Some(bs) = map.get(body) {
                        bg = bs.background_color;
                    }
                }
            }
            let canvas = bb_css::values::system_color("canvas", map.root_dark);
            map.canvas_background = if bg.a == 0 { canvas } else { blend_over(bg, canvas) };
        }
        map
    }

    fn compute_element(
        &self,
        doc: &Document,
        node: NodeId,
        parent: &ComputedStyle,
        root_font_size: f32,
        env: &StyleEnv,
        cand: &mut Vec<usize>,
        pseudo: Option<PseudoElement>,
    ) -> ComputedStyle {
        // The element's tree: the document's rules style the document tree, a shadow tree's
        // its own (CSS Scoping §3); UA rules style every tree.
        let scope = doc.containing_shadow_root(node);
        let depth = if scope.is_some() { tree_depth(doc, scope) } else { 0 };
        let el = DomElement { doc, id: node, scope, featureless: false };
        self.candidates(&el, cand);
        let mut matched: Vec<Matched> = Vec::new();
        for &i in cand.iter() {
            let r = &self.rules[i];
            if r.selector.pseudo_element != pseudo {
                continue;
            }
            if env.no_author_styles && r.origin == Origin::Author {
                continue;
            }
            if r.origin == Origin::Author && r.scope != scope {
                continue;
            }
            if matches(&r.selector, el) {
                push_rule(&mut matched, r, depth);
            }
        }
        if self.shadow_rules && !env.no_author_styles {
            // CSS Scoping §3.2.1: a host's shadow tree's `:host` rules.
            if let Some(sr) = doc.shadow_root(node) {
                let host = DomElement { doc, id: node, scope: Some(sr), featureless: true };
                for &i in &self.host_rules {
                    let r = &self.rules[i];
                    if r.scope == Some(sr)
                        && r.selector.pseudo_element == pseudo
                        && matches(&r.selector, host)
                    {
                        push_rule(&mut matched, r, depth.saturating_add(1));
                    }
                }
            }
            // §3.2.2: `::slotted()` rules of the trees of the slots it is assigned to.
            let mut slot = doc.find_slot(node, false);
            while let Some(s) = slot {
                let tree = doc.tree_root(s);
                let el = DomElement { doc, id: node, scope: Some(tree), featureless: false };
                for &i in &self.slotted_rules {
                    let r = &self.rules[i];
                    if r.scope == Some(tree) && pseudo.is_none() && matches(&r.selector, el) {
                        push_rule(&mut matched, r, tree_depth(doc, Some(tree)));
                    }
                }
                slot = doc.find_slot(s, false);
            }
        }
        // Presentational hints and the style attribute (author origin).
        let hints;
        let inline;
        if pseudo.is_none() && !env.no_author_styles {
            hints = crate::hints::presentational_hints_in(doc, node, env.container_margins);
            for d in &hints {
                matched.push(Matched { key: (2, context_key(depth, false), 0, 0), decl: d });
            }
            inline = doc.attr(node, "style").map(bb_css::parse_declaration_list).unwrap_or_default();
            for d in &inline {
                matched.push(Matched {
                    key: (
                        rank(Origin::Author, d.important),
                        context_key(depth, d.important),
                        u32::MAX,
                        u32::MAX,
                    ),
                    decl: d,
                });
            }
        }
        matched.sort_by_key(|m| m.key);

        let ctx = Ctx {
            parent,
            root_font_size,
            viewport: env.viewport,
            prefers_dark: env.prefers_dark,
            quirks: doc.quirks_mode == QuirksMode::Quirks,
        };
        let mut style = ComputedStyle::inherit_from(parent);
        if pseudo.is_some() {
            style.content = None;
        }
        // Root element: initial `color` is CanvasText for the used color scheme.
        let is_root = doc.parent(node) == Some(Document::ROOT) && pseudo.is_none();

        // 1. Custom properties.
        if matched.iter().any(|m| m.decl.name.starts_with("--")) {
            let mut custom: Vec<(String, Rc<Vec<bb_css::Cv>>)> = (*parent.custom).clone();
            for m in matched.iter().filter(|m| m.decl.name.starts_with("--")) {
                let tmp = ComputedStyle { custom: Rc::new(custom.clone()), ..ComputedStyle::initial() };
                let val = if contains_var(&m.decl.value) {
                    substitute_vars(&m.decl.value, &tmp, 0)
                } else {
                    Some(m.decl.value.clone())
                };
                custom.retain(|(k, _)| *k != m.decl.name);
                if let Some(v) = val {
                    custom.push((m.decl.name.clone(), Rc::new(v)));
                }
            }
            style.custom = Rc::new(custom);
        }
        // 2. Early properties (color-scheme, font, color), then the rest.
        for early in [true, false] {
            if early {
                // color-scheme first so light-dark() in `color` resolves correctly.
                for m in matched.iter().filter(|m| m.decl.name == "color-scheme") {
                    apply_decl(&mut style, m.decl, &ctx);
                }
                if is_root {
                    style.color = bb_css::values::system_color("canvastext", style.color_scheme_dark);
                }
                for m in matched.iter().filter(|m| {
                    properties::is_early(&m.decl.name)
                        && m.decl.name != "color-scheme"
                        && m.decl.name != "color"
                }) {
                    apply_decl(&mut style, m.decl, &ctx);
                }
                for m in matched.iter().filter(|m| m.decl.name == "color") {
                    apply_decl(&mut style, m.decl, &ctx);
                }
                if env.min_font_size > 0.0 && style.font_size < env.min_font_size {
                    style.font_size = env.min_font_size;
                }
            } else {
                for m in matched
                    .iter()
                    .filter(|m| !properties::is_early(&m.decl.name) && !m.decl.name.starts_with("--"))
                {
                    apply_decl(&mut style, m.decl, &ctx);
                }
            }
        }
        fixup(&mut style, is_root, parent);
        style
    }
}

fn apply_decl(style: &mut ComputedStyle, d: &Declaration, ctx: &Ctx) {
    if contains_var(&d.value) {
        match substitute_vars(&d.value, style, 0) {
            Some(v) => {
                if !properties::apply(style, &d.name, &v, ctx) {
                    // Invalid at computed-value time: behaves as `unset`.
                    let unset = bb_css::parse_component_values("unset");
                    properties::apply(style, &d.name, &unset, ctx);
                }
            }
            None => {
                let unset = bb_css::parse_component_values("unset");
                properties::apply(style, &d.name, &unset, ctx);
            }
        }
    } else if !properties::apply(style, &d.name, &d.value, ctx) {
        log::trace!("ignored invalid declaration {}: {:?}", d.name, d.value);
    }
}

/// CSS 2 §9.7 relationships between display, position and float.
fn fixup(style: &mut ComputedStyle, is_root: bool, _parent: &ComputedStyle) {
    if style.display == Display::None {
        return;
    }
    if matches!(style.position, Position::Absolute | Position::Fixed) {
        style.float = Float::None;
        style.display = style.display.blockify();
    } else if style.float != Float::None || is_root {
        style.display = style.display.blockify();
    }
    if is_root && style.display == Display::Contents {
        style.display = Display::Block;
    }
}

fn blend_over(top: bb_css::Rgba, bottom: bb_css::Rgba) -> bb_css::Rgba {
    let a = top.a as f32 / 255.0;
    let mix = |t: u8, b: u8| (t as f32 * a + b as f32 * (1.0 - a)).round() as u8;
    bb_css::Rgba::rgb(mix(top.r, bottom.r), mix(top.g, bottom.g), mix(top.b, bottom.b))
}

//! HTML tree construction (HTML §13.2.6).
//!
//! Implements every insertion mode, the stack of open elements, the list of
//! active formatting elements (with Noah's Ark), the adoption agency
//! algorithm, foster parenting, `<template>` contents, foreign content (SVG
//! and MathML), and the fragment parsing algorithm. Scripting is disabled
//! (BannedBrowser has no JS engine yet), so `<noscript>` parses as normal
//! markup.

use crate::foreign;
use crate::tokenizer::{Doctype, State, Tag, Token, Tokenizer};
use bb_dom::{Attribute, Document, Namespace, NodeData, NodeId, QuirksMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionMode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    InHeadNoscript,
    AfterHead,
    InBody,
    Text,
    InTable,
    InTableText,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    InTemplate,
    AfterBody,
    InFrameset,
    AfterFrameset,
    AfterAfterBody,
    AfterAfterFrameset,
}

use InsertionMode as M;

#[derive(Clone, Debug)]
enum Formatting {
    Marker,
    Element(NodeId, Tag),
}

/// Internal token: characters are pre-split into uniform-class runs.
#[derive(Clone, Debug)]
enum Tok {
    Doctype(Doctype),
    Start(Tag),
    End(Tag),
    /// A comment or a processing instruction: every insertion mode handles them in the same
    /// place ("insert a comment" / "insert a processing instruction").
    Comment(CommentLike),
    /// A run of characters that are all NUL, all whitespace, or all "other".
    Chars(String),
    Eof,
}

#[derive(Clone, Debug)]
enum CommentLike {
    Comment(String),
    ProcessingInstruction { target: String, data: String },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Null,
    Ws,
    Other,
}

fn char_class(c: char) -> CharClass {
    match c {
        '\0' => CharClass::Null,
        '\t' | '\n' | '\x0C' | '\r' | ' ' => CharClass::Ws,
        _ => CharClass::Other,
    }
}

fn run_class(s: &str) -> CharClass {
    s.chars().next().map(char_class).unwrap_or(CharClass::Other)
}

/// Split a character run into maximal runs of the same class.
fn split_runs(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cls = None;
    for c in s.chars() {
        let k = char_class(c);
        if cls != Some(k) && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        cls = Some(k);
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn is_ws_run(s: &str) -> bool {
    run_class(s) == CharClass::Ws
}

const SPECIAL_HTML: &[&str] = &[
    "address",
    "applet",
    "area",
    "article",
    "aside",
    "base",
    "basefont",
    "bgsound",
    "blockquote",
    "body",
    "br",
    "button",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dir",
    "div",
    "dl",
    "dt",
    "embed",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "iframe",
    "img",
    "input",
    "keygen",
    "li",
    "link",
    "listing",
    "main",
    "marquee",
    "menu",
    "meta",
    "nav",
    "noembed",
    "noframes",
    "noscript",
    "object",
    "ol",
    "p",
    "param",
    "plaintext",
    "pre",
    "script",
    "search",
    "section",
    "select",
    "source",
    "style",
    "summary",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
    "wbr",
    "xmp",
];

/// The HTML tree builder.
/// DOM §4.2.2.2 "attach a shadow root" steps 1–4 for the parser: an HTML element with a
/// valid shadow host name that has no shadow root yet (definitions that disable shadow
/// roots are the builder's `shadow_disabled`).
fn can_attach_shadow(doc: &Document, host: NodeId) -> bool {
    let Some(e) = doc.element(host) else { return false };
    if e.ns != Namespace::Html || doc.shadow_root(host).is_some() {
        return false;
    }
    let n = e.name.as_str();
    let custom = n.starts_with(|c: char| c.is_ascii_lowercase())
        && n.contains('-')
        && !n.chars().any(|c| c.is_ascii_uppercase())
        && !matches!(
            n,
            "annotation-xml"
                | "color-profile"
                | "font-face"
                | "font-face-src"
                | "font-face-uri"
                | "font-face-format"
                | "font-face-name"
                | "missing-glyph"
        );
    custom
        || matches!(
            n,
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

/// Why [`TreeBuilder::pump`] returned before the end of the input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pause {
    /// A `<script>` element whose end tag was just processed, for the host to prepare and run.
    Script(NodeId),
    /// The next token is a start tag for an element with a custom element definition (HTML
    /// §13.2.6.1 "create an element for a token" with "will execute script"): the host
    /// creates the element (its constructor runs) and appends these attributes, hands it to
    /// [`TreeBuilder::provide_element`] and pumps again; the parser then inserts it.
    CustomElement { name: String, is: Option<String>, attrs: Vec<(String, String)> },
    /// The element given to `provide_element` was inserted (the host runs the reactions of
    /// its insertion, as "insert an element at the adjusted insertion location" does).
    Inserted(NodeId),
}

pub struct TreeBuilder {
    pub doc: Document,
    tokenizer: Tokenizer,
    mode: InsertionMode,
    original_mode: InsertionMode,
    template_modes: Vec<InsertionMode>,
    open: Vec<NodeId>,
    afe: Vec<Formatting>,
    head: Option<NodeId>,
    form: Option<NodeId>,
    frameset_ok: bool,
    foster_parenting: bool,
    pending_table_text: Vec<String>,
    context: Option<NodeId>,
    ignore_lf: bool,
    scripting: bool,
    stopped: bool,
    /// A `<script>` whose end tag was just processed (HTML §13.2.6.4.8), for `pump`.
    pending_script: Option<NodeId>,
    /// Parse error count (tree construction errors).
    pub errors: usize,
    /// The document's "allow declarative shadow roots" (HTML §13.2.6.4.4: `<template
    /// shadowrootmode>` attaches a shadow root).
    allow_shadow_roots: bool,
    /// Custom element names whose definitions disable shadow roots (DOM §4.2.2.2 step 3),
    /// as the host knows them.
    shadow_disabled: Vec<String>,
    /// The host's custom element definitions as (name, local name), for "will execute
    /// script"; empty for parsers whose host constructs nothing.
    custom_definitions: Vec<(String, String)>,
    /// A start tag token held back while the host constructs its element.
    held: Option<Token>,
    /// The element the host made for the held token, until it is used.
    provided: Option<NodeId>,
    /// The provided element, once it has been used for its token.
    provided_used: Option<NodeId>,
}

impl TreeBuilder {
    pub fn new(input: &str) -> Self {
        TreeBuilder {
            doc: Document::new(),
            tokenizer: Tokenizer::new(input),
            mode: M::Initial,
            original_mode: M::Initial,
            template_modes: Vec::new(),
            open: Vec::new(),
            afe: Vec::new(),
            head: None,
            form: None,
            frameset_ok: true,
            foster_parenting: false,
            pending_table_text: Vec::new(),
            context: None,
            ignore_lf: false,
            scripting: false,
            stopped: false,
            pending_script: None,
            errors: 0,
            allow_shadow_roots: false,
            shadow_disabled: Vec::new(),
            custom_definitions: Vec::new(),
            held: None,
            provided: None,
            provided_used: None,
        }
    }

    /// Set the custom element names (or `is` values) whose definitions have disableShadow.
    pub fn set_shadow_disabled_names(&mut self, names: Vec<String>) {
        self.shadow_disabled = names;
    }

    /// The host's custom element definitions, as (name, local name): a start tag that one
    /// of them defines makes [`TreeBuilder::pump`] pause with [`Pause::CustomElement`].
    pub fn set_custom_definitions(&mut self, defs: Vec<(String, String)>) {
        self.custom_definitions = defs;
    }

    /// The element the host created for a [`Pause::CustomElement`] (with its attributes): the
    /// parser uses it for the held token instead of creating one.
    pub fn provide_element(&mut self, el: Option<NodeId>) {
        self.provided = el;
    }

    /// Let `<template shadowrootmode>` make declarative shadow roots (documents loaded by
    /// navigation, `setHTMLUnsafe`, `parseHTMLUnsafe`; not innerHTML or DOMParser).
    pub fn set_allow_declarative_shadow_roots(&mut self, on: bool) {
        self.allow_shadow_roots = on;
    }

    /// Turn the scripting flag on (HTML §13.2.6.5: `<noscript>` is then raw text).
    pub fn set_scripting(&mut self, on: bool) {
        self.scripting = on;
    }

    /// Parse until a `<script>` element's end tag has been processed (HTML §13.2.6.4.8
    /// "An end tag whose tag name is script") or a custom element is to be constructed: why
    /// it stopped, or `None` when parsing has stopped.
    pub fn pump(&mut self) -> Option<Pause> {
        while !self.stopped {
            if let Some(p) = self.step() {
                return Some(p);
            }
        }
        None
    }

    /// Parse tokens until the tokenizer has passed `end` (the end of text `document.write`
    /// inserted), or until a pause as in [`TreeBuilder::pump`].
    pub fn pump_until(&mut self, end: usize) -> Option<Pause> {
        while !self.stopped && (self.held.is_some() || self.tokenizer.position() < end) {
            if let Some(p) = self.step() {
                return Some(p);
            }
        }
        None
    }

    /// Process one token (the held one first).
    fn step(&mut self) -> Option<Pause> {
        let token = match self.held.take() {
            Some(t) => t,
            None => {
                let t = self.tokenizer.next_token();
                if let Token::StartTag(tag) = &t {
                    if self.will_execute_script(tag) {
                        let p = Pause::CustomElement {
                            name: tag.name.clone(),
                            is: tag.attr("is").map(str::to_string),
                            attrs: tag.attrs.clone(),
                        };
                        self.held = Some(t);
                        return Some(p);
                    }
                }
                t
            }
        };
        self.dispatch_token(token);
        // An element the token didn't use (it was ignored, or went elsewhere) is dropped.
        self.provided = None;
        if let Some(n) = self.provided_used.take() {
            return Some(Pause::Inserted(n));
        }
        self.pending_script.take().map(Pause::Script)
    }

    /// HTML §13.2.6.1 "create an element for a token" steps 4-7: whether a start tag would
    /// make an HTML element that has a definition, outside fragment parsing (and outside
    /// template contents, whose document has no browsing context).
    fn will_execute_script(&self, tag: &Tag) -> bool {
        if self.custom_definitions.is_empty() || self.context.is_some() {
            return false;
        }
        let is = tag.attr("is");
        let defined = self
            .custom_definitions
            .iter()
            .any(|(name, local)| *local == tag.name && (*name == tag.name || Some(name.as_str()) == is));
        if !defined || self.open.iter().any(|&n| self.doc.is_html(n, "template")) {
            return false;
        }
        self.uses_html_rules(&Tok::Start(tag.clone()))
    }

    /// Whether parsing has stopped (the end of the input was processed).
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// The tokenizer, for the insertion point (`document.write`).
    pub fn tokenizer_mut(&mut self) -> &mut Tokenizer {
        &mut self.tokenizer
    }

    /// The parser's document once parsing is done.
    pub fn into_document(self) -> Document {
        self.doc
    }

    /// Run the parser to completion.
    pub fn run(mut self) -> Document {
        while !self.stopped {
            let token = self.tokenizer.next_token();
            self.dispatch_token(token);
        }
        self.doc
    }

    /// HTML §13.4 fragment parsing algorithm. Returns the document and the
    /// root `<html>` element whose children are the fragment's nodes.
    pub fn parse_fragment(input: &str, context_ns: Namespace, context_name: &str) -> (Document, NodeId) {
        Self::parse_fragment_with(input, context_ns, context_name, false)
    }

    /// [`TreeBuilder::parse_fragment`], with declarative shadow roots allowed or not.
    pub fn parse_fragment_with(
        input: &str,
        context_ns: Namespace,
        context_name: &str,
        allow_shadow_roots: bool,
    ) -> (Document, NodeId) {
        let mut tb = TreeBuilder::new(input);
        tb.allow_shadow_roots = allow_shadow_roots;
        let ctx = tb.doc.create_element(context_ns, context_name, Vec::new());
        tb.context = Some(ctx);
        if context_ns == Namespace::Html {
            tb.tokenizer.state = match context_name {
                "title" | "textarea" => State::Rcdata,
                "style" | "xmp" | "iframe" | "noembed" | "noframes" => State::Rawtext,
                "script" => State::ScriptData,
                "noscript" if tb.scripting => State::Rawtext,
                "plaintext" => State::Plaintext,
                _ => State::Data,
            };
        }
        let root = tb.doc.create_html_element("html");
        tb.doc.append_child(Document::ROOT, root);
        tb.open.push(root);
        if context_ns == Namespace::Html && context_name == "template" {
            tb.template_modes.push(M::InTemplate);
        }
        tb.reset_insertion_mode();
        // Nearest form ancestor of the context: the context has no ancestors here.
        tb.update_cdata_flag();
        while !tb.stopped {
            let token = tb.tokenizer.next_token();
            tb.dispatch_token(token);
        }
        (tb.doc, root)
    }

    fn error(&mut self) {
        self.errors += 1;
    }

    // ----- token intake -----

    fn dispatch_token(&mut self, token: Token) {
        match token {
            Token::Characters(mut s) => {
                if self.ignore_lf {
                    self.ignore_lf = false;
                    if s.starts_with('\n') {
                        s.remove(0);
                    }
                    if s.is_empty() {
                        return;
                    }
                }
                for run in split_runs(&s) {
                    self.process(Tok::Chars(run));
                }
            }
            other => {
                self.ignore_lf = false;
                let t = match other {
                    Token::Doctype(d) => Tok::Doctype(d),
                    Token::StartTag(t) => Tok::Start(t),
                    Token::EndTag(t) => Tok::End(t),
                    Token::Comment(c) => Tok::Comment(CommentLike::Comment(c)),
                    Token::ProcessingInstruction { target, data } => {
                        Tok::Comment(CommentLike::ProcessingInstruction { target, data })
                    }
                    Token::Eof => Tok::Eof,
                    Token::Characters(_) => unreachable!("handled above"),
                };
                self.process(t);
            }
        }
        self.update_cdata_flag();
    }

    fn update_cdata_flag(&mut self) {
        let allow = self
            .adjusted_current_node()
            .and_then(|n| self.doc.element(n))
            .is_some_and(|e| e.ns != Namespace::Html);
        self.tokenizer.allow_cdata = allow;
    }

    /// HTML §13.2.6 tree construction dispatcher.
    fn process(&mut self, tok: Tok) {
        let use_html = self.uses_html_rules(&tok);
        if use_html {
            self.process_in_mode(self.mode, tok);
        } else {
            self.process_foreign(tok);
        }
    }

    /// The dispatcher's choice (HTML §13.2.6): the current insertion mode's rules, rather
    /// than the rules for parsing tokens in foreign content.
    fn uses_html_rules(&self, tok: &Tok) -> bool {
        match self.adjusted_current_node() {
            None => true,
            Some(acn) => {
                let e = self.doc.element(acn);
                match e {
                    None => true,
                    Some(e) if e.ns == Namespace::Html => true,
                    Some(_) => {
                        let mtip = self.is_mathml_text_integration_point(acn);
                        let hip = self.is_html_integration_point(acn);
                        match &tok {
                            Tok::Start(t) if mtip && t.name != "mglyph" && t.name != "malignmark" => true,
                            Tok::Chars(_) if mtip => true,
                            Tok::Start(t)
                                if t.name == "svg"
                                    && self.doc.element(acn).is_some_and(|e| {
                                        e.ns == Namespace::MathMl && e.name == "annotation-xml"
                                    }) =>
                            {
                                true
                            }
                            Tok::Start(_) | Tok::Chars(_) if hip => true,
                            Tok::Eof => true,
                            _ => false,
                        }
                    }
                }
            }
        }
    }

    // ----- element/stack helpers -----

    fn current(&self) -> Option<NodeId> {
        self.open.last().copied()
    }

    fn context_is(&self, name: &str) -> bool {
        self.context.is_some_and(|c| self.is_html(c, name))
    }

    fn adjusted_current_node(&self) -> Option<NodeId> {
        if self.open.len() == 1 && self.context.is_some() {
            self.context
        } else {
            self.current()
        }
    }

    fn is_html(&self, id: NodeId, name: &str) -> bool {
        self.doc.is_html(id, name)
    }

    fn is_html_any(&self, id: NodeId, names: &[&str]) -> bool {
        self.doc.element(id).is_some_and(|e| e.ns == Namespace::Html && names.contains(&e.name.as_str()))
    }

    fn current_is(&self, name: &str) -> bool {
        self.current().is_some_and(|c| self.is_html(c, name))
    }

    fn current_is_any(&self, names: &[&str]) -> bool {
        self.current().is_some_and(|c| self.is_html_any(c, names))
    }

    fn elem_name(&self, id: NodeId) -> (Namespace, &str) {
        match self.doc.element(id) {
            Some(e) => (e.ns, e.name.as_str()),
            None => (Namespace::None, ""),
        }
    }

    fn is_special(&self, id: NodeId) -> bool {
        let (ns, name) = self.elem_name(id);
        match ns {
            Namespace::Html => SPECIAL_HTML.contains(&name),
            Namespace::MathMl => matches!(name, "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml"),
            Namespace::Svg => matches!(name, "foreignObject" | "desc" | "title"),
            _ => false,
        }
    }

    fn is_mathml_text_integration_point(&self, id: NodeId) -> bool {
        let (ns, name) = self.elem_name(id);
        ns == Namespace::MathMl && matches!(name, "mi" | "mo" | "mn" | "ms" | "mtext")
    }

    fn is_html_integration_point(&self, id: NodeId) -> bool {
        let Some(e) = self.doc.element(id) else { return false };
        match e.ns {
            Namespace::MathMl if e.name == "annotation-xml" => e.attr("encoding").is_some_and(|v| {
                v.eq_ignore_ascii_case("text/html") || v.eq_ignore_ascii_case("application/xhtml+xml")
            }),
            Namespace::Svg => matches!(e.name.as_str(), "foreignObject" | "desc" | "title"),
            _ => false,
        }
    }

    /// HTML §13.2.4.2 "has an element in the specific scope".
    fn in_scope_generic(&self, target: &dyn Fn(NodeId) -> bool, extra_html: &[&str]) -> bool {
        for &node in self.open.iter().rev() {
            if target(node) {
                return true;
            }
            let (ns, name) = self.elem_name(node);
            // HTML §13.2.4.3 "has an element in scope": the default list of boundary element
            // types (current spec: `select` is one of them).
            let boundary = {
                match ns {
                    Namespace::Html => {
                        matches!(
                            name,
                            "applet"
                                | "caption"
                                | "html"
                                | "table"
                                | "td"
                                | "th"
                                | "marquee"
                                | "object"
                                | "select"
                                | "template"
                        ) || extra_html.contains(&name)
                    }
                    Namespace::MathMl => {
                        matches!(name, "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml")
                    }
                    Namespace::Svg => matches!(name, "foreignObject" | "desc" | "title"),
                    _ => false,
                }
            };
            if boundary {
                return false;
            }
        }
        false
    }

    fn in_scope(&self, name: &str) -> bool {
        self.in_scope_generic(&|n| self.is_html(n, name), &[])
    }

    fn in_scope_any(&self, names: &[&str]) -> bool {
        self.in_scope_generic(&|n| self.is_html_any(n, names), &[])
    }

    fn in_scope_node(&self, node: NodeId) -> bool {
        self.in_scope_generic(&|n| n == node, &[])
    }

    fn in_list_item_scope(&self, name: &str) -> bool {
        self.in_scope_generic(&|n| self.is_html(n, name), &["ol", "ul"])
    }

    fn in_button_scope(&self, name: &str) -> bool {
        self.in_scope_generic(&|n| self.is_html(n, name), &["button"])
    }

    fn in_table_scope(&self, name: &str) -> bool {
        self.in_table_scope_any(&[name])
    }

    fn in_table_scope_any(&self, names: &[&str]) -> bool {
        for &node in self.open.iter().rev() {
            if self.is_html_any(node, names) {
                return true;
            }
            if self.is_html_any(node, &["html", "table", "template"]) {
                return false;
            }
        }
        false
    }

    fn pop(&mut self) -> Option<NodeId> {
        let n = self.open.pop();
        if let Some(n) = n {
            self.finish_children(n);
        }
        n
    }

    /// Pop nodes off the stack until it has `len` entries.
    fn truncate_open(&mut self, len: usize) {
        while self.open.len() > len {
            self.pop();
        }
    }

    /// HTML §13.2.4.3 "finish parsing the children of an element": run the element's
    /// children parsed steps (today only `option` defines any).
    fn finish_children(&mut self, element: NodeId) {
        if self.is_html(element, "option") {
            self.doc.option_children_parsed(element);
        }
    }

    fn pop_until(&mut self, name: &str) {
        while let Some(n) = self.pop() {
            if self.is_html(n, name) {
                break;
            }
        }
    }

    fn pop_until_any(&mut self, names: &[&str]) {
        while let Some(n) = self.pop() {
            if self.is_html_any(n, names) {
                break;
            }
        }
    }

    fn remove_from_stack(&mut self, node: NodeId) {
        if let Some(i) = self.open.iter().rposition(|&n| n == node) {
            self.open.remove(i);
            self.finish_children(node);
        }
    }

    const IMPLIED_END: &'static [&'static str] =
        &["dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc"];

    fn generate_implied_end_tags(&mut self, except: Option<&str>) {
        while let Some(c) = self.current() {
            if self.is_html_any(c, Self::IMPLIED_END) && except.is_none_or(|e| !self.is_html(c, e)) {
                self.pop();
            } else {
                break;
            }
        }
    }

    fn generate_implied_end_tags_thoroughly(&mut self) {
        const T: &[&str] = &[
            "caption", "colgroup", "dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc",
            "tbody", "td", "tfoot", "th", "thead", "tr",
        ];
        while self.current_is_any(T) {
            self.pop();
        }
    }

    // ----- insertion -----

    /// HTML §13.2.6.1 "appropriate place for inserting a node".
    fn insertion_location(&self, override_target: Option<NodeId>) -> (NodeId, Option<NodeId>) {
        let target = override_target.or_else(|| self.current()).unwrap_or(Document::ROOT);
        let (parent, before) = if self.foster_parenting
            && self.is_html_any(target, &["table", "tbody", "tfoot", "thead", "tr"])
        {
            let last_template = self.open.iter().rposition(|&n| self.is_html(n, "template"));
            let last_table = self.open.iter().rposition(|&n| self.is_html(n, "table"));
            match (last_template, last_table) {
                (Some(tp), lt) if lt.is_none_or(|t| tp > t) => (self.open[tp], None),
                (_, None) => (self.open[0], None),
                (_, Some(t)) => {
                    let table = self.open[t];
                    match self.doc.parent(table) {
                        Some(p) => (p, Some(table)),
                        None => (self.open[t - 1], None),
                    }
                }
            }
        } else {
            (target, None)
        };
        // Template contents.
        if let Some(contents) =
            self.doc.element(parent).and_then(
                |e| {
                    if e.is_html("template") {
                        e.template_contents
                    } else {
                        None
                    }
                },
            )
        {
            return (contents, None);
        }
        (parent, before)
    }

    fn insert_at(&mut self, (parent, before): (NodeId, Option<NodeId>), node: NodeId) {
        // HTML §13.2.6.1 "insert an element at the adjusted insertion location": if it can't
        // be inserted there (a script moved things so a second element would go into the
        // Document node), it isn't.
        if parent == Document::ROOT
            && self.doc.node(node).is_element()
            && self.doc.children(Document::ROOT).any(|c| self.doc.node(c).is_element())
        {
            return;
        }
        self.doc.insert_before(parent, node, before);
    }

    fn insert_chars(&mut self, s: &str) {
        let (parent, before) = self.insertion_location(None);
        if parent == Document::ROOT {
            return;
        }
        match before {
            Some(b) => self.doc.insert_text_before(parent, s, b),
            None => self.doc.append_text(parent, s),
        }
    }

    /// HTML §13.2.6.1 "insert a comment" / "insert a processing instruction".
    fn insert_comment(&mut self, c: &CommentLike, location: Option<(NodeId, Option<NodeId>)>) {
        let loc = location.unwrap_or_else(|| self.insertion_location(None));
        let c = match c {
            CommentLike::Comment(text) => self.doc.create_comment(text),
            CommentLike::ProcessingInstruction { target, data } => {
                self.doc.create_processing_instruction(target, data)
            }
        };
        self.insert_at(loc, c);
    }

    fn create_element_for(&mut self, tag: &Tag, ns: Namespace) -> NodeId {
        let attrs: Vec<Attribute> = tag
            .attrs
            .iter()
            .map(|(k, v)| {
                if ns != Namespace::Html {
                    foreign::adjust_foreign_attribute(k, v)
                } else {
                    Attribute::new(k, v)
                }
            })
            .collect();
        let name =
            if ns == Namespace::Svg { foreign::adjust_svg_tag_name(&tag.name) } else { tag.name.clone() };
        let attrs = if ns == Namespace::Svg {
            attrs.into_iter().map(foreign::adjust_svg_attribute).collect()
        } else if ns == Namespace::MathMl {
            attrs.into_iter().map(foreign::adjust_mathml_attribute).collect()
        } else {
            attrs
        };
        // The host's custom element for the held token already has its attributes (a
        // customized built-in's constructor may even have inserted it somewhere already).
        let provided = self.provided.filter(|&p| {
            ns == Namespace::Html
                && self.doc.element(p).is_some_and(|e| e.ns == Namespace::Html && e.name == name)
        });
        let el = match provided {
            Some(p) => {
                self.provided = None;
                self.provided_used = Some(p);
                p
            }
            None => {
                let el = self.doc.create_element(ns, &name, attrs);
                // §13.2.6.1 step 3: the element's `is` value is the token's `is` attribute.
                if let (Namespace::Html, Some(is)) = (ns, tag.attr("is")) {
                    self.doc.set_is_value(el, is);
                }
                el
            }
        };
        // §13.2.6.1 step 12: associate form-associated elements with the form element pointer
        // (unless a template is open, or a listed element has its own `form` attribute). Our
        // form owner lookup also requires the form to be in the element's tree.
        if ns == Namespace::Html && bb_dom::FORM_ASSOCIATED.contains(&name.as_str()) {
            if let Some(form) = self.form {
                let listed_with_form_attr =
                    bb_dom::LISTED.contains(&name.as_str()) && tag.attrs.iter().any(|(k, _)| k == "form");
                let template_open = self.open.iter().any(|&n| self.doc.is_html(n, "template"));
                if !listed_with_form_attr && !template_open {
                    self.doc.set_parser_form_owner(el, form);
                }
            }
        }
        el
    }

    fn insert_element(&mut self, tag: &Tag, ns: Namespace) -> NodeId {
        let loc = self.insertion_location(None);
        let el = self.create_element_for(tag, ns);
        self.insert_at(loc, el);
        self.open.push(el);
        el
    }

    fn insert_html(&mut self, tag: &Tag) -> NodeId {
        self.insert_element(tag, Namespace::Html)
    }

    fn insert_html_named(&mut self, name: &str) -> NodeId {
        self.insert_html(&Tag { name: name.to_string(), ..Default::default() })
    }

    fn insert_foreign(&mut self, tag: &Tag, ns: Namespace) {
        self.insert_element(tag, ns);
        if tag.self_closing {
            self.pop();
        }
    }

    // ----- active formatting elements -----

    fn push_formatting(&mut self, el: NodeId, tag: &Tag) {
        // Noah's Ark clause: at most three matching entries after the last marker.
        let mut count = 0;
        let mut earliest = None;
        for i in (0..self.afe.len()).rev() {
            match &self.afe[i] {
                Formatting::Marker => break,
                Formatting::Element(n, t) => {
                    let same = {
                        let (ns, name) = self.elem_name(*n);
                        ns == Namespace::Html && name == tag.name && {
                            let mut a = t.attrs.clone();
                            let mut b = tag.attrs.clone();
                            a.sort();
                            b.sort();
                            a == b
                        }
                    };
                    if same {
                        count += 1;
                        earliest = Some(i);
                    }
                }
            }
        }
        if count >= 3 {
            if let Some(i) = earliest {
                self.afe.remove(i);
            }
        }
        self.afe.push(Formatting::Element(el, tag.clone()));
    }

    fn afe_index(&self, node: NodeId) -> Option<usize> {
        self.afe.iter().position(|f| matches!(f, Formatting::Element(n, _) if *n == node))
    }

    fn clear_afe_to_marker(&mut self) {
        while let Some(f) = self.afe.pop() {
            if matches!(f, Formatting::Marker) {
                break;
            }
        }
    }

    /// HTML §13.2.4.3 "reconstruct the active formatting elements".
    fn reconstruct_afe(&mut self) {
        if self.afe.is_empty() {
            return;
        }
        let in_stack = |tb: &Self, f: &Formatting| match f {
            Formatting::Marker => true,
            Formatting::Element(n, _) => tb.open.contains(n),
        };
        if in_stack(self, self.afe.last().unwrap()) {
            return;
        }
        let mut i = self.afe.len() - 1;
        // Rewind.
        while i > 0 {
            if in_stack(self, &self.afe[i - 1]) {
                break;
            }
            i -= 1;
        }
        // Advance / create.
        while i < self.afe.len() {
            let tag = match &self.afe[i] {
                Formatting::Element(_, t) => t.clone(),
                Formatting::Marker => unreachable!("markers are always 'in stack'"),
            };
            let el = self.insert_html(&tag);
            self.afe[i] = Formatting::Element(el, tag);
            i += 1;
        }
    }

    /// HTML §13.2.6.4.7 adoption agency algorithm. Returns `false` if the
    /// caller should "act as described in the any other end tag entry".
    fn adoption_agency(&mut self, subject: &str) -> bool {
        if let Some(c) = self.current() {
            if self.is_html(c, subject) && self.afe_index(c).is_none() {
                self.pop();
                return true;
            }
        }
        for _outer in 0..8 {
            // Formatting element: last in list after last marker with this name.
            let mut fe_idx = None;
            for i in (0..self.afe.len()).rev() {
                match &self.afe[i] {
                    Formatting::Marker => break,
                    Formatting::Element(n, _) if self.is_html(*n, subject) => {
                        fe_idx = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            let Some(fe_idx) = fe_idx else { return false };
            let (fe, fe_tag) = match &self.afe[fe_idx] {
                Formatting::Element(n, t) => (*n, t.clone()),
                Formatting::Marker => unreachable!(),
            };
            let Some(fe_stack) = self.open.iter().position(|&n| n == fe) else {
                self.error();
                self.afe.remove(fe_idx);
                return true;
            };
            if !self.in_scope_node(fe) {
                self.error();
                return true;
            }
            if self.current() != Some(fe) {
                self.error();
            }
            let fb_stack = (fe_stack + 1..self.open.len()).find(|&i| self.is_special(self.open[i]));
            let Some(fb_stack) = fb_stack else {
                self.truncate_open(fe_stack);
                self.afe.remove(fe_idx);
                return true;
            };
            let furthest_block = self.open[fb_stack];
            let common_ancestor = self.open[fe_stack - 1];
            let mut bookmark = fe_idx;
            let mut node_idx = fb_stack;
            let mut last_node = furthest_block;
            let mut inner = 0;
            loop {
                inner += 1;
                node_idx -= 1;
                let node = self.open[node_idx];
                if node == fe {
                    break;
                }
                let mut node_afe = self.afe_index(node);
                if inner > 3 {
                    if let Some(ai) = node_afe {
                        self.afe.remove(ai);
                        if ai < bookmark {
                            bookmark -= 1;
                        }
                        node_afe = None;
                    }
                }
                let Some(ai) = node_afe else {
                    self.open.remove(node_idx);
                    self.finish_children(node);
                    continue;
                };
                let tag = match &self.afe[ai] {
                    Formatting::Element(_, t) => t.clone(),
                    Formatting::Marker => unreachable!(),
                };
                let new_el = self.create_element_for(&tag, Namespace::Html);
                self.afe[ai] = Formatting::Element(new_el, tag);
                self.open[node_idx] = new_el;
                if last_node == furthest_block {
                    bookmark = ai + 1;
                }
                self.doc.append_child(new_el, last_node);
                last_node = new_el;
            }
            let loc = self.insertion_location(Some(common_ancestor));
            self.insert_at(loc, last_node);
            let new_el = self.create_element_for(&fe_tag, Namespace::Html);
            self.doc.reparent_children(furthest_block, new_el);
            self.doc.append_child(furthest_block, new_el);
            // Replace the formatting element in the list at the bookmark.
            let old = self.afe_index(fe).expect("formatting element still listed");
            self.afe.remove(old);
            if old < bookmark {
                bookmark -= 1;
            }
            let bookmark = bookmark.min(self.afe.len());
            self.afe.insert(bookmark, Formatting::Element(new_el, fe_tag));
            self.remove_from_stack(fe);
            let fb_pos =
                self.open.iter().position(|&n| n == furthest_block).expect("furthest block in stack");
            self.open.insert(fb_pos + 1, new_el);
        }
        true
    }

    fn close_p(&mut self) {
        self.generate_implied_end_tags(Some("p"));
        if !self.current_is("p") {
            self.error();
        }
        self.pop_until("p");
    }

    /// HTML §13.2.4.1 "reset the insertion mode appropriately".
    fn reset_insertion_mode(&mut self) {
        for i in (0..self.open.len()).rev() {
            let last = i == 0;
            let mut node = self.open[i];
            if last {
                if let Some(c) = self.context {
                    node = c;
                }
            }
            let (ns, name) = self.elem_name(node);
            if ns != Namespace::Html {
                if last {
                    self.mode = M::InBody;
                    return;
                }
                continue;
            }
            let name = name.to_string();
            match name.as_str() {
                "td" | "th" if !last => {
                    self.mode = M::InCell;
                    return;
                }
                "tr" => {
                    self.mode = M::InRow;
                    return;
                }
                "tbody" | "thead" | "tfoot" => {
                    self.mode = M::InTableBody;
                    return;
                }
                "caption" => {
                    self.mode = M::InCaption;
                    return;
                }
                "colgroup" => {
                    self.mode = M::InColumnGroup;
                    return;
                }
                "table" => {
                    self.mode = M::InTable;
                    return;
                }
                "template" => {
                    self.mode = *self.template_modes.last().unwrap_or(&M::InTemplate);
                    return;
                }
                "head" if !last => {
                    self.mode = M::InHead;
                    return;
                }
                "body" => {
                    self.mode = M::InBody;
                    return;
                }
                "frameset" => {
                    self.mode = M::InFrameset;
                    return;
                }
                "html" => {
                    self.mode = if self.head.is_none() { M::BeforeHead } else { M::AfterHead };
                    return;
                }
                _ => {}
            }
            if last {
                self.mode = M::InBody;
                return;
            }
        }
        self.mode = M::InBody;
    }

    // ----- generic parsing algorithms -----

    fn parse_text_element(&mut self, tag: &Tag, state: State) {
        self.insert_html(tag);
        self.tokenizer.state = state;
        self.original_mode = self.mode;
        self.mode = M::Text;
    }

    /// HTML §13.2.7 "stop parsing": pop all the nodes off the stack of open elements, then
    /// (our stand-in for the event loop) run the queued microtasks.
    fn stop_parsing(&mut self) {
        self.truncate_open(0);
        self.doc.run_select_microtasks();
        self.stopped = true;
    }

    fn quirks_from_doctype(d: &Doctype) -> QuirksMode {
        crate::quirks::doctype_quirks(d)
    }

    // ===================== insertion modes =====================

    fn process_in_mode(&mut self, mode: InsertionMode, tok: Tok) {
        match mode {
            M::Initial => self.mode_initial(tok),
            M::BeforeHtml => self.mode_before_html(tok),
            M::BeforeHead => self.mode_before_head(tok),
            M::InHead => self.mode_in_head(tok),
            M::InHeadNoscript => self.mode_in_head_noscript(tok),
            M::AfterHead => self.mode_after_head(tok),
            M::InBody => self.mode_in_body(tok),
            M::Text => self.mode_text(tok),
            M::InTable => self.mode_in_table(tok),
            M::InTableText => self.mode_in_table_text(tok),
            M::InCaption => self.mode_in_caption(tok),
            M::InColumnGroup => self.mode_in_column_group(tok),
            M::InTableBody => self.mode_in_table_body(tok),
            M::InRow => self.mode_in_row(tok),
            M::InCell => self.mode_in_cell(tok),
            M::InTemplate => self.mode_in_template(tok),
            M::AfterBody => self.mode_after_body(tok),
            M::InFrameset => self.mode_in_frameset(tok),
            M::AfterFrameset => self.mode_after_frameset(tok),
            M::AfterAfterBody => self.mode_after_after_body(tok),
            M::AfterAfterFrameset => self.mode_after_after_frameset(tok),
        }
    }

    fn reprocess(&mut self, mode: InsertionMode, tok: Tok) {
        self.mode = mode;
        self.process(tok);
    }

    // §13.2.6.4.1
    fn mode_initial(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(ref s) if is_ws_run(s) => {}
            Tok::Comment(c) => self.insert_comment(&c, Some((Document::ROOT, None))),
            Tok::Doctype(d) => {
                let dt = self.doc.create_doctype(
                    d.name.as_deref().unwrap_or(""),
                    d.public_id.as_deref().unwrap_or(""),
                    d.system_id.as_deref().unwrap_or(""),
                );
                self.doc.append_child(Document::ROOT, dt);
                self.doc.quirks_mode = Self::quirks_from_doctype(&d);
                self.mode = M::BeforeHtml;
            }
            other => {
                self.error();
                self.doc.quirks_mode = QuirksMode::Quirks;
                self.reprocess(M::BeforeHtml, other);
            }
        }
    }

    // §13.2.6.4.2
    fn mode_before_html(&mut self, tok: Tok) {
        match tok {
            Tok::Doctype(_) => self.error(),
            Tok::Comment(c) => self.insert_comment(&c, Some((Document::ROOT, None))),
            Tok::Chars(ref s) if is_ws_run(s) => {}
            Tok::Start(ref t) if t.name == "html" => {
                let el = self.create_element_for(t, Namespace::Html);
                self.doc.append_child(Document::ROOT, el);
                self.open.push(el);
                self.mode = M::BeforeHead;
            }
            Tok::End(ref t) if !matches!(t.name.as_str(), "head" | "body" | "html" | "br") => self.error(),
            other => {
                let el = self.doc.create_html_element("html");
                self.doc.append_child(Document::ROOT, el);
                self.open.push(el);
                self.reprocess(M::BeforeHead, other);
            }
        }
    }

    // §13.2.6.4.3
    fn mode_before_head(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(ref s) if is_ws_run(s) => {}
            Tok::Comment(c) => self.insert_comment(&c, None),
            Tok::Doctype(_) => self.error(),
            Tok::Start(ref t) if t.name == "html" => self.mode_in_body(tok),
            Tok::Start(ref t) if t.name == "head" => {
                let h = self.insert_html(t);
                self.head = Some(h);
                self.mode = M::InHead;
            }
            Tok::End(ref t) if !matches!(t.name.as_str(), "head" | "body" | "html" | "br") => self.error(),
            other => {
                let h = self.insert_html_named("head");
                self.head = Some(h);
                self.reprocess(M::InHead, other);
            }
        }
    }

    // §13.2.6.4.4
    fn mode_in_head(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(ref s) if is_ws_run(s) => self.insert_chars(s),
            Tok::Comment(c) => self.insert_comment(&c, None),
            Tok::Doctype(_) => self.error(),
            Tok::Start(ref t) if t.name == "html" => self.mode_in_body(tok),
            Tok::Start(ref t)
                if matches!(t.name.as_str(), "base" | "basefont" | "bgsound" | "link" | "meta") =>
            {
                self.insert_html(t);
                self.pop();
                // TODO(HTML §13.2.6.4.4): change the encoding when a late <meta charset> appears.
            }
            Tok::Start(ref t) if t.name == "title" => self.parse_text_element(t, State::Rcdata),
            Tok::Start(ref t) if t.name == "noscript" && self.scripting => {
                self.parse_text_element(t, State::Rawtext)
            }
            Tok::Start(ref t) if matches!(t.name.as_str(), "noframes" | "style") => {
                self.parse_text_element(t, State::Rawtext)
            }
            Tok::Start(ref t) if t.name == "noscript" => {
                self.insert_html(t);
                self.mode = M::InHeadNoscript;
            }
            Tok::Start(ref t) if t.name == "script" => {
                let loc = self.insertion_location(None);
                let el = self.create_element_for(t, Namespace::Html);
                self.insert_at(loc, el);
                self.open.push(el);
                self.tokenizer.state = State::ScriptData;
                self.original_mode = self.mode;
                self.mode = M::Text;
            }
            Tok::End(ref t) if t.name == "head" => {
                self.pop();
                self.mode = M::AfterHead;
            }
            Tok::End(ref t) if matches!(t.name.as_str(), "body" | "html" | "br") => {
                self.pop();
                self.reprocess(M::AfterHead, tok);
            }
            Tok::Start(ref t) if t.name == "template" => {
                self.afe.push(Formatting::Marker);
                self.frameset_ok = false;
                self.mode = M::InTemplate;
                self.template_modes.push(M::InTemplate);
                let attr = |n: &str| t.attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
                let mode = attr("shadowrootmode").map(str::to_ascii_lowercase);
                let open = match mode.as_deref() {
                    Some("open") => Some(true),
                    Some("closed") => Some(false),
                    _ => None,
                };
                let host = self.adjusted_current_node();
                match (open, host) {
                    // (The fragment parser's context element is never a host: browsers don't
                    // attach to it, as the shadow-dom WPT tests expect.)
                    (Some(open), Some(host))
                        if self.allow_shadow_roots
                            && self.open.first() != Some(&host)
                            && self.context != Some(host) =>
                    {
                        // A declarative shadow root: the template is only on the stack, and its
                        // contents are the host's new shadow root.
                        let loc = self.insertion_location(None);
                        let template = self.create_element_for(t, Namespace::Html);
                        self.open.push(template);
                        let disabled = self.doc.element(host).is_some_and(|e| {
                            self.shadow_disabled
                                .iter()
                                .any(|n| *n == e.name || e.attr("is") == Some(n.as_str()))
                        });
                        if disabled || !can_attach_shadow(&self.doc, host) {
                            self.insert_at(loc, template);
                            return;
                        }
                        let info = bb_dom::ShadowRootInfo {
                            open,
                            clonable: attr("shadowrootclonable").is_some(),
                            serializable: attr("shadowrootserializable").is_some(),
                            delegates_focus: attr("shadowrootdelegatesfocus").is_some(),
                            slot_assignment: if attr("shadowrootslotassignment")
                                .is_some_and(|v| v.eq_ignore_ascii_case("manual"))
                            {
                                bb_dom::SlotAssignment::Manual
                            } else {
                                bb_dom::SlotAssignment::Named
                            },
                            declarative: true,
                            available_to_element_internals: true,
                            ..Default::default()
                        };
                        let root = self.doc.attach_shadow(host, info);
                        if let Some(e) = self.doc.element_mut(template) {
                            e.template_contents = Some(root);
                        }
                    }
                    _ => {
                        self.insert_html(t);
                    }
                }
            }
            Tok::End(ref t) if t.name == "template" => {
                if !self.open.iter().any(|&n| self.is_html(n, "template")) {
                    self.error();
                    return;
                }
                self.generate_implied_end_tags_thoroughly();
                if !self.current_is("template") {
                    self.error();
                }
                self.pop_until("template");
                self.clear_afe_to_marker();
                self.template_modes.pop();
                self.reset_insertion_mode();
            }
            Tok::Start(ref t) if t.name == "head" => self.error(),
            Tok::End(_) => self.error(),
            other => {
                self.pop();
                self.reprocess(M::AfterHead, other);
            }
        }
    }

    // §13.2.6.4.5
    fn mode_in_head_noscript(&mut self, tok: Tok) {
        match tok {
            Tok::Doctype(_) => self.error(),
            Tok::Start(ref t) if t.name == "html" => self.mode_in_body(tok),
            Tok::End(ref t) if t.name == "noscript" => {
                self.pop();
                self.mode = M::InHead;
            }
            Tok::Chars(ref s) if is_ws_run(s) => self.mode_in_head(tok),
            Tok::Comment(_) => self.mode_in_head(tok),
            Tok::Start(ref t)
                if matches!(
                    t.name.as_str(),
                    "basefont" | "bgsound" | "link" | "meta" | "noframes" | "style"
                ) =>
            {
                self.mode_in_head(tok)
            }
            Tok::End(ref t) if t.name == "br" => {
                self.error();
                self.pop();
                self.reprocess(M::InHead, tok);
            }
            Tok::Start(ref t) if matches!(t.name.as_str(), "head" | "noscript") => self.error(),
            Tok::End(_) => self.error(),
            other => {
                self.error();
                self.pop();
                self.reprocess(M::InHead, other);
            }
        }
    }

    /// HTML §13.2.4.4: the parser is "parsing template contents" if there is a template element
    /// on the stack of open elements, or the fragment context element is a template element.
    fn parsing_template_contents(&self) -> bool {
        self.open.iter().any(|&n| self.is_html(n, "template"))
            || self.context.is_some_and(|c| self.is_html(c, "template"))
    }

    // §13.2.6.4.6
    fn mode_after_head(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(ref s) if is_ws_run(s) => self.insert_chars(s),
            Tok::Comment(c) => self.insert_comment(&c, None),
            Tok::Doctype(_) => self.error(),
            Tok::Start(ref t) if t.name == "html" => self.mode_in_body(tok),
            Tok::Start(ref t) if t.name == "body" => {
                self.insert_html(t);
                self.frameset_ok = false;
                self.mode = M::InBody;
            }
            Tok::Start(ref t) if t.name == "frameset" => {
                self.insert_html(t);
                self.mode = M::InFrameset;
            }
            Tok::Start(ref t)
                if matches!(
                    t.name.as_str(),
                    "base"
                        | "basefont"
                        | "bgsound"
                        | "link"
                        | "meta"
                        | "noframes"
                        | "script"
                        | "style"
                        | "template"
                        | "title"
                ) =>
            {
                self.error();
                let head = self.head.expect("head exists after head");
                self.open.push(head);
                self.mode_in_head(tok);
                self.remove_from_stack(head);
            }
            Tok::End(ref t) if t.name == "template" => self.mode_in_head(tok),
            Tok::End(ref t) if matches!(t.name.as_str(), "body" | "html" | "br") => {
                self.insert_html_named("body");
                self.frameset_ok = true;
                self.reprocess(M::InBody, tok);
            }
            Tok::Start(ref t) if t.name == "head" => self.error(),
            Tok::End(_) => self.error(),
            other => {
                // "Insert an HTML element for a "body" start tag token with no attributes. Set
                // the frameset-ok flag to "ok"." (tokens seen in the head, such as a template,
                // don't block a later frameset).
                self.insert_html_named("body");
                self.frameset_ok = true;
                self.reprocess(M::InBody, other);
            }
        }
    }

    // §13.2.6.4.7 "in body"
    fn mode_in_body(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(ref s) => match run_class(s) {
                CharClass::Null => self.error(),
                CharClass::Ws => {
                    self.reconstruct_afe();
                    self.insert_chars(s);
                }
                CharClass::Other => {
                    self.reconstruct_afe();
                    self.insert_chars(s);
                    self.frameset_ok = false;
                }
            },
            Tok::Comment(c) => self.insert_comment(&c, None),
            Tok::Doctype(_) => self.error(),
            Tok::Start(t) => self.in_body_start(t),
            Tok::End(t) => self.in_body_end(t),
            Tok::Eof => {
                if !self.template_modes.is_empty() {
                    self.mode_in_template(Tok::Eof);
                } else {
                    self.stop_parsing();
                }
            }
        }
    }

    fn in_body_start(&mut self, t: Tag) {
        let name = t.name.as_str();
        match name {
            "html" => {
                self.error();
                if self.open.iter().any(|&n| self.is_html(n, "template")) {
                    return;
                }
                if let Some(&html) = self.open.first() {
                    self.add_missing_attrs(html, &t);
                }
            }
            "base" | "basefont" | "bgsound" | "link" | "meta" | "noframes" | "script" | "style"
            | "template" | "title" => self.mode_in_head(Tok::Start(t)),
            "body" => {
                self.error();
                if self.open.len() == 1
                    || !self.is_html(self.open[1], "body")
                    || self.open.iter().any(|&n| self.is_html(n, "template"))
                {
                    return;
                }
                self.frameset_ok = false;
                let body = self.open[1];
                self.add_missing_attrs(body, &t);
            }
            "frameset" => {
                self.error();
                if self.open.len() == 1 || !self.is_html(self.open[1], "body") || !self.frameset_ok {
                    return;
                }
                let body = self.open[1];
                self.doc.detach(body);
                self.truncate_open(1);
                self.insert_html(&t);
                self.mode = M::InFrameset;
            }
            "address" | "article" | "aside" | "blockquote" | "center" | "details" | "dialog" | "dir"
            | "div" | "dl" | "fieldset" | "figcaption" | "figure" | "footer" | "header" | "hgroup"
            | "main" | "menu" | "nav" | "ol" | "p" | "search" | "section" | "summary" | "ul" => {
                if self.in_button_scope("p") {
                    self.close_p();
                }
                self.insert_html(&t);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if self.in_button_scope("p") {
                    self.close_p();
                }
                if self.current_is_any(&["h1", "h2", "h3", "h4", "h5", "h6"]) {
                    self.error();
                    self.pop();
                }
                self.insert_html(&t);
            }
            "pre" | "listing" => {
                if self.in_button_scope("p") {
                    self.close_p();
                }
                self.insert_html(&t);
                self.ignore_lf = true;
                self.frameset_ok = false;
            }
            "form" => {
                let in_template = self.parsing_template_contents();
                if self.form.is_some() && !in_template {
                    self.error();
                    return;
                }
                if self.in_button_scope("p") {
                    self.close_p();
                }
                let el = self.insert_html(&t);
                if !in_template {
                    self.form = Some(el);
                }
            }
            "li" | "dd" | "dt" => {
                self.frameset_ok = false;
                let targets: &[&str] = if name == "li" { &["li"] } else { &["dd", "dt"] };
                for i in (0..self.open.len()).rev() {
                    let node = self.open[i];
                    if self.is_html_any(node, targets) {
                        let nn = self.elem_name(node).1.to_string();
                        self.generate_implied_end_tags(Some(&nn));
                        if !self.current_is(&nn) {
                            self.error();
                        }
                        self.pop_until(&nn);
                        break;
                    }
                    if self.is_special(node) && !self.is_html_any(node, &["address", "div", "p"]) {
                        break;
                    }
                }
                if self.in_button_scope("p") {
                    self.close_p();
                }
                self.insert_html(&t);
            }
            "plaintext" => {
                if self.in_button_scope("p") {
                    self.close_p();
                }
                self.insert_html(&t);
                self.tokenizer.state = State::Plaintext;
            }
            "button" => {
                if self.in_scope("button") {
                    self.error();
                    self.generate_implied_end_tags(None);
                    self.pop_until("button");
                }
                self.reconstruct_afe();
                self.insert_html(&t);
                self.frameset_ok = false;
            }
            "a" => {
                let mut existing = None;
                for f in self.afe.iter().rev() {
                    match f {
                        Formatting::Marker => break,
                        Formatting::Element(n, _) if self.is_html(*n, "a") => {
                            existing = Some(*n);
                            break;
                        }
                        _ => {}
                    }
                }
                if let Some(a) = existing {
                    self.error();
                    if !self.adoption_agency("a") {
                        self.any_other_end_tag("a");
                    }
                    if let Some(i) = self.afe_index(a) {
                        self.afe.remove(i);
                    }
                    self.remove_from_stack(a);
                }
                self.reconstruct_afe();
                let el = self.insert_html(&t);
                self.push_formatting(el, &t);
            }
            "b" | "big" | "code" | "em" | "font" | "i" | "s" | "small" | "strike" | "strong" | "tt" | "u" => {
                self.reconstruct_afe();
                let el = self.insert_html(&t);
                self.push_formatting(el, &t);
            }
            "nobr" => {
                self.reconstruct_afe();
                if self.in_scope("nobr") {
                    self.error();
                    if !self.adoption_agency("nobr") {
                        self.any_other_end_tag("nobr");
                    }
                    self.reconstruct_afe();
                }
                let el = self.insert_html(&t);
                self.push_formatting(el, &t);
            }
            "applet" | "marquee" | "object" => {
                self.reconstruct_afe();
                self.insert_html(&t);
                self.afe.push(Formatting::Marker);
                self.frameset_ok = false;
            }
            "table" => {
                if self.doc.quirks_mode != QuirksMode::Quirks && self.in_button_scope("p") {
                    self.close_p();
                }
                self.insert_html(&t);
                self.frameset_ok = false;
                self.mode = M::InTable;
            }
            "area" | "br" | "embed" | "img" | "keygen" | "wbr" => {
                self.reconstruct_afe();
                self.insert_html(&t);
                self.pop();
                self.frameset_ok = false;
            }
            "input" => {
                if self.context_is("select") {
                    self.error();
                    return;
                }
                if self.in_scope("select") {
                    self.error();
                    self.pop_until("select");
                }
                self.reconstruct_afe();
                self.insert_html(&t);
                self.pop();
                if !t.attr("type").is_some_and(|v| v.eq_ignore_ascii_case("hidden")) {
                    self.frameset_ok = false;
                }
            }
            "param" | "source" | "track" => {
                self.insert_html(&t);
                self.pop();
            }
            "hr" => {
                if self.in_button_scope("p") {
                    self.close_p();
                }
                if self.in_scope("select") {
                    self.generate_implied_end_tags(None);
                    if self.in_scope("option") || self.in_scope("optgroup") {
                        self.error();
                    }
                }
                self.insert_html(&t);
                self.pop();
                self.frameset_ok = false;
            }
            "image" => {
                self.error();
                let mut t2 = t;
                t2.name = "img".into();
                self.in_body_start(t2);
            }
            "textarea" => {
                self.insert_html(&t);
                self.ignore_lf = true;
                self.tokenizer.state = State::Rcdata;
                self.original_mode = self.mode;
                self.frameset_ok = false;
                self.mode = M::Text;
            }
            "xmp" => {
                if self.in_button_scope("p") {
                    self.close_p();
                }
                self.reconstruct_afe();
                self.frameset_ok = false;
                self.parse_text_element(&t, State::Rawtext);
            }
            "iframe" => {
                self.frameset_ok = false;
                self.parse_text_element(&t, State::Rawtext);
            }
            "noembed" => self.parse_text_element(&t, State::Rawtext),
            "noscript" if self.scripting => self.parse_text_element(&t, State::Rawtext),
            // The current HTML Standard parses select content "in body"
            // (the "in select" insertion modes were removed in 2025).
            "select" => {
                if self.context_is("select") {
                    self.error();
                } else if self.in_scope("select") {
                    self.error();
                    self.pop_until("select");
                } else {
                    self.reconstruct_afe();
                    self.insert_html(&t);
                    self.frameset_ok = false;
                }
            }
            "option" => {
                if self.in_scope("select") {
                    self.generate_implied_end_tags(Some("optgroup"));
                    if self.in_scope("option") {
                        self.error();
                    }
                } else if self.current_is("option") {
                    self.pop();
                }
                self.reconstruct_afe();
                self.insert_html(&t);
            }
            "optgroup" => {
                if self.in_scope("select") {
                    self.generate_implied_end_tags(None);
                    if self.in_scope("option") || self.in_scope("optgroup") {
                        self.error();
                    }
                } else if self.current_is("option") {
                    self.pop();
                }
                self.reconstruct_afe();
                self.insert_html(&t);
            }
            "rb" | "rtc" => {
                if self.in_scope("ruby") {
                    self.generate_implied_end_tags(None);
                    if !self.current_is("ruby") {
                        self.error();
                    }
                }
                self.insert_html(&t);
            }
            "rp" | "rt" => {
                if self.in_scope("ruby") {
                    self.generate_implied_end_tags(Some("rtc"));
                    if !self.current_is_any(&["rtc", "ruby"]) {
                        self.error();
                    }
                }
                self.insert_html(&t);
            }
            "math" => {
                self.reconstruct_afe();
                self.insert_foreign(&t, Namespace::MathMl);
            }
            "svg" => {
                self.reconstruct_afe();
                self.insert_foreign(&t, Namespace::Svg);
            }
            "caption" | "col" | "colgroup" | "frame" | "head" | "tbody" | "td" | "tfoot" | "th" | "thead"
            | "tr" => self.error(),
            _ => {
                self.reconstruct_afe();
                self.insert_html(&t);
            }
        }
    }

    fn add_missing_attrs(&mut self, el: NodeId, t: &Tag) {
        if let Some(e) = self.doc.element_mut(el) {
            for (k, v) in &t.attrs {
                if !e.attrs.iter().any(|a| a.name == *k && a.ns == Namespace::None) {
                    e.attrs.push(Attribute::new(k, v));
                }
            }
        }
    }

    fn in_body_end(&mut self, t: Tag) {
        let name = t.name.as_str();
        match name {
            "template" => self.mode_in_head(Tok::End(t)),
            "body" | "html" => {
                if !self.in_scope("body") {
                    self.error();
                    return;
                }
                self.mode = M::AfterBody;
                if name == "html" {
                    self.process(Tok::End(t));
                }
            }
            "address" | "article" | "aside" | "blockquote" | "button" | "center" | "details" | "dialog"
            | "dir" | "div" | "dl" | "fieldset" | "figcaption" | "figure" | "footer" | "header"
            | "hgroup" | "listing" | "main" | "menu" | "nav" | "ol" | "pre" | "search" | "section"
            | "select" | "summary" | "ul" => {
                if !self.in_scope(name) {
                    self.error();
                    return;
                }
                self.generate_implied_end_tags(None);
                if !self.current_is(name) {
                    self.error();
                }
                self.pop_until(name);
            }
            "form" => {
                if !self.parsing_template_contents() {
                    let node = self.form.take();
                    let Some(node) = node.filter(|&n| self.in_scope_node(n)) else {
                        self.error();
                        return;
                    };
                    self.generate_implied_end_tags(None);
                    if self.current() != Some(node) {
                        self.error();
                    }
                    self.remove_from_stack(node);
                } else {
                    if !self.in_scope("form") {
                        self.error();
                        return;
                    }
                    self.generate_implied_end_tags(None);
                    if !self.current_is("form") {
                        self.error();
                    }
                    self.pop_until("form");
                }
            }
            "p" => {
                if !self.in_button_scope("p") {
                    self.error();
                    self.insert_html_named("p");
                }
                self.close_p();
            }
            "li" => {
                if !self.in_list_item_scope("li") {
                    self.error();
                    return;
                }
                self.generate_implied_end_tags(Some("li"));
                if !self.current_is("li") {
                    self.error();
                }
                self.pop_until("li");
            }
            "dd" | "dt" => {
                if !self.in_scope(name) {
                    self.error();
                    return;
                }
                self.generate_implied_end_tags(Some(name));
                if !self.current_is(name) {
                    self.error();
                }
                self.pop_until(name);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                const H: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];
                if !self.in_scope_any(H) {
                    self.error();
                    return;
                }
                self.generate_implied_end_tags(None);
                if !self.current_is(name) {
                    self.error();
                }
                self.pop_until_any(H);
            }
            "a" | "b" | "big" | "code" | "em" | "font" | "i" | "nobr" | "s" | "small" | "strike"
            | "strong" | "tt" | "u" => {
                if !self.adoption_agency(name) {
                    self.any_other_end_tag(name);
                }
            }
            "applet" | "marquee" | "object" => {
                if !self.in_scope(name) {
                    self.error();
                    return;
                }
                self.generate_implied_end_tags(None);
                if !self.current_is(name) {
                    self.error();
                }
                self.pop_until(name);
                self.clear_afe_to_marker();
            }
            "br" => {
                self.error();
                self.in_body_start(Tag { name: "br".into(), ..Default::default() });
            }
            _ => self.any_other_end_tag(name),
        }
    }

    fn any_other_end_tag(&mut self, name: &str) {
        for i in (0..self.open.len()).rev() {
            let node = self.open[i];
            if self.is_html(node, name) {
                self.generate_implied_end_tags(Some(name));
                if self.current() != Some(node) {
                    self.error();
                }
                self.truncate_open(i);
                return;
            }
            if self.is_special(node) {
                self.error();
                return;
            }
        }
    }

    // §13.2.6.4.8
    fn mode_text(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(s) => self.insert_chars(&s),
            Tok::Eof => {
                self.error();
                self.pop();
                let m = self.original_mode;
                self.reprocess(m, Tok::Eof);
            }
            Tok::End(ref t) => {
                let node = self.pop();
                if t.name == "script" {
                    self.pending_script = node;
                }
                self.mode = self.original_mode;
            }
            _ => {}
        }
    }

    // §13.2.6.4.9
    fn mode_in_table(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(_) if self.current_is_any(&["table", "tbody", "template", "tfoot", "thead", "tr"]) => {
                self.pending_table_text.clear();
                self.original_mode = self.mode;
                self.reprocess(M::InTableText, tok);
            }
            Tok::Comment(c) => self.insert_comment(&c, None),
            Tok::Doctype(_) => self.error(),
            Tok::Start(ref t) => match t.name.as_str() {
                "caption" => {
                    self.clear_stack_to_table_context();
                    self.afe.push(Formatting::Marker);
                    self.insert_html(t);
                    self.mode = M::InCaption;
                }
                "colgroup" => {
                    self.clear_stack_to_table_context();
                    self.insert_html(t);
                    self.mode = M::InColumnGroup;
                }
                "col" => {
                    self.clear_stack_to_table_context();
                    self.insert_html_named("colgroup");
                    self.reprocess(M::InColumnGroup, tok);
                }
                "tbody" | "tfoot" | "thead" => {
                    self.clear_stack_to_table_context();
                    self.insert_html(t);
                    self.mode = M::InTableBody;
                }
                "td" | "th" | "tr" => {
                    self.clear_stack_to_table_context();
                    self.insert_html_named("tbody");
                    self.reprocess(M::InTableBody, tok);
                }
                "table" => {
                    self.error();
                    if !self.in_table_scope("table") {
                        return;
                    }
                    self.pop_until("table");
                    self.reset_insertion_mode();
                    self.process(tok);
                }
                "style" | "script" | "template" => self.mode_in_head(tok),
                "input" if t.attr("type").is_some_and(|v| v.eq_ignore_ascii_case("hidden")) => {
                    self.error();
                    self.insert_html(t);
                    self.pop();
                }
                "form" => {
                    self.error();
                    let in_template = self.parsing_template_contents();
                    if self.form.is_some() && !in_template {
                        return;
                    }
                    let el = self.insert_html(t);
                    if !in_template {
                        self.form = Some(el);
                    }
                    self.pop();
                }
                _ => self.in_table_anything_else(tok),
            },
            Tok::End(ref t) => match t.name.as_str() {
                "table" => {
                    if !self.in_table_scope("table") {
                        self.error();
                        return;
                    }
                    self.pop_until("table");
                    self.reset_insertion_mode();
                }
                "body" | "caption" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot" | "th"
                | "thead" | "tr" => self.error(),
                "template" => self.mode_in_head(tok),
                _ => self.in_table_anything_else(tok),
            },
            Tok::Eof => self.mode_in_body(tok),
            Tok::Chars(_) => self.in_table_anything_else(tok),
        }
    }

    fn in_table_anything_else(&mut self, tok: Tok) {
        self.error();
        self.foster_parenting = true;
        self.mode_in_body(tok);
        self.foster_parenting = false;
    }

    fn clear_stack_to_table_context(&mut self) {
        while !self.current_is_any(&["table", "template", "html"]) && !self.open.is_empty() {
            self.pop();
        }
    }

    fn clear_stack_to_table_body_context(&mut self) {
        while !self.current_is_any(&["tbody", "tfoot", "thead", "template", "html"]) && !self.open.is_empty()
        {
            self.pop();
        }
    }

    fn clear_stack_to_table_row_context(&mut self) {
        while !self.current_is_any(&["tr", "template", "html"]) && !self.open.is_empty() {
            self.pop();
        }
    }

    // §13.2.6.4.10
    fn mode_in_table_text(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(ref s) if run_class(s) == CharClass::Null => self.error(),
            Tok::Chars(s) => self.pending_table_text.push(s),
            other => {
                let pending = std::mem::take(&mut self.pending_table_text);
                if pending.iter().any(|s| !is_ws_run(s)) {
                    self.error();
                    for s in pending {
                        self.foster_parenting = true;
                        self.mode_in_body(Tok::Chars(s));
                        self.foster_parenting = false;
                    }
                } else {
                    for s in pending {
                        self.insert_chars(&s);
                    }
                }
                let m = self.original_mode;
                self.reprocess(m, other);
            }
        }
    }

    // §13.2.6.4.11
    fn mode_in_caption(&mut self, tok: Tok) {
        let close_caption = |tb: &mut Self| -> bool {
            if !tb.in_table_scope("caption") {
                tb.error();
                return false;
            }
            tb.generate_implied_end_tags(None);
            if !tb.current_is("caption") {
                tb.error();
            }
            tb.pop_until("caption");
            tb.clear_afe_to_marker();
            tb.mode = M::InTable;
            true
        };
        match tok {
            Tok::End(ref t) if t.name == "caption" => {
                close_caption(self);
            }
            Tok::Start(ref t)
                if matches!(
                    t.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr"
                ) =>
            {
                if close_caption(self) {
                    self.process(tok);
                }
            }
            Tok::End(ref t) if t.name == "table" => {
                if close_caption(self) {
                    self.process(tok);
                }
            }
            Tok::End(ref t)
                if matches!(
                    t.name.as_str(),
                    "body" | "col" | "colgroup" | "html" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr"
                ) =>
            {
                self.error()
            }
            other => self.mode_in_body(other),
        }
    }

    // §13.2.6.4.12
    fn mode_in_column_group(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(ref s) if is_ws_run(s) => self.insert_chars(s),
            Tok::Comment(c) => self.insert_comment(&c, None),
            Tok::Doctype(_) => self.error(),
            Tok::Start(ref t) if t.name == "html" => self.mode_in_body(tok),
            Tok::Start(ref t) if t.name == "col" => {
                self.insert_html(t);
                self.pop();
            }
            Tok::End(ref t) if t.name == "colgroup" => {
                if !self.current_is("colgroup") {
                    self.error();
                    return;
                }
                self.pop();
                self.mode = M::InTable;
            }
            Tok::End(ref t) if t.name == "col" => self.error(),
            Tok::Start(ref t) if t.name == "template" => self.mode_in_head(tok),
            Tok::End(ref t) if t.name == "template" => self.mode_in_head(tok),
            Tok::Eof => self.mode_in_body(tok),
            other => {
                if !self.current_is("colgroup") {
                    self.error();
                    return;
                }
                self.pop();
                self.reprocess(M::InTable, other);
            }
        }
    }

    // §13.2.6.4.13
    fn mode_in_table_body(&mut self, tok: Tok) {
        match tok {
            Tok::Start(ref t) if t.name == "tr" => {
                self.clear_stack_to_table_body_context();
                self.insert_html(t);
                self.mode = M::InRow;
            }
            Tok::Start(ref t) if matches!(t.name.as_str(), "th" | "td") => {
                self.error();
                self.clear_stack_to_table_body_context();
                self.insert_html_named("tr");
                self.reprocess(M::InRow, tok);
            }
            Tok::End(ref t) if matches!(t.name.as_str(), "tbody" | "tfoot" | "thead") => {
                if !self.in_table_scope(&t.name) {
                    self.error();
                    return;
                }
                self.clear_stack_to_table_body_context();
                self.pop();
                self.mode = M::InTable;
            }
            Tok::Start(ref t)
                if matches!(
                    t.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead"
                ) =>
            {
                self.table_body_escape(tok)
            }
            Tok::End(ref t) if t.name == "table" => self.table_body_escape(tok),
            Tok::End(ref t)
                if matches!(
                    t.name.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th" | "tr"
                ) =>
            {
                self.error()
            }
            other => self.mode_in_table(other),
        }
    }

    fn table_body_escape(&mut self, tok: Tok) {
        if !self.in_table_scope_any(&["tbody", "thead", "tfoot"]) {
            self.error();
            return;
        }
        self.clear_stack_to_table_body_context();
        self.pop();
        self.reprocess(M::InTable, tok);
    }

    // §13.2.6.4.14
    fn mode_in_row(&mut self, tok: Tok) {
        match tok {
            Tok::Start(ref t) if matches!(t.name.as_str(), "th" | "td") => {
                self.clear_stack_to_table_row_context();
                self.insert_html(t);
                self.mode = M::InCell;
                self.afe.push(Formatting::Marker);
            }
            Tok::End(ref t) if t.name == "tr" => {
                if !self.in_table_scope("tr") {
                    self.error();
                    return;
                }
                self.clear_stack_to_table_row_context();
                self.pop();
                self.mode = M::InTableBody;
            }
            Tok::Start(ref t)
                if matches!(
                    t.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "tfoot" | "thead" | "tr"
                ) =>
            {
                self.row_escape(tok)
            }
            Tok::End(ref t) if t.name == "table" => self.row_escape(tok),
            Tok::End(ref t) if matches!(t.name.as_str(), "tbody" | "tfoot" | "thead") => {
                if !self.in_table_scope(&t.name) {
                    self.error();
                    return;
                }
                if !self.in_table_scope("tr") {
                    return;
                }
                self.clear_stack_to_table_row_context();
                self.pop();
                self.reprocess(M::InTableBody, tok);
            }
            Tok::End(ref t)
                if matches!(
                    t.name.as_str(),
                    "body" | "caption" | "col" | "colgroup" | "html" | "td" | "th"
                ) =>
            {
                self.error()
            }
            other => self.mode_in_table(other),
        }
    }

    fn row_escape(&mut self, tok: Tok) {
        if !self.in_table_scope("tr") {
            self.error();
            return;
        }
        self.clear_stack_to_table_row_context();
        self.pop();
        self.reprocess(M::InTableBody, tok);
    }

    fn close_cell(&mut self) {
        self.generate_implied_end_tags(None);
        if !self.current_is_any(&["td", "th"]) {
            self.error();
        }
        self.pop_until_any(&["td", "th"]);
        self.clear_afe_to_marker();
        self.mode = M::InRow;
    }

    // §13.2.6.4.15
    fn mode_in_cell(&mut self, tok: Tok) {
        match tok {
            Tok::End(ref t) if matches!(t.name.as_str(), "td" | "th") => {
                if !self.in_table_scope(&t.name) {
                    self.error();
                    return;
                }
                self.generate_implied_end_tags(None);
                if !self.current_is(&t.name) {
                    self.error();
                }
                self.pop_until(&t.name);
                self.clear_afe_to_marker();
                self.mode = M::InRow;
            }
            Tok::Start(ref t)
                if matches!(
                    t.name.as_str(),
                    "caption" | "col" | "colgroup" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr"
                ) =>
            {
                if !self.in_table_scope_any(&["td", "th"]) {
                    self.error();
                    return;
                }
                self.close_cell();
                self.process(tok);
            }
            Tok::End(ref t)
                if matches!(t.name.as_str(), "body" | "caption" | "col" | "colgroup" | "html") =>
            {
                self.error()
            }
            Tok::End(ref t) if matches!(t.name.as_str(), "table" | "tbody" | "tfoot" | "thead" | "tr") => {
                if !self.in_table_scope(&t.name) {
                    self.error();
                    return;
                }
                self.close_cell();
                self.process(tok);
            }
            other => self.mode_in_body(other),
        }
    }

    // §13.2.6.4.18
    fn mode_in_template(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(_) | Tok::Comment(_) | Tok::Doctype(_) => self.mode_in_body(tok),
            Tok::Start(ref t)
                if matches!(
                    t.name.as_str(),
                    "base"
                        | "basefont"
                        | "bgsound"
                        | "link"
                        | "meta"
                        | "noframes"
                        | "script"
                        | "style"
                        | "template"
                        | "title"
                ) =>
            {
                self.mode_in_head(tok)
            }
            Tok::End(ref t) if t.name == "template" => self.mode_in_head(tok),
            Tok::Start(ref t) => {
                let new_mode = match t.name.as_str() {
                    "caption" | "colgroup" | "tbody" | "tfoot" | "thead" => M::InTable,
                    "col" => M::InColumnGroup,
                    "tr" => M::InTableBody,
                    "td" | "th" => M::InRow,
                    _ => M::InBody,
                };
                self.template_modes.pop();
                self.template_modes.push(new_mode);
                self.reprocess(new_mode, tok);
            }
            Tok::End(_) => self.error(),
            Tok::Eof => {
                if !self.open.iter().any(|&n| self.is_html(n, "template")) {
                    self.stop_parsing();
                    return;
                }
                self.error();
                self.pop_until("template");
                self.clear_afe_to_marker();
                self.template_modes.pop();
                self.reset_insertion_mode();
                self.process(Tok::Eof);
            }
        }
    }

    // §13.2.6.4.19
    fn mode_after_body(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(ref s) if is_ws_run(s) => self.mode_in_body(tok),
            Tok::Comment(c) => {
                let html = self.open[0];
                self.insert_comment(&c, Some((html, None)));
            }
            Tok::Doctype(_) => self.error(),
            Tok::Start(ref t) if t.name == "html" => self.mode_in_body(tok),
            Tok::End(ref t) if t.name == "html" => {
                if self.context.is_some() {
                    self.error();
                    return;
                }
                self.mode = M::AfterAfterBody;
            }
            Tok::Eof => self.stop_parsing(),
            other => {
                self.error();
                self.reprocess(M::InBody, other);
            }
        }
    }

    // §13.2.6.4.20
    fn mode_in_frameset(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(ref s) if is_ws_run(s) => self.insert_chars(s),
            Tok::Comment(c) => self.insert_comment(&c, None),
            Tok::Doctype(_) => self.error(),
            Tok::Start(ref t) if t.name == "html" => self.mode_in_body(tok),
            Tok::Start(ref t) if t.name == "frameset" => {
                self.insert_html(t);
            }
            Tok::End(ref t) if t.name == "frameset" => {
                if self.open.len() == 1 && self.current_is("html") {
                    self.error();
                    return;
                }
                self.pop();
                if self.context.is_none() && !self.current_is("frameset") {
                    self.mode = M::AfterFrameset;
                }
            }
            Tok::Start(ref t) if t.name == "frame" => {
                self.insert_html(t);
                self.pop();
            }
            Tok::Start(ref t) if t.name == "noframes" => self.mode_in_head(tok),
            Tok::Eof => {
                if !(self.open.len() == 1 && self.current_is("html")) {
                    self.error();
                }
                self.stop_parsing();
            }
            Tok::Chars(ref s) => {
                // Keep only whitespace characters.
                let ws: String = s.chars().filter(|c| char_class(*c) == CharClass::Ws).collect();
                if !ws.is_empty() {
                    self.insert_chars(&ws);
                }
                self.error();
            }
            _ => self.error(),
        }
    }

    // §13.2.6.4.21
    fn mode_after_frameset(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(ref s) if is_ws_run(s) => self.insert_chars(s),
            Tok::Comment(c) => self.insert_comment(&c, None),
            Tok::Doctype(_) => self.error(),
            Tok::Start(ref t) if t.name == "html" => self.mode_in_body(tok),
            Tok::End(ref t) if t.name == "html" => self.mode = M::AfterAfterFrameset,
            Tok::Start(ref t) if t.name == "noframes" => self.mode_in_head(tok),
            Tok::Eof => self.stop_parsing(),
            _ => self.error(),
        }
    }

    // §13.2.6.4.22
    fn mode_after_after_body(&mut self, tok: Tok) {
        match tok {
            Tok::Comment(c) => self.insert_comment(&c, Some((Document::ROOT, None))),
            Tok::Doctype(_) => self.mode_in_body(tok),
            Tok::Chars(ref s) if is_ws_run(s) => self.mode_in_body(tok),
            Tok::Start(ref t) if t.name == "html" => self.mode_in_body(tok),
            Tok::Eof => self.stop_parsing(),
            other => {
                self.error();
                self.reprocess(M::InBody, other);
            }
        }
    }

    // §13.2.6.4.23
    fn mode_after_after_frameset(&mut self, tok: Tok) {
        match tok {
            Tok::Comment(c) => self.insert_comment(&c, Some((Document::ROOT, None))),
            Tok::Doctype(_) => self.mode_in_body(tok),
            Tok::Chars(ref s) if is_ws_run(s) => self.mode_in_body(tok),
            Tok::Start(ref t) if t.name == "html" => self.mode_in_body(tok),
            Tok::Eof => self.stop_parsing(),
            Tok::Start(ref t) if t.name == "noframes" => self.mode_in_head(tok),
            _ => self.error(),
        }
    }

    // §13.2.6.5 rules for parsing tokens in foreign content
    fn process_foreign(&mut self, tok: Tok) {
        match tok {
            Tok::Chars(ref s) => match run_class(s) {
                CharClass::Null => {
                    self.error();
                    let rep: String = s.chars().map(|_| '\u{FFFD}').collect();
                    self.insert_chars(&rep);
                }
                CharClass::Ws => self.insert_chars(s),
                CharClass::Other => {
                    self.insert_chars(s);
                    self.frameset_ok = false;
                }
            },
            Tok::Comment(c) => self.insert_comment(&c, None),
            Tok::Doctype(_) => self.error(),
            Tok::Start(ref t) if foreign::breaks_out_of_foreign(t) => {
                self.error();
                while let Some(c) = self.current() {
                    let html = self.doc.element(c).is_some_and(|e| e.ns == Namespace::Html);
                    if html || self.is_mathml_text_integration_point(c) || self.is_html_integration_point(c) {
                        break;
                    }
                    self.pop();
                }
                self.process_in_mode(self.mode, tok);
            }
            Tok::End(ref t) if t.name == "br" || t.name == "p" => {
                self.error();
                while let Some(c) = self.current() {
                    let html = self.doc.element(c).is_some_and(|e| e.ns == Namespace::Html);
                    if html || self.is_mathml_text_integration_point(c) || self.is_html_integration_point(c) {
                        break;
                    }
                    self.pop();
                }
                self.process_in_mode(self.mode, tok);
            }
            Tok::Start(ref t) => {
                let ns = self
                    .adjusted_current_node()
                    .and_then(|n| self.doc.element(n))
                    .map(|e| e.ns)
                    .unwrap_or(Namespace::Html);
                self.insert_foreign(t, ns);
            }
            Tok::End(ref t) => {
                if t.name == "script"
                    && self.current().is_some_and(|c| self.elem_name(c) == (Namespace::Svg, "script"))
                {
                    self.pop();
                    return;
                }
                let mut i = self.open.len() - 1;
                let node = self.open[i];
                if !self.elem_name(node).1.eq_ignore_ascii_case(&t.name) {
                    self.error();
                }
                loop {
                    if i == 0 {
                        return;
                    }
                    let node = self.open[i];
                    if self.elem_name(node).1.eq_ignore_ascii_case(&t.name) {
                        self.truncate_open(i);
                        return;
                    }
                    i -= 1;
                    let next = self.open[i];
                    if self.doc.element(next).is_some_and(|e| e.ns == Namespace::Html) {
                        self.process_in_mode(self.mode, tok);
                        return;
                    }
                }
            }
            Tok::Eof => self.process_in_mode(self.mode, tok),
        }
    }
}

/// Convenience: whether a document node is an element with any of the names.
pub fn is_element_named(doc: &Document, id: NodeId, names: &[&str]) -> bool {
    matches!(&doc.node(id).data, NodeData::Element(e) if names.contains(&e.name.as_str()))
}

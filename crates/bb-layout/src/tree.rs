//! Box tree construction (CSS 2 §9.2, CSS Display 3) from DOM + computed styles.

use bb_dom::{Document, Namespace, NodeData, NodeId};
use bb_style::{
    ComputedStyle, ContentItem, Display, Float, ListStylePosition, ListStyleType, Position, PseudoKind,
    StyleMap, TextTransform, WhiteSpace,
};
use std::rc::Rc;
use std::sync::Arc;

pub type BoxId = usize;

/// Image loading state supplied by the engine.
#[derive(Debug, Clone)]
pub enum ImageState {
    Loading,
    Ready(Arc<bb_image::Image>),
    Broken,
}

/// Resources layout needs from the engine (resolved by URL string).
pub trait Resources {
    fn image(&self, url: &str) -> ImageState;
    /// Resolve a (possibly relative) URL against the document base.
    fn resolve(&self, url: &str) -> String;
    /// Is this link visited? (used only for hit testing / UI)
    fn is_visited(&self, _url: &str) -> bool {
        false
    }
    /// A canvas element's bitmap (premultiplied, as an image), if scripts drew into it.
    fn canvas(&self, _node: NodeId) -> Option<Arc<bb_image::Image>> {
        None
    }
}

/// A form control's rendering data.
#[derive(Debug, Clone)]
pub enum Control {
    /// `value` is the placeholder when `placeholder` is set (the value is empty). `size` is the
    /// visible width in characters (HTML §4.10.5.3.2, default 20).
    TextField {
        value: String,
        placeholder: bool,
        password: bool,
        size: u32,
        edit: Option<EditView>,
    },
    Button {
        label: String,
    },
    Checkbox {
        checked: bool,
        radio: bool,
    },
    /// The selected option's label, and every option's label (the box fits the widest).
    Select {
        label: String,
        options: Vec<String>,
    },
    /// `cols` × `rows` characters (HTML §4.10.11, defaults 20 × 2).
    TextArea {
        value: String,
        placeholder: bool,
        cols: u32,
        rows: u32,
        edit: Option<EditView>,
    },
}

/// The caret and selection of the focused text control (byte offsets into its value).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditView {
    /// The text being edited, shown instead of the DOM value (which may be sanitized, e.g. an
    /// invalid number reads as "" but still shows what was typed).
    pub text: String,
    pub caret: usize,
    pub selection: Option<(usize, usize)>,
    /// The page has keyboard focus (the caret is hidden while e.g. the URL bar has it).
    pub caret_visible: bool,
}

/// HTML §2.3.4.1 rules for parsing non-negative integers; `None` for zero or junk.
fn positive_attr(doc: &Document, node: NodeId, name: &str) -> Option<u32> {
    let v = doc.attr(node, name)?.trim_start_matches([' ', '\t', '\n', '\x0c', '\r']);
    let v = v.strip_prefix('+').unwrap_or(v);
    let digits = &v[..v.bytes().take_while(u8::is_ascii_digit).count()];
    digits.parse::<u32>().ok().filter(|&n| n > 0)
}

/// The rendering data of a form control from its DOM state (`None` for hidden inputs, image
/// inputs, which are images, and other elements).
pub fn control_data(doc: &Document, node: NodeId) -> Option<Control> {
    use bb_dom::InputType as T;
    let attr = |n: &str| doc.attr(node, n).map(str::to_string);
    let name = doc.element(node)?.name.as_str();
    let c = match name {
        "input" => match doc.input_type(node) {
            T::Hidden | T::Image => return None,
            T::Submit => Control::Button { label: attr("value").unwrap_or_else(|| "Submit".into()) },
            T::Reset => Control::Button { label: attr("value").unwrap_or_else(|| "Reset".into()) },
            T::Button => Control::Button { label: attr("value").unwrap_or_default() },
            T::File => Control::Button { label: "Browse…".into() },
            T::Checkbox => Control::Checkbox { checked: doc.checkedness(node), radio: false },
            T::Radio => Control::Checkbox { checked: doc.checkedness(node), radio: true },
            ty => {
                let size = positive_attr(doc, node, "size").unwrap_or(20).min(1000);
                let value = doc.control_value(node);
                if value.is_empty() {
                    let ph = attr("placeholder").unwrap_or_default().replace(['\n', '\r'], "");
                    Control::TextField { value: ph, placeholder: true, password: false, size, edit: None }
                } else {
                    Control::TextField {
                        value,
                        placeholder: false,
                        password: ty == T::Password,
                        size,
                        edit: None,
                    }
                }
            }
        },
        "button" => {
            let label = doc.text_content(node).split_whitespace().collect::<Vec<_>>().join(" ");
            Control::Button { label }
        }
        "select" => {
            // The drop-down shows the first option in the list of options whose selectedness
            // is true (HTML §4.10.7; the DOM keeps selectedness up to date).
            let label_of = |o: NodeId| match doc.attr(o, "label").filter(|l| !l.is_empty()) {
                Some(l) => l.to_string(),
                None => doc.text_content(o).split_whitespace().collect::<Vec<_>>().join(" "),
            };
            let opts = doc.list_of_options(node);
            let label =
                opts.iter().copied().find(|&o| doc.option_selectedness(o)).map(label_of).unwrap_or_default();
            Control::Select { label, options: opts.into_iter().map(label_of).collect() }
        }
        "textarea" => {
            let cols = positive_attr(doc, node, "cols").unwrap_or(20).min(1000);
            let rows = positive_attr(doc, node, "rows").unwrap_or(2).min(1000);
            let value = doc.control_value(node);
            if value.is_empty() {
                let ph = attr("placeholder").unwrap_or_default();
                Control::TextArea { value: ph, placeholder: true, cols, rows, edit: None }
            } else {
                Control::TextArea { value, placeholder: false, cols, rows, edit: None }
            }
        }
        _ => return None,
    };
    Some(c)
}

/// Re-read a control's DOM state into its box (after the user typed, checked or picked
/// something) and set its caret; the box keeps its size. Returns whether `node` has a
/// control box.
pub fn refresh_control(tree: &mut LayoutTree, doc: &Document, node: NodeId, edit: Option<EditView>) -> bool {
    let Some(&b) = tree.node_box.get(&node) else { return false };
    let BoxKind::Replaced(Replaced::Control(c)) = &mut tree.boxes[b].kind else { return false };
    let Some(mut new) = control_data(doc, node) else { return false };
    match &mut new {
        Control::TextField { value, placeholder, edit: e, .. }
        | Control::TextArea { value, placeholder, edit: e, .. } => {
            if let Some(ed) = edit.as_ref().filter(|ed| !ed.text.is_empty()) {
                *value = ed.text.clone();
                *placeholder = false;
            }
            *e = edit;
        }
        _ => {}
    }
    *c = new;
    true
}

#[derive(Debug, Clone)]
pub enum Replaced {
    Image {
        image: Option<Arc<bb_image::Image>>,
        attr_size: (Option<f32>, Option<f32>),
        alt: Option<String>,
        broken: bool,
    },
    Control(Control),
    /// Inline `<svg>`: space is reserved, nothing is drawn yet (TODO(SVG 2)).
    Svg {
        default: (f32, f32),
    },
    /// `<iframe>`: a nested document painted by the engine (HTML §4.8.5).
    Frame {
        default: (f32, f32),
    },
    /// embed / object / canvas / video placeholders.
    Placeholder {
        label: String,
        default: (f32, f32),
    },
}

#[derive(Debug, Clone)]
pub enum BoxKind {
    Block,
    Inline,
    Text(Rc<str>),
    Replaced(Replaced),
    InlineBlock,
    Table,
    TableRowGroup,
    TableRow,
    TableCell { colspan: usize, rowspan: usize },
    TableCaption,
    TableColumn { span: usize },
    TableColumnGroup,
    LineBreak,
    Flex,
}

/// An outside list marker.
#[derive(Debug, Clone)]
pub struct Marker {
    pub text: String,
    pub style: Rc<ComputedStyle>,
    /// Bullet shape instead of text (disc/circle/square).
    pub bullet: Option<ListStyleType>,
    pub image: Option<Arc<bb_image::Image>>,
}

/// A laid-out line of inline content.
#[derive(Debug, Clone, Default)]
pub struct Line {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub baseline: f32,
    pub items: Vec<LineItem>,
}

#[derive(Debug, Clone)]
pub enum LineItem {
    Text {
        x: f32,
        baseline: f32,
        width: f32,
        glyphs: Arc<bb_text::ShapedText>,
        style: Rc<ComputedStyle>,
        node: Option<NodeId>,
        /// The box whose inline content this text belongs to.
        owner: BoxId,
        /// Byte range in the text node (for selection / find).
        range: (usize, usize),
        /// The text box's (whitespace-processed) text; `range` indexes into it
        /// and glyph clusters are relative to `range.0`.
        text: Rc<str>,
        ascent: f32,
        descent: f32,
    },
    /// An atomic inline (replaced element or inline-block) placed on the line.
    Atomic { id: BoxId },
    /// The portion of an inline box on this line (for backgrounds/borders).
    Span { id: BoxId, x0: f32, x1: f32, top: f32, bottom: f32, first: bool, last: bool },
}

#[derive(Debug, Clone)]
pub struct LBox {
    pub kind: BoxKind,
    pub style: Rc<ComputedStyle>,
    pub node: Option<NodeId>,
    pub parent: Option<BoxId>,
    pub children: Vec<BoxId>,
    pub anonymous: bool,
    // ---- layout results (absolute CSS px; x/y/w/h = border box) ----
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub margin: [f32; 4],
    pub border: [f32; 4],
    pub padding: [f32; 4],
    pub lines: Vec<Line>,
    pub marker: Option<Marker>,
    pub rel_offset: (f32, f32),
    pub baseline: Option<f32>,
    /// Cached (min-content, max-content) border-box widths.
    pub intrinsic: Option<(f32, f32)>,
    /// Bottom of all content including overflow (absolute).
    pub content_bottom: f32,
    /// Static position for absolutely positioned boxes.
    pub static_pos: (f32, f32),
    pub laid_out: bool,
    /// Decoded background images, one per `background` layer (None = not loaded / gradient).
    pub bg_images: Vec<Option<Arc<bb_image::Image>>>,
}

impl LBox {
    pub fn new(kind: BoxKind, style: Rc<ComputedStyle>, node: Option<NodeId>) -> LBox {
        LBox {
            kind,
            style,
            node,
            parent: None,
            children: Vec::new(),
            anonymous: false,
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            margin: [0.0; 4],
            border: [0.0; 4],
            padding: [0.0; 4],
            lines: Vec::new(),
            marker: None,
            rel_offset: (0.0, 0.0),
            baseline: None,
            intrinsic: None,
            content_bottom: 0.0,
            static_pos: (0.0, 0.0),
            laid_out: false,
            bg_images: Vec::new(),
        }
    }

    pub fn is_inline_level(&self) -> bool {
        match &self.kind {
            BoxKind::Inline | BoxKind::Text(_) | BoxKind::InlineBlock | BoxKind::LineBreak => true,
            BoxKind::Replaced(_) => self.style.display.is_inline_level(),
            _ => false,
        }
    }

    pub fn is_out_of_flow(&self) -> bool {
        // Text boxes share their parent's style but are never floated/positioned themselves.
        if matches!(self.kind, BoxKind::Text(_) | BoxKind::LineBreak) {
            return false;
        }
        matches!(self.style.position, Position::Absolute | Position::Fixed) || self.style.float != Float::None
    }

    pub fn is_atomic_inline(&self) -> bool {
        matches!(self.kind, BoxKind::InlineBlock)
            || (matches!(self.kind, BoxKind::Replaced(_)) && self.style.display.is_inline_level())
    }

    /// Content box (absolute).
    pub fn content_rect(&self) -> (f32, f32, f32, f32) {
        let x = self.x + self.border[3] + self.padding[3];
        let y = self.y + self.border[0] + self.padding[0];
        let w = (self.w - self.border[1] - self.border[3] - self.padding[1] - self.padding[3]).max(0.0);
        let h = (self.h - self.border[0] - self.border[2] - self.padding[0] - self.padding[2]).max(0.0);
        (x, y, w, h)
    }

    /// Padding box (absolute).
    pub fn padding_rect(&self) -> (f32, f32, f32, f32) {
        (
            self.x + self.border[3],
            self.y + self.border[0],
            (self.w - self.border[1] - self.border[3]).max(0.0),
            (self.h - self.border[0] - self.border[2]).max(0.0),
        )
    }
}

/// The box tree.
pub struct LayoutTree {
    pub boxes: Vec<LBox>,
    pub root: BoxId,
    pub quirks: bool,
    /// The (CSS px) viewport size layout was performed for.
    pub viewport: (f32, f32),
    pub canvas_background: bb_css::Rgba,
    /// Box for each element node that generated one.
    pub node_box: rustc_hash::FxHashMap<NodeId, BoxId>,
}

impl LayoutTree {
    pub fn add(&mut self, b: LBox, parent: Option<BoxId>) -> BoxId {
        self.add_box(b, parent)
    }

    fn add_box(&mut self, mut b: LBox, parent: Option<BoxId>) -> BoxId {
        let id = self.boxes.len();
        b.parent = parent;
        self.boxes.push(b);
        if let Some(p) = parent {
            self.boxes[p].children.push(id);
        }
        id
    }
}

// ---------------- whitespace & text ----------------

/// CSS Text 3 §4.1.1 white space collapsing (phase I) for one text node.
/// `prev_space` carries the "previous character was collapsible space" state
/// across text nodes in the same inline formatting context.
pub fn collapse_whitespace(text: &str, ws: WhiteSpace, prev_space: &mut bool) -> String {
    let mut out = String::with_capacity(text.len());
    match ws {
        WhiteSpace::Normal | WhiteSpace::Nowrap => {
            for c in text.chars() {
                if matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0C') {
                    if !*prev_space {
                        out.push(' ');
                        *prev_space = true;
                    }
                } else {
                    out.push(c);
                    *prev_space = false;
                }
            }
        }
        WhiteSpace::PreLine => {
            // Collapse spaces/tabs, keep newlines; spaces around newlines removed.
            let mut pending_space = false;
            for c in text.chars() {
                match c {
                    ' ' | '\t' | '\r' => {
                        if !*prev_space {
                            pending_space = true;
                        }
                    }
                    '\n' => {
                        pending_space = false;
                        out.push('\n');
                        *prev_space = true;
                    }
                    c => {
                        if pending_space {
                            out.push(' ');
                            pending_space = false;
                        }
                        out.push(c);
                        *prev_space = false;
                    }
                }
            }
            if pending_space {
                out.push(' ');
                *prev_space = true;
            }
        }
        WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::BreakSpaces => {
            // Expand tabs to the next multiple of 8 columns (approximation of tab-size: 8).
            let mut col = 0;
            for c in text.chars() {
                match c {
                    '\t' => {
                        let n = 8 - col % 8;
                        for _ in 0..n {
                            out.push(' ');
                        }
                        col += n;
                    }
                    '\n' => {
                        out.push('\n');
                        col = 0;
                    }
                    '\r' => {}
                    c => {
                        out.push(c);
                        col += 1;
                    }
                }
            }
            *prev_space = false;
        }
    }
    out
}

pub fn apply_text_transform(s: &str, t: TextTransform, word_start: &mut bool) -> String {
    match t {
        TextTransform::None => s.to_string(),
        TextTransform::Uppercase => s.to_uppercase(),
        TextTransform::Lowercase => s.to_lowercase(),
        TextTransform::Capitalize => {
            let mut out = String::with_capacity(s.len());
            for c in s.chars() {
                if *word_start && c.is_alphanumeric() {
                    out.extend(c.to_uppercase());
                    *word_start = false;
                } else {
                    out.push(c);
                    if c.is_whitespace() || c == '-' {
                        *word_start = true;
                    } else if c.is_alphanumeric() {
                        *word_start = false;
                    }
                }
            }
            out
        }
    }
}

// ---------------- list markers ----------------

fn roman(mut n: i32, upper: bool) -> String {
    if n <= 0 || n >= 4000 {
        return n.to_string();
    }
    let table = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut s = String::new();
    for (v, r) in table {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    if upper {
        s.to_uppercase()
    } else {
        s
    }
}

fn alpha(n: i32, upper: bool, alphabet: &[char]) -> String {
    if n <= 0 {
        return n.to_string();
    }
    let mut n = n as usize;
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push(alphabet[n % alphabet.len()]);
        n /= alphabet.len();
    }
    s.reverse();
    let out: String = s.into_iter().collect();
    if upper {
        out.to_uppercase()
    } else {
        out
    }
}

/// CSS Counter Styles 3: marker string for `n`.
pub fn counter_text(n: i32, t: ListStyleType) -> String {
    let latin: Vec<char> = ('a'..='z').collect();
    let greek: Vec<char> = "αβγδεζηθικλμνξοπρστυφχψω".chars().collect();
    match t {
        ListStyleType::Decimal => n.to_string(),
        ListStyleType::DecimalLeadingZero => format!("{n:02}"),
        ListStyleType::LowerAlpha => alpha(n, false, &latin),
        ListStyleType::UpperAlpha => alpha(n, true, &latin),
        ListStyleType::LowerRoman => roman(n, false),
        ListStyleType::UpperRoman => roman(n, true),
        ListStyleType::LowerGreek => alpha(n, false, &greek),
        ListStyleType::Disc => "•".into(),
        ListStyleType::Circle => "◦".into(),
        ListStyleType::Square => "▪".into(),
        ListStyleType::DisclosureOpen => "▾".into(),
        ListStyleType::DisclosureClosed => "▸".into(),
        ListStyleType::None => String::new(),
    }
}

// ---------------- builder ----------------

struct Builder<'a> {
    doc: &'a Document,
    styles: &'a StyleMap,
    res: &'a dyn Resources,
    tree: LayoutTree,
    /// List item counters: (list element, next value, step).
    counters: Vec<(i32, i32)>,
}

fn attr_px(doc: &Document, n: NodeId, name: &str) -> Option<f32> {
    let v = doc.attr(n, name)?.trim();
    let d: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    if v.contains('%') {
        return None;
    }
    d.parse().ok()
}

fn anon_style(parent: &ComputedStyle, display: Display) -> Rc<ComputedStyle> {
    let mut s = ComputedStyle::inherit_from(parent);
    s.display = display;
    Rc::new(s)
}

impl<'a> Builder<'a> {
    fn control_for(&self, node: NodeId, name: &str) -> Option<Replaced> {
        let doc = self.doc;
        if name == "input" && doc.input_type(node) == bb_dom::InputType::Image {
            let src = doc.attr(node, "src").unwrap_or_default().to_string();
            return Some(self.image_replaced(node, &src));
        }
        control_data(doc, node).map(Replaced::Control)
    }

    fn image_replaced(&self, node: NodeId, src: &str) -> Replaced {
        let attr_size = (attr_px(self.doc, node, "width"), attr_px(self.doc, node, "height"));
        let alt = self.doc.attr(node, "alt").map(str::to_string);
        if src.trim().is_empty() {
            return Replaced::Image { image: None, attr_size, alt, broken: true };
        }
        let url = self.res.resolve(src);
        match self.res.image(&url) {
            ImageState::Ready(img) => Replaced::Image { image: Some(img), attr_size, alt, broken: false },
            ImageState::Loading => Replaced::Image { image: None, attr_size, alt, broken: false },
            ImageState::Broken => Replaced::Image { image: None, attr_size, alt, broken: true },
        }
    }

    /// A canvas: its bitmap as an image, or a transparent box of its size (HTML §4.12.5:
    /// the width and height attributes, 300 × 150 by default).
    fn canvas_replaced(&self, node: NodeId) -> Replaced {
        if let Some(img) = self.res.canvas(node) {
            return Replaced::Image { image: Some(img), attr_size: (None, None), alt: None, broken: false };
        }
        let dim = |name: &str, def: f32| {
            self.doc
                .attr(node, name)
                .and_then(|v| {
                    v.trim().split(|c: char| !c.is_ascii_digit()).next().and_then(|d| d.parse::<f32>().ok())
                })
                .unwrap_or(def)
        };
        Replaced::Image {
            image: None,
            attr_size: (Some(dim("width", 300.0)), Some(dim("height", 150.0))),
            alt: None,
            broken: false,
        }
    }

    /// Build boxes for `node` (and descendants) into `parent`.
    fn build(&mut self, node: NodeId, parent: BoxId, prev_space: &mut bool, word_start: &mut bool) {
        let doc = self.doc;
        match &doc.node(node).data {
            NodeData::Text(t) => {
                let pstyle = self.tree.boxes[parent].style.clone();
                let s = collapse_whitespace(t, pstyle.white_space, prev_space);
                if s.is_empty() {
                    return;
                }
                let s = apply_text_transform(&s, pstyle.text_transform, word_start);
                let mut b = LBox::new(BoxKind::Text(s.into()), pstyle, Some(node));
                b.anonymous = true;
                self.tree.add(b, Some(parent));
            }
            NodeData::Element(e) => {
                let Some(style) = self.styles.get(node).cloned() else { return };
                if style.display == Display::None {
                    return;
                }
                let name = e.name.clone();
                let html = e.ns == Namespace::Html;
                if style.display == Display::Contents {
                    self.build_children(node, parent, prev_space, word_start);
                    return;
                }
                // Replaced elements.
                let replaced = if html {
                    match name.as_str() {
                        "img" => Some(self.image_replaced(node, doc.attr(node, "src").unwrap_or(""))),
                        "input" | "button" | "select" | "textarea" => match self.control_for(node, &name) {
                            Some(r) => Some(r),
                            None => return,
                        },
                        "iframe" => Some(Replaced::Frame { default: (300.0, 150.0) }),
                        "canvas" => Some(self.canvas_replaced(node)),
                        "embed" | "object" | "video" | "audio" | "applet" => {
                            if name == "object"
                                && doc.children(node).any(|c| {
                                    doc.node(c).is_element()
                                        || doc.node(c).as_text().is_some_and(|t| !t.trim().is_empty())
                                })
                            {
                                None // render fallback content
                            } else {
                                let label = String::new();
                                let default = if name == "audio" { (300.0, 40.0) } else { (300.0, 150.0) };
                                Some(Replaced::Placeholder { label, default })
                            }
                        }
                        _ => None,
                    }
                } else if e.ns == Namespace::Svg && name == "svg" {
                    // TODO(SVG): render SVG; reserve its box for now.
                    let w = attr_px(doc, node, "width").unwrap_or(300.0);
                    let h = attr_px(doc, node, "height").unwrap_or(150.0);
                    Some(Replaced::Svg { default: (w, h) })
                } else {
                    None
                };
                if let Some(r) = replaced {
                    let b = LBox::new(BoxKind::Replaced(r), style.clone(), Some(node));
                    let id = self.tree.add(b, Some(parent));
                    self.tree.node_box.insert(node, id);
                    *prev_space = false;
                    return;
                }
                if html && name == "br" {
                    let b = LBox::new(BoxKind::LineBreak, style, Some(node));
                    self.tree.add(b, Some(parent));
                    *prev_space = true;
                    return;
                }
                let kind = match style.display {
                    Display::Inline => BoxKind::Inline,
                    Display::InlineBlock => BoxKind::InlineBlock,
                    Display::Block | Display::FlowRoot | Display::ListItem | Display::Grid => BoxKind::Block,
                    Display::Flex | Display::InlineFlex => BoxKind::Flex,
                    Display::Table | Display::InlineTable => BoxKind::Table,
                    Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup => {
                        BoxKind::TableRowGroup
                    }
                    Display::TableRow => BoxKind::TableRow,
                    Display::TableCell => {
                        let span = |n: &str| {
                            doc.attr(node, n).and_then(|v| v.trim().parse::<usize>().ok()).filter(|&v| v > 0)
                        };
                        BoxKind::TableCell {
                            colspan: span("colspan").unwrap_or(1).min(1000),
                            rowspan: span("rowspan").unwrap_or(1).min(65534),
                        }
                    }
                    Display::TableCaption => BoxKind::TableCaption,
                    Display::TableColumn => {
                        let span = doc
                            .attr(node, "span")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(1)
                            .clamp(1, 1000);
                        BoxKind::TableColumn { span }
                    }
                    Display::TableColumnGroup => BoxKind::TableColumnGroup,
                    Display::InlineGrid => BoxKind::InlineBlock,
                    Display::None | Display::Contents => unreachable!(),
                };
                // inline-table / inline-flex: atomic inline wrapper around the table/flex box.
                let wrapper = matches!(style.display, Display::InlineTable | Display::InlineFlex);
                let target_parent = if wrapper {
                    let mut ws = ComputedStyle::inherit_from(&style);
                    ws.display = Display::InlineBlock;
                    ws.vertical_align = style.vertical_align.clone();
                    ws.margin = style.margin.clone();
                    let mut wb = LBox::new(BoxKind::InlineBlock, Rc::new(ws), Some(node));
                    wb.anonymous = true;
                    self.tree.add(wb, Some(parent))
                } else {
                    parent
                };
                let mut inner_style = style.clone();
                if wrapper {
                    let mut s = (*style).clone();
                    s.margin =
                        [bb_style::LPA::ZERO, bb_style::LPA::ZERO, bb_style::LPA::ZERO, bb_style::LPA::ZERO];
                    s.display = s.display.blockify();
                    inner_style = Rc::new(s);
                }
                let b = LBox::new(kind.clone(), inner_style, Some(node));
                let id = self.tree.add(b, Some(target_parent));
                self.tree.node_box.insert(node, id);

                // List counters (CSS Lists 3 §4, HTML ol/li semantics).
                let is_list = html && matches!(name.as_str(), "ol" | "ul" | "menu" | "dir");
                if is_list {
                    let reversed = name == "ol" && doc.attr(node, "reversed").is_some();
                    let count = doc.element_children(node).filter(|&c| doc.is_html(c, "li")).count() as i32;
                    let start = doc
                        .attr(node, "start")
                        .and_then(|v| v.trim().parse::<i32>().ok())
                        .unwrap_or(if reversed { count } else { 1 });
                    self.counters.push((start, if reversed { -1 } else { 1 }));
                }
                if style.display == Display::ListItem {
                    if let Some(v) = doc.attr(node, "value").and_then(|v| v.trim().parse::<i32>().ok()) {
                        if let Some(c) = self.counters.last_mut() {
                            c.0 = v;
                        }
                    }
                    let n = self.counters.last().map(|c| c.0).unwrap_or(1);
                    if let Some(c) = self.counters.last_mut() {
                        c.0 += c.1;
                    }
                    self.make_marker(id, node, &style, n, prev_space, word_start);
                }
                if matches!(
                    kind,
                    BoxKind::Block
                        | BoxKind::InlineBlock
                        | BoxKind::TableCell { .. }
                        | BoxKind::TableCaption
                        | BoxKind::Flex
                ) {
                    *prev_space = true; // new block: leading whitespace collapses away
                }
                self.pseudo(node, PseudoKind::Before, id, prev_space, word_start);
                if html && name == "template" {
                    // template contents are not rendered
                } else {
                    self.build_children(node, id, prev_space, word_start);
                }
                self.pseudo(node, PseudoKind::After, id, prev_space, word_start);
                if is_list {
                    self.counters.pop();
                }
                if matches!(
                    kind,
                    BoxKind::Block
                        | BoxKind::TableCell { .. }
                        | BoxKind::TableCaption
                        | BoxKind::Flex
                        | BoxKind::Table
                ) {
                    *prev_space = true;
                }
                if matches!(kind, BoxKind::InlineBlock) {
                    *prev_space = false;
                }
            }
            _ => {}
        }
    }

    fn make_marker(
        &mut self,
        id: BoxId,
        node: NodeId,
        style: &Rc<ComputedStyle>,
        n: i32,
        prev_space: &mut bool,
        word_start: &mut bool,
    ) {
        let mstyle = self.styles.pseudo(node, PseudoKind::Marker).cloned().unwrap_or_else(|| {
            let mut s = ComputedStyle::inherit_from(style);
            s.display = Display::Inline;
            s.text_decoration_line = Default::default();
            Rc::new(s)
        });
        let image =
            style.list_style_image.as_ref().and_then(|u| match self.res.image(&self.res.resolve(u)) {
                ImageState::Ready(i) => Some(i),
                _ => None,
            });
        let ty = style.list_style_type;
        if ty == ListStyleType::None && image.is_none() {
            return;
        }
        let bullet =
            matches!(ty, ListStyleType::Disc | ListStyleType::Circle | ListStyleType::Square).then_some(ty);
        let text = match ty {
            ListStyleType::Disc | ListStyleType::Circle | ListStyleType::Square | ListStyleType::None => {
                String::new()
            }
            ListStyleType::DisclosureOpen | ListStyleType::DisclosureClosed => {
                format!("{} ", counter_text(n, ty))
            }
            _ => format!("{}. ", counter_text(n, ty)),
        };
        if style.list_style_position == ListStylePosition::Inside && image.is_none() {
            let t = if bullet.is_some() { format!("{} ", counter_text(n, ty)) } else { text };
            let mut b = LBox::new(BoxKind::Text(t.into()), mstyle, None);
            b.anonymous = true;
            self.tree.add(b, Some(id));
            *prev_space = true;
            *word_start = true;
        } else {
            self.tree.boxes[id].marker = Some(Marker { text, style: mstyle, bullet, image });
        }
    }

    fn pseudo(
        &mut self,
        node: NodeId,
        kind: PseudoKind,
        parent: BoxId,
        prev_space: &mut bool,
        word_start: &mut bool,
    ) {
        let Some(ps) = self.styles.pseudo(node, kind).cloned() else { return };
        let Some(content) = ps.content.clone() else { return };
        let mut text = String::new();
        let mut q = ps.quotes_depth;
        for item in &content {
            match item {
                ContentItem::Text(t) => text.push_str(t),
                ContentItem::Attr(a) => text.push_str(self.doc.attr(node, a).unwrap_or("")),
                ContentItem::OpenQuote => {
                    text.push(if q % 2 == 0 { '\u{201C}' } else { '\u{2018}' });
                    q = q.saturating_add(1);
                }
                ContentItem::CloseQuote => {
                    q = q.saturating_sub(1);
                    text.push(if q % 2 == 0 { '\u{201D}' } else { '\u{2019}' });
                }
                ContentItem::Counter(_, _) => {} // TODO(css-lists-3 §4): CSS counters
                ContentItem::Url(_) => {}
            }
        }
        let display = ps.display;
        let kind_box = match display {
            Display::Block | Display::ListItem | Display::FlowRoot => BoxKind::Block,
            Display::InlineBlock => BoxKind::InlineBlock,
            _ => BoxKind::Inline,
        };
        let pb = LBox::new(kind_box, ps.clone(), None);
        let pid = self.tree.add(pb, Some(parent));
        let s = collapse_whitespace(&text, ps.white_space, prev_space);
        let s = apply_text_transform(&s, ps.text_transform, word_start);
        if !s.is_empty() {
            let mut t = LBox::new(BoxKind::Text(s.into()), ps, None);
            t.anonymous = true;
            self.tree.add(t, Some(pid));
        }
    }

    fn build_children(&mut self, node: NodeId, parent: BoxId, prev_space: &mut bool, word_start: &mut bool) {
        // The flat tree (CSS Scoping §2.2): a host's shadow tree, a slot's assigned nodes.
        let kids: Vec<NodeId> = self.doc.flat_children(node);
        if self.doc.is_html(node, "details") {
            self.build_details(node, &kids, parent, prev_space, word_start);
            return;
        }
        for k in kids {
            self.build(k, parent, prev_space, word_start);
        }
    }

    /// HTML §4.11.1 / §15.5.4: a `details` element shows its first `summary` child (or a
    /// UA-provided "Details" summary) and, only while it has the `open` attribute, the rest of
    /// its contents. The summary always comes first.
    fn build_details(
        &mut self,
        node: NodeId,
        kids: &[NodeId],
        parent: BoxId,
        prev_space: &mut bool,
        word_start: &mut bool,
    ) {
        let open = self.doc.attr(node, "open").is_some();
        let summary = kids.iter().copied().find(|&k| self.doc.is_html(k, "summary"));
        match summary {
            Some(sm) => self.build(sm, parent, prev_space, word_start),
            None => {
                // The default summary: a block with a disclosure triangle and "Details".
                let pstyle = self.tree.boxes[parent].style.clone();
                let mut st = ComputedStyle::inherit_from(&pstyle);
                st.display = Display::Block;
                let st = Rc::new(st);
                let mut b = LBox::new(BoxKind::Block, st.clone(), None);
                b.anonymous = true;
                let id = self.tree.add(b, Some(parent));
                let ty = if open { ListStyleType::DisclosureOpen } else { ListStyleType::DisclosureClosed };
                let mut t =
                    LBox::new(BoxKind::Text(format!("{} Details", counter_text(1, ty)).into()), st, None);
                t.anonymous = true;
                self.tree.add(t, Some(id));
                *prev_space = true;
                *word_start = true;
            }
        }
        if open {
            for &k in kids {
                if Some(k) != summary {
                    self.build(k, parent, prev_space, word_start);
                }
            }
        }
    }
}

/// Build the box tree for a styled document.
pub fn build_tree(
    doc: &Document,
    styles: &StyleMap,
    res: &dyn Resources,
    viewport: (f32, f32),
) -> LayoutTree {
    let mut b = Builder {
        doc,
        styles,
        res,
        tree: LayoutTree {
            boxes: Vec::new(),
            root: 0,
            quirks: doc.quirks_mode == bb_dom::QuirksMode::Quirks,
            viewport,
            canvas_background: styles.canvas_background,
            node_box: Default::default(),
        },
        counters: Vec::new(),
    };
    // The initial containing block box.
    let icb_style = Rc::new({
        let mut s = ComputedStyle::initial();
        s.display = Display::Block;
        s
    });
    let mut icb = LBox::new(BoxKind::Block, icb_style, None);
    icb.anonymous = true;
    let root = b.tree.add(icb, None);
    b.tree.root = root;
    if let Some(html) = doc.document_element() {
        let mut ps = true;
        let mut ws = true;
        b.build(html, root, &mut ps, &mut ws);
    }
    let mut tree = b.tree;
    // Resolve background images.
    for i in 0..tree.boxes.len() {
        if tree.boxes[i].style.background.is_empty() || tree.boxes[i].anonymous {
            continue;
        }
        let imgs: Vec<Option<Arc<bb_image::Image>>> = tree.boxes[i]
            .style
            .background
            .iter()
            .map(|l| match &l.image {
                Some(bb_style::Image::Url(u)) => match res.image(&res.resolve(u)) {
                    ImageState::Ready(img) => Some(img),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        tree.boxes[i].bg_images = imgs;
    }
    fixup(&mut tree, root);
    tree
}

fn is_table_part(k: &BoxKind) -> bool {
    matches!(
        k,
        BoxKind::TableRowGroup
            | BoxKind::TableRow
            | BoxKind::TableCell { .. }
            | BoxKind::TableCaption
            | BoxKind::TableColumn { .. }
            | BoxKind::TableColumnGroup
    )
}

fn is_ws_text(t: &LBox) -> bool {
    matches!(&t.kind, BoxKind::Text(s) if s.chars().all(|c| c == ' ' || c == '\n' || c == '\t'))
}

/// Post-build fixups: anonymous block wrapping and table structure (CSS 2 §9.2.1.1, §17.2.1).
fn fixup(tree: &mut LayoutTree, id: BoxId) {
    let kids = tree.boxes[id].children.clone();
    for &k in &kids {
        fixup(tree, k);
    }
    let kind = tree.boxes[id].kind.clone();
    match kind {
        BoxKind::Table => fix_table(tree, id),
        BoxKind::TableRowGroup => {
            wrap_children(tree, id, |k| matches!(k, BoxKind::TableRow), BoxKind::TableRow, Display::TableRow)
        }
        BoxKind::TableRow => wrap_children(
            tree,
            id,
            |k| matches!(k, BoxKind::TableCell { .. }),
            BoxKind::TableCell { colspan: 1, rowspan: 1 },
            Display::TableCell,
        ),
        BoxKind::Block
        | BoxKind::InlineBlock
        | BoxKind::TableCell { .. }
        | BoxKind::TableCaption
        | BoxKind::Flex => {
            wrap_stray_table_parts(tree, id);
            anonymous_blocks(tree, id);
        }
        BoxKind::Inline => {
            // Block-in-inline: CSS 2 §9.2.1.1 splits the inline. We approximate by
            // letting the block child break the line (handled in inline layout).
        }
        _ => {}
    }
}

/// Wrap children of a table part that are not of the expected kind into anonymous boxes.
fn wrap_children(
    tree: &mut LayoutTree,
    id: BoxId,
    ok: fn(&BoxKind) -> bool,
    kind: BoxKind,
    display: Display,
) {
    let kids = std::mem::take(&mut tree.boxes[id].children);
    let mut out = Vec::new();
    let mut run: Vec<BoxId> = Vec::new();
    let flush = |tree: &mut LayoutTree, run: &mut Vec<BoxId>, out: &mut Vec<BoxId>| {
        if run.is_empty() {
            return;
        }
        if run.iter().all(|&r| is_ws_text(&tree.boxes[r])) {
            run.clear();
            return;
        }
        let st = anon_style(&tree.boxes[id].style.clone(), display);
        let mut b = LBox::new(kind.clone(), st, None);
        b.anonymous = true;
        b.parent = Some(id);
        let nid = tree.boxes.len();
        tree.boxes.push(b);
        for &r in run.iter() {
            tree.boxes[r].parent = Some(nid);
        }
        tree.boxes[nid].children = std::mem::take(run);
        fixup(tree, nid);
        out.push(nid);
    };
    for k in kids {
        if ok(&tree.boxes[k].kind) {
            flush(tree, &mut run, &mut out);
            out.push(k);
        } else {
            run.push(k);
        }
    }
    flush(tree, &mut run, &mut out);
    tree.boxes[id].children = out;
}

fn fix_table(tree: &mut LayoutTree, id: BoxId) {
    // Rows directly in a table, or stray cells, get an anonymous row group.
    let kids = std::mem::take(&mut tree.boxes[id].children);
    let mut out = Vec::new();
    let mut run: Vec<BoxId> = Vec::new();
    for k in kids {
        let kk = tree.boxes[k].kind.clone();
        match kk {
            BoxKind::TableRowGroup
            | BoxKind::TableCaption
            | BoxKind::TableColumn { .. }
            | BoxKind::TableColumnGroup => {
                flush_group(tree, id, &mut run, &mut out);
                out.push(k);
            }
            _ => {
                if out.is_empty() && run.is_empty() && is_ws_text(&tree.boxes[k]) {
                    continue;
                }
                run.push(k);
            }
        }
    }
    flush_group(tree, id, &mut run, &mut out);
    tree.boxes[id].children = out;
}

fn flush_group(tree: &mut LayoutTree, table: BoxId, run: &mut Vec<BoxId>, out: &mut Vec<BoxId>) {
    if run.is_empty() || run.iter().all(|&r| is_ws_text(&tree.boxes[r])) {
        run.clear();
        return;
    }
    let st = anon_style(&tree.boxes[table].style.clone(), Display::TableRowGroup);
    let mut g = LBox::new(BoxKind::TableRowGroup, st, None);
    g.anonymous = true;
    g.parent = Some(table);
    let gid = tree.boxes.len();
    tree.boxes.push(g);
    for &r in run.iter() {
        tree.boxes[r].parent = Some(gid);
    }
    tree.boxes[gid].children = std::mem::take(run);
    wrap_children(tree, gid, |k| matches!(k, BoxKind::TableRow), BoxKind::TableRow, Display::TableRow);
    out.push(gid);
}

/// Table parts outside a table (e.g. `display: table-cell` divs) get an anonymous table.
fn wrap_stray_table_parts(tree: &mut LayoutTree, id: BoxId) {
    if !tree.boxes[id].children.iter().any(|&c| is_table_part(&tree.boxes[c].kind)) {
        return;
    }
    let kids = std::mem::take(&mut tree.boxes[id].children);
    let mut out = Vec::new();
    let mut run: Vec<BoxId> = Vec::new();
    let flush = |tree: &mut LayoutTree, run: &mut Vec<BoxId>, out: &mut Vec<BoxId>| {
        if run.is_empty() {
            return;
        }
        let st = anon_style(&tree.boxes[id].style.clone(), Display::Table);
        let mut t = LBox::new(BoxKind::Table, st, None);
        t.anonymous = true;
        t.parent = Some(id);
        let tid = tree.boxes.len();
        tree.boxes.push(t);
        for &r in run.iter() {
            tree.boxes[r].parent = Some(tid);
        }
        tree.boxes[tid].children = std::mem::take(run);
        fix_table(tree, tid);
        out.push(tid);
    };
    for k in kids {
        if is_table_part(&tree.boxes[k].kind) || (!run.is_empty() && is_ws_text(&tree.boxes[k])) {
            run.push(k);
        } else {
            flush(tree, &mut run, &mut out);
            out.push(k);
        }
    }
    flush(tree, &mut run, &mut out);
    tree.boxes[id].children = out;
}

/// CSS 2 §9.2.1.1: if a block container has both block-level and inline-level
/// children, wrap each inline run in an anonymous block.
fn anonymous_blocks(tree: &mut LayoutTree, id: BoxId) {
    let kids = tree.boxes[id].children.clone();
    let is_block_level = |b: &LBox| !b.is_inline_level() && !b.is_out_of_flow();
    let has_block = kids.iter().any(|&k| is_block_level(&tree.boxes[k]));
    let has_inline = kids.iter().any(|&k| tree.boxes[k].is_inline_level() && !is_ws_text(&tree.boxes[k]));
    if !has_block || !has_inline {
        if has_block {
            // Drop whitespace-only text between blocks.
            let kept: Vec<BoxId> = kids.into_iter().filter(|&k| !is_ws_text(&tree.boxes[k])).collect();
            tree.boxes[id].children = kept;
        }
        return;
    }
    let mut out = Vec::new();
    let mut run: Vec<BoxId> = Vec::new();
    let parent_style = tree.boxes[id].style.clone();
    let flush = |tree: &mut LayoutTree, run: &mut Vec<BoxId>, out: &mut Vec<BoxId>| {
        if run.is_empty() {
            return;
        }
        let has_content =
            run.iter().any(|&r| !is_ws_text(&tree.boxes[r]) && (tree.boxes[r].is_inline_level()));
        if !has_content {
            // Keep out-of-flow boxes, drop pure whitespace.
            for &r in run.iter() {
                if tree.boxes[r].is_out_of_flow() {
                    out.push(r);
                }
            }
            run.clear();
            return;
        }
        let st = anon_style(&parent_style, Display::Block);
        let mut b = LBox::new(BoxKind::Block, st, None);
        b.anonymous = true;
        b.parent = Some(id);
        let nid = tree.boxes.len();
        tree.boxes.push(b);
        for &r in run.iter() {
            tree.boxes[r].parent = Some(nid);
        }
        tree.boxes[nid].children = std::mem::take(run);
        out.push(nid);
    };
    for k in kids {
        if is_block_level(&tree.boxes[k]) {
            flush(tree, &mut run, &mut out);
            out.push(k);
        } else {
            run.push(k);
        }
    }
    flush(tree, &mut run, &mut out);
    tree.boxes[id].children = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace() {
        let mut p = true;
        assert_eq!(collapse_whitespace("  a \n b  ", WhiteSpace::Normal, &mut p), "a b ");
        let mut p = false;
        assert_eq!(collapse_whitespace("a\tb", WhiteSpace::Pre, &mut p), "a       b");
        let mut p = false;
        assert_eq!(collapse_whitespace("a  \n  b", WhiteSpace::PreLine, &mut p), "a\nb");
    }

    #[test]
    fn counters() {
        assert_eq!(counter_text(4, ListStyleType::UpperRoman), "IV");
        assert_eq!(counter_text(28, ListStyleType::LowerAlpha), "ab");
        assert_eq!(counter_text(1994, ListStyleType::LowerRoman), "mcmxciv");
    }
}

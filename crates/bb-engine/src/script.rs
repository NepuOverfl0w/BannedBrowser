//! Page scripts (milestone E): a bb-webapi Window per page. The Window owns the document
//! while scripts run; the page holds it the rest of the time (styling, layout, painting) and
//! lends it back around each task (`Page::run_scripts`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bb_dom::Document;
use bb_layout::{ImageState, LayoutTree};
use bb_net::{Client, Request};
use bb_style::StyleMap;
use bb_url::Url;

/// What the page's style and layout need, shared with the embedder so scripts' layout
/// queries (getComputedStyle, getBoundingClientRect, …) can style and lay out the document
/// they hold. The page keeps it current after each layout.
pub struct LayoutCtx {
    pub sheets: Vec<bb_style::Sheet>,
    pub images: HashMap<String, ImageState>,
    pub base: Url,
    pub config: crate::PageConfig,
    /// The style markup `sheets` were collected from (see `crate::style_signature`).
    pub signature: Option<String>,
    pub client: Client,
    /// The last query's results, by (document generation, node count).
    cache: Option<((u64, usize), StyleMap, LayoutTree)>,
}

impl LayoutCtx {
    pub fn new(base: Url, config: crate::PageConfig, client: Client) -> LayoutCtx {
        LayoutCtx {
            sheets: Vec::new(),
            images: HashMap::new(),
            base,
            config,
            signature: None,
            client,
            cache: None,
        }
    }

    /// Styles and layout for `doc` as it is now (recomputed when it changed).
    fn ensure(&mut self, doc: &Document) -> (&StyleMap, &LayoutTree) {
        let key = (doc.generation, doc.len());
        if self.cache.as_ref().map(|c| c.0) != Some(key) {
            let c = &self.config;
            let media = bb_css::MediaContext {
                width: c.viewport.0,
                height: c.viewport.1,
                dark: c.prefers_dark,
                print: false,
                dppx: 1.0,
            };
            // Stylesheets as the document now has them (scripts can run before the page
            // collected them, or add new ones).
            let sig = crate::style_signature(doc);
            if self.signature.as_deref() != Some(sig.as_str()) {
                let deadline = Instant::now() + c.subresource_timeout;
                let author = if c.no_author_styles {
                    Vec::new()
                } else {
                    crate::collect_stylesheets(&self.client, doc, &self.base, &media, deadline)
                };
                let mut sheets = bb_style::ua_sheets(doc.quirks_mode == bb_dom::QuirksMode::Quirks);
                sheets.push(bb_style::ua_scripting_sheet());
                sheets.extend(author);
                self.sheets = sheets;
                self.signature = Some(sig);
            }
            let c = &self.config;
            let env = bb_style::StyleEnv {
                viewport: c.viewport,
                prefers_dark: c.prefers_dark,
                no_author_styles: c.no_author_styles,
                min_font_size: 0.0,
                container_margins: c.container_margins,
            };
            let styles = bb_style::Stylist::new(&self.sheets, media).compute(doc, &env);
            let res = crate::PageResources { base: &self.base, images: &self.images };
            let mut tree = bb_layout::build_tree(doc, &styles, &res, c.viewport);
            bb_layout::layout(&mut tree);
            self.cache = Some((key, styles, tree));
        }
        let c = self.cache.as_ref().expect("filled above");
        (&c.1, &c.2)
    }
}

/// What a page's Window gets from the engine: script fetching, the clock, the viewport,
/// and style and layout answers.
pub struct PageEmbedder {
    pub client: Client,
    pub page_url: Url,
    pub start: Instant,
    pub origin_ms: f64,
    pub viewport: Cell<(f64, f64)>,
    pub layout: Rc<RefCell<LayoutCtx>>,
}

impl PageEmbedder {
    pub fn new(
        client: Client,
        page_url: Url,
        viewport: (f32, f32),
        layout: Rc<RefCell<LayoutCtx>>,
    ) -> PageEmbedder {
        let origin_ms =
            SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0);
        PageEmbedder {
            client,
            page_url,
            start: Instant::now(),
            origin_ms,
            viewport: Cell::new((viewport.0 as f64, viewport.1 as f64)),
            layout,
        }
    }
}

impl bb_webapi::Embedder for PageEmbedder {
    fn fetch_script(&self, url: &Url) -> Result<String, String> {
        let mut req = Request::get(url.clone());
        req.timeout = Duration::from_secs(20);
        req.cache_mode = crate::cache_mode();
        req.cookie_context = crate::subresource_context(&self.page_url);
        let resp = self.client.fetch(req).map_err(|e| e.to_string())?;
        if !(200..300).contains(&resp.status) {
            return Err(format!("HTTP {}", resp.status));
        }
        // HTML §4.12.1.1: classic scripts default to UTF-8 (a BOM wins).
        let (text, _, _) = encoding_rs::UTF_8.decode(&resp.body);
        Ok(text.into_owned())
    }
    fn now_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }
    fn time_origin_ms(&self) -> f64 {
        self.origin_ms
    }
    fn viewport(&self) -> (f64, f64) {
        self.viewport.get()
    }
    fn user_agent(&self) -> String {
        self.client.user_agent().to_string()
    }
    fn computed_style(&self, doc: &Document, node: NodeId) -> Vec<(String, String)> {
        let Ok(mut ctx) = self.layout.try_borrow_mut() else { return Vec::new() };
        let (styles, tree) = ctx.ensure(doc);
        let Some(st) = styles.get(node) else { return Vec::new() };
        let used = tree.node_box.get(&node).map(|&b| {
            let (_, _, w, h) = tree.boxes[b].content_rect();
            (w, h)
        });
        bb_style::computed::css_values(st, used)
    }
    fn box_info(&self, doc: &Document, node: NodeId) -> Option<bb_webapi::BoxInfo> {
        let Ok(mut ctx) = self.layout.try_borrow_mut() else { return None };
        let (_, tree) = ctx.ensure(doc);
        if let Some(&b) = tree.node_box.get(&node) {
            let b = &tree.boxes[b];
            let bw = |i: usize| b.border[i] as f64;
            let border =
                [(b.x + b.rel_offset.0) as f64, (b.y + b.rel_offset.1) as f64, b.w as f64, b.h as f64];
            let client =
                [bw(3), bw(0), (b.w as f64 - bw(1) - bw(3)).max(0.0), (b.h as f64 - bw(0) - bw(2)).max(0.0)];
            return Some(bb_webapi::BoxInfo { border, client, scroll: (client[2], client[3]) });
        }
        // Inline elements: the union of their line fragments.
        let r = bb_layout::node_rects(tree, doc, node).into_iter().reduce(|a, b| a.union(&b))?;
        let border = [r.x as f64, r.y as f64, r.w as f64, r.h as f64];
        Some(bb_webapi::BoxInfo { border, client: [0.0; 4], scroll: (0.0, 0.0) })
    }
    fn element_at(&self, doc: &Document, x: f64, y: f64) -> Option<NodeId> {
        let Ok(mut ctx) = self.layout.try_borrow_mut() else { return None };
        let (_, tree) = ctx.ensure(doc);
        let mut n = bb_layout::hit_test(tree, x as f32, y as f32).node?;
        while !doc.node(n).is_element() {
            n = doc.parent(n)?;
        }
        Some(n)
    }
}

use bb_dom::NodeId;
use bb_js::Value;
use bb_webapi::class::C;

use crate::interact::{KeyMods, PageKey};
use crate::Page;

/// UI Events KeyboardEvent `key` and `code` values for the keys the page handles.
fn key_names(k: PageKey) -> (&'static str, &'static str) {
    match k {
        PageKey::Tab => ("Tab", "Tab"),
        PageKey::Enter => ("Enter", "Enter"),
        PageKey::Space => (" ", "Space"),
        PageKey::Escape => ("Escape", "Escape"),
        PageKey::Backspace => ("Backspace", "Backspace"),
        PageKey::Delete => ("Delete", "Delete"),
        PageKey::Left => ("ArrowLeft", "ArrowLeft"),
        PageKey::Right => ("ArrowRight", "ArrowRight"),
        PageKey::Up => ("ArrowUp", "ArrowUp"),
        PageKey::Down | PageKey::Open => ("ArrowDown", "ArrowDown"),
        PageKey::Home => ("Home", "Home"),
        PageKey::End => ("End", "End"),
        PageKey::SelectAll => ("a", "KeyA"),
        PageKey::Undo => ("z", "KeyZ"),
        PageKey::Redo => ("y", "KeyY"),
        PageKey::Copy => ("c", "KeyC"),
        PageKey::Cut => ("x", "KeyX"),
    }
}

/// The legacy keyCode of a key (UI Events §7.3).
fn key_code(k: PageKey) -> f64 {
    match k {
        PageKey::Tab => 9.0,
        PageKey::Enter => 13.0,
        PageKey::Space => 32.0,
        PageKey::Escape => 27.0,
        PageKey::Backspace => 8.0,
        PageKey::Delete => 46.0,
        PageKey::Left => 37.0,
        PageKey::Up => 38.0,
        PageKey::Right => 39.0,
        PageKey::Down | PageKey::Open => 40.0,
        PageKey::Home => 36.0,
        PageKey::End => 35.0,
        PageKey::SelectAll => 65.0,
        PageKey::Undo => 90.0,
        PageKey::Redo => 89.0,
        PageKey::Copy => 67.0,
        PageKey::Cut => 88.0,
    }
}

impl Page {
    /// The element a point hits (text hits count as their parent element).
    fn element_at(&self, x: f32, y: f32) -> Option<NodeId> {
        let mut n = bb_layout::hit_test(&self.layout, x, y).node?;
        while !self.doc.node(n).is_element() {
            n = self.doc.parent(n)?;
        }
        Some(n)
    }

    /// A primary click's mousedown, mouseup and click events (UI Events §3.5, HTML §6.5.3):
    /// returns false if the page canceled the click (no default action then).
    pub fn script_click(&mut self, x: f32, y: f32) -> bool {
        if self.script.is_none() || self.frame_at(x, y).is_some() {
            return true;
        }
        let Some(target) = self.element_at(x, y) else { return true };
        let coords = |buttons: f64| {
            vec![
                ("clientX", Value::Number(x.round() as f64)),
                ("clientY", Value::Number(y.round() as f64)),
                ("screenX", Value::Number(x.round() as f64)),
                ("screenY", Value::Number(y.round() as f64)),
                ("button", Value::Number(0.0)),
                ("buttons", Value::Number(buttons)),
                ("detail", Value::Number(1.0)),
            ]
        };
        let mut proceed = true;
        self.with_window(|w| {
            w.dispatch_ui(C::MouseEvent, "mousedown", Some(target), true, true, &coords(1.0));
            w.dispatch_ui(C::MouseEvent, "mouseup", Some(target), true, true, &coords(0.0));
            proceed = w.dispatch_ui(C::MouseEvent, "click", Some(target), true, true, &coords(0.0));
        });
        proceed
    }

    /// The element key events go to: the focused element, else the body.
    fn key_target(&self) -> Option<NodeId> {
        self.focus.node.or_else(|| self.doc.body()).or_else(|| self.doc.document_element())
    }

    fn key_fields(key: &str, code: &str, key_code: f64, m: KeyMods) -> Vec<(&'static str, Value)> {
        vec![
            ("key", Value::str(key)),
            ("code", Value::str(code)),
            ("keyCode", Value::Number(key_code)),
            ("shiftKey", Value::Bool(m.shift)),
            ("ctrlKey", Value::Bool(m.primary || m.word)),
        ]
    }

    /// keydown for `k` (UI Events §5.6): false if the page canceled it.
    pub fn script_key_down(&mut self, k: PageKey, m: KeyMods) -> bool {
        if self.script.is_none() || self.focus.frame.is_some() {
            return true;
        }
        let target = self.key_target();
        let (key, code) = key_names(k);
        let fields = Self::key_fields(key, code, key_code(k), m);
        let mut proceed = true;
        self.with_window(|w| {
            proceed = w.dispatch_ui(C::KeyboardEvent, "keydown", target, true, true, &fields)
        });
        proceed
    }

    /// keyup for `k`.
    pub fn script_key_up(&mut self, k: PageKey, m: KeyMods) {
        if self.script.is_none() || self.focus.frame.is_some() {
            return;
        }
        let target = self.key_target();
        let (key, code) = key_names(k);
        let fields = Self::key_fields(key, code, key_code(k), m);
        self.with_window(|w| {
            w.dispatch_ui(C::KeyboardEvent, "keyup", target, true, false, &fields);
        });
    }

    /// keydown and keypress for typed text: false if the page canceled them.
    pub fn script_text_key(&mut self, text: &str) -> bool {
        if self.script.is_none() || self.focus.frame.is_some() {
            return true;
        }
        let target = self.key_target();
        let code = text.chars().next().map_or(0.0, |c| c.to_ascii_uppercase() as u32 as f64);
        let fields = Self::key_fields(text, "", code, KeyMods::default());
        let mut proceed = true;
        self.with_window(|w| {
            proceed = w.dispatch_ui(C::KeyboardEvent, "keydown", target, true, true, &fields)
                && w.dispatch_ui(C::KeyboardEvent, "keypress", target, true, true, &fields);
            w.dispatch_ui(C::KeyboardEvent, "keyup", target, true, false, &fields);
        });
        proceed
    }

    /// `input` at the focused control after its value changed (HTML §4.10.5.5).
    pub fn script_input(&mut self) {
        if self.script.is_none() || self.focus.frame.is_some() {
            return;
        }
        let Some(target) = self.focus.node else { return };
        self.with_window(|w| {
            w.dispatch_ui(C::Event, "input", Some(target), true, false, &[]);
        });
    }
}

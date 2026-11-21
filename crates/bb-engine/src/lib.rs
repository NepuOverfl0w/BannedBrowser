//! `bb-engine` — the per-page pipeline:
//! fetch → decode → parse → stylesheets → images → style → layout → display list.
//!
//! [`Page::load`] runs the whole pipeline synchronously (used by headless mode
//! and by each tab's worker thread). Panics inside the pipeline are caught and
//! turned into the "BannedBrowser hit a snag" page.
#![forbid(unsafe_code)]

mod internal;

pub use internal::{about_handler, error_page_html, snag_page_html};

use bb_css::{MediaContext, MediaQueryList, Rule, Stylesheet};
use bb_dom::{Document, NodeId};
use bb_layout::{ImageState, LayoutTree, Resources};
use bb_net::{Client, NetError, NetErrorKind, Request};
use bb_paint::DisplayList;
use bb_style::{Origin, Sheet, StyleEnv, StyleMap, Stylist};
use bb_url::Url;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Rendering parameters for a page.
#[derive(Debug, Clone, Copy)]
pub struct PageConfig {
    /// Viewport in CSS px.
    pub viewport: (f32, f32),
    pub prefers_dark: bool,
    pub no_author_styles: bool,
    /// Overall budget for subresources (stylesheets + images).
    pub subresource_timeout: Duration,
    pub load_images: bool,
}

impl Default for PageConfig {
    fn default() -> Self {
        PageConfig {
            viewport: (1024.0, 768.0),
            prefers_dark: false,
            no_author_styles: false,
            subresource_timeout: Duration::from_secs(20),
            load_images: true,
        }
    }
}

/// Timing / diagnostic information for a load.
#[derive(Debug, Clone, Default)]
pub struct LoadStats {
    pub fetch_ms: u128,
    pub parse_ms: u128,
    pub subresources_ms: u128,
    pub style_ms: u128,
    pub layout_ms: u128,
    pub stylesheets: usize,
    pub images: usize,
    pub images_failed: usize,
}

/// A loaded page: DOM, styles, layout and display list.
pub struct Page {
    pub url: Url,
    pub base_url: Url,
    pub doc: Document,
    pub title: String,
    pub status: u16,
    pub secure: bool,
    pub sheets: Vec<Sheet>,
    pub images: HashMap<String, ImageState>,
    pub styles: StyleMap,
    pub layout: LayoutTree,
    pub display_list: DisplayList,
    pub favicon_url: Option<Url>,
    pub config: PageConfig,
    pub stats: LoadStats,
    /// Set when the page is an internal error page (network / snag).
    pub error: Option<NetError>,
}

struct PageResources<'a> {
    base: &'a Url,
    images: &'a HashMap<String, ImageState>,
}

impl Resources for PageResources<'_> {
    fn image(&self, url: &str) -> ImageState {
        self.images.get(url).cloned().unwrap_or(ImageState::Loading)
    }
    fn resolve(&self, url: &str) -> String {
        self.base.join(url.trim()).map(|u| u.href()).unwrap_or_else(|_| url.to_string())
    }
}

/// A fetched document ready to parse.
struct Fetched {
    url: Url,
    status: u16,
    secure: bool,
    html: String,
    error: Option<NetError>,
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn fetch_document(client: &Client, url: &Url) -> Fetched {
    let mut req = Request::get(url.clone());
    req.timeout = Duration::from_secs(45);
    match client.fetch(req) {
        Ok(resp) => {
            let ct = resp.content_type();
            let essence = ct
                .as_ref()
                .map(|c| c.essence.clone())
                .unwrap_or_else(|| bb_net::mime::sniff(&resp.body).to_string());
            let charset = ct.as_ref().and_then(|c| c.charset.clone());
            let html = if essence == "text/html" || essence == "application/xhtml+xml" || essence.is_empty() {
                bb_net::encoding::decode_html(&resp.body, charset.as_deref()).0
            } else if essence.starts_with("image/") {
                format!(
                    "<!DOCTYPE html><html><head><title>{t}</title><style>html{{background:#222}}body{{margin:0;display:flex}}img{{margin:auto}}</style></head><body><img src=\"{u}\"></body></html>",
                    t = escape_html(resp.url.pathname().rsplit('/').next().unwrap_or("")),
                    u = escape_html(&resp.url.href())
                )
            } else if essence.starts_with("text/")
                || essence == "application/json"
                || essence == "application/javascript"
                || essence == "application/xml"
            {
                let (text, _) =
                    bb_net::encoding::decode_html(&resp.body, charset.as_deref().or(Some("utf-8")));
                format!("<!DOCTYPE html><html><head><title>{}</title></head><body><pre style=\"white-space:pre-wrap;word-wrap:break-word\">{}</pre></body></html>", escape_html(&resp.url.href()), escape_html(&text))
            } else {
                // TODO(M9): hand non-renderable types to the downloads manager.
                error_page_html(
                    "Can't show this file",
                    &format!("BannedBrowser can't display files of type {essence} yet."),
                    &resp.url.href(),
                    "",
                )
            };
            Fetched { url: resp.url, status: resp.status, secure: resp.secure, html, error: None }
        }
        Err(e) => {
            let (title, msg) = match e.kind {
                NetErrorKind::Dns => (
                    "Hmm. We're having trouble finding that site.",
                    "BannedBrowser can't find the server for this address.",
                ),
                NetErrorKind::Connect => {
                    ("Unable to connect", "BannedBrowser can't establish a connection to the server.")
                }
                NetErrorKind::Timeout => {
                    ("The connection has timed out", "The server took too long to respond.")
                }
                NetErrorKind::Certificate => (
                    "Warning: Potential Security Risk Ahead",
                    "The site's certificate is not trusted. Someone could be trying to impersonate it.",
                ),
                NetErrorKind::Tls => {
                    ("Secure Connection Failed", "An error occurred during a connection to the server.")
                }
                NetErrorKind::FileNotFound => {
                    ("File not found", "BannedBrowser can't find the file or page.")
                }
                NetErrorKind::UnsupportedScheme => {
                    ("The address wasn't understood", "BannedBrowser doesn't know how to open this address.")
                }
                NetErrorKind::Blocked => {
                    ("Access blocked", "The connection was refused by a proxy or policy.")
                }
                NetErrorKind::TooManyRedirects => (
                    "The page isn't redirecting properly",
                    "The server is redirecting in a way that will never complete.",
                ),
                _ => ("Problem loading page", "The page could not be loaded."),
            };
            let html = error_page_html(title, msg, &url.href(), &e.message);
            Fetched { url: url.clone(), status: 0, secure: false, html, error: Some(e) }
        }
    }
}

/// Collect `<style>` and `<link rel=stylesheet>` in document order, fetching
/// external sheets (and `@import`s) in parallel.
fn collect_stylesheets(
    client: &Client,
    doc: &Document,
    base: &Url,
    media: &MediaContext,
    deadline: Instant,
) -> Vec<Sheet> {
    enum Src {
        Inline(String, MediaQueryList),
        External(Url, MediaQueryList),
    }
    let mut srcs = Vec::new();
    for n in doc.descendants(Document::ROOT) {
        let Some(e) = doc.element(n) else { continue };
        if e.ns != bb_dom::Namespace::Html {
            continue;
        }
        let media_attr = || e.attr("media").map(MediaQueryList::parse_str).unwrap_or_default();
        if e.name == "style" {
            srcs.push(Src::Inline(doc.text_content(n), media_attr()));
        } else if e.name == "link" {
            let rel = e.attr("rel").unwrap_or("").to_ascii_lowercase();
            let rels: Vec<&str> = rel.split_ascii_whitespace().collect();
            if rels.contains(&"stylesheet") && !rels.contains(&"alternate") {
                if let Some(href) = e.attr("href") {
                    if let Ok(u) = base.join(href.trim()) {
                        srcs.push(Src::External(u, media_attr()));
                    }
                }
            }
        }
    }
    // Fetch external sheets in parallel.
    let urls: Vec<Url> = srcs
        .iter()
        .filter_map(|s| if let Src::External(u, _) = s { Some(u.clone()) } else { None })
        .collect();
    let fetched = fetch_texts(client, &urls, deadline);
    let mut out = Vec::new();
    for s in srcs {
        match s {
            Src::Inline(css, m) => {
                let ss = Stylesheet::parse(&css);
                resolve_imports(client, ss, base, m, media, deadline, 0, &mut out);
            }
            Src::External(u, m) => {
                if let Some(Some(css)) = fetched.get(&u.href()) {
                    let ss = Stylesheet::parse(css);
                    resolve_imports(client, ss, &u, m, media, deadline, 0, &mut out);
                }
            }
        }
    }
    out
}

/// Inline `@import`ed sheets before the importing sheet (CSS Cascade 4 §2).
#[allow(clippy::too_many_arguments)]
fn resolve_imports(
    client: &Client,
    ss: Stylesheet,
    base: &Url,
    media: MediaQueryList,
    ctx: &MediaContext,
    deadline: Instant,
    depth: usize,
    out: &mut Vec<Sheet>,
) {
    let imports: Vec<(Url, MediaQueryList)> = ss
        .rules
        .iter()
        .filter_map(|r| match r {
            Rule::Import { url, media } => base.join(url).ok().map(|u| (u, media.clone())),
            _ => None,
        })
        .collect();
    if depth < 4 && !imports.is_empty() && media.evaluate(ctx) {
        let urls: Vec<Url> = imports.iter().map(|(u, _)| u.clone()).collect();
        let texts = fetch_texts(client, &urls, deadline);
        for (u, m) in imports {
            if !m.evaluate(ctx) {
                continue;
            }
            if let Some(Some(css)) = texts.get(&u.href()) {
                resolve_imports(client, Stylesheet::parse(css), &u, m, ctx, deadline, depth + 1, out);
            }
        }
    }
    out.push(Sheet { origin: Origin::Author, stylesheet: Rc::new(ss), media });
}

fn fetch_bytes(client: &Client, urls: &[Url], deadline: Instant) -> HashMap<String, Result<Vec<u8>, String>> {
    let mut out = HashMap::new();
    if urls.is_empty() {
        return out;
    }
    let mut uniq: Vec<Url> = Vec::new();
    for u in urls {
        if !uniq.iter().any(|x| x.href() == u.href()) {
            uniq.push(u.clone());
        }
    }
    let workers = uniq.len().min(6);
    let queue = std::sync::Mutex::new(uniq);
    let results = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let next = queue.lock().ok().and_then(|mut q| q.pop());
                let Some(u) = next else { break };
                let remaining = deadline.saturating_duration_since(Instant::now());
                let r = if remaining.is_zero() {
                    Err("timed out".to_string())
                } else {
                    let mut req = Request::get(u.clone());
                    req.timeout = remaining.min(Duration::from_secs(20));
                    match client.fetch(req) {
                        Ok(resp) if resp.ok() || resp.status == 0 => Ok(resp.body),
                        Ok(resp) => Err(format!("HTTP {}", resp.status)),
                        Err(e) => Err(e.to_string()),
                    }
                };
                if let Ok(mut res) = results.lock() {
                    res.push((u.href(), r));
                }
            });
        }
    });
    for (k, v) in results.into_inner().unwrap_or_default() {
        out.insert(k, v);
    }
    out
}

fn fetch_texts(client: &Client, urls: &[Url], deadline: Instant) -> HashMap<String, Option<String>> {
    fetch_bytes(client, urls, deadline)
        .into_iter()
        .map(|(k, v)| (k, v.ok().map(|b| bb_net::encoding::decode_html(&b, Some("utf-8")).0)))
        .collect()
}

/// `<base href>` (HTML §4.2.3), else the document URL.
fn base_url(doc: &Document, url: &Url) -> Url {
    for n in doc.elements_by_tag("base") {
        if let Some(h) = doc.attr(n, "href") {
            if let Ok(u) = url.join(h.trim()) {
                return u;
            }
        }
    }
    url.clone()
}

fn favicon(doc: &Document, base: &Url) -> Option<Url> {
    for n in doc.elements_by_tag("link") {
        let rel = doc.attr(n, "rel").unwrap_or("").to_ascii_lowercase();
        if rel.split_ascii_whitespace().any(|r| r == "icon") {
            if let Some(h) = doc.attr(n, "href") {
                if let Ok(u) = base.join(h.trim()) {
                    return Some(u);
                }
            }
        }
    }
    if matches!(base.scheme(), "http" | "https") {
        return base.join("/favicon.ico").ok();
    }
    None
}

/// HTML frameset fallback (v0.1): frames render as a list of links.
/// TODO(HTML §15.6): real frame rendering with nested documents.
fn frameset_fallback(doc: &Document, base: &Url) -> Option<String> {
    let body = doc.body()?;
    if !doc.is_html(body, "frameset") {
        return None;
    }
    let mut html = format!("<!DOCTYPE html><html><head><title>{}</title><link rel=stylesheet href=\"about:style.css\"></head><body><main><h1>{}</h1><p>This page uses frames. BannedBrowser doesn't render framesets yet; open a frame:</p><ul>", escape_html(&doc.title()), escape_html(&doc.title()));
    for f in doc.descendants(body) {
        if doc.is_html(f, "frame") {
            if let Some(src) = doc.attr(f, "src") {
                let u = base.join(src.trim()).map(|u| u.href()).unwrap_or_else(|_| src.to_string());
                let name = doc.attr(f, "name").unwrap_or(src);
                html.push_str(&format!("<li><a href=\"{}\">{}</a></li>", escape_html(&u), escape_html(name)));
            }
        }
    }
    html.push_str("</ul></main></body></html>");
    Some(html)
}

fn image_urls(doc: &Document, styles: &StyleMap, base: &Url) -> Vec<Url> {
    let mut out = Vec::new();
    for n in doc.descendants(Document::ROOT) {
        let Some(e) = doc.element(n) else { continue };
        let src = match e.name.as_str() {
            "img" => e.attr("src"),
            "input" if e.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("image")) => e.attr("src"),
            _ => None,
        };
        if let Some(s) = src.filter(|s| !s.trim().is_empty()) {
            if let Ok(u) = base.join(s.trim()) {
                out.push(u);
            }
        }
        if let Some(st) = styles.get(n) {
            if st.display == bb_style::Display::None {
                continue;
            }
            for layer in &st.background {
                if let Some(bb_style::Image::Url(u)) = &layer.image {
                    if let Ok(u) = base.join(u.trim()) {
                        out.push(u);
                    }
                }
            }
            if let Some(li) = &st.list_style_image {
                if let Ok(u) = base.join(li.trim()) {
                    out.push(u);
                }
            }
        }
    }
    out
}

impl Page {
    /// Load and render `url` completely (blocking).
    pub fn load(client: &Client, url: &Url, config: PageConfig) -> Page {
        let started = Instant::now();
        let fetched = fetch_document(client, url);
        let fetch_ms = started.elapsed().as_millis();
        let mut page = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Page::from_html(client, &fetched.html, &fetched.url, config, fetched.status, fetched.secure)
        })) {
            Ok(p) => p,
            Err(panic) => {
                let detail = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                log::error!("page pipeline panicked for {}: {detail}", fetched.url);
                let html = snag_page_html(&fetched.url.href(), &detail);
                Page::from_html(client, &html, &fetched.url, config, 0, false)
            }
        };
        page.error = fetched.error;
        page.stats.fetch_ms = fetch_ms;
        page
    }

    /// Run the pipeline on already-fetched HTML.
    pub fn from_html(
        client: &Client,
        html: &str,
        url: &Url,
        config: PageConfig,
        status: u16,
        secure: bool,
    ) -> Page {
        let mut stats = LoadStats::default();
        let t0 = Instant::now();
        let mut doc = bb_html::parse_document(html);
        doc.url = url.href();
        let base = base_url(&doc, url);
        if let Some(fb) = frameset_fallback(&doc, &base) {
            doc = bb_html::parse_document(&fb);
            doc.url = url.href();
        }
        stats.parse_ms = t0.elapsed().as_millis();
        let t1 = Instant::now();
        let deadline = Instant::now() + config.subresource_timeout;
        let media = MediaContext {
            width: config.viewport.0,
            height: config.viewport.1,
            dark: config.prefers_dark,
            print: false,
            dppx: 1.0,
        };
        let author = if config.no_author_styles {
            Vec::new()
        } else {
            collect_stylesheets(client, &doc, &base, &media, deadline)
        };
        stats.stylesheets = author.len();
        let mut sheets = bb_style::ua_sheets(doc.quirks_mode == bb_dom::QuirksMode::Quirks);
        sheets.extend(author);
        let env = StyleEnv {
            viewport: config.viewport,
            prefers_dark: config.prefers_dark,
            no_author_styles: config.no_author_styles,
            min_font_size: 0.0,
        };
        let ts = Instant::now();
        let stylist = Stylist::new(&sheets, media);
        let styles = stylist.compute(&doc, &env);
        stats.style_ms = ts.elapsed().as_millis();
        // Images (after styles, so background images are known).
        let mut images = HashMap::new();
        if config.load_images {
            let urls = image_urls(&doc, &styles, &base);
            let fetched = fetch_bytes(client, &urls, deadline);
            for (k, v) in fetched {
                let st = match v {
                    Ok(bytes) => match bb_image::decode(&bytes) {
                        Ok(img) => ImageState::Ready(Arc::new(img)),
                        Err(e) => {
                            log::debug!("image {k}: {e}");
                            ImageState::Broken
                        }
                    },
                    Err(e) => {
                        log::debug!("image {k}: {e}");
                        ImageState::Broken
                    }
                };
                if matches!(st, ImageState::Broken) {
                    stats.images_failed += 1;
                }
                stats.images += 1;
                images.insert(k, st);
            }
        }
        stats.subresources_ms = t1.elapsed().as_millis();
        let title = doc.title();
        let favicon_url = favicon(&doc, &base);
        let mut page = Page {
            url: url.clone(),
            base_url: base,
            title,
            status,
            secure,
            sheets,
            images,
            styles,
            layout: LayoutTree {
                boxes: Vec::new(),
                root: 0,
                quirks: false,
                viewport: config.viewport,
                canvas_background: bb_css::Rgba::WHITE,
                node_box: Default::default(),
            },
            display_list: DisplayList::default(),
            favicon_url,
            config,
            stats,
            doc,
            error: None,
        };
        page.relayout_styles_ready();
        page
    }

    /// Recompute styles (e.g. after a viewport or color-scheme change), then lay out.
    pub fn restyle(&mut self) {
        let media = MediaContext {
            width: self.config.viewport.0,
            height: self.config.viewport.1,
            dark: self.config.prefers_dark,
            print: false,
            dppx: 1.0,
        };
        let env = StyleEnv {
            viewport: self.config.viewport,
            prefers_dark: self.config.prefers_dark,
            no_author_styles: self.config.no_author_styles,
            min_font_size: 0.0,
        };
        let ts = Instant::now();
        self.styles = Stylist::new(&self.sheets, media).compute(&self.doc, &env);
        self.stats.style_ms = ts.elapsed().as_millis();
        self.relayout_styles_ready();
    }

    fn relayout_styles_ready(&mut self) {
        let tl = Instant::now();
        let res = PageResources { base: &self.base_url, images: &self.images };
        let mut tree = bb_layout::build_tree(&self.doc, &self.styles, &res, self.config.viewport);
        bb_layout::layout(&mut tree);
        self.display_list = bb_layout::paint::build_display_list(&tree);
        self.layout = tree;
        self.stats.layout_ms = tl.elapsed().as_millis();
        self.doc.clear_dirty();
    }

    /// Change the viewport; restyles (media queries, vw/vh) and relayouts.
    pub fn set_viewport(&mut self, w: f32, h: f32) {
        if (self.config.viewport.0 - w).abs() < 0.5 && (self.config.viewport.1 - h).abs() < 0.5 {
            return;
        }
        self.config.viewport = (w, h);
        self.restyle();
    }

    pub fn set_dark(&mut self, dark: bool) {
        if self.config.prefers_dark != dark {
            self.config.prefers_dark = dark;
            self.restyle();
        }
    }

    /// Document height in CSS px.
    pub fn content_height(&self) -> f32 {
        self.display_list.height
    }

    /// The link (resolved URL) at a page point, if any.
    pub fn link_at(&self, x: f32, y: f32) -> Option<Url> {
        let hit = bb_layout::hit_test(&self.layout, x, y);
        let node = hit.node?;
        self.link_for_node(node, x, y)
    }

    fn link_for_node(&self, node: NodeId, x: f32, y: f32) -> Option<Url> {
        let doc = &self.doc;
        let mut cur = Some(node);
        while let Some(n) = cur {
            if let Some(e) = doc.element(n) {
                if e.ns == bb_dom::Namespace::Html {
                    if (e.name == "a" || e.name == "area") && e.has_attr("href") {
                        return self.base_url.join(e.attr("href")?.trim()).ok();
                    }
                    if e.name == "img" {
                        if let Some(u) = self.image_map_link(n, x, y) {
                            return Some(u);
                        }
                    }
                }
            }
            cur = doc.parent(n);
        }
        None
    }

    /// Client-side image maps (HTML §4.8.13): `<img usemap>` + `<map>/<area>`.
    fn image_map_link(&self, img: NodeId, x: f32, y: f32) -> Option<Url> {
        let doc = &self.doc;
        let usemap = doc.attr(img, "usemap")?.trim().trim_start_matches('#').to_string();
        let map = doc.descendants(Document::ROOT).find(|&n| {
            doc.is_html(n, "map")
                && (doc.attr(n, "name") == Some(&usemap) || doc.attr(n, "id") == Some(&usemap))
        })?;
        let b = &self.layout.boxes[*self.layout.node_box.get(&img)?];
        let (cx, cy, _, _) = b.content_rect();
        let (lx, ly) = (x - cx - b.rel_offset.0, y - cy - b.rel_offset.1);
        for area in doc.descendants(map).filter(|&n| doc.is_html(n, "area")) {
            let coords: Vec<f32> = doc
                .attr(area, "coords")
                .unwrap_or("")
                .split([',', ' '])
                .filter_map(|v| v.trim().parse().ok())
                .collect();
            let shape = doc.attr(area, "shape").unwrap_or("rect").to_ascii_lowercase();
            let inside = match shape.as_str() {
                "circle" | "circ" if coords.len() >= 3 => {
                    (lx - coords[0]).powi(2) + (ly - coords[1]).powi(2) <= coords[2].powi(2)
                }
                "poly" | "polygon" if coords.len() >= 6 => {
                    let pts: Vec<(f32, f32)> = coords.chunks_exact(2).map(|c| (c[0], c[1])).collect();
                    let mut inside = false;
                    let mut j = pts.len() - 1;
                    for i in 0..pts.len() {
                        let (xi, yi) = pts[i];
                        let (xj, yj) = pts[j];
                        if (yi > ly) != (yj > ly) && lx < (xj - xi) * (ly - yi) / (yj - yi) + xi {
                            inside = !inside;
                        }
                        j = i;
                    }
                    inside
                }
                "default" => true,
                _ if coords.len() >= 4 => {
                    lx >= coords[0].min(coords[2])
                        && lx < coords[0].max(coords[2])
                        && ly >= coords[1].min(coords[3])
                        && ly < coords[1].max(coords[3])
                }
                _ => false,
            };
            if inside {
                if let Some(h) = doc.attr(area, "href") {
                    return self.base_url.join(h.trim()).ok();
                }
                return None;
            }
        }
        None
    }

    /// Cursor style at a page point.
    pub fn cursor_at(&self, x: f32, y: f32) -> Option<bb_style::Cursor> {
        let hit = bb_layout::hit_test(&self.layout, x, y);
        if let Some(n) = hit.node {
            if self.link_for_node(n, x, y).is_some() {
                return Some(bb_style::Cursor::Pointer);
            }
        }
        hit.cursor
    }

    /// Render the viewport at `scroll` into a new canvas of device size.
    pub fn render(
        &self,
        scroll_y: f32,
        scale: f32,
        width_px: u32,
        height_px: u32,
    ) -> bb_paint::SoftwareCanvas {
        let mut c = bb_paint::SoftwareCanvas::new(width_px, height_px);
        self.render_into(&mut c, scroll_y, scale);
        c
    }

    pub fn render_into(&self, c: &mut bb_paint::SoftwareCanvas, scroll_y: f32, scale: f32) {
        use bb_paint::Canvas;
        c.clear(self.display_list.background);
        bb_paint::render(&self.display_list, c, (0.0, scroll_y), scale);
    }
}

/// Build the network client with the internal `about:` handler installed.
pub fn new_client(extra_ca: Vec<std::path::PathBuf>) -> Client {
    Client::new(bb_net::ClientConfig {
        extra_ca_files: extra_ca,
        about_handler: Some(Arc::new(about_handler)),
        ..Default::default()
    })
}

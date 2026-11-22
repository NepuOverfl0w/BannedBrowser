//! Frames: `<frameset>`/`<frame>` (HTML §15.6) and `<iframe>` (HTML §4.8.5).
//!
//! Each frame owns a nested [`Page`]. Layout and paint of the parent emit an
//! [`Item::SubDocument`] where the frame goes; [`Page::render_into`] draws the
//! child page there. Link targets follow HTML §7.3.1.7 "the rules for choosing
//! a navigable".

use crate::{fetch_document, Page, PageConfig};
use bb_dom::{Document, NodeId};
use bb_net::Client;
use bb_paint::{Color, DisplayList, Item, Rect};
use bb_url::Url;

/// Frames nest at most this deep (guards against self-framing pages).
pub const MAX_FRAME_DEPTH: u8 = 4;
/// At most this many child frames per document.
pub const MAX_FRAMES: usize = 32;

/// A child browsing context.
pub struct ChildFrame {
    /// The `<frame>` / `<iframe>` element in the parent document.
    pub node: NodeId,
    /// The `name` attribute (target for links).
    pub name: String,
    /// Content rect in the parent's page coordinates (CSS px).
    pub rect: Rect,
    pub page: Box<Page>,
    pub scroll_y: f32,
    /// `scrolling="no"` turns scrolling off.
    pub scrolling: bool,
}

/// A link under the pointer, possibly inside nested frames.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkHit {
    pub url: Url,
    /// The link's `target` (or the document's `<base target>`), lowercased
    /// for keywords; empty when absent.
    pub target: String,
    /// Indices into `frames` from the top page down to the document holding the link.
    pub frame_path: Vec<usize>,
    /// A form POST's body (form submissions navigate like links).
    pub post: Option<crate::PostBody>,
}

/// What following a link did.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkDisposition {
    /// Navigate the whole tab.
    Top(crate::NavRequest),
    /// Open in a new tab (`_blank` or an unknown name).
    NewTab(crate::NavRequest),
    /// A frame inside the page was navigated; re-render.
    Frame,
}

// ---------------------------------------------------------------------------
// Frameset layout

#[derive(Debug, Clone, Copy, PartialEq)]
enum Dim {
    Absolute(f32),
    Percent(f32),
    Relative(f32),
}

/// HTML §2.3.4.6 "rules for parsing a list of dimensions".
fn parse_dimensions(s: &str) -> Vec<Dim> {
    let s = s.strip_suffix(',').unwrap_or(s);
    let mut out = Vec::new();
    for raw in s.split(',') {
        let t = raw.trim_start_matches([' ', '\t', '\n', '\r', '\x0c']);
        let mut chars = t.chars().peekable();
        let mut num = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                num.push(c);
                chars.next();
            } else {
                break;
            }
        }
        if chars.peek() == Some(&'.') {
            chars.next();
            let mut frac = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() || c == ' ' {
                    if c != ' ' {
                        frac.push(c);
                    }
                    chars.next();
                } else {
                    break;
                }
            }
            if !frac.is_empty() {
                num.push('.');
                num.push_str(&frac);
            }
        }
        while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            chars.next();
        }
        let had_number = !num.is_empty();
        let value: f32 = num.parse().unwrap_or(0.0);
        out.push(match chars.peek() {
            Some('%') => Dim::Percent(value),
            Some('*') => Dim::Relative(if had_number { value } else { 1.0 }),
            _ => Dim::Absolute(value),
        });
    }
    out
}

/// HTML §15.6.2 "convert a list of dimensions to a list of pixel values".
fn to_pixels(dims: &[Dim], total: f32) -> Vec<f32> {
    let total = total.max(0.0);
    let mut px: Vec<f32> = dims
        .iter()
        .map(|d| match *d {
            Dim::Absolute(v) => v,
            Dim::Percent(v) => v * total / 100.0,
            Dim::Relative(_) => 0.0,
        })
        .collect();
    let fixed: f32 = px.iter().sum();
    let rel_sum: f32 = dims.iter().map(|d| if let Dim::Relative(v) = d { *v } else { 0.0 }).sum();
    let has_rel = dims.iter().any(|d| matches!(d, Dim::Relative(_)));
    if fixed > total && fixed > 0.0 {
        // Too big: shrink absolute and percentage sizes proportionally.
        let k = total / fixed;
        px.iter_mut().for_each(|v| *v *= k);
    } else if has_rel {
        let rest = total - fixed;
        for (p, d) in px.iter_mut().zip(dims) {
            if let Dim::Relative(v) = d {
                *p = if rel_sum > 0.0 { rest * v / rel_sum } else { rest / dims.len() as f32 };
            }
        }
    } else if fixed > 0.0 && fixed < total {
        // Nothing relative to absorb the slack: grow everything proportionally.
        let k = total / fixed;
        px.iter_mut().for_each(|v| *v *= k);
    }
    px
}

/// Split `start..start+len` (minus gaps) into rounded segments.
fn segments(dims: &[Dim], start: f32, len: f32, gap: f32) -> Vec<(f32, f32)> {
    let n = dims.len().max(1);
    let avail = (len - gap * (n as f32 - 1.0)).max(0.0);
    let px = to_pixels(dims, avail);
    let mut out = Vec::with_capacity(n);
    let mut acc = 0.0;
    for (i, p) in px.iter().enumerate() {
        let a = (start + acc + gap * i as f32).round();
        acc += p;
        let b = (start + acc + gap * i as f32).round();
        out.push((a, (b - a).max(0.0)));
    }
    out
}

fn border_off(v: &str) -> bool {
    let v = v.trim();
    v == "0" || v.eq_ignore_ascii_case("no")
}

/// Lay out a `<frameset>` filling the viewport. Returns the display list
/// (borders + one [`Item::SubDocument`] per frame).
pub(crate) fn layout_frameset(doc: &Document, frameset: NodeId, viewport: (f32, f32)) -> DisplayList {
    let mut dl =
        DisplayList { width: viewport.0, height: viewport.1, background: Color::WHITE, items: Vec::new() };
    lay_set(doc, frameset, Rect::new(0.0, 0.0, viewport.0, viewport.1), 6.0, true, &mut dl, 0);
    dl
}

fn lay_set(doc: &Document, set: NodeId, r: Rect, border: f32, on: bool, dl: &mut DisplayList, depth: usize) {
    if depth > 16 {
        return;
    }
    let attr = |n: &str| doc.attr(set, n);
    let on = match attr("frameborder") {
        Some(v) => !border_off(v),
        None => on,
    };
    let border = attr("border")
        .or(attr("framespacing"))
        .and_then(|v| v.trim().parse::<f32>().ok())
        .map(|v| v.clamp(0.0, 100.0))
        .unwrap_or(border);
    let gap = if on { border } else { 0.0 };
    let rows =
        attr("rows").map(parse_dimensions).filter(|d| !d.is_empty()).unwrap_or(vec![Dim::Relative(1.0)]);
    let cols =
        attr("cols").map(parse_dimensions).filter(|d| !d.is_empty()).unwrap_or(vec![Dim::Relative(1.0)]);
    if gap > 0.0 {
        // Frame borders: a light 3D bar between cells (CSS system color ButtonFace).
        dl.push(Item::Rect { rect: r, color: Color::rgb(0xE3, 0xE3, 0xE3) });
    }
    let ys = segments(&rows, r.y, r.h, gap);
    let xs = segments(&cols, r.x, r.w, gap);
    let mut kids = doc.children(set).filter(|&c| doc.is_html(c, "frame") || doc.is_html(c, "frameset"));
    for &(y, h) in &ys {
        for &(x, w) in &xs {
            let Some(kid) = kids.next() else { return };
            let cell = Rect::new(x, y, w, h);
            if doc.is_html(kid, "frameset") {
                lay_set(doc, kid, cell, border, on, dl, depth + 1);
            } else {
                if gap > 0.0 {
                    // Sunken edge around each frame.
                    let edge = Rect::new(x - 1.0, y - 1.0, w + 2.0, h + 2.0);
                    dl.push(Item::Rect { rect: edge, color: Color::rgb(0xA0, 0xA0, 0xA0) });
                }
                dl.push(Item::SubDocument { rect: cell, id: kid.0 });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Page integration

/// Top-level sub-document rects in a display list (nested lists excluded).
fn subdocuments(dl: &DisplayList) -> Vec<(NodeId, Rect)> {
    dl.items
        .iter()
        .filter_map(
            |i| if let Item::SubDocument { rect, id } = i { Some((NodeId(*id), *rect)) } else { None },
        )
        .collect()
}

impl Page {
    /// Update frame rects after a relayout (resizing child viewports).
    pub(crate) fn sync_frames(&mut self) {
        let subs = subdocuments(&self.display_list);
        for f in &mut self.frames {
            let rect = subs.iter().find(|(n, _)| *n == f.node).map(|(_, r)| *r).unwrap_or_default();
            f.rect = rect;
            if rect.w >= 1.0 && rect.h >= 1.0 {
                f.page.set_viewport(rect.w, rect.h);
                let max = (f.page.content_height() - rect.h).max(0.0);
                f.scroll_y = f.scroll_y.min(max);
            }
        }
    }

    /// Load documents for frames that appear in the display list but have no page yet.
    pub(crate) fn load_frames(&mut self, client: &Client) {
        if self.config.frame_depth >= MAX_FRAME_DEPTH {
            return;
        }
        let subs = subdocuments(&self.display_list);
        enum Src {
            Url(Url),
            Doc(String),
        }
        let mut todo: Vec<(NodeId, Rect, Src)> = Vec::new();
        for (node, rect) in subs {
            if self.frames.iter().any(|f| f.node == node) || self.frames.len() + todo.len() >= MAX_FRAMES {
                continue;
            }
            let doc = &self.doc;
            let src = if doc.is_html(node, "iframe") && doc.attr(node, "srcdoc").is_some() {
                Src::Doc(doc.attr(node, "srcdoc").unwrap_or("").to_string())
            } else {
                match doc.attr(node, "src").map(str::trim).filter(|s| !s.is_empty()) {
                    Some(s) => match self.base_url.join(s) {
                        Ok(u) => Src::Url(u),
                        Err(_) => continue,
                    },
                    None => Src::Doc(String::new()),
                }
            };
            todo.push((node, rect, src));
        }
        if todo.is_empty() {
            return;
        }
        // Fetch the frame documents in parallel; build pages on this thread
        // (pages hold `Rc`s and are not `Send`).
        let urls: Vec<Option<Url>> =
            todo.iter().map(|(_, _, s)| if let Src::Url(u) = s { Some(u.clone()) } else { None }).collect();
        // Frames' requests are made in the top-level document's site (cookies).
        let top = crate::top_site(&self.url);
        let top_ref = &top;
        let mode = crate::cache_mode();
        let fetched: Vec<Option<crate::Fetched>> = std::thread::scope(|sc| {
            let handles: Vec<_> = urls
                .iter()
                .map(|u| {
                    u.as_ref().map(|u| {
                        sc.spawn(move || {
                            crate::with_cache_mode(mode, || fetch_document(client, u, Some(top_ref), None))
                        })
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.and_then(|h| h.join().ok())).collect()
        });
        for ((node, rect, src), f) in todo.into_iter().zip(fetched) {
            let px = |n: &str| {
                self.doc.attr(node, n).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
            };
            let cfg = PageConfig {
                viewport: (rect.w.max(1.0), rect.h.max(1.0)),
                frame_depth: self.config.frame_depth + 1,
                container_margins: (px("marginwidth"), px("marginheight")),
                ..self.config
            };
            let page = match (&src, f) {
                (Src::Url(_), Some(f)) => {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        crate::with_top_site(&top, || {
                            Page::from_html(client, &f.html, &f.url, cfg, f.status, f.secure)
                        })
                    }));
                    match r {
                        Ok(mut p) => {
                            p.error = f.error;
                            p.encoding = f.encoding;
                            p
                        }
                        Err(_) => Page::from_html(
                            client,
                            &crate::snag_page_html(&f.url.href(), "internal error in frame"),
                            &f.url,
                            cfg,
                            0,
                            false,
                        ),
                    }
                }
                (Src::Doc(html), _) => Page::from_html(client, html, &self.base_url, cfg, 200, self.secure),
                (Src::Url(u), None) => Page::from_html(
                    client,
                    &crate::snag_page_html(&u.href(), "frame fetch failed"),
                    u,
                    cfg,
                    0,
                    false,
                ),
            };
            let name = self.doc.attr(node, "name").unwrap_or("").to_string();
            let scrolling =
                !self.doc.attr(node, "scrolling").is_some_and(|v| v.trim().eq_ignore_ascii_case("no"));
            self.frames.push(ChildFrame { node, name, rect, page: Box::new(page), scroll_y: 0.0, scrolling });
        }
    }

    /// The child frame under a page point, with the point in the child's page coordinates.
    pub(crate) fn frame_at(&self, x: f32, y: f32) -> Option<(usize, f32, f32)> {
        let inside = |f: &ChildFrame| {
            x >= f.rect.x && y >= f.rect.y && x < f.rect.x + f.rect.w && y < f.rect.y + f.rect.h
        };
        let idx = if self.frameset.is_some() {
            self.frames.iter().rposition(inside)?
        } else {
            // iframes: only when the iframe itself is the topmost hit.
            let hit = bb_layout::hit_test(&self.layout, x, y).node?;
            self.frames.iter().position(|f| f.node == hit && inside(f))?
        };
        let f = &self.frames[idx];
        Some((idx, x - f.rect.x, y - f.rect.y + f.scroll_y))
    }

    /// The image (`<img src>`) at a page point, descending into frames.
    pub fn image_at(&self, x: f32, y: f32) -> Option<Url> {
        if let Some((i, lx, ly)) = self.frame_at(x, y) {
            return self.frames[i].page.image_at(lx, ly);
        }
        let node = bb_layout::hit_test(&self.layout, x, y).node?;
        if !self.doc.is_html(node, "img") {
            return None;
        }
        let src = self.doc.attr(node, "src")?.trim();
        if src.is_empty() {
            return None;
        }
        self.base_url.join(src).ok()
    }

    /// Rects (page coordinates) of the top-level child frames.
    pub fn frame_rects(&self) -> Vec<Rect> {
        self.frames.iter().map(|f| f.rect).filter(|r| r.w > 0.0 && r.h > 0.0).collect()
    }

    /// The link at a page point, descending into frames.
    pub fn link_hit_at(&self, x: f32, y: f32) -> Option<LinkHit> {
        if let Some((i, lx, ly)) = self.frame_at(x, y) {
            let mut h = self.frames[i].page.link_hit_at(lx, ly)?;
            h.frame_path.insert(0, i);
            return Some(h);
        }
        let node = bb_layout::hit_test(&self.layout, x, y).node?;
        let (url, el) = self.link_for_node(node, x, y)?;
        let target = self
            .doc
            .attr(el, "target")
            .or_else(|| self.doc.elements_by_tag("base").into_iter().find_map(|b| self.doc.attr(b, "target")))
            .unwrap_or("")
            .trim()
            .to_string();
        Some(LinkHit { url, target, frame_path: Vec::new(), post: None })
    }

    fn find_frame_named(&self, name: &str, path: &mut Vec<usize>) -> bool {
        for (i, f) in self.frames.iter().enumerate() {
            path.push(i);
            if f.name == name || f.page.find_frame_named(name, path) {
                return true;
            }
            path.pop();
        }
        false
    }

    /// Follow a link hit: HTML §7.3.1.7 "the rules for choosing a navigable".
    pub fn follow_link(&mut self, client: &Client, hit: LinkHit) -> LinkDisposition {
        let t = hit.target.to_ascii_lowercase();
        let req = crate::NavRequest {
            url: hit.url.clone(),
            post: hit.post.clone(),
            cache_mode: Default::default(),
        };
        let path = match t.as_str() {
            "" | "_self" => hit.frame_path,
            "_parent" => {
                let mut p = hit.frame_path;
                p.pop();
                p
            }
            "_top" => Vec::new(),
            "_blank" => return LinkDisposition::NewTab(req),
            _ => {
                let mut p = Vec::new();
                if !self.find_frame_named(&hit.target, &mut p) {
                    return LinkDisposition::NewTab(req);
                }
                p
            }
        };
        if path.is_empty() {
            return LinkDisposition::Top(req);
        }
        self.navigate_frame_request(client, &path, &req);
        LinkDisposition::Frame
    }

    /// The URL of every child frame, recursively, as (path of indices into `frames`, URL),
    /// parents before their children. A session history entry stores this so Back/Forward can
    /// restore frame navigations (HTML §7.4 joint session history, simplified).
    pub fn frame_urls(&self) -> Vec<(Vec<usize>, Url)> {
        let mut out = Vec::new();
        for (i, f) in self.frames.iter().enumerate() {
            out.push((vec![i], f.page.url.clone()));
            for (mut sub, url) in f.page.frame_urls() {
                sub.insert(0, i);
                out.push((sub, url));
            }
        }
        out
    }

    fn frame_url_at(&self, path: &[usize]) -> Option<&Url> {
        let (&first, rest) = path.split_first()?;
        let f = self.frames.get(first)?;
        if rest.is_empty() {
            Some(&f.page.url)
        } else {
            f.page.frame_url_at(rest)
        }
    }

    /// Bring the frames back to a `frame_urls` snapshot, navigating only the frames whose URL
    /// differs (parents first, so their new documents' frames are then compared in turn).
    /// Returns whether any frame was navigated.
    pub fn restore_frames(&mut self, client: &Client, state: &[(Vec<usize>, Url)]) -> bool {
        let mut state: Vec<&(Vec<usize>, Url)> = state.iter().collect();
        state.sort_by_key(|(path, _)| path.len());
        let mut changed = false;
        for (path, url) in state {
            if self.frame_url_at(path) != Some(url) {
                changed |= self.navigate_frame(client, path, url);
            }
        }
        changed
    }

    /// Load `url` into the frame at `path`.
    pub fn navigate_frame(&mut self, client: &Client, path: &[usize], url: &Url) -> bool {
        self.navigate_frame_request(client, path, &crate::NavRequest::get(url.clone()))
    }

    /// Load a request (a URL or a form POST) into the frame at `path`.
    pub fn navigate_frame_request(
        &mut self,
        client: &Client,
        path: &[usize],
        req: &crate::NavRequest,
    ) -> bool {
        let top = crate::top_site(&self.url);
        crate::with_top_site(&top.clone(), || self.navigate_frame_in(client, path, req, &top))
    }

    fn navigate_frame_in(
        &mut self,
        client: &Client,
        path: &[usize],
        req: &crate::NavRequest,
        top: &Url,
    ) -> bool {
        let Some((&first, rest)) = path.split_first() else { return false };
        let Some(f) = self.frames.get_mut(first) else { return false };
        if !rest.is_empty() {
            return f.page.navigate_frame_in(client, rest, req, top);
        }
        let cfg = f.page.config;
        let url = &req.url;
        let page = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Page::load_in(client, url, cfg, Some(top), req.post.as_ref())
        }))
        .unwrap_or_else(|_| {
            Page::from_html(client, &crate::snag_page_html(&url.href(), "frame"), url, cfg, 0, false)
        });
        *f.page = page;
        f.scroll_y = 0.0;
        true
    }

    /// Scroll the innermost scrollable frame under a page point. Returns
    /// whether a frame consumed the scroll (otherwise the parent scrolls).
    pub fn scroll_frame_at(&mut self, x: f32, y: f32, dy: f32) -> bool {
        let Some((i, lx, ly)) = self.frame_at(x, y) else { return false };
        let f = &mut self.frames[i];
        if f.page.scroll_frame_at(lx, ly, dy) {
            return true;
        }
        if !f.scrolling {
            return false;
        }
        let max = (f.page.content_height() - f.rect.h).max(0.0);
        let ny = (f.scroll_y + dy).clamp(0.0, max);
        if (ny - f.scroll_y).abs() > 0.01 {
            f.scroll_y = ny;
            return true;
        }
        false
    }

    /// Paint this page (and its frames, recursively) onto `c`.
    pub(crate) fn paint_into(&self, c: &mut bb_paint::SoftwareCanvas, scroll: (f32, f32), scale: f32) {
        use bb_paint::Canvas;
        self.paint_document(c, scroll, scale);
        // Find-in-page highlights over the text (active match in the glitch cyan).
        for (i, m) in self.find_matches.iter().enumerate() {
            let col = if Some(i) == self.find_active {
                Color::rgba(0x3D, 0xF2, 0xE0, 0x90)
            } else {
                Color::rgba(0x7C, 0x5C, 0xFF, 0x55)
            };
            for r in m {
                let d =
                    Rect::new((r.x - scroll.0) * scale, (r.y - scroll.1) * scale, r.w * scale, r.h * scale);
                if d.bottom() >= 0.0 && d.y <= c.height() as f32 {
                    c.fill_rect(Rect::new(d.x.round(), d.y.round(), d.w.round().max(1.0), d.h.round()), col);
                }
            }
        }
    }

    fn paint_document(&self, c: &mut bb_paint::SoftwareCanvas, scroll: (f32, f32), scale: f32) {
        use bb_paint::Canvas;
        bb_paint::render_with(&self.display_list, c, scroll, scale, &mut |canvas, p| {
            let Some(f) = self.frames.iter().find(|f| f.node.0 == p.id) else { return };
            canvas.fill_rect(p.device_rect, f.page.display_list.background);
            f.page.paint_into(canvas, (p.scroll.0, p.scroll.1 + f.scroll_y), p.scale);
            // A thin scroll indicator for frames with overflowing content.
            let ch = f.page.content_height();
            if f.scrolling && ch > f.rect.h + 0.5 && f.rect.h > 0.0 {
                let r = p.device_rect;
                let w = 4.0 * p.scale;
                let th = (r.h * f.rect.h / ch).max(16.0 * p.scale).min(r.h);
                let max = (ch - f.rect.h).max(1.0);
                let ty = r.y + (r.h - th) * (f.scroll_y / max).clamp(0.0, 1.0);
                let thumb = Rect::new(r.x + r.w - w - 2.0 * p.scale, ty, w, th);
                let radii = bb_paint::Radii::uniform(w / 2.0);
                canvas.fill_path(
                    &bb_paint::Path::rounded_rect(thumb, radii),
                    &bb_paint::Paint::Solid(Color::rgba(0, 0, 0, 0x60)),
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_parse() {
        assert_eq!(
            parse_dimensions("100, 25%,*, 2*,"),
            vec![Dim::Absolute(100.0), Dim::Percent(25.0), Dim::Relative(1.0), Dim::Relative(2.0)]
        );
        assert_eq!(parse_dimensions("1.5*"), vec![Dim::Relative(1.5)]);
        assert_eq!(parse_dimensions("junk"), vec![Dim::Absolute(0.0)]);
    }

    #[test]
    fn dimensions_to_pixels() {
        let d = parse_dimensions("100,25%,*,2*");
        assert_eq!(to_pixels(&d, 1000.0), vec![100.0, 250.0, 650.0 / 3.0, 1300.0 / 3.0]);
        // Absolute overflow shrinks proportionally.
        assert_eq!(to_pixels(&parse_dimensions("600,400"), 500.0), vec![300.0, 200.0]);
        // No relative: scale up to fill.
        assert_eq!(to_pixels(&parse_dimensions("100,100"), 400.0), vec![200.0, 200.0]);
    }

    #[test]
    fn segments_cover_the_axis() {
        let s = segments(&parse_dimensions("150,*"), 0.0, 800.0, 6.0);
        assert_eq!(s, vec![(0.0, 150.0), (156.0, 644.0)]);
    }
}

//! Display list construction in CSS 2 Appendix E painting order (simplified:
//! backgrounds of blocks, floats, inline content, then positioned descendants
//! sorted by z-index; opacity creates a layer).

use crate::inline::first_baseline;
use crate::tree::{BoxId, BoxKind, Control, LayoutTree, LineItem, Replaced};
use bb_css::Rgba;
use bb_paint::display::linear_gradient_for;
use bb_paint::{
    BorderSide, BorderStyle as PBS, Color, DisplayList, GlyphRun, ImageData, Item, Paint, Path, Radii, Rect,
    Stop,
};
use bb_style::{
    BgRepeat, BgSize, BorderStyle, ComputedStyle, Float, Image, ListStyleType, Overflow, Position, Visibility,
};

pub fn color(c: Rgba) -> Color {
    Color::rgba(c.r, c.g, c.b, c.a)
}

fn pbs(s: BorderStyle) -> Option<PBS> {
    Some(match s {
        BorderStyle::None | BorderStyle::Hidden => return None,
        BorderStyle::Solid => PBS::Solid,
        BorderStyle::Dashed => PBS::Dashed,
        BorderStyle::Dotted => PBS::Dotted,
        BorderStyle::Double => PBS::Double,
        BorderStyle::Groove => PBS::Groove,
        BorderStyle::Ridge => PBS::Ridge,
        BorderStyle::Inset => PBS::Inset,
        BorderStyle::Outset => PBS::Outset,
    })
}

fn image_data(img: &bb_image::Image) -> ImageData {
    ImageData { width: img.width, height: img.height, pixels: img.frames[0].pixels.clone() }
}

struct Painter<'a> {
    tree: &'a LayoutTree,
    dl: DisplayList,
    /// Box whose background was propagated to the canvas (not painted again).
    propagated: Option<BoxId>,
}

pub fn build_display_list(tree: &LayoutTree) -> DisplayList {
    let mut p = Painter { tree, dl: DisplayList::default(), propagated: None };
    p.dl.background = color(tree.canvas_background);
    let root = tree.root;
    let rb = &tree.boxes[root];
    p.dl.width = rb.w.max(tree.viewport.0);
    p.dl.height = rb.content_bottom.max(rb.h).max(tree.viewport.1);
    // Canvas background images (CSS Backgrounds 3 §2.11.2): root element, else body.
    if let Some(&html) = rb.children.first() {
        let hs = &tree.boxes[html].style;
        let src = if hs.background_color.a == 0 && hs.background.is_empty() {
            tree.boxes[html].children.iter().copied().find(|&c| {
                tree.boxes[c].node.is_some()
                    && !tree.boxes[c].anonymous
                    && matches!(tree.boxes[c].kind, BoxKind::Block)
            })
        } else {
            Some(html)
        };
        if let Some(src) = src {
            p.propagated = Some(src);
            let area = Rect::new(0.0, 0.0, p.dl.width, p.dl.height);
            p.paint_bg_images(src, area, area, Radii::default());
        }
    }
    p.stacking_context(root, (0.0, 0.0));
    p.dl
}

fn radii_for(s: &ComputedStyle, w: f32, h: f32) -> Radii {
    let r = |i: usize, base: f32| s.border_radius[i].resolve(base).max(0.0);
    Radii { tl: (r(0, w), r(0, h)), tr: (r(1, w), r(1, h)), br: (r(2, w), r(2, h)), bl: (r(3, w), r(3, h)) }
}

impl<'a> Painter<'a> {
    fn creates_stacking(&self, id: BoxId) -> bool {
        let s = &self.tree.boxes[id].style;
        s.position != Position::Static || s.opacity < 1.0
    }

    fn stacking_context(&mut self, id: BoxId, off: (f32, f32)) {
        let b = &self.tree.boxes[id];
        let off = (off.0 + b.rel_offset.0, off.1 + b.rel_offset.1);
        let opacity = b.style.opacity;
        if opacity <= 0.0 {
            return;
        }
        if opacity < 1.0 {
            self.dl.push(Item::PushOpacity(opacity));
        }
        if !matches!(b.kind, BoxKind::Inline) {
            self.bg_border(id, off);
        }
        let mut positioned = Vec::new();
        self.collect_positioned(id, off, &mut positioned);
        positioned.sort_by_key(|(c, _)| self.tree.boxes[*c].style.z_index.unwrap_or(0));
        for &(c, o) in positioned.iter().filter(|(c, _)| self.tree.boxes[*c].style.z_index.unwrap_or(0) < 0) {
            self.stacking_context(c, o);
        }
        let clip = self.push_overflow_clip(id, off);
        self.walk_blocks(id, off);
        self.walk_floats(id, off);
        self.paint_own_inline(id, off);
        self.walk_inline(id, off);
        if clip {
            self.dl.push(Item::PopClip);
        }
        for &(c, o) in positioned.iter().filter(|(c, _)| self.tree.boxes[*c].style.z_index.unwrap_or(0) >= 0)
        {
            self.stacking_context(c, o);
        }
        if opacity < 1.0 {
            self.dl.push(Item::PopOpacity);
        }
    }

    fn collect_positioned(&self, id: BoxId, off: (f32, f32), out: &mut Vec<(BoxId, (f32, f32))>) {
        for &c in &self.tree.boxes[id].children {
            if self.creates_stacking(c) {
                out.push((c, off));
                continue;
            }
            let b = &self.tree.boxes[c];
            if b.style.float != Float::None || b.is_atomic_inline() {
                continue; // painted atomically
            }
            let o = (off.0 + b.rel_offset.0, off.1 + b.rel_offset.1);
            self.collect_positioned(c, o, out);
        }
    }

    fn push_overflow_clip(&mut self, id: BoxId, off: (f32, f32)) -> bool {
        let b = &self.tree.boxes[id];
        if matches!(b.style.overflow_x, Overflow::Visible) && matches!(b.style.overflow_y, Overflow::Visible)
        {
            return false;
        }
        if b.parent.is_none() {
            return false;
        }
        let (x, y, w, h) = b.padding_rect();
        let radii = radii_for(&b.style, b.w, b.h).shrink(b.border[0], b.border[1], b.border[2], b.border[3]);
        self.dl.push(Item::PushClip {
            rect: Rect::new(x + off.0, y + off.1, w, h),
            radii: if radii.is_zero() { None } else { Some(radii) },
        });
        true
    }

    fn skip_in_flow_walk(&self, c: BoxId) -> bool {
        let b = &self.tree.boxes[c];
        self.creates_stacking(c) || b.style.float != Float::None || b.is_atomic_inline()
    }

    fn walk_blocks(&mut self, id: BoxId, off: (f32, f32)) {
        let kids = self.tree.boxes[id].children.clone();
        for c in kids {
            if self.skip_in_flow_walk(c) {
                continue;
            }
            let b = &self.tree.boxes[c];
            let o = (off.0 + b.rel_offset.0, off.1 + b.rel_offset.1);
            if !matches!(b.kind, BoxKind::Inline | BoxKind::Text(_) | BoxKind::LineBreak) {
                self.bg_border(c, o);
            }
            let clip = self.push_overflow_clip(c, o);
            self.walk_blocks(c, o);
            if clip {
                self.dl.push(Item::PopClip);
            }
        }
    }

    fn walk_floats(&mut self, id: BoxId, off: (f32, f32)) {
        let kids = self.tree.boxes[id].children.clone();
        for c in kids {
            let b = &self.tree.boxes[c];
            if self.creates_stacking(c) || b.is_atomic_inline() {
                continue;
            }
            let o = (off.0 + b.rel_offset.0, off.1 + b.rel_offset.1);
            if b.style.float != Float::None {
                self.stacking_context(c, off);
                continue;
            }
            let clip = self.push_overflow_clip(c, o);
            self.walk_floats(c, o);
            if clip {
                self.dl.push(Item::PopClip);
            }
        }
    }

    fn walk_inline(&mut self, id: BoxId, off: (f32, f32)) {
        let kids = self.tree.boxes[id].children.clone();
        for c in kids {
            if self.skip_in_flow_walk(c) {
                continue;
            }
            let b = &self.tree.boxes[c];
            let o = (off.0 + b.rel_offset.0, off.1 + b.rel_offset.1);
            let clip = self.push_overflow_clip(c, o);
            self.paint_own_inline(c, o);
            self.walk_inline(c, o);
            if clip {
                self.dl.push(Item::PopClip);
            }
        }
    }

    /// Paint a box's own inline-level content: lines, markers, replaced content.
    fn paint_own_inline(&mut self, id: BoxId, off: (f32, f32)) {
        let b = &self.tree.boxes[id];
        if b.style.visibility == Visibility::Visible {
            if let BoxKind::Replaced(r) = &b.kind {
                self.paint_replaced(id, r, off);
            }
            if b.marker.is_some() {
                self.paint_marker(id, off);
            }
        }
        let lines = &self.tree.boxes[id].lines;
        if lines.is_empty() {
            return;
        }
        let lines = lines.clone();
        for line in &lines {
            for item in &line.items {
                match item {
                    LineItem::Span { id: sid, x0, x1, top, bottom, first, last } => {
                        let sb = &self.tree.boxes[*sid];
                        let s = &sb.style;
                        if s.visibility != Visibility::Visible {
                            continue;
                        }
                        let ro = (off.0 + sb.rel_offset.0, off.1 + sb.rel_offset.1);
                        let (l, r) = (
                            if *first { sb.border[3] + sb.padding[3] } else { 0.0 },
                            if *last { sb.border[1] + sb.padding[1] } else { 0.0 },
                        );
                        let rect = Rect::new(x0 + ro.0, top + ro.1, (x1 - x0).max(0.0), bottom - top);
                        let _ = (l, r);
                        if s.background_color.a > 0 {
                            self.dl.push(Item::Rect { rect, color: color(s.background_color) });
                        }
                        let side = |i: usize| -> Option<BorderSide> {
                            if (i == 3 && !first) || (i == 1 && !last) {
                                return None;
                            }
                            pbs(s.border_style[i]).map(|st| BorderSide {
                                width: sb.border[i],
                                color: color(s.border_color[i]),
                                style: st,
                            })
                        };
                        let sides = [side(0), side(1), side(2), side(3)];
                        if sides.iter().any(|s| s.is_some()) {
                            self.dl.push(Item::Border { rect, radii: Radii::default(), sides });
                        }
                    }
                    LineItem::Text { x, baseline, width, glyphs, style, owner, .. } => {
                        if style.visibility != Visibility::Visible || glyphs.glyphs.is_empty() {
                            continue;
                        }
                        let ob = &self.tree.boxes[*owner];
                        let ro = if matches!(ob.kind, BoxKind::Inline) {
                            (off.0 + ob.rel_offset.0, off.1 + ob.rel_offset.1)
                        } else {
                            off
                        };
                        let mut gx = x + ro.0;
                        let by = baseline + ro.1;
                        let mut gl = Vec::with_capacity(glyphs.glyphs.len());
                        for g in &glyphs.glyphs {
                            gl.push((g.font, g.id, gx + g.x_offset, by + g.y_offset));
                            gx += g.x_advance;
                        }
                        let c = color(style.color);
                        self.dl.push(Item::Text(GlyphRun {
                            glyphs: gl,
                            size: style.font_size,
                            color: c,
                            bold: glyphs.synthetic_bold,
                            italic: glyphs.synthetic_italic,
                        }));
                        self.decorations(*owner, x + ro.0, by, *width, style);
                    }
                    LineItem::Atomic { id: aid } => {
                        let ab = &self.tree.boxes[*aid];
                        if !self.creates_stacking(*aid) && ab.style.float == Float::None {
                            self.stacking_context(*aid, off);
                        }
                    }
                }
            }
        }
    }

    /// Text decorations propagated from the owner and its ancestors (CSS Text Decoration 3 §2).
    fn decorations(&mut self, owner: BoxId, x: f32, baseline: f32, width: f32, text_style: &ComputedStyle) {
        let mut cur = Some(owner);
        let mut decos: Vec<(u8, Rgba)> = Vec::new();
        let mut guard = 0;
        while let Some(c) = cur {
            guard += 1;
            if guard > 256 {
                break;
            }
            let b = &self.tree.boxes[c];
            let s = &b.style;
            let col = s.text_decoration_color.unwrap_or(s.color);
            if s.text_decoration_line.underline {
                decos.push((0, col));
            }
            if s.text_decoration_line.overline {
                decos.push((1, col));
            }
            if s.text_decoration_line.line_through {
                decos.push((2, col));
            }
            if b.is_atomic_inline()
                || b.style.float != Float::None
                || matches!(b.style.position, Position::Absolute | Position::Fixed)
                || matches!(b.kind, BoxKind::TableCell { .. })
            {
                break;
            }
            cur = b.parent;
        }
        if decos.is_empty() || width <= 0.0 {
            return;
        }
        let fl = crate::inline::font_list(text_style);
        let m = bb_text::FontSystem::lock().metrics(fl.primary());
        let size = text_style.font_size;
        let thick = (m.underline_thickness * size).max(1.0);
        for (kind, col) in decos {
            let y = match kind {
                0 => baseline + (m.underline_offset * size).max(1.0) + thick / 2.0,
                1 => baseline - m.ascent * size + thick / 2.0,
                _ => baseline - m.strikeout_offset * size,
            };
            self.dl.push(Item::Line {
                x0: x,
                y0: y,
                x1: x + width,
                y1: y,
                width: thick,
                color: color(col),
                dashed: false,
            });
        }
    }

    fn paint_marker(&mut self, id: BoxId, off: (f32, f32)) {
        let b = &self.tree.boxes[id];
        let Some(m) = b.marker.clone() else { return };
        let (cx, cy, _, _) = b.content_rect();
        let s = &m.style;
        let (asc, _, _, _) = crate::inline::metrics(s);
        let baseline = first_baseline(self.tree, id).unwrap_or(cy + asc);
        let fs = s.font_size;
        if let Some(img) = &m.image {
            let w = img.width as f32;
            let h = img.height as f32;
            let dst = Rect::new(cx - w - fs * 0.5 + off.0, baseline - h + off.1, w, h);
            self.dl.push(Item::Image { image: image_data(img), dst, clip: dst, opacity: 1.0, smooth: true });
            return;
        }
        let col = color(s.color);
        if let Some(bullet) = m.bullet {
            let d = (fs * 0.35).max(3.0);
            let r = d / 2.0;
            let mx = cx - fs * 0.55 - r + off.0;
            let my = baseline - fs * 0.3 + off.1;
            match bullet {
                ListStyleType::Disc => {
                    self.dl.push(Item::Path { path: Path::ellipse(mx, my, r, r), paint: Paint::Solid(col) })
                }
                ListStyleType::Circle => {
                    let mut p = Path::ellipse(mx, my, r, r);
                    p.add_rounded_rect(
                        Rect::new(mx - r + 1.0, my - r + 1.0, d - 2.0, d - 2.0),
                        Radii::uniform(r - 1.0),
                        true,
                    );
                    self.dl.push(Item::Path { path: p, paint: Paint::Solid(col) });
                }
                _ => self.dl.push(Item::Rect { rect: Rect::new(mx - r, my - r, d, d), color: col }),
            }
            return;
        }
        if m.text.is_empty() {
            return;
        }
        let shaped = crate::inline::shape(&m.text, s);
        let mut x = cx - shaped.width + off.0;
        let mut gl = Vec::new();
        for g in &shaped.glyphs {
            gl.push((g.font, g.id, x + g.x_offset, baseline + off.1 + g.y_offset));
            x += g.x_advance;
        }
        self.dl.push(Item::Text(GlyphRun {
            glyphs: gl,
            size: fs,
            color: col,
            bold: shaped.synthetic_bold,
            italic: shaped.synthetic_italic,
        }));
    }

    fn bg_border(&mut self, id: BoxId, off: (f32, f32)) {
        let b = &self.tree.boxes[id];
        let s = &b.style;
        if s.visibility != Visibility::Visible || b.parent.is_none() {
            return;
        }
        if b.w <= 0.0 && b.h <= 0.0 {
            return;
        }
        let rect = Rect::new(b.x + off.0, b.y + off.1, b.w, b.h);
        let radii = radii_for(s, b.w, b.h);
        // Outer box shadows.
        for sh in s.box_shadow.iter().rev().filter(|sh| !sh.inset) {
            let r = Rect::new(
                rect.x + sh.x - sh.spread,
                rect.y + sh.y - sh.spread,
                rect.w + 2.0 * sh.spread,
                rect.h + 2.0 * sh.spread,
            );
            self.dl.push(Item::Shadow {
                rect: r,
                radii,
                blur: sh.blur,
                color: color(sh.color),
                box_rect: rect,
                box_radii: radii,
            });
        }
        if self.propagated != Some(id) {
            if s.background_color.a > 0 {
                self.dl.push(Item::RoundedRect {
                    rect,
                    radii,
                    paint: Paint::Solid(color(s.background_color)),
                });
            }
            let (px, py, pw, ph) = b.padding_rect();
            let origin = Rect::new(px + off.0, py + off.1, pw, ph);
            self.paint_bg_images(id, rect, origin, radii);
        }
        // Borders.
        let side = |i: usize| -> Option<BorderSide> {
            if b.border[i] <= 0.0 {
                return None;
            }
            pbs(s.border_style[i]).map(|st| BorderSide {
                width: b.border[i],
                color: color(s.border_color[i]),
                style: st,
            })
        };
        let sides = [side(0), side(1), side(2), side(3)];
        if sides.iter().any(|x| x.is_some()) {
            self.dl.push(Item::Border { rect, radii, sides });
        }
        // Outline (drawn outside the border box).
        if s.outline_style.is_visible() && s.outline_width > 0.0 {
            let w = s.outline_width;
            let oc = color(s.outline_color.unwrap_or(s.color));
            let side =
                Some(BorderSide { width: w, color: oc, style: pbs(s.outline_style).unwrap_or(PBS::Solid) });
            self.dl.push(Item::Border {
                rect: Rect::new(rect.x - w, rect.y - w, rect.w + 2.0 * w, rect.h + 2.0 * w),
                radii: Radii::default(),
                sides: [side; 4],
            });
        }
    }

    /// Background image layers (bottom layer painted first).
    fn paint_bg_images(&mut self, id: BoxId, area: Rect, origin: Rect, radii: Radii) {
        let b = &self.tree.boxes[id];
        let s = b.style.clone();
        if s.background.is_empty() {
            return;
        }
        let clip_needed = !radii.is_zero();
        if clip_needed {
            self.dl.push(Item::PushClip { rect: area, radii: Some(radii) });
        }
        for (i, layer) in s.background.iter().enumerate().rev() {
            match &layer.image {
                Some(Image::Url(_)) => {
                    let Some(Some(img)) = b.bg_images.get(i) else { continue };
                    let (iw, ih) = (img.width as f32, img.height as f32);
                    let (tw, th) = match &layer.size {
                        BgSize::Cover => {
                            let k = (origin.w / iw).max(origin.h / ih);
                            (iw * k, ih * k)
                        }
                        BgSize::Contain => {
                            let k = (origin.w / iw).min(origin.h / ih);
                            (iw * k, ih * k)
                        }
                        BgSize::Auto(w, h) => {
                            let w = w.resolve(origin.w);
                            let h = h.resolve(origin.h);
                            match (w, h) {
                                (Some(w), Some(h)) => (w, h),
                                (Some(w), None) => (w, w * ih / iw.max(1.0)),
                                (None, Some(h)) => (h * iw / ih.max(1.0), h),
                                (None, None) => (iw, ih),
                            }
                        }
                    };
                    if tw <= 0.0 || th <= 0.0 {
                        continue;
                    }
                    let px = origin.x + layer.pos_x.resolve(origin.w - tw);
                    let py = origin.y + layer.pos_y.resolve(origin.h - th);
                    let rx = layer.repeat_x != BgRepeat::NoRepeat;
                    let ry = layer.repeat_y != BgRepeat::NoRepeat;
                    self.dl.push(Item::TiledImage {
                        image: image_data(img),
                        area,
                        tile: Rect::new(px, py, tw, th),
                        repeat_x: rx,
                        repeat_y: ry,
                    });
                }
                Some(Image::LinearGradient { angle, stops, repeating }) => {
                    let paint = linear_gradient_for(
                        area,
                        *angle,
                        resolve_stops(stops, area_len(area, *angle)),
                        *repeating,
                    );
                    self.dl.push(Item::Gradient { rect: area, radii, paint });
                }
                Some(Image::RadialGradient { stops, repeating }) => {
                    let (rx, ry) =
                        (area.w / 2.0 * std::f32::consts::SQRT_2, area.h / 2.0 * std::f32::consts::SQRT_2);
                    let paint = Paint::Radial {
                        cx: area.x + area.w / 2.0,
                        cy: area.y + area.h / 2.0,
                        rx,
                        ry,
                        stops: resolve_stops(stops, rx.max(ry)),
                        repeating: *repeating,
                    };
                    self.dl.push(Item::Gradient { rect: area, radii, paint });
                }
                None => {}
            }
        }
        if clip_needed {
            self.dl.push(Item::PopClip);
        }
    }

    fn paint_replaced(&mut self, id: BoxId, r: &Replaced, off: (f32, f32)) {
        let b = &self.tree.boxes[id];
        let (x, y, w, h) = b.content_rect();
        let content = Rect::new(x + off.0, y + off.1, w, h);
        let s = b.style.clone();
        match r {
            Replaced::Image { image: Some(img), .. } => {
                let dst = object_fit(content, img.width as f32, img.height as f32, s.object_fit);
                self.dl.push(Item::Image {
                    image: image_data(img),
                    dst,
                    clip: content,
                    opacity: 1.0,
                    smooth: true,
                });
            }
            Replaced::Image { image: None, alt, broken, .. } => {
                if *broken {
                    if let Some(a) = alt.as_deref().filter(|a| !a.trim().is_empty()) {
                        if w > 0.0 && h > 0.0 {
                            self.dl.push(Item::PushClip { rect: content, radii: None });
                            self.text_at(a, &s, content.x + 2.0, content.y, s.color);
                            self.dl.push(Item::PopClip);
                        }
                    } else if w >= 16.0 && h >= 16.0 {
                        let side = Some(BorderSide {
                            width: 1.0,
                            color: Color::rgb(0xa0, 0xa0, 0xa0),
                            style: PBS::Inset,
                        });
                        self.dl.push(Item::Border {
                            rect: content,
                            radii: Radii::default(),
                            sides: [side; 4],
                        });
                    }
                } else if w > 0.0 && h > 0.0 {
                    // Loading placeholder: a faint frame.
                    self.dl.push(Item::Rect { rect: content, color: Color::rgba(128, 128, 128, 24) });
                }
            }
            Replaced::Control(c) => self.paint_control(c, content, &s),
            Replaced::Svg { .. } => {}
            Replaced::Frame { .. } => {
                if let Some(node) = b.node {
                    self.dl.push(Item::SubDocument { rect: content, id: node.0 });
                }
            }
            Replaced::Placeholder { label, .. } => {
                self.dl.push(Item::Rect { rect: content, color: Color::rgba(128, 128, 128, 30) });
                if !label.is_empty() {
                    self.dl.push(Item::PushClip { rect: content, radii: None });
                    self.text_at(label, &s, content.x + 6.0, content.y + 4.0, Rgba::rgb(110, 110, 110));
                    self.dl.push(Item::PopClip);
                }
            }
        }
    }

    fn text_at(&mut self, text: &str, s: &ComputedStyle, x: f32, top: f32, col: Rgba) {
        let shaped = crate::inline::shape(text, s);
        let (a, _) = crate::inline::inline_extents(s);
        let mut gx = x;
        let mut gl = Vec::new();
        for g in &shaped.glyphs {
            gl.push((g.font, g.id, gx + g.x_offset, top + a + g.y_offset));
            gx += g.x_advance;
        }
        self.dl.push(Item::Text(GlyphRun {
            glyphs: gl,
            size: s.font_size,
            color: color(col),
            bold: shaped.synthetic_bold,
            italic: shaped.synthetic_italic,
        }));
    }

    fn paint_control(&mut self, c: &Control, r: Rect, s: &ComputedStyle) {
        let (a, d) = crate::inline::inline_extents(s);
        let line_top = r.y + ((r.h - (a + d)) / 2.0).max(0.0);
        match c {
            Control::TextField { value, placeholder, password } => {
                let text = if *password { "•".repeat(value.chars().count()) } else { value.clone() };
                let col = if *placeholder { Rgba::rgb(0x75, 0x75, 0x75) } else { s.color };
                self.dl.push(Item::PushClip { rect: r, radii: None });
                self.text_at(&text, s, r.x, line_top, col);
                self.dl.push(Item::PopClip);
            }
            Control::TextArea { value } => {
                self.dl.push(Item::PushClip { rect: r, radii: None });
                let lh = a + d;
                for (i, line) in value.trim_start_matches('\n').lines().enumerate() {
                    self.text_at(line, s, r.x, r.y + i as f32 * lh, s.color);
                }
                self.dl.push(Item::PopClip);
            }
            Control::Button { label } => {
                let w = crate::inline::shape(label, s).width;
                self.text_at(label, s, r.x + ((r.w - w) / 2.0).max(0.0), line_top, s.color);
            }
            Control::Checkbox { checked, radio } => {
                if *checked {
                    let accent = Color::rgb(0x7c, 0x5c, 0xff);
                    if *radio {
                        let rr = r.w.min(r.h) / 4.0;
                        self.dl.push(Item::Path {
                            path: Path::ellipse(r.x + r.w / 2.0, r.y + r.h / 2.0, rr, rr),
                            paint: Paint::Solid(accent),
                        });
                    } else {
                        self.dl.push(Item::RoundedRect {
                            rect: r,
                            radii: Radii::uniform(2.0),
                            paint: Paint::Solid(accent),
                        });
                        let mut p = Path::new();
                        p.move_to(r.x + r.w * 0.22, r.y + r.h * 0.52)
                            .line_to(r.x + r.w * 0.42, r.y + r.h * 0.72)
                            .line_to(r.x + r.w * 0.78, r.y + r.h * 0.3);
                        self.dl.push(Item::Path {
                            path: p.stroke((r.w / 7.0).max(1.5)),
                            paint: Paint::Solid(Color::WHITE),
                        });
                    }
                }
            }
            Control::Select { label } => {
                self.dl.push(Item::PushClip { rect: r, radii: None });
                self.text_at(label, s, r.x, line_top, s.color);
                self.dl.push(Item::PopClip);
                let ax = r.right() - 10.0;
                let ay = r.y + r.h / 2.0;
                let mut p = Path::new();
                p.move_to(ax - 4.0, ay - 2.0).line_to(ax, ay + 2.0).line_to(ax + 4.0, ay - 2.0);
                self.dl.push(Item::Path { path: p.stroke(1.5), paint: Paint::Solid(color(s.color)) });
            }
        }
    }
}

fn area_len(r: Rect, angle: f32) -> f32 {
    let a = angle.to_radians();
    (r.w * a.sin()).abs() + (r.h * a.cos()).abs()
}

/// Resolve gradient stop positions (CSS Images 3 §3.5.3: fixup).
fn resolve_stops(stops: &[bb_style::GradientStop], len: f32) -> Vec<Stop> {
    let n = stops.len();
    let mut pos: Vec<Option<f32>> =
        stops.iter().map(|s| s.pos.as_ref().map(|p| p.resolve(len) / len.max(1e-3))).collect();
    if pos.first().is_some_and(|p| p.is_none()) {
        pos[0] = Some(0.0);
    }
    if pos.last().is_some_and(|p| p.is_none()) {
        pos[n - 1] = Some(1.0);
    }
    let mut maxp: f32 = 0.0;
    for p in pos.iter_mut().flatten() {
        if *p < maxp {
            *p = maxp;
        }
        maxp = *p;
    }
    let mut i = 0;
    while i < n {
        if pos[i].is_none() {
            let start = i - 1;
            let mut end = i;
            while pos[end].is_none() {
                end += 1;
            }
            let (a, b) = (pos[start].unwrap_or(0.0), pos[end].unwrap_or(1.0));
            for (k, slot) in pos.iter_mut().enumerate().take(end).skip(i) {
                *slot = Some(a + (b - a) * (k - start) as f32 / (end - start) as f32);
            }
            i = end;
        }
        i += 1;
    }
    stops.iter().zip(pos).map(|(s, p)| Stop { offset: p.unwrap_or(0.0), color: color(s.color) }).collect()
}

fn object_fit(content: Rect, iw: f32, ih: f32, fit: bb_style::ObjectFit) -> Rect {
    use bb_style::ObjectFit as F;
    let scale = match fit {
        F::Fill => return content,
        F::Contain => (content.w / iw).min(content.h / ih),
        F::Cover => (content.w / iw).max(content.h / ih),
        F::None => 1.0,
        F::ScaleDown => (content.w / iw).min(content.h / ih).min(1.0),
    };
    let (w, h) = (iw * scale, ih * scale);
    Rect::new(content.x + (content.w - w) / 2.0, content.y + (content.h - h) / 2.0, w, h)
}

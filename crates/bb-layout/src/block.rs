//! Block formatting (CSS 2 §9.4.1, §10.3, §10.6, §8.3.1 margin collapsing),
//! floats (§9.5) and positioned boxes (§10.3.7 / §10.6.4).

use crate::tree::{BoxId, BoxKind, LayoutTree, Replaced};
use bb_style::{BoxSizing, Clear, ComputedStyle, Display, Float, Overflow, Position, TextAlign, LP, LPA};

/// A placed float (margin box, absolute coordinates).
#[derive(Debug, Clone, Copy)]
pub struct FloatBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub left: bool,
}

/// Block formatting context state: the floats placed so far.
#[derive(Debug, Default)]
pub struct Bfc {
    pub floats: Vec<FloatBox>,
}

impl Bfc {
    /// Available [left, right] edges for content spanning `y0..y1` within [x0, x1].
    pub fn band(&self, y0: f32, y1: f32, x0: f32, x1: f32) -> (f32, f32) {
        let (mut l, mut r) = (x0, x1);
        for f in &self.floats {
            if f.y < y1.max(y0 + 0.01) && f.y + f.h > y0 && f.h > 0.0 {
                if f.left {
                    l = l.max(f.x + f.w);
                } else {
                    r = r.min(f.x);
                }
            }
        }
        (l, r.max(l))
    }

    /// The y below which `clear` is satisfied.
    pub fn clear_y(&self, clear: Clear) -> f32 {
        let mut y = f32::MIN;
        for f in &self.floats {
            let applies = match clear {
                Clear::Left => f.left,
                Clear::Right => !f.left,
                Clear::Both => true,
                Clear::None => false,
            };
            if applies {
                y = y.max(f.y + f.h);
            }
        }
        y
    }

    pub fn bottom(&self) -> f32 {
        self.floats.iter().map(|f| f.y + f.h).fold(f32::MIN, f32::max)
    }

    /// Place a float of margin-box size `w × h` at or below `y` (CSS 2 §9.5.1 rules).
    pub fn place(&mut self, w: f32, h: f32, left: bool, y: f32, x0: f32, x1: f32) -> (f32, f32) {
        let mut y = y;
        for _ in 0..200 {
            let (l, r) = self.band(y, y + h.max(0.01), x0, x1);
            if r - l + 0.01 >= w || (l <= x0 + 0.01 && r >= x1 - 0.01) {
                let x = if left { l } else { r - w };
                self.floats.push(FloatBox { x, y, w, h, left });
                return (x, y);
            }
            // Move down to the next float bottom.
            let next = self
                .floats
                .iter()
                .filter(|f| f.y + f.h > y + 0.01 && f.y < y + h.max(0.01))
                .map(|f| f.y + f.h)
                .fold(f32::MAX, f32::min);
            if next == f32::MAX {
                break;
            }
            y = next;
        }
        let x = if left { x0 } else { x1 - w };
        self.floats.push(FloatBox { x, y, w, h, left });
        (x, y)
    }
}

/// A pair of collapsible margins (largest positive, most negative).
#[derive(Debug, Clone, Copy, Default)]
pub struct Margins {
    pos: f32,
    neg: f32,
}

impl Margins {
    pub fn of(m: f32) -> Margins {
        let mut x = Margins::default();
        x.add(m);
        x
    }
    pub fn add(&mut self, m: f32) {
        if m >= 0.0 {
            self.pos = self.pos.max(m);
        } else {
            self.neg = self.neg.min(m);
        }
    }
    pub fn merge(&mut self, o: Margins) {
        self.pos = self.pos.max(o.pos);
        self.neg = self.neg.min(o.neg);
    }
    pub fn value(&self) -> f32 {
        self.pos + self.neg
    }
}

pub struct Layout<'a> {
    pub tree: &'a mut LayoutTree,
    /// Absolutely positioned boxes awaiting layout: (box, containing block box).
    pub abs_queue: Vec<BoxId>,
}

pub fn lp(v: &LP, base: f32) -> f32 {
    v.resolve(base)
}

pub fn lpa(v: &LPA, base: f32) -> Option<f32> {
    v.resolve(base)
}

impl<'a> Layout<'a> {
    pub fn style(&self, id: BoxId) -> &ComputedStyle {
        &self.tree.boxes[id].style
    }

    /// Resolve padding and border widths (margins are resolved separately).
    pub fn resolve_box_model(&mut self, id: BoxId, cb_w: f32) {
        let s = self.tree.boxes[id].style.clone();
        let b = &mut self.tree.boxes[id];
        for i in 0..4 {
            b.padding[i] = lp(&s.padding[i], cb_w).max(0.0);
            b.border[i] = s.border_px(i);
            b.margin[i] = lpa(&s.margin[i], cb_w).unwrap_or(0.0);
        }
        // Table cells / rows: margins don't apply.
        if matches!(b.kind, BoxKind::TableCell { .. } | BoxKind::TableRow | BoxKind::TableRowGroup) {
            b.margin = [0.0; 4];
        }
    }

    pub fn horiz_extra(&self, id: BoxId) -> f32 {
        let b = &self.tree.boxes[id];
        b.padding[1] + b.padding[3] + b.border[1] + b.border[3]
    }

    pub fn vert_extra(&self, id: BoxId) -> f32 {
        let b = &self.tree.boxes[id];
        b.padding[0] + b.padding[2] + b.border[0] + b.border[2]
    }

    /// Specified width as border-box width, if definite.
    pub fn specified_border_width(&self, id: BoxId, cb_w: f32) -> Option<f32> {
        let s = self.style(id);
        let w = lpa(&s.width, cb_w)?;
        Some(match s.box_sizing {
            BoxSizing::BorderBox => w.max(self.horiz_extra(id)),
            BoxSizing::ContentBox => w + self.horiz_extra(id),
        })
    }

    fn clamp_width(&self, id: BoxId, w: f32, cb_w: f32) -> f32 {
        let s = self.style(id);
        let extra = if s.box_sizing == BoxSizing::ContentBox { self.horiz_extra(id) } else { 0.0 };
        let mut w = w;
        if let Some(mx) = &s.max_width {
            w = w.min(lp(mx, cb_w) + extra);
        }
        if let Some(mn) = lpa(&s.min_width, cb_w) {
            w = w.max(mn + extra);
        }
        w.max(self.horiz_extra(id))
    }

    pub fn clamp_height(&self, id: BoxId, h: f32, cb_h: Option<f32>) -> f32 {
        let s = self.style(id);
        let extra = if s.box_sizing == BoxSizing::ContentBox { self.vert_extra(id) } else { 0.0 };
        let mut h = h;
        if let Some(mx) = &s.max_height {
            if !mx.has_percent() || cb_h.is_some() {
                h = h.min(lp(mx, cb_h.unwrap_or(0.0)) + extra);
            }
        }
        if !s.min_height.has_percent() || cb_h.is_some() {
            if let Some(mn) = lpa(&s.min_height, cb_h.unwrap_or(0.0)) {
                h = h.max(mn + extra);
            }
        }
        h.max(self.vert_extra(id))
    }

    /// Specified height as border-box height, if definite.
    pub fn specified_border_height(&self, id: BoxId, cb_h: Option<f32>) -> Option<f32> {
        let s = self.style(id);
        if s.height.has_percent() && cb_h.is_none() {
            return None;
        }
        let h = lpa(&s.height, cb_h.unwrap_or(0.0))?;
        Some(match s.box_sizing {
            BoxSizing::BorderBox => h.max(self.vert_extra(id)),
            BoxSizing::ContentBox => h + self.vert_extra(id),
        })
    }

    /// Whether `id` establishes a new block formatting context (CSS 2 §9.4.1).
    pub fn is_bfc_root(&self, id: BoxId) -> bool {
        let b = &self.tree.boxes[id];
        let s = &b.style;
        b.parent.is_none()
            || s.float != Float::None
            || matches!(s.position, Position::Absolute | Position::Fixed)
            || !matches!(s.overflow_x, Overflow::Visible | Overflow::Clip)
            || matches!(
                s.display,
                Display::FlowRoot
                    | Display::InlineBlock
                    | Display::TableCell
                    | Display::TableCaption
                    | Display::Flex
                    | Display::InlineFlex
                    | Display::Grid
            )
            || matches!(
                b.kind,
                BoxKind::InlineBlock
                    | BoxKind::TableCell { .. }
                    | BoxKind::TableCaption
                    | BoxKind::Table
                    | BoxKind::Flex
                    | BoxKind::Replaced(_)
            )
            || self.tree.boxes[b.parent.unwrap_or(0)].parent.is_none() // the root element
    }

    /// Does this block container hold inline-level content?
    pub fn has_inline_content(&self, id: BoxId) -> bool {
        let b = &self.tree.boxes[id];
        !b.children.is_empty()
            && b.children
                .iter()
                .all(|&c| self.tree.boxes[c].is_inline_level() || self.tree.boxes[c].is_out_of_flow())
            && b.children.iter().any(|&c| self.tree.boxes[c].is_inline_level())
    }

    fn adjoins_top(&self, id: BoxId) -> bool {
        let b = &self.tree.boxes[id];
        matches!(b.kind, BoxKind::Block)
            && !self.is_bfc_root(id)
            && b.border[0] == 0.0
            && b.padding[0] == 0.0
            && !self.has_inline_content(id)
    }

    fn adjoins_bottom(&self, id: BoxId) -> bool {
        let b = &self.tree.boxes[id];
        matches!(b.kind, BoxKind::Block)
            && !self.is_bfc_root(id)
            && b.border[2] == 0.0
            && b.padding[2] == 0.0
            && b.style.height.is_auto()
            && !self.has_inline_content(id)
    }

    fn first_in_flow_block(&self, id: BoxId) -> Option<BoxId> {
        self.tree.boxes[id].children.iter().copied().find(|&c| !self.tree.boxes[c].is_out_of_flow())
    }

    /// Top margin of `id` combined with first-child margins it collapses with.
    fn collapsible_top(&mut self, id: BoxId, cb_w: f32) -> Margins {
        self.resolve_box_model(id, cb_w);
        let mut m = Margins::of(self.tree.boxes[id].margin[0]);
        if self.adjoins_top(id) {
            if let Some(f) = self.first_in_flow_block(id) {
                if !self.tree.boxes[f].is_inline_level() && self.style(f).clear == Clear::None {
                    let inner_w = (cb_w - self.horiz_extra(id)).max(0.0);
                    m.merge(self.collapsible_top(f, inner_w));
                }
            }
        }
        m
    }

    /// CSS 2 §10.3.3: used width and horizontal margins of a block-level box in normal flow.
    fn block_width(&mut self, id: BoxId, cb_w: f32) -> (f32, f32, f32) {
        let s = self.tree.boxes[id].style.clone();
        let extra = self.horiz_extra(id);
        let ml_auto = s.margin[3].is_auto();
        let mr_auto = s.margin[1].is_auto();
        let ml = lpa(&s.margin[3], cb_w).unwrap_or(0.0);
        let mr = lpa(&s.margin[1], cb_w).unwrap_or(0.0);
        let is_table = matches!(self.tree.boxes[id].kind, BoxKind::Table);
        let replaced = matches!(self.tree.boxes[id].kind, BoxKind::Replaced(_));
        let mut w = match self.specified_border_width(id, cb_w) {
            Some(w) => self.clamp_width(id, w, cb_w),
            None if is_table || replaced => {
                // Tables and replaced elements shrink-to-fit.
                let (min, max) = crate::intrinsic::intrinsic_widths(self, id);
                let avail = (cb_w - ml - mr).max(0.0);
                self.clamp_width(id, max.min(avail).max(min), cb_w)
            }
            None => self.clamp_width(id, (cb_w - ml - mr).max(extra), cb_w),
        };
        if is_table {
            // A table is never narrower than its minimum content width.
            let (min, _) = crate::intrinsic::intrinsic_widths(self, id);
            w = w.max(min);
        }
        let remaining = cb_w - w;
        let (ml, mr) = if ml_auto && mr_auto {
            let half = (remaining / 2.0).max(0.0);
            (half, half)
        } else if ml_auto {
            ((remaining - mr).max(0.0), mr)
        } else {
            // margin-right auto, or over-constrained: margin-right absorbs (ltr).
            (ml, remaining - ml)
        };
        (w, ml, mr)
    }

    /// Lay out the in-flow block-level box `id` with its border box at
    /// (`x`, `y`) inside a containing block of width `cb_w`. Returns the
    /// bottom margin to collapse with the following content.
    #[allow(clippy::too_many_arguments)]
    pub fn layout_block(
        &mut self,
        id: BoxId,
        cb_x: f32,
        cb_w: f32,
        cb_h: Option<f32>,
        y: f32,
        bfc: &mut Bfc,
        first_top_merged: bool,
    ) -> Margins {
        let (w, ml, _mr) = self.block_width(id, cb_w);
        let parent_align = self.tree.boxes[id].parent.map(|p| self.style(p).text_align);
        let mut x = cb_x + ml;
        // HTML legacy alignment: <center> / align=center center block children
        // whose margins are not specified (approximated: margins resolve to 0).
        if let Some(TextAlign::LegacyCenter | TextAlign::LegacyRight) = parent_align {
            let s = self.style(id);
            let zero_margins = !s.margin[3].is_auto()
                && !s.margin[1].is_auto()
                && lpa(&s.margin[3], cb_w) == Some(0.0)
                && lpa(&s.margin[1], cb_w) == Some(0.0);
            if zero_margins && w < cb_w {
                x = if parent_align == Some(TextAlign::LegacyCenter) {
                    cb_x + (cb_w - w) / 2.0
                } else {
                    cb_x + cb_w - w
                };
            }
        }
        {
            let b = &mut self.tree.boxes[id];
            b.x = x;
            b.y = y;
            b.w = w;
        }
        self.layout_contents(id, cb_h, bfc, first_top_merged)
    }

    /// Lay out the contents of a positioned box (x, y, w already set). Returns bottom margins out.
    pub fn layout_contents(
        &mut self,
        id: BoxId,
        cb_h: Option<f32>,
        bfc: &mut Bfc,
        first_top_merged: bool,
    ) -> Margins {
        let kind = self.tree.boxes[id].kind.clone();
        let spec_h = self.specified_border_height(id, cb_h);
        let (cx, cy, cw, _) = self.tree.boxes[id].content_rect_with_w();
        let content_h_for_children = spec_h.map(|h| (h - self.vert_extra(id)).max(0.0));
        let own_bfc = self.is_bfc_root(id);
        let mut local = Bfc::default();
        let bfc_ref: &mut Bfc = if own_bfc { &mut local } else { bfc };
        let mut bottom_out = Margins::default();
        let content_h = match kind {
            BoxKind::Table => {
                let h = crate::table::layout_table(self, id, cb_h);
                self.tree.boxes[id].h = h;
                self.tree.boxes[id].laid_out = true;
                return Margins::of(self.tree.boxes[id].margin[2]);
            }
            BoxKind::Replaced(_) => {
                let cb_w = self.tree.boxes[id]
                    .parent
                    .map(|p| self.tree.boxes[p].content_rect_with_w().2)
                    .unwrap_or(0.0);
                let used = self.tree.boxes[id].w;
                let (_, h) = crate::intrinsic::replaced_size(self, id, Some(used), cb_w);
                h - self.vert_extra(id)
            }
            BoxKind::Flex => crate::flex::layout_flex(self, id, cx, cy, cw, content_h_for_children),
            _ => {
                if self.has_inline_content(id) {
                    crate::inline::layout_inline(self, id, cx, cy, cw, bfc_ref)
                } else {
                    let (h, out) = self.layout_block_children(
                        id,
                        cx,
                        cy,
                        cw,
                        content_h_for_children,
                        bfc_ref,
                        first_top_merged,
                    );
                    if self.adjoins_bottom(id) {
                        bottom_out = out;
                        h
                    } else {
                        h + out.value()
                    }
                }
            }
        };
        let mut content_h = content_h;
        if own_bfc {
            // CSS 2 §10.6.7: BFC roots grow to contain their floats.
            let fb = bfc_ref.bottom();
            if fb > cy + content_h {
                content_h = fb - cy;
            }
        }
        let border_h = match spec_h {
            Some(h) => h,
            None => content_h + self.vert_extra(id),
        };
        let h = self.clamp_height(id, border_h, cb_h);
        let b = &mut self.tree.boxes[id];
        b.h = h;
        b.laid_out = true;
        b.content_bottom = b.content_bottom.max(b.y + h);
        let mut out = Margins::of(b.margin[2]);
        out.merge(bottom_out);
        out
    }

    /// Lay out block-level children of `id`. Returns (content height, pending bottom margins).
    #[allow(clippy::too_many_arguments)]
    pub fn layout_block_children(
        &mut self,
        id: BoxId,
        cx: f32,
        cy: f32,
        cw: f32,
        ch: Option<f32>,
        bfc: &mut Bfc,
        first_top_merged: bool,
    ) -> (f32, Margins) {
        let kids = self.tree.boxes[id].children.clone();
        let mut cursor = cy;
        let mut pending = Margins::default();
        let mut first = true;
        let mut max_bottom = cy;
        for c in kids {
            let cs = self.tree.boxes[c].style.clone();
            if matches!(cs.position, Position::Absolute | Position::Fixed) {
                self.tree.boxes[c].static_pos = (cx, cursor + pending.value());
                self.abs_queue.push(c);
                continue;
            }
            if cs.float != Float::None {
                let y = cursor + pending.value();
                self.layout_float(c, cx, cw, ch, y, bfc);
                continue;
            }
            if self.tree.boxes[c].is_inline_level() {
                // Should have been wrapped in an anonymous block; lay out defensively.
                continue;
            }
            let top = self.collapsible_top(c, cw);
            if !(first && first_top_merged) {
                pending.merge(top);
            }
            let mut y = cursor + pending.value();
            if cs.clear != Clear::None {
                let cl = bfc.clear_y(cs.clear);
                if cl > y {
                    y = cl;
                    pending = Margins::default();
                }
            }
            let merged = self.adjoins_top(c);
            // BFC roots must not overlap floats: shrink into the available band.
            let (bx, bw) = if self.is_bfc_root(c) && !bfc.floats.is_empty() {
                let est_h = 1.0;
                let (l, r) = bfc.band(y, y + est_h, cx, cx + cw);
                (l, r - l)
            } else {
                (cx, cw)
            };
            let out = self.layout_block(c, bx, bw, ch, y, bfc, merged);
            let b = &self.tree.boxes[c];
            let collapses_through = b.h <= 0.0
                && self.vert_extra(c) == 0.0
                && b.lines.iter().all(|l| l.items.is_empty())
                && b.style.height.is_auto()
                && !matches!(b.kind, BoxKind::Table | BoxKind::Replaced(_));
            if collapses_through {
                pending.merge(out);
            } else {
                cursor = y + b.h;
                pending = out;
            }
            max_bottom = max_bottom.max(self.tree.boxes[c].content_bottom);
            first = false;
        }
        let b = &mut self.tree.boxes[id];
        b.content_bottom = b.content_bottom.max(max_bottom);
        (cursor - cy, pending)
    }

    /// Shrink-to-fit width (CSS 2 §10.3.5).
    pub fn shrink_to_fit(&mut self, id: BoxId, avail: f32) -> f32 {
        let (min, max) = crate::intrinsic::intrinsic_widths(self, id);
        max.min(avail).max(min)
    }

    /// Lay out and place a float child. `y` is the current line/flow position.
    pub fn layout_float(&mut self, id: BoxId, cx: f32, cw: f32, ch: Option<f32>, y: f32, bfc: &mut Bfc) {
        self.resolve_box_model(id, cw);
        let (ml, mr, mt, mb) = {
            let b = &self.tree.boxes[id];
            (b.margin[3], b.margin[1], b.margin[0], b.margin[2])
        };
        let w = match self.specified_border_width(id, cw) {
            Some(w) => self.clamp_width(id, w, cw),
            None => {
                if matches!(self.tree.boxes[id].kind, BoxKind::Replaced(_)) {
                    crate::intrinsic::replaced_size(self, id, None, cw).0
                } else {
                    let stf = self.shrink_to_fit(id, (cw - ml - mr).max(0.0));
                    self.clamp_width(id, stf, cw)
                }
            }
        };
        let cs = self.tree.boxes[id].style.clone();
        let mut y0 = y;
        if cs.clear != Clear::None {
            y0 = y0.max(bfc.clear_y(cs.clear));
        }
        // Lay out at a provisional position to learn the height, then move.
        {
            let b = &mut self.tree.boxes[id];
            b.x = cx + ml;
            b.y = y0 + mt;
            b.w = w;
        }
        let mut inner = Bfc::default();
        self.layout_contents(id, ch, &mut inner, false);
        let h = self.tree.boxes[id].h;
        let (fx, fy) = bfc.place(w + ml + mr, h + mt + mb, cs.float == Float::Left, y0, cx, cx + cw);
        let (dx, dy) = (fx + ml - self.tree.boxes[id].x, fy + mt - self.tree.boxes[id].y);
        crate::translate(self.tree, id, dx, dy);
    }

    /// CSS 2 §10.3.7 / §10.6.4: absolutely positioned boxes.
    pub fn layout_absolute(&mut self, id: BoxId) {
        let s = self.tree.boxes[id].style.clone();
        // Containing block: nearest positioned ancestor's padding box, else the ICB.
        let mut cb = None;
        let mut p = self.tree.boxes[id].parent;
        while let Some(pp) = p {
            if self.tree.boxes[pp].style.position != Position::Static && s.position != Position::Fixed {
                cb = Some(pp);
                break;
            }
            p = self.tree.boxes[pp].parent;
        }
        let (cbx, cby, cbw, cbh) = match cb {
            Some(c) => {
                let (x, y, w, h) = self.tree.boxes[c].padding_rect();
                (x + self.tree.boxes[c].rel_offset.0, y + self.tree.boxes[c].rel_offset.1, w, h)
            }
            None => (0.0, 0.0, self.tree.viewport.0, self.tree.viewport.1),
        };
        self.resolve_box_model(id, cbw);
        let (sx, sy) = self.tree.boxes[id].static_pos;
        let left = lpa(&s.left, cbw);
        let right = lpa(&s.right, cbw);
        let top = lpa(&s.top, cbh);
        let bottom = lpa(&s.bottom, cbh);
        let (ml, mr, mt, mb) = {
            let b = &self.tree.boxes[id];
            (b.margin[3], b.margin[1], b.margin[0], b.margin[2])
        };
        let w = match self.specified_border_width(id, cbw) {
            Some(w) => self.clamp_width(id, w, cbw),
            None => {
                if let (Some(l), Some(r)) = (left, right) {
                    self.clamp_width(id, (cbw - l - r - ml - mr).max(0.0), cbw)
                } else if matches!(self.tree.boxes[id].kind, BoxKind::Replaced(_)) {
                    crate::intrinsic::replaced_size(self, id, None, cbw).0
                } else {
                    let avail = (cbw - left.unwrap_or(0.0) - right.unwrap_or(0.0) - ml - mr).max(0.0);
                    let stf = self.shrink_to_fit(id, avail);
                    self.clamp_width(id, stf, cbw)
                }
            }
        };
        let x = match (left, right) {
            (Some(l), _) => cbx + l + ml,
            (None, Some(r)) => cbx + cbw - r - mr - w,
            (None, None) => sx + ml,
        };
        {
            let b = &mut self.tree.boxes[id];
            b.x = x;
            b.y = top.map(|t| cby + t + mt).unwrap_or(sy + mt);
            b.w = w;
        }
        let mut bfc = Bfc::default();
        let stretch_h = if let (Some(t), Some(bt)) = (top, bottom) {
            Some((cbh - t - bt - mt - mb).max(0.0))
        } else {
            None
        };
        self.layout_contents(id, Some(cbh), &mut bfc, false);
        if s.height.is_auto() {
            if let Some(h) = stretch_h {
                self.tree.boxes[id].h = self.clamp_height(id, h, Some(cbh));
            }
        }
        if top.is_none() {
            if let Some(bt) = bottom {
                let h = self.tree.boxes[id].h;
                let ny = cby + cbh - bt - mb - h;
                let dy = ny - self.tree.boxes[id].y;
                crate::translate(self.tree, id, 0.0, dy);
            }
        }
        self.drain_abs();
    }

    pub fn drain_abs(&mut self) {
        while let Some(a) = self.abs_queue.pop() {
            self.layout_absolute(a);
        }
    }

    /// Relative positioning offsets (CSS 2 §9.4.3), applied at paint time.
    pub fn compute_relative_offsets(&mut self) {
        for i in 0..self.tree.boxes.len() {
            let s = self.tree.boxes[i].style.clone();
            if !matches!(s.position, Position::Relative | Position::Sticky)
                && s.translate == (LP::ZERO, LP::ZERO)
            {
                continue;
            }
            let cb_w = self.tree.boxes[i]
                .parent
                .map(|p| self.tree.boxes[p].content_rect_with_w().2)
                .unwrap_or(self.tree.viewport.0);
            let mut dx = 0.0;
            let mut dy = 0.0;
            if matches!(s.position, Position::Relative | Position::Sticky) {
                dx = match (lpa(&s.left, cb_w), lpa(&s.right, cb_w)) {
                    (Some(l), _) => l,
                    (None, Some(r)) => -r,
                    _ => 0.0,
                };
                dy = match (lpa(&s.top, 0.0), lpa(&s.bottom, 0.0)) {
                    (Some(t), _) if !s.top.has_percent() => t,
                    (None, Some(b)) if !s.bottom.has_percent() => -b,
                    _ => 0.0,
                };
            }
            let b = &self.tree.boxes[i];
            dx += s.translate.0.resolve(b.w);
            dy += s.translate.1.resolve(b.h);
            self.tree.boxes[i].rel_offset = (dx, dy);
        }
    }
}

impl crate::tree::LBox {
    /// Content rect computed from x/y/w (height may not be final yet).
    pub fn content_rect_with_w(&self) -> (f32, f32, f32, f32) {
        let x = self.x + self.border[3] + self.padding[3];
        let y = self.y + self.border[0] + self.padding[0];
        let w = (self.w - self.border[1] - self.border[3] - self.padding[1] - self.padding[3]).max(0.0);
        (x, y, w, 0.0)
    }
}

/// Is this a replaced image whose image hasn't loaded and has no size?
pub fn is_unsized_placeholder(r: &Replaced) -> bool {
    matches!(r, Replaced::Image { image: None, attr_size: (None, None), .. })
}

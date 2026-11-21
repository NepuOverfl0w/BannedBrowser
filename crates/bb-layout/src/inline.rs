//! Inline formatting contexts (CSS 2 §9.4.2, §10.8; CSS Text 3 line breaking).

use crate::block::{Bfc, Layout};
use crate::tree::{BoxId, BoxKind, Line, LineItem};
use bb_style::{ComputedStyle, Float, Position, TextAlign, VerticalAlign};
use bb_text::{FontList, FontSystem, ShapedText};
use std::rc::Rc;
use std::sync::Arc;

/// A flattened inline-level item.
#[derive(Debug, Clone)]
pub enum Item {
    Text {
        text: Rc<str>,
        style: Rc<ComputedStyle>,
        node: Option<bb_dom::NodeId>,
        owner: BoxId,
    },
    SpanStart(BoxId),
    SpanEnd(BoxId),
    Atomic(BoxId),
    Break,
    Float(BoxId),
    Abs(BoxId),
    /// A block-level box inside an inline (block-in-inline).
    Block(BoxId),
}

pub fn collect_items(l: &Layout, id: BoxId, out: &mut Vec<Item>) {
    for &c in &l.tree.boxes[id].children {
        let b = &l.tree.boxes[c];
        let text_like = matches!(b.kind, BoxKind::Text(_) | BoxKind::LineBreak);
        if !text_like && matches!(b.style.position, Position::Absolute | Position::Fixed) {
            out.push(Item::Abs(c));
            continue;
        }
        if !text_like && b.style.float != Float::None {
            out.push(Item::Float(c));
            continue;
        }
        match &b.kind {
            BoxKind::Text(t) => {
                out.push(Item::Text { text: t.clone(), style: b.style.clone(), node: b.node, owner: id })
            }
            BoxKind::Inline => {
                out.push(Item::SpanStart(c));
                collect_items(l, c, out);
                out.push(Item::SpanEnd(c));
            }
            BoxKind::LineBreak => out.push(Item::Break),
            _ if b.is_atomic_inline() => out.push(Item::Atomic(c)),
            _ => out.push(Item::Block(c)),
        }
    }
}

pub fn font_list(style: &ComputedStyle) -> FontList {
    let italic = style.font_style != bb_style::FontStyle::Normal;
    FontSystem::lock().resolve(&style.font_family, style.font_weight, italic)
}

/// Font metrics (ascent, descent, normal line height, x-height) in px.
pub fn metrics(style: &ComputedStyle) -> (f32, f32, f32, f32) {
    let fl = font_list(style);
    let fs = FontSystem::lock();
    let m = fs.metrics(fl.primary());
    let size = style.font_size;
    (m.ascent * size, m.descent * size, m.normal_line_height(size), m.x_height * size)
}

/// The half-leading inline box extents (above, below baseline) for a style.
pub fn inline_extents(style: &ComputedStyle) -> (f32, f32) {
    let (a, d, normal, _) = metrics(style);
    let lh = style.line_height_px(normal);
    let half = (lh - (a + d)) / 2.0;
    (a + half, d + half)
}

pub fn shape(text: &str, style: &ComputedStyle) -> Arc<ShapedText> {
    let fl = font_list(style);
    let mut fs = FontSystem::lock();
    // TODO(css-fonts-4 §6.4): synthesize small-caps.
    let size = style.font_size;
    let mut shaped = fs.shape(text, &fl, size);
    if style.letter_spacing != 0.0 || style.word_spacing != 0.0 {
        let mut s = (*shaped).clone();
        let mut extra = 0.0;
        for g in s.glyphs.iter_mut() {
            g.x_advance += style.letter_spacing;
            extra += style.letter_spacing;
            if text.as_bytes().get(g.cluster as usize) == Some(&b' ') {
                g.x_advance += style.word_spacing;
                extra += style.word_spacing;
            }
        }
        s.width += extra;
        shaped = Arc::new(s);
    }
    shaped
}

/// A measured piece of text between break opportunities.
#[derive(Debug, Clone)]
struct Piece {
    item: usize,
    start: usize,
    end: usize,
    /// Width including any trailing collapsible space.
    width: f32,
    /// Width of trailing collapsible whitespace (hangs at line end).
    trailing_space: f32,
    break_before: bool,
    forced_after: bool,
    glyphs: Arc<ShapedText>,
}

/// Concatenate item text (with U+FFFC for atomics) to find UAX #14 opportunities.
fn paragraph_breaks(items: &[Item]) -> (Vec<(usize, usize)>, Vec<(usize, bool)>) {
    let mut text = String::new();
    let mut ranges = Vec::with_capacity(items.len());
    for it in items {
        let start = text.len();
        match it {
            Item::Text { text: t, .. } => text.push_str(t),
            Item::Atomic(_) => text.push('\u{FFFC}'),
            Item::Break => text.push('\n'),
            _ => {}
        }
        ranges.push((start, text.len()));
    }
    let breaks = bb_text::break_opportunities(&text);
    (ranges, breaks)
}

fn pieces_for(items: &[Item]) -> Vec<Piece> {
    let (ranges, breaks) = paragraph_breaks(items);
    let is_break = |pos: usize| breaks.binary_search_by_key(&pos, |b| b.0).is_ok();
    let mut out = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let Item::Text { text, style, .. } = it else { continue };
        let (gs, ge) = ranges[i];
        let wraps = style.white_space.wraps();
        let preserve_nl = style.white_space.preserves_newlines();
        // Split points within this item.
        let mut cuts: Vec<usize> = vec![0];
        for &(b, _) in &breaks {
            if b > gs && b < ge {
                cuts.push(b - gs);
            }
        }
        if preserve_nl {
            for (k, ch) in text.char_indices() {
                if ch == '\n' {
                    cuts.push(k);
                    cuts.push(k + 1);
                }
            }
        }
        cuts.push(text.len());
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let (s, e) = (w[0], w[1]);
            if s >= e {
                continue;
            }
            let slice = &text[s..e];
            if slice == "\n" {
                if let Some(last) = out.last_mut().filter(|p: &&mut Piece| p.item == i) {
                    last.forced_after = true;
                } else {
                    out.push(Piece {
                        item: i,
                        start: s,
                        end: s,
                        width: 0.0,
                        trailing_space: 0.0,
                        break_before: false,
                        forced_after: true,
                        glyphs: Arc::new(ShapedText::default()),
                    });
                }
                continue;
            }
            let glyphs = shape(slice, style);
            let collapsible = !style.white_space.preserves_spaces();
            let trailing =
                if collapsible && slice.ends_with(' ') && slice.trim_end_matches(' ').len() < slice.len() {
                    let n = slice.len() - slice.trim_end_matches(' ').len();
                    shape(&" ".repeat(n), style).width
                } else {
                    0.0
                };
            let global = gs + s;
            let break_before = wraps && (s > 0 || is_break(global)) && global > 0;
            out.push(Piece {
                item: i,
                start: s,
                end: e,
                width: glyphs.width,
                trailing_space: trailing,
                break_before,
                forced_after: false,
                glyphs,
            });
        }
    }
    out
}

/// (min-content, max-content) contribution of inline content.
pub fn inline_intrinsic(l: &mut Layout, id: BoxId) -> (f32, f32) {
    let mut items = Vec::new();
    collect_items(l, id, &mut items);
    let pieces = pieces_for(&items);
    let mut min: f32 = 0.0;
    let mut max: f32 = 0.0;
    let mut line: f32 = 0.0;
    let mut word: f32 = 0.0;
    let mut pi = 0;
    for (i, it) in items.iter().enumerate() {
        match it {
            Item::Text { .. } => {
                while pi < pieces.len() && pieces[pi].item == i {
                    let p = &pieces[pi];
                    if p.break_before {
                        min = min.max(word);
                        word = 0.0;
                    }
                    word += p.width - p.trailing_space;
                    if p.trailing_space > 0.0 {
                        min = min.max(word);
                        word = 0.0;
                    }
                    line += p.width;
                    if p.forced_after {
                        max = max.max(line);
                        line = 0.0;
                        min = min.max(word);
                        word = 0.0;
                    }
                    pi += 1;
                }
            }
            Item::Atomic(a) | Item::Float(a) | Item::Block(a) => {
                let (amin, amax) = crate::intrinsic::intrinsic_widths(l, *a);
                let m = {
                    let b = &l.tree.boxes[*a];
                    b.margin[1] + b.margin[3]
                };
                min = min.max(word);
                word = 0.0;
                min = min.max(amin + m);
                if matches!(it, Item::Block(_)) {
                    max = max.max(line).max(amax + m);
                    line = 0.0;
                } else {
                    line += amax + m;
                }
            }
            Item::Break => {
                max = max.max(line);
                line = 0.0;
                min = min.max(word);
                word = 0.0;
            }
            Item::SpanStart(s) | Item::SpanEnd(s) => {
                let side = if matches!(it, Item::SpanStart(_)) { 3 } else { 1 };
                l.resolve_box_model(*s, 0.0);
                let b = &l.tree.boxes[*s];
                let e = b.margin[side] + b.border[side] + b.padding[side];
                line += e;
                word += e;
            }
            Item::Abs(_) => {}
        }
    }
    min = min.max(word);
    max = max.max(line);
    let indent = l.tree.boxes[id].style.text_indent.resolve(0.0).max(0.0);
    (min, max + indent)
}

/// One element placed on the line being built.
#[derive(Debug, Clone)]
enum Placed {
    Text { piece: usize, x: f32, width: f32 },
    Atomic { id: BoxId, x: f32 },
    SpanEdge { id: BoxId, start: bool, x: f32 },
}

struct LineBuilder {
    placed: Vec<Placed>,
    width: f32,
    has_content: bool,
    /// Open spans at line start (continued from previous line).
    open_spans: Vec<BoxId>,
}

struct LineCtx {
    block_style: Rc<ComputedStyle>,
    indent: f32,
    strut_a: f32,
    strut_d: f32,
    quirks: bool,
    cx: f32,
    cw: f32,
    est_h: f32,
}

struct LineState {
    lines: Vec<Line>,
    y: f32,
    first_line: bool,
    band: (f32, f32),
}

#[allow(clippy::too_many_arguments)]
fn finish_line(
    l: &mut Layout,
    lb: &mut LineBuilder,
    st: &mut LineState,
    ctx: &LineCtx,
    bfc: &Bfc,
    forced: bool,
    last: bool,
    span_stack: &[BoxId],
    pieces: &[Piece],
    items: &[Item],
) {
    let x0 = st.band.0 + if st.first_line { ctx.indent } else { 0.0 };
    if let Some(line) = build_line(
        l,
        lb,
        &ctx.block_style,
        x0,
        st.y,
        st.band.1 - x0,
        ctx.strut_a,
        ctx.strut_d,
        ctx.quirks,
        forced || last,
        pieces,
        items,
    ) {
        st.y += line.h;
        st.lines.push(line);
    }
    st.first_line = false;
    lb.placed.clear();
    lb.width = 0.0;
    lb.has_content = false;
    lb.open_spans = span_stack.to_vec();
    st.band = bfc.band(st.y, st.y + ctx.est_h, ctx.cx, ctx.cx + ctx.cw);
}

/// Lay out inline content of block container `id` in content box (cx, cy, cw).
/// Returns content height.
pub fn layout_inline(l: &mut Layout, id: BoxId, cx: f32, cy: f32, cw: f32, bfc: &mut Bfc) -> f32 {
    let mut items = Vec::new();
    collect_items(l, id, &mut items);
    let mut pieces = pieces_for(&items);
    let block_style = l.tree.boxes[id].style.clone();
    let (strut_a, strut_d) = inline_extents(&block_style);
    let ctx = LineCtx {
        indent: block_style.text_indent.resolve(cw),
        block_style,
        strut_a,
        strut_d,
        quirks: l.tree.quirks,
        cx,
        cw,
        est_h: strut_a + strut_d,
    };
    let mut st = LineState {
        lines: Vec::new(),
        y: cy,
        first_line: true,
        band: bfc.band(cy, cy + ctx.est_h, cx, cx + cw),
    };
    let mut lb = LineBuilder { placed: Vec::new(), width: 0.0, has_content: false, open_spans: Vec::new() };
    let mut span_stack: Vec<BoxId> = Vec::new();
    let mut pi = 0usize;

    // Lay out atomic inlines up front (sizes are independent of line position).
    for it in &items {
        if let Item::Atomic(a) = it {
            layout_atomic(l, *a, cw);
        }
    }
    let avail = |st: &LineState| st.band.1 - st.band.0 - if st.first_line { ctx.indent } else { 0.0 };

    for i in 0..items.len() {
        match items[i].clone() {
            Item::Text { .. } => {
                while pi < pieces.len() && pieces[pi].item == i {
                    let mut p = pieces[pi].clone();
                    // CSS Text 3 §5: decide at a break opportunity using the whole
                    // unbreakable run up to the next opportunity, which may span
                    // several pieces (e.g. "D<span>OWNLOAD</span>").
                    let run_w = if p.break_before || !lb.has_content {
                        let (mut w, mut k) = (0.0, pi);
                        let trail = loop {
                            let q = &pieces[k];
                            w += q.width;
                            k += 1;
                            if q.forced_after || k >= pieces.len() || pieces[k].break_before {
                                break q.trailing_space;
                            }
                        };
                        w - trail
                    } else {
                        p.width - p.trailing_space
                    };
                    let fits = lb.width + run_w <= avail(&st) + 0.01;
                    if !fits && lb.has_content && p.break_before {
                        finish_line(
                            l,
                            &mut lb,
                            &mut st,
                            &ctx,
                            bfc,
                            false,
                            false,
                            &span_stack,
                            &pieces,
                            &items,
                        );
                        continue;
                    }
                    if !fits && !lb.has_content && !bfc.floats.is_empty() && st.band.1 - st.band.0 < cw - 0.5
                    {
                        // Move below the floats that narrow this line.
                        let y = st.y;
                        let next = bfc
                            .floats
                            .iter()
                            .filter(|f| f.y + f.h > y + 0.01)
                            .map(|f| f.y + f.h)
                            .fold(f32::MAX, f32::min);
                        if next < f32::MAX && next > st.y {
                            st.y = next;
                            st.band = bfc.band(st.y, st.y + ctx.est_h, cx, cx + cw);
                            continue;
                        }
                    }
                    // Strip leading collapsible spaces at line start.
                    if !lb.has_content {
                        if let Item::Text { text, style, .. } = &items[p.item] {
                            let slice = &text[p.start..p.end];
                            if !style.white_space.preserves_spaces() && slice.starts_with(' ') {
                                p.start += slice.len() - slice.trim_start_matches(' ').len();
                                let g = shape(&text[p.start..p.end], style);
                                p.width = g.width;
                                p.glyphs = g;
                                if p.start >= p.end {
                                    p.trailing_space = 0.0;
                                }
                                pieces[pi] = p.clone();
                            }
                        }
                    }
                    let x = lb.width;
                    lb.width += p.width;
                    if p.end > p.start {
                        lb.has_content = true;
                    }
                    lb.placed.push(Placed::Text { piece: pi, x, width: p.width });
                    pi += 1;
                    if p.forced_after {
                        finish_line(
                            l,
                            &mut lb,
                            &mut st,
                            &ctx,
                            bfc,
                            true,
                            false,
                            &span_stack,
                            &pieces,
                            &items,
                        );
                    }
                }
            }
            Item::Atomic(a) => {
                let b = &l.tree.boxes[a];
                let w = b.w + b.margin[1] + b.margin[3];
                let wraps =
                    l.tree.boxes[a].parent.map(|p| l.tree.boxes[p].style.white_space.wraps()).unwrap_or(true);
                if lb.width + w > avail(&st) + 0.01 && lb.has_content && wraps {
                    finish_line(l, &mut lb, &mut st, &ctx, bfc, false, false, &span_stack, &pieces, &items);
                }
                let x = lb.width;
                lb.width += w;
                lb.has_content = true;
                lb.placed.push(Placed::Atomic { id: a, x });
            }
            Item::SpanStart(s) => {
                l.resolve_box_model(s, cw);
                let b = &l.tree.boxes[s];
                let e = b.margin[3] + b.border[3] + b.padding[3];
                lb.placed.push(Placed::SpanEdge { id: s, start: true, x: lb.width });
                lb.width += e;
                if b.border[3] + b.padding[3] > 0.0 {
                    lb.has_content = true;
                }
                span_stack.push(s);
            }
            Item::SpanEnd(s) => {
                let b = &l.tree.boxes[s];
                let e = b.margin[1] + b.border[1] + b.padding[1];
                lb.width += e;
                lb.placed.push(Placed::SpanEdge { id: s, start: false, x: lb.width });
                span_stack.retain(|x| *x != s);
            }
            Item::Break => {
                finish_line(l, &mut lb, &mut st, &ctx, bfc, true, false, &span_stack, &pieces, &items)
            }
            Item::Float(f) => {
                l.layout_float(f, cx, cw, None, st.y, bfc);
                st.band = bfc.band(st.y, st.y + ctx.est_h, cx, cx + cw);
            }
            Item::Abs(a) => {
                l.tree.boxes[a].static_pos = (st.band.0 + lb.width, st.y);
                l.abs_queue.push(a);
            }
            Item::Block(bid) => {
                if !lb.placed.is_empty() {
                    finish_line(l, &mut lb, &mut st, &ctx, bfc, true, false, &span_stack, &pieces, &items);
                }
                let mut tmp = Bfc::default();
                l.resolve_box_model(bid, cw);
                let top = st.y + l.tree.boxes[bid].margin[0];
                let out = l.layout_block(bid, cx, cw, None, top, &mut tmp, false);
                st.y = l.tree.boxes[bid].y + l.tree.boxes[bid].h + out.value();
                st.band = bfc.band(st.y, st.y + ctx.est_h, cx, cx + cw);
            }
        }
    }
    if !lb.placed.is_empty() {
        finish_line(l, &mut lb, &mut st, &ctx, bfc, false, true, &span_stack, &pieces, &items);
    }
    let h = st.y - cy;
    let b = &mut l.tree.boxes[id];
    b.baseline = st.lines.first().map(|ln| ln.baseline);
    b.lines = st.lines;
    h
}

/// Visual x offsets for text pieces on a line when it contains right-to-left
/// text. Returns piece index → x. Empty when the line is purely LTR.
fn bidi_positions(lb: &LineBuilder, pieces: &[Piece], base_rtl: bool) -> rustc_hash::FxHashMap<usize, f32> {
    let mut map = rustc_hash::FxHashMap::default();
    let texts: Vec<(usize, f32, f32)> = lb
        .placed
        .iter()
        .filter_map(|p| match p {
            Placed::Text { piece, x, width } => Some((*piece, *x, *width)),
            _ => None,
        })
        .collect();
    let any_rtl = texts.iter().any(|(p, _, _)| pieces[*p].glyphs.rtl == Some(true));
    if !any_rtl || texts.len() != lb.placed.iter().filter(|p| !matches!(p, Placed::SpanEdge { .. })).count() {
        // Mixed atomics + RTL: TODO(UAX #9 L2) full reordering with atomics.
        if !any_rtl {
            return map;
        }
    }
    let base = if base_rtl { 1u8 } else { 0u8 };
    let mut levels: Vec<u8> = texts
        .iter()
        .map(|(p, _, _)| match pieces[*p].glyphs.rtl {
            Some(true) => 1,
            Some(false) => {
                if base == 1 {
                    2
                } else {
                    0
                }
            }
            None => base,
        })
        .collect();
    // Neutral pieces between two RTL pieces take RTL (W/N rules, approximated).
    for i in 1..levels.len().saturating_sub(1) {
        if pieces[texts[i].0].glyphs.rtl.is_none() && levels[i - 1] % 2 == 1 && levels[i + 1] % 2 == 1 {
            levels[i] = 1;
        }
    }
    let mut order: Vec<usize> = (0..texts.len()).collect();
    let max = levels.iter().copied().max().unwrap_or(0);
    let min_odd = levels.iter().copied().filter(|l| l % 2 == 1).min().unwrap_or(1);
    let mut lvl = max;
    while lvl >= min_odd && lvl > 0 {
        let mut i = 0;
        while i < order.len() {
            if levels[order[i]] >= lvl {
                let start = i;
                while i < order.len() && levels[order[i]] >= lvl {
                    i += 1;
                }
                order[start..i].reverse();
            } else {
                i += 1;
            }
        }
        lvl -= 1;
    }
    let start_x = texts.iter().map(|t| t.1).fold(f32::MAX, f32::min);
    let mut x = start_x;
    for &i in &order {
        let (p, _, w) = texts[i];
        map.insert(p, x);
        x += w;
    }
    map
}

fn piece_at(pieces: &[Piece], idx: usize) -> &Piece {
    &pieces[idx]
}

/// Lay out an atomic inline (inline-block / replaced) so its size is known.
fn layout_atomic(l: &mut Layout, a: BoxId, cw: f32) {
    l.resolve_box_model(a, cw);
    let kind = l.tree.boxes[a].kind.clone();
    match kind {
        BoxKind::Replaced(_) => {
            let (w, h) = crate::intrinsic::replaced_size(l, a, None, cw);
            let b = &mut l.tree.boxes[a];
            b.w = w;
            b.h = h;
            b.x = 0.0;
            b.y = 0.0;
            b.laid_out = true;
        }
        _ => {
            let (ml, mr) = (l.tree.boxes[a].margin[3], l.tree.boxes[a].margin[1]);
            let w = match l.specified_border_width(a, cw) {
                Some(w) => w,
                None => l.shrink_to_fit(a, (cw - ml - mr).max(0.0)),
            };
            {
                let b = &mut l.tree.boxes[a];
                b.x = 0.0;
                b.y = 0.0;
                b.w = w;
            }
            let mut bfc = Bfc::default();
            l.layout_contents(a, None, &mut bfc, false);
        }
    }
}

/// The baseline offset of an atomic inline from its margin-box top.
fn atomic_baseline(l: &Layout, a: BoxId) -> f32 {
    let b = &l.tree.boxes[a];
    let mbox_h = b.h + b.margin[0] + b.margin[2];
    match b.kind {
        BoxKind::Replaced(_) => mbox_h,
        _ => {
            let overflow_visible = matches!(b.style.overflow_x, bb_style::Overflow::Visible);
            match first_baseline(l.tree, a) {
                Some(bl) if overflow_visible => bl - b.y + b.margin[0],
                _ => mbox_h,
            }
        }
    }
}

/// Absolute y of the first line's baseline inside `id`, if any (last-line for inline-block per CSS 2 is approximated by first).
pub fn first_baseline(t: &crate::tree::LayoutTree, id: BoxId) -> Option<f32> {
    let b = &t.boxes[id];
    if let Some(line) = b.lines.first() {
        return Some(line.baseline);
    }
    for &c in &b.children {
        if t.boxes[c].is_out_of_flow() {
            continue;
        }
        if let Some(bl) = first_baseline(t, c) {
            return Some(bl);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn build_line(
    l: &mut Layout,
    lb: &LineBuilder,
    block_style: &Rc<ComputedStyle>,
    x0: f32,
    y: f32,
    avail: f32,
    strut_a: f32,
    strut_d: f32,
    quirks: bool,
    last_line: bool,
    pieces: &[Piece],
    items: &[Item],
) -> Option<Line> {
    if lb.placed.is_empty() {
        // Empty line from consecutive <br>s: occupies strut height.
        let h = strut_a + strut_d;
        return Some(Line { x: x0, y, w: 0.0, h, baseline: y + strut_a, items: Vec::new() });
    }
    // Trailing collapsible whitespace hangs.
    let mut width = lb.width;
    if let Some(Placed::Text { piece, .. }) =
        lb.placed.iter().rev().find(|p| !matches!(p, Placed::SpanEdge { .. }))
    {
        width -= piece_at(pieces, *piece).trailing_space;
    }
    // Horizontal alignment.
    let align = block_style.text_align;
    let rtl = block_style.direction_rtl;
    let slack = (avail - width).max(0.0);
    let (shift, justify_extra) = match align {
        TextAlign::Center | TextAlign::LegacyCenter => (slack / 2.0, 0.0),
        TextAlign::Right | TextAlign::LegacyRight => (slack, 0.0),
        TextAlign::End if !rtl => (slack, 0.0),
        TextAlign::Start if rtl => (slack, 0.0),
        TextAlign::Justify if !last_line => (0.0, slack),
        _ => (0.0, 0.0),
    };
    // Count justification opportunities.
    let spaces = if justify_extra > 0.0 {
        lb.placed
            .iter()
            .filter(
                |p| matches!(p, Placed::Text { piece, .. } if piece_at(pieces, *piece).trailing_space > 0.0),
            )
            .count()
            .saturating_sub(1)
            .max(1)
    } else {
        1
    };
    let per_space = justify_extra / spaces as f32;

    // UAX #9 L2 (approximated per piece): reorder RTL runs visually.
    let visual_x = bidi_positions(lb, pieces, block_style.direction_rtl);

    // Vertical metrics: each item's (above, below) relative to the line baseline.
    let mut has_text = false;
    let mut above: f32 = 0.0;
    let mut below: f32 = 0.0;
    let mut top_aligned: Vec<f32> = Vec::new();
    let (_, _, _, parent_xh) = metrics(block_style);
    struct V {
        shift: f32,
        a: f32,
        d: f32,
    }
    let mut vinfo: Vec<V> = Vec::with_capacity(lb.placed.len());
    for p in &lb.placed {
        let (style, a, d, is_text) = match p {
            Placed::Text { piece, .. } => {
                let pc = piece_at(pieces, *piece);
                let Item::Text { style, .. } = &items[pc.item] else { unreachable!() };
                let (a, d) = inline_extents(style);
                let visible = pc.end > pc.start;
                (style.clone(), a, d, visible)
            }
            Placed::Atomic { id, .. } => {
                let b = &l.tree.boxes[*id];
                let bl = atomic_baseline(l, *id);
                let mh = b.h + b.margin[0] + b.margin[2];
                (b.style.clone(), bl, mh - bl, false)
            }
            Placed::SpanEdge { id, .. } => {
                let s = l.tree.boxes[*id].style.clone();
                let (a, d) = inline_extents(&s);
                let b = &l.tree.boxes[*id];
                let has_box = b.border[0] + b.padding[0] + b.border[2] + b.padding[2] > 0.0;
                (s, a, d, has_box)
            }
        };
        if is_text {
            has_text = true;
        }
        let va = style.vertical_align.clone();
        let sh = match va {
            VerticalAlign::Baseline => 0.0,
            VerticalAlign::Sub => style.font_size * 0.2,
            VerticalAlign::Super => -style.font_size * 0.35,
            VerticalAlign::Middle => {
                // Align the box midpoint with baseline - x-height/2 of the parent.
                let mid = (a - d) / 2.0;
                mid - parent_xh / 2.0
            }
            VerticalAlign::TextTop => {
                let (pa, _, _, _) = metrics(block_style);
                a - pa
            }
            VerticalAlign::TextBottom => {
                let (_, pd, _, _) = metrics(block_style);
                -(d - pd)
            }
            VerticalAlign::Length(ref lpv) => -lpv.resolve(0.0),
            VerticalAlign::Top | VerticalAlign::Bottom => 0.0,
        };
        if matches!(va, VerticalAlign::Top | VerticalAlign::Bottom) {
            top_aligned.push(a + d);
        } else {
            let include = is_text || matches!(p, Placed::Atomic { .. }) || !quirks;
            if include {
                above = above.max(a - sh);
                below = below.max(d + sh);
            }
        }
        vinfo.push(V { shift: sh, a, d });
    }
    // The strut: always in standards mode; in quirks mode only for lines with text
    // ("line height calculation quirk").
    if !quirks || has_text {
        above = above.max(strut_a);
        below = below.max(strut_d);
    }
    let mut h = above + below;
    for t in &top_aligned {
        h = h.max(*t);
    }
    let baseline = y + above;
    let mut out = Vec::new();
    let mut extra_x = 0.0;
    let mut span_starts: Vec<(BoxId, f32)> = lb.open_spans.iter().map(|s| (*s, x0 + shift)).collect();
    let mut spans_done: Vec<(BoxId, f32, f32, bool, bool)> = Vec::new();
    for (p, v) in lb.placed.iter().zip(vinfo.iter()) {
        match p {
            Placed::Text { piece, x, width } => {
                let pc = piece_at(pieces, *piece);
                let Item::Text { style, node, owner, text, .. } = &items[pc.item] else { unreachable!() };
                let x = visual_x.get(piece).copied().unwrap_or(*x);
                let px = x0 + shift + x + extra_x;
                out.push(LineItem::Text {
                    x: px,
                    baseline: baseline + v.shift,
                    width: *width,
                    glyphs: pc.glyphs.clone(),
                    style: style.clone(),
                    node: *node,
                    owner: *owner,
                    range: (pc.start, pc.end),
                    text: text.clone(),
                    ascent: v.a,
                    descent: v.d,
                });
                if pc.trailing_space > 0.0 {
                    extra_x += per_space;
                }
            }
            Placed::Atomic { id, x, .. } => {
                let b = &l.tree.boxes[*id];
                let bx = x0 + shift + x + extra_x + b.margin[3];
                let va = &b.style.vertical_align;
                let mh = b.h + b.margin[0] + b.margin[2];
                let top = match va {
                    VerticalAlign::Top => y,
                    VerticalAlign::Bottom => y + h - mh,
                    _ => baseline + v.shift - v.a,
                } + b.margin[0];
                let (dx, dy) = (bx - b.x, top - b.y);
                crate::translate(l.tree, *id, dx, dy);
                out.push(LineItem::Atomic { id: *id });
            }
            Placed::SpanEdge { id, start, x } => {
                let px = x0 + shift + x + extra_x;
                if *start {
                    let ml = l.tree.boxes[*id].margin[3];
                    span_starts.push((*id, px + ml));
                } else if let Some(pos) = span_starts.iter().rposition(|(s, _)| s == id) {
                    let (_, sx) = span_starts.remove(pos);
                    let mr = l.tree.boxes[*id].margin[1];
                    let first = !lb.open_spans.contains(id);
                    spans_done.push((*id, sx, px - mr, first, true));
                }
            }
        }
    }
    // Spans continuing to the next line.
    let line_right = x0 + shift + width + extra_x;
    for (id, sx) in span_starts {
        let first = !lb.open_spans.contains(&id);
        spans_done.push((id, sx, line_right, first, false));
    }
    let mut span_items = Vec::new();
    for (id, sx, ex, first, last) in spans_done {
        let s = l.tree.boxes[id].style.clone();
        let (a, d) = {
            let (fa, fd, _, _) = metrics(&s);
            (fa, fd)
        };
        let b = &l.tree.boxes[id];
        let top = baseline - a - b.padding[0] - b.border[0];
        let bottom = baseline + d + b.padding[2] + b.border[2];
        span_items.push(LineItem::Span { id, x0: sx, x1: ex, top, bottom, first, last });
    }
    // Spans paint beneath text: put them first.
    span_items.extend(out);
    Some(Line { x: x0, y, w: width, h, baseline, items: span_items })
}

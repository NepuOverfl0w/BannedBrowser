//! Flexbox (CSS Flexible Box Layout 1, §9 algorithm, simplified):
//! direction, wrap, order, grow/shrink/basis, justify-content, align-items/self,
//! gaps. TODO(css-flexbox-1 §9.4): baseline alignment, align-content for multi-line.

use crate::block::{lpa, Bfc, Layout};
use crate::tree::BoxId;
use bb_style::{Align, FlexDirection, FlexWrap, Position};

struct ItemInfo {
    id: BoxId,
    base: f32,
    hypo: f32,
    min_main: f32,
    margin_main: f32,
    margin_cross: (f32, f32),
    grow: f32,
    shrink: f32,
    main: f32,
}

/// Lay out flex container `id` content box (cx, cy, cw). Returns content height.
pub fn layout_flex(l: &mut Layout, id: BoxId, cx: f32, cy: f32, cw: f32, ch: Option<f32>) -> f32 {
    let style = l.tree.boxes[id].style.clone();
    let row = matches!(style.flex_direction, FlexDirection::Row | FlexDirection::RowReverse);
    let reverse = matches!(style.flex_direction, FlexDirection::RowReverse | FlexDirection::ColumnReverse);
    let wrap = style.flex_wrap != FlexWrap::NoWrap;
    let main_gap = if row { style.column_gap.resolve(cw) } else { style.row_gap.resolve(ch.unwrap_or(0.0)) };
    let cross_gap = if row { style.row_gap.resolve(ch.unwrap_or(0.0)) } else { style.column_gap.resolve(cw) };
    let mut kids: Vec<BoxId> = l.tree.boxes[id].children.clone();
    kids.retain(|&k| {
        let b = &l.tree.boxes[k];
        if matches!(b.style.position, Position::Absolute | Position::Fixed) {
            l.abs_queue.push(k);
            false
        } else {
            !matches!(&b.kind, crate::tree::BoxKind::Text(t) if t.trim().is_empty())
        }
    });
    kids.sort_by_key(|&k| l.tree.boxes[k].style.order);
    let main_size = if row { cw } else { ch.unwrap_or(f32::INFINITY) };

    // Determine hypothetical main sizes.
    let mut items: Vec<ItemInfo> = Vec::new();
    for &k in &kids {
        l.resolve_box_model(k, cw);
        let s = l.tree.boxes[k].style.clone();
        let (mt, mr, mb, ml) = {
            let b = &l.tree.boxes[k];
            (b.margin[0], b.margin[1], b.margin[2], b.margin[3])
        };
        let (min_c, max_c) = crate::intrinsic::intrinsic_widths(l, k);
        let base = if row {
            match lpa(&s.flex_basis, cw) {
                Some(b) => {
                    b + if s.box_sizing == bb_style::BoxSizing::ContentBox { l.horiz_extra(k) } else { 0.0 }
                }
                None => l.specified_border_width(k, cw).unwrap_or(max_c),
            }
        } else {
            match lpa(&s.flex_basis, ch.unwrap_or(0.0))
                .filter(|_| ch.is_some() || !s.flex_basis.has_percent())
            {
                Some(b) => b,
                None => {
                    // Column: lay out to measure height.
                    let w = l.specified_border_width(k, cw).unwrap_or((cw - ml - mr).max(0.0));
                    {
                        let b = &mut l.tree.boxes[k];
                        b.x = cx + ml;
                        b.y = cy;
                        b.w = w;
                    }
                    let mut bfc = Bfc::default();
                    l.layout_contents(k, None, &mut bfc, false);
                    l.tree.boxes[k].h
                }
            }
        };
        let min_main = if row {
            if s.min_width.is_auto() {
                min_c.min(base)
            } else {
                lpa(&s.min_width, cw).unwrap_or(0.0)
            }
        } else {
            0.0
        };
        items.push(ItemInfo {
            id: k,
            base,
            hypo: base.max(min_main),
            min_main,
            margin_main: if row { ml + mr } else { mt + mb },
            margin_cross: if row { (mt, mb) } else { (ml, mr) },
            grow: s.flex_grow,
            shrink: s.flex_shrink,
            main: base,
        });
    }
    // Break into lines.
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut used = 0.0;
    for (i, it) in items.iter().enumerate() {
        let need = it.hypo + it.margin_main + if cur.is_empty() { 0.0 } else { main_gap };
        if wrap && !cur.is_empty() && used + need > main_size + 0.01 {
            lines.push(std::mem::take(&mut cur));
            used = 0.0;
        }
        used += it.hypo + it.margin_main + if cur.is_empty() { 0.0 } else { main_gap };
        cur.push(i);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    // Resolve flexible lengths per line (§9.7, simplified single pass with clamping).
    for line in &lines {
        let gaps = main_gap * (line.len().saturating_sub(1)) as f32;
        let total: f32 = line.iter().map(|&i| items[i].hypo + items[i].margin_main).sum::<f32>() + gaps;
        let free = if main_size.is_finite() { main_size - total } else { 0.0 };
        if free > 0.0 {
            let g: f32 = line.iter().map(|&i| items[i].grow).sum();
            for &i in line {
                items[i].main = items[i].hypo + if g > 0.0 { free * items[i].grow / g.max(1.0) } else { 0.0 };
            }
        } else if free < 0.0 {
            let sh: f32 = line.iter().map(|&i| items[i].shrink * items[i].base).sum();
            for &i in line {
                let it = &items[i];
                let ratio = if sh > 0.0 { it.shrink * it.base / sh } else { 0.0 };
                items[i].main = (it.hypo + free * ratio).max(it.min_main);
            }
        } else {
            for &i in line {
                items[i].main = items[i].hypo;
            }
        }
    }
    // Lay out each item with its main size and position.
    let mut cross_pos = if row { cy } else { cx };
    let mut total_cross = 0.0;
    for (li, line) in lines.iter().enumerate() {
        // Measure cross sizes.
        let mut line_cross: f32 = 0.0;
        for &i in line {
            let k = items[i].id;
            if row {
                let b = &mut l.tree.boxes[k];
                b.w = items[i].main;
                b.x = 0.0;
                b.y = 0.0;
                let mut bfc = Bfc::default();
                l.layout_contents(k, ch, &mut bfc, false);
                let ch_item = l.tree.boxes[k].h + items[i].margin_cross.0 + items[i].margin_cross.1;
                line_cross = line_cross.max(ch_item);
            } else {
                let w = l.specified_border_width(k, cw);
                let (_, maxc) = crate::intrinsic::intrinsic_widths(l, k);
                let stretch =
                    matches!(effective_align(l, id, k), Align::Stretch | Align::Normal) && w.is_none();
                let ww = w.unwrap_or(if stretch {
                    (cw - items[i].margin_cross.0 - items[i].margin_cross.1).max(0.0)
                } else {
                    maxc.min(cw)
                });
                let b = &mut l.tree.boxes[k];
                b.w = ww;
                b.x = 0.0;
                b.y = 0.0;
                let mut bfc = Bfc::default();
                l.layout_contents(k, None, &mut bfc, false);
                l.tree.boxes[k].h = l.tree.boxes[k].h.max(items[i].main);
                line_cross = line_cross.max(ww + items[i].margin_cross.0 + items[i].margin_cross.1);
            }
        }
        if !wrap {
            if row {
                if let Some(h) = ch {
                    line_cross = line_cross.max(h);
                }
            } else {
                line_cross = line_cross.max(cw);
            }
        }
        // Main-axis positioning (justify-content).
        let gaps = main_gap * (line.len().saturating_sub(1)) as f32;
        let used: f32 = line.iter().map(|&i| items[i].main + items[i].margin_main).sum::<f32>() + gaps;
        let avail_main = if main_size.is_finite() { main_size } else { used };
        let free = (avail_main - used).max(0.0);
        let n = line.len() as f32;
        let (mut pos, between) = match style.justify_content {
            Align::End => (free, 0.0),
            Align::Center => (free / 2.0, 0.0),
            Align::SpaceBetween if n > 1.0 => (0.0, free / (n - 1.0)),
            Align::SpaceAround => (free / n / 2.0, free / n),
            Align::SpaceEvenly => (free / (n + 1.0), free / (n + 1.0)),
            _ => (0.0, 0.0),
        };
        let order: Vec<usize> = if reverse { line.iter().rev().copied().collect() } else { line.clone() };
        for &i in &order {
            let k = items[i].id;
            let (m0, m1) = items[i].margin_cross;
            let item_cross = if row { l.tree.boxes[k].h } else { l.tree.boxes[k].w };
            let align = effective_align(l, id, k);
            let spec_cross = if row {
                l.tree.boxes[k].style.height.is_auto()
            } else {
                l.tree.boxes[k].style.width.is_auto()
            };
            let cross_off = match align {
                Align::End => line_cross - item_cross - m1,
                Align::Center => (line_cross - item_cross - m0 - m1) / 2.0 + m0,
                Align::Stretch | Align::Normal if spec_cross && row => {
                    l.tree.boxes[k].h = (line_cross - m0 - m1).max(item_cross);
                    m0
                }
                _ => m0,
            };
            let (lead, trail) = {
                let b = &l.tree.boxes[k];
                if row {
                    (b.margin[3], b.margin[1])
                } else {
                    (b.margin[0], b.margin[2])
                }
            };
            let main_start = if row { cx } else { cy };
            let (nx, ny) = if row {
                (main_start + pos + lead, cross_pos + cross_off)
            } else {
                (cross_pos + cross_off, main_start + pos + lead)
            };
            let (dx, dy) = (nx - l.tree.boxes[k].x, ny - l.tree.boxes[k].y);
            crate::translate(l.tree, k, dx, dy);
            pos += lead + items[i].main + trail + main_gap + between;
        }
        cross_pos += line_cross + if li + 1 < lines.len() { cross_gap } else { 0.0 };
        total_cross += line_cross + if li + 1 < lines.len() { cross_gap } else { 0.0 };
    }
    if row {
        total_cross
    } else {
        let mut max_bottom: f32 = cy;
        for it in &items {
            let b = &l.tree.boxes[it.id];
            max_bottom = max_bottom.max(b.y + b.h + b.margin[2]);
        }
        max_bottom - cy
    }
}

fn effective_align(l: &Layout, container: BoxId, item: BoxId) -> Align {
    let s = &l.tree.boxes[item].style;
    if s.align_self != Align::Auto {
        s.align_self
    } else {
        l.tree.boxes[container].style.align_items
    }
}

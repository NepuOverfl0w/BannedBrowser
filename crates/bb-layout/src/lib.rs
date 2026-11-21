//! `bb-layout` — box tree construction and layout: block flow with margin
//! collapsing, inline formatting with UAX #14 line breaking, floats, HTML
//! tables, basic flexbox, positioned boxes, list markers and replaced
//! elements; plus display-list generation and hit testing.
#![forbid(unsafe_code)]

pub mod block;
pub mod find;
pub mod flex;
pub mod inline;
pub mod intrinsic;
pub mod paint;
pub mod table;
pub mod tree;

pub use tree::{build_tree, BoxId, BoxKind, ImageState, LayoutTree, Line, LineItem, Resources};

use block::{Bfc, Layout};

/// Move `id` and all of its descendants (and their line contents) by (dx, dy).
pub fn translate(tree: &mut LayoutTree, id: BoxId, dx: f32, dy: f32) {
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    let mut stack = vec![id];
    while let Some(b) = stack.pop() {
        let bx = &mut tree.boxes[b];
        bx.x += dx;
        bx.y += dy;
        bx.content_bottom += dy;
        bx.static_pos.0 += dx;
        bx.static_pos.1 += dy;
        if let Some(bl) = bx.baseline.as_mut() {
            *bl += dy;
        }
        for line in bx.lines.iter_mut() {
            translate_line(line, dx, dy);
        }
        stack.extend(bx.children.iter().copied());
    }
}

pub fn translate_line(line: &mut Line, dx: f32, dy: f32) {
    line.x += dx;
    line.y += dy;
    line.baseline += dy;
    for it in line.items.iter_mut() {
        match it {
            LineItem::Text { x, baseline, .. } => {
                *x += dx;
                *baseline += dy;
            }
            LineItem::Span { x0, x1, top, bottom, .. } => {
                *x0 += dx;
                *x1 += dx;
                *top += dy;
                *bottom += dy;
            }
            LineItem::Atomic { .. } => {} // the atomic box itself is translated via the tree
        }
    }
}

/// Lay out the whole tree for its viewport.
pub fn layout(tree: &mut LayoutTree) {
    let root = tree.root;
    let (vw, vh) = tree.viewport;
    {
        let r = &mut tree.boxes[root];
        r.x = 0.0;
        r.y = 0.0;
        r.w = vw;
    }
    let mut l = Layout { tree, abs_queue: Vec::new() };
    let mut bfc = Bfc::default();
    let kids = l.tree.boxes[root].children.clone();
    let mut y = 0.0;
    for k in kids {
        if l.tree.boxes[k].is_out_of_flow()
            && matches!(
                l.tree.boxes[k].style.position,
                bb_style::Position::Absolute | bb_style::Position::Fixed
            )
        {
            l.abs_queue.push(k);
            continue;
        }
        l.resolve_box_model(k, vw);
        let top = y + l.tree.boxes[k].margin[0];
        let out = l.layout_block(k, 0.0, vw, Some(vh), top, &mut bfc, false);
        y = l.tree.boxes[k].y + l.tree.boxes[k].h + out.value();
        let cb = l.tree.boxes[k].content_bottom;
        l.tree.boxes[root].content_bottom = l.tree.boxes[root].content_bottom.max(cb).max(y);
    }
    l.tree.boxes[root].h = y.max(vh);
    l.compute_relative_offsets();
    l.drain_abs();
    // Document extent includes positioned descendants and floats.
    let mut bottom = l.tree.boxes[root].content_bottom.max(bfc.bottom());
    for b in &l.tree.boxes {
        if b.laid_out {
            bottom = bottom.max(b.y + b.h + b.margin[2]);
        }
    }
    l.tree.boxes[root].content_bottom = bottom;
}

/// The topmost box/DOM node at page point (x, y).
#[derive(Debug, Clone, Default)]
pub struct Hit {
    /// Deepest DOM node under the point (text node or element).
    pub node: Option<bb_dom::NodeId>,
    pub cursor: Option<bb_style::Cursor>,
}

/// Hit test at page coordinates (CSS px).
pub fn hit_test(tree: &LayoutTree, x: f32, y: f32) -> Hit {
    let mut best: Option<(usize, bb_dom::NodeId, bb_style::Cursor, u32)> = None;
    // Walk in tree order with accumulated relative offsets; later (painted later) wins.
    let mut stack: Vec<(BoxId, (f32, f32), u32)> = vec![(tree.root, (0.0, 0.0), 0)];
    let mut order = 0usize;
    while let Some((id, off, depth)) = stack.pop() {
        let b = &tree.boxes[id];
        let o = (off.0 + b.rel_offset.0, off.1 + b.rel_offset.1);
        order += 1;
        let z = if b.style.position != bb_style::Position::Static {
            1 + b.style.z_index.unwrap_or(0).max(0) as u32
        } else {
            0
        };
        let inside = x >= b.x + o.0 && x < b.x + o.0 + b.w && y >= b.y + o.1 && y < b.y + o.1 + b.h;
        if inside && b.style.visibility == bb_style::Visibility::Visible {
            if let Some(n) = b.node {
                let key = z * 1_000_000 + order as u32;
                if best.is_none_or(|bb| key >= bb.3) {
                    best = Some((id, n, b.style.cursor, key));
                }
            }
        }
        for line in &b.lines {
            if y < line.y + o.1 || y >= line.y + line.h + o.1 {
                continue;
            }
            for it in &line.items {
                if let LineItem::Text { x: tx, width, node: Some(n), style, .. } = it {
                    if x >= tx + o.0 && x < tx + width + o.0 {
                        let key = z * 1_000_000 + order as u32 + 1;
                        if best.is_none_or(|bb| key >= bb.3) {
                            best = Some((id, *n, style.cursor, key));
                        }
                    }
                }
            }
        }
        for &c in b.children.iter().rev() {
            stack.push((c, o, depth + 1));
        }
    }
    match best {
        Some((_, n, c, _)) => Hit { node: Some(n), cursor: Some(c) },
        None => Hit::default(),
    }
}

/// Text dump of the fragment tree (for `--dump-layout`).
pub fn dump(tree: &LayoutTree) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let mut stack = vec![(tree.root, 0usize)];
    while let Some((id, d)) = stack.pop() {
        let b = &tree.boxes[id];
        let kind = match &b.kind {
            BoxKind::Text(t) => format!("text {:?}", t.chars().take(30).collect::<String>()),
            k => format!("{k:?}").split(['{', '(']).next().unwrap_or("").trim().to_string(),
        };
        let _ = writeln!(
            out,
            "{}{}{} [{:.1},{:.1} {:.1}x{:.1}]{}",
            "  ".repeat(d),
            kind,
            if b.anonymous { " (anon)" } else { "" },
            b.x,
            b.y,
            b.w,
            b.h,
            if b.lines.is_empty() { String::new() } else { format!(" lines={}", b.lines.len()) }
        );
        for (i, line) in b.lines.iter().enumerate() {
            let text: String = line
                .items
                .iter()
                .filter_map(|it| match it {
                    LineItem::Text { glyphs, .. } => Some(glyphs.glyphs.len()),
                    _ => None,
                })
                .map(|n| format!("{n}g "))
                .collect();
            let _ = writeln!(
                out,
                "{}  line {i} [{:.1},{:.1} {:.1}x{:.1}] baseline {:.1} {}",
                "  ".repeat(d),
                line.x,
                line.y,
                line.w,
                line.h,
                line.baseline,
                text.trim()
            );
        }
        for &c in b.children.iter().rev() {
            stack.push((c, d + 1));
        }
    }
    out
}

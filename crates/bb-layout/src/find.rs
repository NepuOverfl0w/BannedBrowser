//! Find in page: locate text matches in the fragment tree and map them to
//! rectangles (page coordinates) for highlighting and scrolling.

use crate::{BoxId, LayoutTree, LineItem};
use bb_paint::Rect;

struct Run<'a> {
    text: &'a str,
    /// Pen x of the run's first glyph and its glyphs (clusters relative to `text`).
    x: f32,
    top: f32,
    bottom: f32,
    glyphs: &'a bb_text::ShapedText,
    line_key: (BoxId, usize),
}

/// x offset (from the run's start) of the glyph cluster at byte `b`.
fn x_at(glyphs: &bb_text::ShapedText, b: usize) -> f32 {
    glyphs.glyphs.iter().filter(|g| (g.cluster as usize) < b).map(|g| g.x_advance).sum()
}

fn fold(c: char) -> char {
    // Simple case folding; good enough for find-as-you-type.
    c.to_lowercase().next().unwrap_or(c)
}

/// All matches of `query` (case-insensitive, whitespace runs match any
/// whitespace), each as one rect per text run it covers, in document order.
pub fn find_text(tree: &LayoutTree, query: &str) -> Vec<Vec<Rect>> {
    let q: Vec<char> = query.split_whitespace().collect::<Vec<_>>().join(" ").chars().map(fold).collect();
    if q.is_empty() || tree.boxes.is_empty() {
        return Vec::new();
    }
    // Collect text runs in tree (document) order with relative-position offsets.
    let mut runs: Vec<Run> = Vec::new();
    let mut stack: Vec<(BoxId, (f32, f32))> = vec![(tree.root, (0.0, 0.0))];
    while let Some((id, off)) = stack.pop() {
        let b = &tree.boxes[id];
        let o = (off.0 + b.rel_offset.0, off.1 + b.rel_offset.1);
        if b.style.visibility == bb_style::Visibility::Visible {
            for (li, line) in b.lines.iter().enumerate() {
                for it in &line.items {
                    if let LineItem::Text { x, baseline, glyphs, range, text, ascent, descent, .. } = it {
                        let Some(t) = text.get(range.0..range.1) else { continue };
                        runs.push(Run {
                            text: t,
                            x: x + o.0,
                            top: baseline - ascent + o.1,
                            bottom: baseline + descent + o.1,
                            glyphs,
                            line_key: (id, li),
                        });
                    }
                }
            }
        }
        for &c in b.children.iter().rev() {
            stack.push((c, o));
        }
    }
    // Flatten to (folded char, run, byte start, byte end); soft line breaks
    // between runs count as a space.
    let mut flat: Vec<(char, usize, usize, usize)> = Vec::new();
    for (ri, r) in runs.iter().enumerate() {
        if ri > 0 && runs[ri - 1].line_key != r.line_key {
            let prev_space = flat.last().is_some_and(|f| f.0 == ' ');
            if !prev_space && !r.text.starts_with(char::is_whitespace) {
                flat.push((' ', usize::MAX, 0, 0));
            }
        }
        for (bi, orig) in r.text.char_indices() {
            let c = if orig.is_whitespace() { ' ' } else { fold(orig) };
            if c == ' ' && flat.last().is_some_and(|f| f.0 == ' ') {
                continue;
            }
            flat.push((c, ri, bi, bi + orig.len_utf8()));
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i + q.len() <= flat.len() {
        if flat[i..i + q.len()].iter().zip(&q).all(|(f, c)| f.0 == *c) {
            let mut rects: Vec<Rect> = Vec::new();
            let mut cur: Option<(usize, usize, usize)> = None; // run, start, end
            let flush = |cur: Option<(usize, usize, usize)>, rects: &mut Vec<Rect>| {
                if let Some((ri, s, e)) = cur {
                    let r = &runs[ri];
                    let (x0, x1) = (x_at(r.glyphs, s), x_at(r.glyphs, e));
                    let (a, b) = (x0.min(x1), x0.max(x1));
                    rects.push(Rect::new(r.x + a, r.top, (b - a).max(1.0), r.bottom - r.top));
                }
            };
            for f in &flat[i..i + q.len()] {
                if f.1 == usize::MAX {
                    continue;
                }
                match cur {
                    Some((ri, s, _)) if ri == f.1 => cur = Some((ri, s, f.3)),
                    _ => {
                        flush(cur, &mut rects);
                        cur = Some((f.1, f.2, f.3));
                    }
                }
            }
            flush(cur, &mut rects);
            out.push(rects);
            i += q.len();
        } else {
            i += 1;
        }
    }
    out
}

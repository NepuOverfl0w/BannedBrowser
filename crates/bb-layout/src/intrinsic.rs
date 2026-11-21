//! Intrinsic sizes: min-/max-content widths (CSS Sizing 3) and replaced
//! element sizing (CSS 2 §10.3.2, §10.6.2).

use crate::block::{lpa, Layout};
use crate::tree::{BoxId, BoxKind, Control, Replaced};
use bb_style::BoxSizing;

/// (min-content, max-content) border-box widths of `id`.
pub fn intrinsic_widths(l: &mut Layout, id: BoxId) -> (f32, f32) {
    if let Some(c) = l.tree.boxes[id].intrinsic {
        return c;
    }
    l.resolve_box_model(id, 0.0);
    let extra = l.horiz_extra(id);
    let kind = l.tree.boxes[id].kind.clone();
    let style = l.tree.boxes[id].style.clone();
    let (mut min, mut max) = match &kind {
        BoxKind::Replaced(_) => {
            let (w, _) = replaced_size(l, id, None, 0.0);
            let w = w - extra;
            (w, w)
        }
        BoxKind::Table => crate::table::table_intrinsic(l, id),
        BoxKind::Text(_) | BoxKind::LineBreak => (0.0, 0.0),
        BoxKind::Flex => {
            let kids = l.tree.boxes[id].children.clone();
            let row = matches!(
                style.flex_direction,
                bb_style::FlexDirection::Row | bb_style::FlexDirection::RowReverse
            );
            let mut mn: f32 = 0.0;
            let mut mx: f32 = 0.0;
            let gap = style.column_gap.resolve(0.0);
            for (i, c) in kids.iter().enumerate() {
                if l.tree.boxes[*c].is_out_of_flow() {
                    continue;
                }
                let (a, b) = intrinsic_widths(l, *c);
                let m = l.tree.boxes[*c].margin[1] + l.tree.boxes[*c].margin[3];
                if row {
                    mn = mn.max(a + m);
                    mx += b + m + if i > 0 { gap } else { 0.0 };
                } else {
                    mn = mn.max(a + m);
                    mx = mx.max(b + m);
                }
            }
            (mn, mx)
        }
        _ => {
            if l.has_inline_content(id) {
                crate::inline::inline_intrinsic(l, id)
            } else {
                let kids = l.tree.boxes[id].children.clone();
                let mut mn: f32 = 0.0;
                let mut mx: f32 = 0.0;
                let mut float_line: f32 = 0.0;
                for c in kids {
                    let b = &l.tree.boxes[c];
                    if matches!(b.style.position, bb_style::Position::Absolute | bb_style::Position::Fixed) {
                        continue;
                    }
                    let is_float = b.style.float != bb_style::Float::None;
                    let (a, bmax) = intrinsic_widths(l, c);
                    let s = l.tree.boxes[c].style.clone();
                    let ml = lpa(&s.margin[3], 0.0).unwrap_or(0.0);
                    let mr = lpa(&s.margin[1], 0.0).unwrap_or(0.0);
                    mn = mn.max(a + ml + mr);
                    if is_float {
                        float_line += bmax + ml + mr;
                        mx = mx.max(float_line);
                    } else {
                        float_line = 0.0;
                        mx = mx.max(bmax + ml + mr);
                    }
                }
                (mn, mx)
            }
        }
    };
    // Specified widths override content sizes (percentages behave as auto here).
    if !style.width.has_percent() {
        if let Some(w) = lpa(&style.width, 0.0) {
            let bw = if style.box_sizing == BoxSizing::BorderBox { (w - extra).max(0.0) } else { w };
            if matches!(kind, BoxKind::TableCell { .. }) {
                // Cells: the specified width acts as a minimum preferred width.
                max = max.max(bw).max(min);
                min = min.max(0.0);
            } else {
                min = if matches!(kind, BoxKind::Table) { min.max(bw) } else { bw };
                max = bw.max(if matches!(kind, BoxKind::Table) { min } else { 0.0 });
            }
        }
    }
    if let Some(mxw) = &style.max_width {
        if !mxw.has_percent() {
            let v = mxw.resolve(0.0);
            max = max.min(v);
            min = min.min(v);
        }
    }
    if !style.min_width.has_percent() {
        if let Some(v) = lpa(&style.min_width, 0.0) {
            min = min.max(v);
            max = max.max(v);
        }
    }
    let max = max.max(min);
    let r = (min + extra, max + extra);
    l.tree.boxes[id].intrinsic = Some(r);
    r
}

fn text_width(text: &str, style: &bb_style::ComputedStyle) -> f32 {
    crate::inline::shape(text, style).width
}

/// Border-box size of a replaced element. `used_w` is a border-box width
/// already chosen by the caller (e.g. block layout), `cb_w` resolves percentages.
pub fn replaced_size(l: &mut Layout, id: BoxId, used_w: Option<f32>, cb_w: f32) -> (f32, f32) {
    l.resolve_box_model(id, cb_w);
    let hx = l.horiz_extra(id);
    let vx = l.vert_extra(id);
    let style = l.tree.boxes[id].style.clone();
    let BoxKind::Replaced(r) = l.tree.boxes[id].kind.clone() else { return (hx, vx) };
    let to_content =
        |v: f32, extra: f32| if style.box_sizing == BoxSizing::BorderBox { (v - extra).max(0.0) } else { v };
    let sw = if style.width.has_percent() && cb_w <= 0.0 {
        None
    } else {
        lpa(&style.width, cb_w).map(|v| to_content(v, hx))
    };
    let sh =
        if style.height.has_percent() { None } else { lpa(&style.height, 0.0).map(|v| to_content(v, vx)) };
    let sw = used_w.map(|w| (w - hx).max(0.0)).or(sw);
    let (iw, ih, ratio) = match &r {
        Replaced::Image { image: Some(img), .. } => (
            Some(img.width as f32),
            Some(img.height as f32),
            Some(img.width as f32 / img.height.max(1) as f32),
        ),
        Replaced::Image { image: None, attr_size, alt, broken } => {
            let (aw, ah) = *attr_size;
            if *broken && aw.is_none() && ah.is_none() {
                // Broken image without dimensions: render its alt text inline.
                match alt.as_deref().filter(|a| !a.trim().is_empty()) {
                    Some(a) => {
                        let (asc, desc) = crate::inline::inline_extents(&style);
                        (Some(text_width(a, &style) + 4.0), Some(asc + desc), None)
                    }
                    None => (Some(0.0), Some(0.0), None),
                }
            } else {
                // Loading (or broken with size hints): a sized placeholder.
                (
                    aw.or(Some(0.0)),
                    ah.or(Some(0.0)),
                    match (aw, ah) {
                        (Some(w), Some(h)) if h > 0.0 => Some(w / h),
                        _ => None,
                    },
                )
            }
        }
        Replaced::Control(c) => {
            let (asc, desc, normal, _) = crate::inline::metrics(&style);
            let line = style.line_height_px(normal).max(asc + desc);
            match c {
                Control::TextField { .. } => {
                    let ch = text_width("0", &style);
                    (Some(ch * 20.0), Some(line), None)
                }
                Control::TextArea { .. } => {
                    let ch = text_width("0", &style);
                    (Some(ch * 20.0), Some(line * 2.0), None)
                }
                Control::Button { label } => (Some(text_width(label, &style)), Some(line), None),
                Control::Checkbox { .. } => (Some(13.0), Some(13.0), Some(1.0)),
                Control::Select { label } => (Some(text_width(label, &style) + 18.0), Some(line), None),
            }
        }
        Replaced::Placeholder { default, .. } | Replaced::Svg { default } => {
            (Some(default.0), Some(default.1), Some(default.0 / default.1.max(1.0)))
        }
    };
    let (mut w, mut h) = match (sw, sh) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, ratio.map(|r| w / r.max(1e-6)).or(ih).unwrap_or(150.0)),
        (None, Some(h)) => (ratio.map(|r| h * r).or(iw).unwrap_or(300.0), h),
        (None, None) => (iw.unwrap_or(300.0), ih.unwrap_or(150.0)),
    };
    // min/max constraints (simplified: applied independently).
    if let Some(mx) = &style.max_width {
        let m = mx.resolve(cb_w);
        if !mx.has_percent() || cb_w > 0.0 {
            if w > m {
                if sh.is_none() {
                    if let Some(r) = ratio {
                        h = m / r.max(1e-6);
                    }
                }
                w = m;
            }
        }
    }
    if let Some(mx) = &style.max_height {
        if !mx.has_percent() {
            let m = mx.resolve(0.0);
            if h > m {
                if sw.is_none() {
                    if let Some(r) = ratio {
                        w = m * r;
                    }
                }
                h = m;
            }
        }
    }
    if let Some(m) = lpa(&style.min_width, cb_w) {
        w = w.max(m);
    }
    (w + hx, h + vx)
}

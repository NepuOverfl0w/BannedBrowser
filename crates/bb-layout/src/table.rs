//! Table layout (CSS 2 §17.5, HTML table processing model §4.9.12).
//!
//! Auto layout follows the usual browser approach: per-column min/max
//! content widths (with colspan distribution and percentage columns), then
//! the available width is distributed between min and max.

use crate::block::{lpa, Bfc, Layout};
use crate::tree::{BoxId, BoxKind};
use bb_style::{BorderCollapse, CaptionSide, Display, TableLayout, VerticalAlign};

#[derive(Debug, Clone)]
struct Cell {
    id: BoxId,
    row: usize,
    col: usize,
    colspan: usize,
    rowspan: usize,
}

#[derive(Debug, Default)]
struct Grid {
    rows: Vec<BoxId>,
    row_group: Vec<BoxId>,
    cells: Vec<Cell>,
    ncols: usize,
    captions: Vec<BoxId>,
    /// Explicit column widths from <col>/<colgroup>.
    col_widths: Vec<Option<bb_style::LPA>>,
}

fn build_grid(l: &Layout, table: BoxId) -> Grid {
    let mut g = Grid::default();
    let kids = l.tree.boxes[table].children.clone();
    let mut groups: Vec<BoxId> = Vec::new();
    let (mut head, mut foot) = (Vec::new(), Vec::new());
    for &k in &kids {
        let b = &l.tree.boxes[k];
        match b.kind {
            BoxKind::TableCaption => g.captions.push(k),
            BoxKind::TableRowGroup => match b.style.display {
                Display::TableHeaderGroup if head.is_empty() => head.push(k),
                Display::TableFooterGroup if foot.is_empty() => foot.push(k),
                _ => groups.push(k),
            },
            BoxKind::TableColumnGroup => {
                let cols: Vec<BoxId> = b
                    .children
                    .iter()
                    .copied()
                    .filter(|&c| matches!(l.tree.boxes[c].kind, BoxKind::TableColumn { .. }))
                    .collect();
                if cols.is_empty() {
                    let span = l.tree.boxes[k].node.and(None::<usize>).unwrap_or(1);
                    for _ in 0..span {
                        g.col_widths.push(Some(b.style.width.clone()).filter(|w| !w.is_auto()));
                    }
                } else {
                    for c in cols {
                        let BoxKind::TableColumn { span } = l.tree.boxes[c].kind else { continue };
                        let w = l.tree.boxes[c].style.width.clone();
                        let w = if w.is_auto() { b.style.width.clone() } else { w };
                        for _ in 0..span {
                            g.col_widths.push(Some(w.clone()).filter(|w| !w.is_auto()));
                        }
                    }
                }
            }
            BoxKind::TableColumn { span } => {
                for _ in 0..span {
                    g.col_widths.push(Some(b.style.width.clone()).filter(|w| !w.is_auto()));
                }
            }
            _ => {}
        }
    }
    let ordered: Vec<BoxId> = head.into_iter().chain(groups).chain(foot).collect();
    for grp in ordered {
        for &r in &l.tree.boxes[grp].children {
            if matches!(l.tree.boxes[r].kind, BoxKind::TableRow) {
                g.rows.push(r);
                g.row_group.push(grp);
            }
        }
    }
    let nrows = g.rows.len();
    let mut occupied: Vec<Vec<bool>> = vec![Vec::new(); nrows];
    for (ri, &r) in g.rows.iter().enumerate() {
        let mut col = 0;
        for &c in &l.tree.boxes[r].children {
            let BoxKind::TableCell { colspan, rowspan } = l.tree.boxes[c].kind else { continue };
            while occupied[ri].get(col).copied().unwrap_or(false) {
                col += 1;
            }
            let rowspan = if rowspan == 0 { nrows - ri } else { rowspan.min(nrows - ri) }.max(1);
            for rr in ri..ri + rowspan {
                let row = &mut occupied[rr];
                if row.len() < col + colspan {
                    row.resize(col + colspan, false);
                }
                for cc in col..col + colspan {
                    row[cc] = true;
                }
            }
            g.cells.push(Cell { id: c, row: ri, col, colspan, rowspan });
            col += colspan;
            g.ncols = g.ncols.max(col);
        }
    }
    g.ncols = g.ncols.max(g.col_widths.len().min(1000)).max(if g.cells.is_empty() { 0 } else { 1 });
    g
}

fn spacing(l: &Layout, table: BoxId) -> (f32, f32) {
    let s = &l.tree.boxes[table].style;
    if s.border_collapse == BorderCollapse::Collapse {
        (0.0, 0.0)
    } else {
        s.border_spacing
    }
}

/// Column (min, max) widths and percentage columns.
fn column_widths(l: &mut Layout, table: BoxId, g: &Grid) -> (Vec<f32>, Vec<f32>, Vec<Option<f32>>) {
    let n = g.ncols;
    let mut mins = vec![0.0f32; n];
    let mut maxs = vec![0.0f32; n];
    let mut pcts: Vec<Option<f32>> = vec![None; n];
    let (hs, _) = spacing(l, table);
    // Explicit <col> widths.
    for (i, w) in g.col_widths.iter().enumerate().take(n) {
        match w {
            Some(bb_style::LPA::LP(bb_style::LP::Pct(p))) => pcts[i] = Some(*p),
            Some(w) => {
                if let Some(px) = lpa(w, 0.0) {
                    mins[i] = mins[i].max(px);
                    maxs[i] = maxs[i].max(px);
                }
            }
            None => {}
        }
    }
    let mut multi: Vec<&Cell> = Vec::new();
    for c in &g.cells {
        let (cmin, cmax) = crate::intrinsic::intrinsic_widths(l, c.id);
        let pct = match &l.tree.boxes[c.id].style.width {
            bb_style::LPA::LP(bb_style::LP::Pct(p)) => Some(*p),
            _ => None,
        };
        if c.colspan == 1 {
            mins[c.col] = mins[c.col].max(cmin);
            maxs[c.col] = maxs[c.col].max(cmax);
            if let Some(p) = pct {
                pcts[c.col] = Some(pcts[c.col].unwrap_or(0.0).max(p));
            }
        } else {
            multi.push(c);
        }
    }
    multi.sort_by_key(|c| c.colspan);
    for c in multi {
        let (cmin, cmax) = crate::intrinsic::intrinsic_widths(l, c.id);
        let span = c.col..(c.col + c.colspan).min(n);
        let inner_spacing = hs * (c.colspan as f32 - 1.0);
        let cur_min: f32 = mins[span.clone()].iter().sum::<f32>() + inner_spacing;
        let cur_max: f32 = maxs[span.clone()].iter().sum::<f32>() + inner_spacing;
        let k = span.len() as f32;
        if cmin > cur_min {
            let extra = cmin - cur_min;
            let total_max: f32 = maxs[span.clone()].iter().sum();
            for i in span.clone() {
                let share = if total_max > 0.0 { maxs[i] / total_max } else { 1.0 / k };
                mins[i] += extra * share;
            }
        }
        if cmax > cur_max {
            let extra = cmax - cur_max;
            let total_max: f32 = maxs[span.clone()].iter().sum();
            for i in span.clone() {
                let share = if total_max > 0.0 { maxs[i] / total_max } else { 1.0 / k };
                maxs[i] += extra * share;
            }
        }
        if let bb_style::LPA::LP(bb_style::LP::Pct(p)) = l.tree.boxes[c.id].style.width {
            let unassigned: Vec<usize> = span.clone().filter(|&i| pcts[i].is_none()).collect();
            if !unassigned.is_empty() {
                let assigned: f32 = span.clone().filter_map(|i| pcts[i]).sum();
                let rest = (p - assigned).max(0.0) / unassigned.len() as f32;
                for i in unassigned {
                    pcts[i] = Some(rest);
                }
            }
        }
    }
    for i in 0..n {
        maxs[i] = maxs[i].max(mins[i]);
    }
    (mins, maxs, pcts)
}

/// (min, max) content widths of the table (excluding its own border/padding).
pub fn table_intrinsic(l: &mut Layout, table: BoxId) -> (f32, f32) {
    let g = build_grid(l, table);
    let (mins, maxs, pcts) = column_widths(l, table, &g);
    let (hs, _) = spacing(l, table);
    let sp = hs * (g.ncols as f32 + 1.0);
    let min: f32 = mins.iter().sum::<f32>() + sp;
    let mut max: f32 = maxs.iter().sum::<f32>() + sp;
    // Percentage columns can force a wider max (CSS 2 / Gecko heuristic).
    let pct_total: f32 = pcts.iter().flatten().sum();
    if pct_total > 0.0 && pct_total < 100.0 {
        let non_pct: f32 = maxs.iter().zip(&pcts).filter(|(_, p)| p.is_none()).map(|(m, _)| *m).sum();
        let need = non_pct * 100.0 / (100.0 - pct_total);
        max = max.max(need + sp);
    }
    let mut cap_min: f32 = 0.0;
    for &c in &g.captions {
        cap_min = cap_min.max(crate::intrinsic::intrinsic_widths(l, c).0);
    }
    (min.max(cap_min), max.max(min).max(cap_min))
}

/// Distribute `avail` content width among columns.
fn distribute(mins: &[f32], maxs: &[f32], pcts: &[Option<f32>], avail: f32, fixed_width: bool) -> Vec<f32> {
    let n = mins.len();
    let mut w = mins.to_vec();
    let sum_min: f32 = mins.iter().sum();
    if avail <= sum_min + 0.01 {
        return w;
    }
    // Percentage columns first.
    let mut remaining = avail;
    let mut pct_cols = vec![false; n];
    for i in 0..n {
        if let Some(p) = pcts[i] {
            w[i] = (avail * p / 100.0).max(mins[i]);
            pct_cols[i] = true;
        }
    }
    let pct_used: f32 = (0..n).filter(|&i| pct_cols[i]).map(|i| w[i]).sum();
    remaining -= pct_used;
    let auto: Vec<usize> = (0..n).filter(|&i| !pct_cols[i]).collect();
    let amin: f32 = auto.iter().map(|&i| mins[i]).sum();
    let amax: f32 = auto.iter().map(|&i| maxs[i]).sum();
    if auto.is_empty() {
        // Scale percentage columns to fill.
        let total: f32 = w.iter().sum();
        if total > 0.0 && (total < avail || fixed_width) {
            let f = avail / total;
            for x in w.iter_mut() {
                *x *= f;
            }
        }
        return w;
    }
    if remaining <= amin {
        for &i in &auto {
            w[i] = mins[i];
        }
    } else if remaining <= amax {
        let span = (amax - amin).max(1e-6);
        let t = (remaining - amin) / span;
        for &i in &auto {
            w[i] = mins[i] + (maxs[i] - mins[i]) * t;
        }
    } else {
        // Extra space: proportional to max widths.
        let extra = remaining - amax;
        for &i in &auto {
            let share = if amax > 0.0 { maxs[i] / amax } else { 1.0 / auto.len() as f32 };
            w[i] = maxs[i] + extra * share;
        }
    }
    w
}

fn fixed_column_widths(l: &mut Layout, g: &Grid, avail: f32) -> Vec<f32> {
    let n = g.ncols;
    let mut w: Vec<Option<f32>> = vec![None; n];
    for (i, cw) in g.col_widths.iter().enumerate().take(n) {
        if let Some(c) = cw {
            w[i] = lpa(c, avail);
        }
    }
    for c in g.cells.iter().filter(|c| c.row == 0) {
        if w[c.col].is_some() {
            continue;
        }
        if let Some(bw) = l.specified_border_width(c.id, avail) {
            let per = bw / c.colspan as f32;
            for i in c.col..(c.col + c.colspan).min(n) {
                w[i] = Some(per);
            }
        }
    }
    let used: f32 = w.iter().flatten().sum();
    let unknown = w.iter().filter(|x| x.is_none()).count();
    let rest = if unknown > 0 { ((avail - used) / unknown as f32).max(0.0) } else { 0.0 };
    let mut out: Vec<f32> = w.iter().map(|x| x.unwrap_or(rest)).collect();
    let total: f32 = out.iter().sum();
    if total < avail && total > 0.0 && unknown == 0 {
        let f = avail / total;
        for x in out.iter_mut() {
            *x *= f;
        }
    }
    out
}

/// Lay out table `id` (x, y, w already set). Returns its border-box height.
pub fn layout_table(l: &mut Layout, id: BoxId, cb_h: Option<f32>) -> f32 {
    let g = build_grid(l, id);
    let (hs, vs) = spacing(l, id);
    let (cx, cy, cw, _) = l.tree.boxes[id].content_rect_with_w();
    let style = l.tree.boxes[id].style.clone();
    let mut y = cy;
    // Captions (top).
    let caption_top = style.caption_side == CaptionSide::Top;
    let place_captions = |l: &mut Layout, y: &mut f32| {
        for &c in &g.captions {
            let mut bfc = Bfc::default();
            l.resolve_box_model(c, cw);
            let top = *y + l.tree.boxes[c].margin[0];
            let out = l.layout_block(c, cx, cw, None, top, &mut bfc, false);
            *y = l.tree.boxes[c].y + l.tree.boxes[c].h + out.value();
        }
    };
    if caption_top {
        place_captions(l, &mut y);
    }
    let n = g.ncols;
    let avail = (cw - hs * (n as f32 + 1.0)).max(0.0);
    let widths = if style.table_layout == TableLayout::Fixed && !style.width.is_auto() {
        fixed_column_widths(l, &g, avail)
    } else {
        let (mins, maxs, pcts) = column_widths(l, id, &g);
        let w = distribute(&mins, &maxs, &pcts, avail, !style.width.is_auto());
        // If the table is auto-width and narrower than avail, columns already sum to the shrink-to-fit width.
        w
    };
    let mut col_x = Vec::with_capacity(n + 1);
    let mut x = cx + hs;
    for w in &widths {
        col_x.push(x);
        x += w + hs;
    }
    col_x.push(x);
    let nrows = g.rows.len();
    // Pass 1: lay out cells at provisional positions to get heights.
    let mut row_h = vec![0.0f32; nrows];
    for (ri, &r) in g.rows.iter().enumerate() {
        l.resolve_box_model(r, cw);
        if let Some(h) = l.specified_border_height(r, cb_h) {
            row_h[ri] = row_h[ri].max(h);
        }
    }
    let mut cell_content_h = vec![0.0f32; g.cells.len()];
    let mut cell_baseline = vec![None; g.cells.len()];
    for (ci, c) in g.cells.iter().enumerate() {
        let w: f32 =
            widths[c.col..(c.col + c.colspan).min(n)].iter().sum::<f32>() + hs * (c.colspan as f32 - 1.0);
        l.resolve_box_model(c.id, cw);
        {
            let b = &mut l.tree.boxes[c.id];
            b.x = col_x[c.col];
            b.y = 0.0;
            b.w = w.max(0.0);
        }
        let mut bfc = Bfc::default();
        // Lay out with auto height to measure content.
        l.layout_contents(c.id, None, &mut bfc, false);
        let h = l.tree.boxes[c.id].h;
        cell_content_h[ci] = h;
        cell_baseline[ci] = crate::inline::first_baseline(l.tree, c.id).map(|b| b - l.tree.boxes[c.id].y);
        if c.rowspan == 1 {
            row_h[c.row] = row_h[c.row].max(h);
        }
    }
    // Baseline alignment within rows: rows grow so baseline-aligned cells line up.
    let mut row_baseline = vec![0.0f32; nrows];
    for (ci, c) in g.cells.iter().enumerate() {
        if c.rowspan == 1 && matches!(l.tree.boxes[c.id].style.vertical_align, VerticalAlign::Baseline) {
            if let Some(bl) = cell_baseline[ci] {
                row_baseline[c.row] = row_baseline[c.row].max(bl);
            }
        }
    }
    for (ci, c) in g.cells.iter().enumerate() {
        if c.rowspan == 1 && matches!(l.tree.boxes[c.id].style.vertical_align, VerticalAlign::Baseline) {
            if let Some(bl) = cell_baseline[ci] {
                let needed = row_baseline[c.row] - bl + cell_content_h[ci];
                row_h[c.row] = row_h[c.row].max(needed);
            }
        }
    }
    // Rowspan cells: grow the last spanned row if needed.
    for (ci, c) in g.cells.iter().enumerate() {
        if c.rowspan > 1 {
            let end = (c.row + c.rowspan).min(nrows);
            let have: f32 = row_h[c.row..end].iter().sum::<f32>() + vs * (c.rowspan as f32 - 1.0);
            if cell_content_h[ci] > have {
                row_h[end - 1] += cell_content_h[ci] - have;
            }
        }
    }
    // Specified table height: distribute extra to rows.
    let spec_h = l.specified_border_height(id, cb_h);
    let rows_total: f32 = row_h.iter().sum::<f32>() + vs * (nrows as f32 + 1.0);
    let caption_h = y - cy;
    if let Some(th) = spec_h {
        let inner = th - l.vert_extra(id) - caption_h;
        if inner > rows_total && nrows > 0 {
            let extra = (inner - rows_total) / nrows as f32;
            for h in row_h.iter_mut() {
                *h += extra;
            }
        }
    }
    // Pass 2: position rows and cells.
    let mut row_y = Vec::with_capacity(nrows);
    let mut ry = y + vs;
    for h in &row_h {
        row_y.push(ry);
        ry += h + vs;
    }
    for (ri, &r) in g.rows.iter().enumerate() {
        let b = &mut l.tree.boxes[r];
        b.x = cx + hs;
        b.y = row_y[ri];
        b.w = (cw - 2.0 * hs).max(0.0);
        b.h = row_h[ri];
        b.laid_out = true;
    }
    // Row groups span their rows.
    let mut seen_groups: Vec<BoxId> = Vec::new();
    for (ri, &grp) in g.row_group.iter().enumerate() {
        if !seen_groups.contains(&grp) {
            seen_groups.push(grp);
            let last = g.row_group.iter().rposition(|&x| x == grp).unwrap_or(ri);
            let b = &mut l.tree.boxes[grp];
            b.x = cx + hs;
            b.y = row_y[ri];
            b.w = (cw - 2.0 * hs).max(0.0);
            b.h = row_y[last] + row_h[last] - row_y[ri];
            b.laid_out = true;
        }
    }
    for (ci, c) in g.cells.iter().enumerate() {
        let end = (c.row + c.rowspan).min(nrows).max(c.row + 1);
        let h: f32 = row_h[c.row..end].iter().sum::<f32>() + vs * (end - c.row - 1) as f32;
        let target_y = row_y[c.row];
        let dy = target_y - l.tree.boxes[c.id].y;
        crate::translate(l.tree, c.id, 0.0, dy);
        // Vertical alignment of the cell's content (CSS 2 §17.5.4).
        let content = cell_content_h[ci];
        let off = match l.tree.boxes[c.id].style.vertical_align {
            VerticalAlign::Middle => (h - content) / 2.0,
            VerticalAlign::Bottom => h - content,
            VerticalAlign::Baseline if c.rowspan == 1 => {
                cell_baseline[ci].map(|bl| row_baseline[c.row] - bl).unwrap_or(0.0)
            }
            _ => 0.0,
        };
        if off.abs() > 0.01 {
            let kids = l.tree.boxes[c.id].children.clone();
            for k in kids {
                crate::translate(l.tree, k, 0.0, off);
            }
            for line in l.tree.boxes[c.id].lines.iter_mut() {
                crate::translate_line(line, 0.0, off);
            }
            if let Some(b) = l.tree.boxes[c.id].baseline.as_mut() {
                *b += off;
            }
        }
        l.tree.boxes[c.id].h = h;
    }
    y = ry;
    if !caption_top {
        place_captions(l, &mut y);
    }
    let content_h = y - cy;
    let mut h = content_h + l.vert_extra(id);
    if let Some(sh) = spec_h {
        h = h.max(sh);
    }
    h
}

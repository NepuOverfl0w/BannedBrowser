//! The display list: a flat, ordered list of drawing commands in CSS px,
//! produced by layout (stacking order already resolved) and replayed onto a
//! [`Canvas`] with a scroll offset and device scale.

use crate::canvas::{Canvas, Color, ImageData, Paint, Stop};
use crate::glyphs::{draw_glyphs, PlacedGlyph};
use crate::path::{Path, Point, Radii, Rect};
use bb_text::FontId;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderStyle {
    Solid,
    Dashed,
    Dotted,
    Double,
    Groove,
    Ridge,
    Inset,
    Outset,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderSide {
    pub width: f32,
    pub color: Color,
    pub style: BorderStyle,
}

/// A run of glyphs sharing a size and color (positions in CSS px).
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphRun {
    pub glyphs: Vec<(FontId, u16, f32, f32)>,
    pub size: f32,
    pub color: Color,
    pub bold: bool,
    pub italic: bool,
}

#[derive(Debug, Clone)]
pub enum Item {
    Rect {
        rect: Rect,
        color: Color,
    },
    RoundedRect {
        rect: Rect,
        radii: Radii,
        paint: Paint,
    },
    /// Gradient over `rect` (paint coordinates in CSS px).
    Gradient {
        rect: Rect,
        radii: Radii,
        paint: Paint,
    },
    Border {
        rect: Rect,
        radii: Radii,
        sides: [Option<BorderSide>; 4],
    },
    Text(GlyphRun),
    Image {
        image: ImageData,
        dst: Rect,
        clip: Rect,
        opacity: f32,
        smooth: bool,
    },
    /// A tiled background image.
    TiledImage {
        image: ImageData,
        area: Rect,
        tile: Rect,
        repeat_x: bool,
        repeat_y: bool,
    },
    Line {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        width: f32,
        color: Color,
        dashed: bool,
    },
    Shadow {
        rect: Rect,
        radii: Radii,
        blur: f32,
        color: Color,
        box_rect: Rect,
        box_radii: Radii,
    },
    Path {
        path: Path,
        paint: Paint,
    },
    PushClip {
        rect: Rect,
        radii: Option<Radii>,
    },
    PopClip,
    PushOpacity(f32),
    PopOpacity,
}

#[derive(Debug, Clone, Default)]
pub struct DisplayList {
    pub items: Vec<Item>,
    /// Total content size in CSS px.
    pub width: f32,
    pub height: f32,
    pub background: Color,
}

impl DisplayList {
    pub fn push(&mut self, i: Item) {
        self.items.push(i);
    }
}

fn shade(c: Color, f: f32) -> Color {
    let s = |v: u8| ((v as f32) * f).round().clamp(0.0, 255.0) as u8;
    Color { r: s(c.r), g: s(c.g), b: s(c.b), a: c.a }
}

/// Replay `list` onto `canvas`: CSS px → device px with `scale`, after
/// translating by `(-scroll_x, -scroll_y)`.
pub fn render<C: Canvas + ?Sized>(list: &DisplayList, canvas: &mut C, scroll: (f32, f32), scale: f32) {
    let (sx, sy) = scroll;
    let tr = |r: &Rect| Rect::new((r.x - sx) * scale, (r.y - sy) * scale, r.w * scale, r.h * scale);
    let rad = |r: &Radii| Radii {
        tl: (r.tl.0 * scale, r.tl.1 * scale),
        tr: (r.tr.0 * scale, r.tr.1 * scale),
        br: (r.br.0 * scale, r.br.1 * scale),
        bl: (r.bl.0 * scale, r.bl.1 * scale),
    };
    let tp = |p: &Paint| -> Paint {
        match p {
            Paint::Solid(c) => Paint::Solid(*c),
            Paint::Linear { x0, y0, x1, y1, stops, repeating } => Paint::Linear {
                x0: (x0 - sx) * scale,
                y0: (y0 - sy) * scale,
                x1: (x1 - sx) * scale,
                y1: (y1 - sy) * scale,
                stops: stops.clone(),
                repeating: *repeating,
            },
            Paint::Radial { cx, cy, rx, ry, stops, repeating } => Paint::Radial {
                cx: (cx - sx) * scale,
                cy: (cy - sy) * scale,
                rx: rx * scale,
                ry: ry * scale,
                stops: stops.clone(),
                repeating: *repeating,
            },
        }
    };
    let view = Rect::new(0.0, 0.0, canvas.width() as f32, canvas.height() as f32);
    // Snap device rects to whole pixels for crisp boxes.
    let snap = |r: Rect| {
        let x0 = r.x.round();
        let y0 = r.y.round();
        Rect::new(x0, y0, (r.right().round() - x0).max(0.0), (r.bottom().round() - y0).max(0.0))
    };
    for item in &list.items {
        match item {
            Item::Rect { rect, color } => {
                let r = tr(rect);
                if r.intersects(&view) {
                    canvas.fill_rect(snap(r), *color);
                }
            }
            Item::RoundedRect { rect, radii, paint } | Item::Gradient { rect, radii, paint } => {
                let r = tr(rect);
                if !r.intersects(&view) {
                    continue;
                }
                let r = snap(r);
                if radii.is_zero() {
                    if let Paint::Solid(c) = paint {
                        canvas.fill_rect(r, *c);
                        continue;
                    }
                    canvas.fill_path(&Path::rect(r), &tp(paint));
                } else {
                    canvas.fill_path(&Path::rounded_rect(r, rad(radii)), &tp(paint));
                }
            }
            Item::Border { rect, radii, sides } => {
                let r = snap(tr(rect));
                if r.intersects(&view) {
                    draw_border(canvas, r, rad(radii), sides, scale);
                }
            }
            Item::Text(run) => {
                let placed: Vec<PlacedGlyph> = run
                    .glyphs
                    .iter()
                    .map(|(f, id, x, y)| PlacedGlyph {
                        font: *f,
                        id: *id,
                        x: (x - sx) * scale,
                        y: (y - sy) * scale,
                    })
                    .collect();
                if let (Some(first), Some(last)) = (placed.first(), placed.last()) {
                    let h = run.size * scale * 2.0;
                    if last.x + h < 0.0
                        || first.x - h > view.w * 4.0
                        || first.y + h < 0.0
                        || first.y - h > view.h
                    {
                        continue;
                    }
                }
                draw_glyphs(canvas, &placed, run.size * scale, run.color, run.bold, run.italic);
            }
            Item::Image { image, dst, clip, opacity, smooth } => {
                let d = tr(dst);
                let c = tr(clip);
                if !d.intersects(&view) {
                    continue;
                }
                canvas.push_clip(snap(c), None);
                canvas.draw_image(image, d, *opacity, *smooth);
                canvas.pop_clip();
            }
            Item::TiledImage { image, area, tile, repeat_x, repeat_y } => {
                let a = snap(tr(area));
                if !a.intersects(&view) || tile.w <= 0.0 || tile.h <= 0.0 {
                    continue;
                }
                let t = tr(tile);
                canvas.push_clip(a, None);
                let vis = a.intersect(&view);
                let (x_start, x_end) = if *repeat_x {
                    let n0 = ((vis.x - t.x) / t.w).floor();
                    (t.x + n0 * t.w, vis.right())
                } else {
                    (t.x, t.x + 0.5)
                };
                let (y_start, y_end) = if *repeat_y {
                    let n0 = ((vis.y - t.y) / t.h).floor();
                    (t.y + n0 * t.h, vis.bottom())
                } else {
                    (t.y, t.y + 0.5)
                };
                let mut y = y_start;
                let mut guard = 0;
                while y < y_end && guard < 20_000 {
                    let mut x = x_start;
                    while x < x_end && guard < 20_000 {
                        canvas.draw_image(image, Rect::new(x, y, t.w, t.h), 1.0, true);
                        x += t.w;
                        guard += 1;
                    }
                    y += t.h;
                }
                canvas.pop_clip();
            }
            Item::Line { x0, y0, x1, y1, width, color, dashed } => {
                let w = (width * scale).max(1.0);
                let (ax, ay, bx, by) =
                    ((x0 - sx) * scale, (y0 - sy) * scale, (x1 - sx) * scale, (y1 - sy) * scale);
                if (ay - by).abs() < 0.01 {
                    // Horizontal line: snap to whole device pixels.
                    let yy = (ay - w / 2.0).round();
                    if *dashed {
                        let dash = (w * 3.0).max(3.0);
                        let mut x = ax;
                        while x < bx {
                            canvas.fill_rect(
                                Rect::new(x.round(), yy, dash.min(bx - x).round(), w.round().max(1.0)),
                                *color,
                            );
                            x += dash * 2.0;
                        }
                    } else {
                        canvas.fill_rect(
                            Rect::new(ax.round(), yy, (bx - ax).round(), w.round().max(1.0)),
                            *color,
                        );
                    }
                } else {
                    let mut p = Path::new();
                    p.move_to(ax, ay).line_to(bx, by);
                    canvas.fill_path(&p.stroke(w), &Paint::Solid(*color));
                }
            }
            Item::Shadow { rect, radii, blur, color, box_rect, box_radii } => {
                let r = tr(rect);
                let br = tr(box_rect);
                let b = blur * scale;
                if r.translate(-b * 2.0, -b * 2.0).union(&r.translate(b * 2.0, b * 2.0)).intersects(&view) {
                    canvas.box_shadow(r, rad(radii), b, *color, Some((br, rad(box_radii))));
                }
            }
            Item::Path { path, paint } => {
                canvas.fill_path(&path.transform(scale, scale, -sx * scale, -sy * scale), &tp(paint));
            }
            Item::PushClip { rect, radii } => canvas.push_clip(snap(tr(rect)), radii.map(|r| rad(&r))),
            Item::PopClip => canvas.pop_clip(),
            Item::PushOpacity(o) => canvas.push_layer(*o),
            Item::PopOpacity => canvas.pop_layer(),
        }
    }
}

/// Draw the four border sides as filled trapezoids (CSS Backgrounds 3 §4).
fn draw_border<C: Canvas + ?Sized>(
    canvas: &mut C,
    r: Rect,
    radii: Radii,
    sides: &[Option<BorderSide>; 4],
    scale: f32,
) {
    let w: Vec<f32> = sides
        .iter()
        .map(|s| {
            s.map(|s| (s.width * scale).round().max(if s.width > 0.0 { 1.0 } else { 0.0 })).unwrap_or(0.0)
        })
        .collect();
    let (t, rgt, b, l) = (w[0], w[1], w[2], w[3]);
    if t + rgt + b + l <= 0.0 {
        return;
    }
    if !radii.is_zero() {
        // Rounded border: outer rounded rect minus inner (all sides use top's color if uniform).
        let s0 = sides.iter().flatten().next().copied();
        if let Some(s0) = s0 {
            let mut p = Path::rounded_rect(r, radii);
            let inner = r.inset(t, rgt, b, l);
            p.add_rounded_rect(inner, radii.clamp_to(&r).shrink(t, rgt, b, l), true);
            canvas.fill_path(&p, &Paint::Solid(s0.color));
        }
        return;
    }
    let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
    let (ix0, iy0, ix1, iy1) = (x0 + l, y0 + t, x1 - rgt, y1 - b);
    let pts = [
        // top
        [Point::new(x0, y0), Point::new(x1, y0), Point::new(ix1, iy0), Point::new(ix0, iy0)],
        // right
        [Point::new(x1, y0), Point::new(x1, y1), Point::new(ix1, iy1), Point::new(ix1, iy0)],
        // bottom
        [Point::new(x1, y1), Point::new(x0, y1), Point::new(ix0, iy1), Point::new(ix1, iy1)],
        // left
        [Point::new(x0, y1), Point::new(x0, y0), Point::new(ix0, iy0), Point::new(ix0, iy1)],
    ];
    for (i, side) in sides.iter().enumerate() {
        let Some(s) = side else { continue };
        if w[i] <= 0.0 || s.color.a == 0 {
            continue;
        }
        let lighter = shade(s.color, 1.0).lerp(Color::WHITE, 0.35);
        let darker = shade(s.color, 0.55);
        let (top_left_color, bottom_right_color) = match s.style {
            BorderStyle::Inset => (darker, lighter),
            BorderStyle::Outset => (lighter, darker),
            _ => (s.color, s.color),
        };
        let color = match s.style {
            BorderStyle::Inset | BorderStyle::Outset => {
                if i == 0 || i == 3 {
                    top_left_color
                } else {
                    bottom_right_color
                }
            }
            _ => s.color,
        };
        match s.style {
            BorderStyle::Dashed | BorderStyle::Dotted => {
                let horizontal = i == 0 || i == 2;
                let thick = w[i];
                let seg = if s.style == BorderStyle::Dotted { thick } else { (thick * 3.0).max(3.0) };
                let (start, len) = if horizontal { (x0, r.w) } else { (y0, r.h) };
                let mut pos = start;
                let mut on = true;
                while pos < start + len {
                    let l2 = seg.min(start + len - pos);
                    if on {
                        let rect = match i {
                            0 => Rect::new(pos, y0, l2, thick),
                            2 => Rect::new(pos, y1 - thick, l2, thick),
                            1 => Rect::new(x1 - thick, pos, thick, l2),
                            _ => Rect::new(x0, pos, thick, l2),
                        };
                        if s.style == BorderStyle::Dotted && thick >= 3.0 {
                            canvas.fill_path(
                                &Path::ellipse(
                                    rect.x + rect.w / 2.0,
                                    rect.y + rect.h / 2.0,
                                    thick / 2.0,
                                    thick / 2.0,
                                ),
                                &Paint::Solid(color),
                            );
                        } else {
                            canvas.fill_rect(rect, color);
                        }
                    }
                    on = !on;
                    pos += seg;
                }
            }
            BorderStyle::Double if w[i] >= 3.0 => {
                let third = (w[i] / 3.0).round().max(1.0);
                let outer = [
                    pts[i][0],
                    pts[i][1],
                    lerp_pt(pts[i][1], pts[i][2], third / w[i]),
                    lerp_pt(pts[i][0], pts[i][3], third / w[i]),
                ];
                let inner = [
                    lerp_pt(pts[i][3], pts[i][0], third / w[i]),
                    lerp_pt(pts[i][2], pts[i][1], third / w[i]),
                    pts[i][2],
                    pts[i][3],
                ];
                canvas.fill_path(&Path::polygon(&outer), &Paint::Solid(color));
                canvas.fill_path(&Path::polygon(&inner), &Paint::Solid(color));
            }
            BorderStyle::Groove | BorderStyle::Ridge => {
                let half = 0.5;
                // Groove: top/left outer half dark; ridge is the inverse.
                let top_left = i == 0 || i == 3;
                let groove = s.style == BorderStyle::Groove;
                let (a, bcol) = if top_left == groove { (darker, lighter) } else { (lighter, darker) };
                let outer = [
                    pts[i][0],
                    pts[i][1],
                    lerp_pt(pts[i][1], pts[i][2], half),
                    lerp_pt(pts[i][0], pts[i][3], half),
                ];
                let inner = [
                    lerp_pt(pts[i][0], pts[i][3], half),
                    lerp_pt(pts[i][1], pts[i][2], half),
                    pts[i][2],
                    pts[i][3],
                ];
                canvas.fill_path(&Path::polygon(&outer), &Paint::Solid(a));
                canvas.fill_path(&Path::polygon(&inner), &Paint::Solid(bcol));
            }
            _ => {
                // Axis-aligned sides with square joins can use fast rects.
                let simple = (i == 0 || i == 2) && l == 0.0 && rgt == 0.0
                    || (i == 1 || i == 3) && t == 0.0 && b == 0.0;
                if simple {
                    let rect = match i {
                        0 => Rect::new(x0, y0, r.w, t),
                        2 => Rect::new(x0, y1 - b, r.w, b),
                        1 => Rect::new(x1 - rgt, y0, rgt, r.h),
                        _ => Rect::new(x0, y0, l, r.h),
                    };
                    canvas.fill_rect(rect, color);
                } else {
                    canvas.fill_path(&Path::polygon(&pts[i]), &Paint::Solid(color));
                }
            }
        }
    }
}

fn lerp_pt(a: Point, b: Point, t: f32) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// Convenience to build gradient paints for a box.
pub fn linear_gradient_for(rect: Rect, angle_deg: f32, stops: Vec<Stop>, repeating: bool) -> Paint {
    // CSS Images 3 §3.1.1: gradient line length for the box and angle.
    let a = angle_deg.to_radians();
    let (dx, dy) = (a.sin(), -a.cos());
    let len = (rect.w * a.sin()).abs() + (rect.h * a.cos()).abs();
    let (cx, cy) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
    Paint::Linear {
        x0: cx - dx * len / 2.0,
        y0: cy - dy * len / 2.0,
        x1: cx + dx * len / 2.0,
        y1: cy + dy * len / 2.0,
        stops,
        repeating,
    }
}

/// Shared handle type for decoded images inside display lists.
pub type SharedImage = Arc<bb_image::Image>;

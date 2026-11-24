//! The 2D rendering context's drawing model (HTML §4.12.5.1 "The 2D rendering context"):
//! a premultiplied RGBA bitmap, the drawing state stack, paths, the stroker (joins, caps,
//! dashes), fill and stroke styles (colors, linear, two-circle radial and conic gradients,
//! patterns), clipping, compositing and blending (Compositing and Blending §10),
//! shadows, images, and text through bb-text. bb-webapi exposes it as
//! CanvasRenderingContext2D.

use std::f32::consts::{PI, TAU};
use std::sync::Arc;

use crate::canvas::Color;
use crate::path::Point;
use crate::raster::{blur_mask, rasterize_polys_rule, Mask};

/// A 2D affine transform [a c e; b d f; 0 0 1] (DOMMatrix 2D components).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Default for Matrix {
    fn default() -> Self {
        Matrix::IDENTITY
    }
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Matrix {
        Matrix { a, b, c, d, e, f }
    }

    /// self × o (o applied first).
    pub fn mul(&self, o: &Matrix) -> Matrix {
        Matrix {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }

    pub fn apply(&self, x: f32, y: f32) -> Point {
        Point::new(self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }

    pub fn det(&self) -> f32 {
        self.a * self.d - self.b * self.c
    }

    pub fn invert(&self) -> Option<Matrix> {
        let det = self.det();
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let i = 1.0 / det;
        Some(Matrix {
            a: self.d * i,
            b: -self.b * i,
            c: -self.c * i,
            d: self.a * i,
            e: (self.c * self.f - self.d * self.e) * i,
            f: (self.b * self.e - self.a * self.f) * i,
        })
    }

    pub fn is_finite(&self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f].iter().all(|v| v.is_finite())
    }

    /// How much the transform scales lengths at most (for flattening tolerance).
    fn scale(&self) -> f32 {
        let sx = (self.a * self.a + self.b * self.b).sqrt();
        let sy = (self.c * self.c + self.d * self.d).sqrt();
        sx.max(sy).max(1e-6)
    }
}

/// A premultiplied RGBA8 bitmap.
#[derive(Clone, Debug, Default)]
pub struct Bitmap {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

impl Bitmap {
    pub fn new(w: u32, h: u32) -> Bitmap {
        let n = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        Bitmap { w, h, data: vec![0; n] }
    }

    /// Bilinear (or nearest) sample at (x, y) in pixel space, clamped to `bounds`
    /// (x0, y0, x1, y1); premultiplied, 0..255 floats.
    fn sample(&self, x: f32, y: f32, smooth: bool, bounds: (f32, f32, f32, f32)) -> [f32; 4] {
        let px = |ix: i32, iy: i32| -> [f32; 4] {
            let ix = ix.clamp(bounds.0 as i32, (bounds.2 as i32 - 1).max(bounds.0 as i32));
            let iy = iy.clamp(bounds.1 as i32, (bounds.3 as i32 - 1).max(bounds.1 as i32));
            if ix < 0 || iy < 0 || ix >= self.w as i32 || iy >= self.h as i32 {
                return [0.0; 4];
            }
            let i = (iy as usize * self.w as usize + ix as usize) * 4;
            [self.data[i] as f32, self.data[i + 1] as f32, self.data[i + 2] as f32, self.data[i + 3] as f32]
        };
        if !smooth {
            return px(x.floor() as i32, y.floor() as i32);
        }
        let (fx, fy) = (x - 0.5, y - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let (p00, p10, p01, p11) = (px(x0, y0), px(x0 + 1, y0), px(x0, y0 + 1), px(x0 + 1, y0 + 1));
        let mut out = [0.0; 4];
        for k in 0..4 {
            let top = p00[k] + (p10[k] - p00[k]) * tx;
            let bot = p01[k] + (p11[k] - p01[k]) * tx;
            out[k] = top + (bot - top) * ty;
        }
        out
    }
}

// ---- paths ----

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Line(Point),
    Quad(Point, Point),
    Cubic(Point, Point, Point),
}

#[derive(Clone, Debug, Default)]
pub struct SubPath {
    pub start: Point,
    pub segs: Vec<Seg>,
    pub closed: bool,
}

/// A canvas path (HTML "path" of the 2D context): subpaths of lines and Béziers. The context's
/// current default path holds points already transformed; a Path2D holds them as given.
#[derive(Clone, Debug, Default)]
pub struct CanvasPath {
    pub subpaths: Vec<SubPath>,
    /// The next point starts a new subpath ("need new subpath" flag).
    pub need_new: bool,
}

impl CanvasPath {
    pub fn new() -> CanvasPath {
        CanvasPath { subpaths: Vec::new(), need_new: true }
    }

    fn last_point(&self) -> Option<Point> {
        let s = self.subpaths.last()?;
        Some(match s.segs.last() {
            Some(Seg::Line(p)) | Some(Seg::Quad(_, p)) | Some(Seg::Cubic(_, _, p)) => *p,
            None => s.start,
        })
    }

    pub fn move_to(&mut self, p: Point) {
        if !finite(p) {
            return;
        }
        self.subpaths.push(SubPath { start: p, segs: Vec::new(), closed: false });
        self.need_new = false;
    }

    /// "ensure there is a subpath" for `p`.
    fn ensure(&mut self, p: Point) -> bool {
        if self.need_new || self.subpaths.is_empty() {
            self.move_to(p);
            return false;
        }
        true
    }

    pub fn line_to(&mut self, p: Point) {
        if !finite(p) {
            return;
        }
        if self.ensure(p) {
            if let Some(s) = self.subpaths.last_mut() {
                s.segs.push(Seg::Line(p));
            }
        }
    }

    pub fn quad_to(&mut self, c: Point, p: Point) {
        if !finite(c) || !finite(p) {
            return;
        }
        self.ensure(c);
        if let Some(s) = self.subpaths.last_mut() {
            s.segs.push(Seg::Quad(c, p));
        }
    }

    pub fn cubic_to(&mut self, c1: Point, c2: Point, p: Point) {
        if !finite(c1) || !finite(c2) || !finite(p) {
            return;
        }
        self.ensure(c1);
        if let Some(s) = self.subpaths.last_mut() {
            s.segs.push(Seg::Cubic(c1, c2, p));
        }
    }

    pub fn close(&mut self) {
        let Some(s) = self.subpaths.last_mut() else { return };
        s.closed = true;
        // A new subpath starts at the closed one's start.
        let start = s.start;
        self.subpaths.push(SubPath { start, segs: Vec::new(), closed: false });
    }

    /// The path with every point transformed by `m`.
    pub fn transformed(&self, m: &Matrix) -> CanvasPath {
        let t = |p: &Point| m.apply(p.x, p.y);
        CanvasPath {
            subpaths: self
                .subpaths
                .iter()
                .map(|s| SubPath {
                    start: t(&s.start),
                    closed: s.closed,
                    segs: s
                        .segs
                        .iter()
                        .map(|g| match g {
                            Seg::Line(p) => Seg::Line(t(p)),
                            Seg::Quad(c, p) => Seg::Quad(t(c), t(p)),
                            Seg::Cubic(a, b, p) => Seg::Cubic(t(a), t(b), t(p)),
                        })
                        .collect(),
                })
                .collect(),
            need_new: self.need_new,
        }
    }

    /// Append `o`'s subpaths (Path2D addPath).
    pub fn add(&mut self, o: &CanvasPath) {
        self.subpaths.extend(o.subpaths.iter().cloned());
        self.need_new = o.need_new && self.need_new;
    }

    /// Flatten to polylines (with whether each subpath is closed) at tolerance `tol`.
    pub fn flatten(&self, tol: f32) -> Vec<(Vec<Point>, bool)> {
        let mut out = Vec::new();
        for s in &self.subpaths {
            let mut pts = vec![s.start];
            let mut cur = s.start;
            for g in &s.segs {
                match *g {
                    Seg::Line(p) => {
                        pts.push(p);
                        cur = p;
                    }
                    Seg::Quad(c, p) => {
                        let n = curve_steps(&[cur, c, p], tol);
                        for i in 1..=n {
                            let t = i as f32 / n as f32;
                            let mt = 1.0 - t;
                            pts.push(Point::new(
                                mt * mt * cur.x + 2.0 * mt * t * c.x + t * t * p.x,
                                mt * mt * cur.y + 2.0 * mt * t * c.y + t * t * p.y,
                            ));
                        }
                        cur = p;
                    }
                    Seg::Cubic(a, b, p) => {
                        let n = curve_steps(&[cur, a, b, p], tol);
                        for i in 1..=n {
                            let t = i as f32 / n as f32;
                            let mt = 1.0 - t;
                            pts.push(Point::new(
                                mt * mt * mt * cur.x
                                    + 3.0 * mt * mt * t * a.x
                                    + 3.0 * mt * t * t * b.x
                                    + t * t * t * p.x,
                                mt * mt * mt * cur.y
                                    + 3.0 * mt * mt * t * a.y
                                    + 3.0 * mt * t * t * b.y
                                    + t * t * t * p.y,
                            ));
                        }
                        cur = p;
                    }
                }
            }
            out.push((pts, s.closed));
        }
        out
    }
}

fn finite(p: Point) -> bool {
    p.x.is_finite() && p.y.is_finite()
}

fn curve_steps(pts: &[Point], tol: f32) -> usize {
    let mut len = 0.0;
    for w in pts.windows(2) {
        len += ((w[1].x - w[0].x).powi(2) + (w[1].y - w[0].y).powi(2)).sqrt();
    }
    ((len / tol.max(0.05)).sqrt().ceil() as usize).clamp(1, 256)
}

/// Path building in user space with transform `m` (the context's current default path, or a
/// Path2D with the identity): arcs and the like.
pub struct PathBuilder<'a> {
    pub path: &'a mut CanvasPath,
    pub m: Matrix,
}

impl PathBuilder<'_> {
    fn t(&self, x: f32, y: f32) -> Point {
        self.m.apply(x, y)
    }

    pub fn move_to(&mut self, x: f32, y: f32) {
        let p = self.t(x, y);
        self.path.move_to(p);
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        let p = self.t(x, y);
        self.path.line_to(p);
    }

    pub fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (c, p) = (self.t(cx, cy), self.t(x, y));
        self.path.quad_to(c, p);
    }

    pub fn cubic_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        let (a, b, p) = (self.t(c1x, c1y), self.t(c2x, c2y), self.t(x, y));
        self.path.cubic_to(a, b, p);
    }

    pub fn close(&mut self) {
        self.path.close();
    }

    /// The last point, in user space.
    fn last_user(&self) -> Option<Point> {
        let p = self.path.last_point()?;
        let inv = self.m.invert()?;
        Some(inv.apply(p.x, p.y))
    }

    /// HTML rect(): a closed subpath, then a new subpath at (x, y).
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if ![x, y, w, h].iter().all(|v| v.is_finite()) {
            return;
        }
        self.move_to(x, y);
        self.line_to(x + w, y);
        self.line_to(x + w, y + h);
        self.line_to(x, y + h);
        self.close();
        self.move_to(x, y);
    }

    /// HTML roundRect() with corner radii (x, y) for upper-left, upper-right, lower-right,
    /// lower-left, already normalized.
    pub fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, radii: [(f32, f32); 4]) {
        if ![x, y, w, h].iter().all(|v| v.is_finite()) {
            return;
        }
        // Negative sizes flip the rectangle and its corners.
        let (mut ul, mut ur, mut lr, mut ll) = (radii[0], radii[1], radii[2], radii[3]);
        let (mut x, mut y, mut w, mut h) = (x, y, w, h);
        if w < 0.0 {
            x += w;
            w = -w;
            std::mem::swap(&mut ul, &mut ur);
            std::mem::swap(&mut ll, &mut lr);
        }
        if h < 0.0 {
            y += h;
            h = -h;
            std::mem::swap(&mut ul, &mut ll);
            std::mem::swap(&mut ur, &mut lr);
        }
        // Scale the radii down if they overlap (HTML roundRect step 9).
        let top = ul.0 + ur.0;
        let right = ur.1 + lr.1;
        let bottom = lr.0 + ll.0;
        let left = ul.1 + ll.1;
        let mut scale: f32 = 1.0;
        for (sum, side) in [(top, w), (right, h), (bottom, w), (left, h)] {
            if sum > side && sum > 0.0 {
                scale = scale.min(side / sum);
            }
        }
        let s = |r: (f32, f32)| (r.0 * scale, r.1 * scale);
        let (ul, ur, lr, ll) = (s(ul), s(ur), s(lr), s(ll));
        const K: f32 = 0.552_284_8;
        self.move_to(x + ul.0, y);
        self.line_to(x + w - ur.0, y);
        if ur.0 > 0.0 || ur.1 > 0.0 {
            self.cubic_to(x + w - ur.0 * (1.0 - K), y, x + w, y + ur.1 * (1.0 - K), x + w, y + ur.1);
        }
        self.line_to(x + w, y + h - lr.1);
        if lr.0 > 0.0 || lr.1 > 0.0 {
            self.cubic_to(
                x + w,
                y + h - lr.1 * (1.0 - K),
                x + w - lr.0 * (1.0 - K),
                y + h,
                x + w - lr.0,
                y + h,
            );
        }
        self.line_to(x + ll.0, y + h);
        if ll.0 > 0.0 || ll.1 > 0.0 {
            self.cubic_to(x + ll.0 * (1.0 - K), y + h, x, y + h - ll.1 * (1.0 - K), x, y + h - ll.1);
        }
        self.line_to(x, y + ul.1);
        if ul.0 > 0.0 || ul.1 > 0.0 {
            self.cubic_to(x, y + ul.1 * (1.0 - K), x + ul.0 * (1.0 - K), y, x + ul.0, y);
        }
        self.close();
        self.move_to(x, y);
    }

    /// HTML ellipse() (and arc() with rx = ry), after argument checks.
    #[allow(clippy::too_many_arguments)]
    pub fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, rot: f32, start: f32, end: f32, ccw: bool) {
        if ![cx, cy, rx, ry, rot, start, end].iter().all(|v| v.is_finite()) {
            return;
        }
        // The sweep (HTML: when |end − start| ≥ 2π the whole ellipse is drawn).
        let mut sweep = end - start;
        if !ccw {
            if sweep >= TAU {
                sweep = TAU;
            } else {
                sweep = sweep.rem_euclid(TAU);
            }
        } else if sweep <= -TAU {
            sweep = -TAU;
        } else {
            sweep = -((-sweep).rem_euclid(TAU));
        }
        let (cr, sr) = (rot.cos(), rot.sin());
        let pt = |a: f32| {
            let (x, y) = (rx * a.cos(), ry * a.sin());
            (cx + x * cr - y * sr, cy + x * sr + y * cr)
        };
        let p0 = pt(start);
        // A line from the current point to the start (or a new subpath there).
        if self.path.need_new || self.path.subpaths.is_empty() {
            self.move_to(p0.0, p0.1);
        } else {
            self.line_to(p0.0, p0.1);
        }
        if sweep == 0.0 {
            return;
        }
        let n = ((sweep.abs() / (PI / 2.0)).ceil() as usize).max(1);
        let step = sweep / n as f32;
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        let mut a = start;
        for _ in 0..n {
            let b = a + step;
            let (ca, sa, cb, sb) = (a.cos(), a.sin(), b.cos(), b.sin());
            let e = |x: f32, y: f32| (cx + rx * x * cr - ry * y * sr, cy + rx * x * sr + ry * y * cr);
            let c1 = e(ca - k * sa, sa + k * ca);
            let c2 = e(cb + k * sb, sb - k * cb);
            let p = e(cb, sb);
            self.cubic_to(c1.0, c1.1, c2.0, c2.1, p.0, p.1);
            a = b;
        }
    }

    /// HTML arcTo(x1, y1, x2, y2, radius), after argument checks.
    pub fn arc_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, r: f32) {
        if ![x1, y1, x2, y2, r].iter().all(|v| v.is_finite()) {
            return;
        }
        if self.path.need_new || self.path.subpaths.is_empty() {
            self.move_to(x1, y1);
        }
        let Some(p0) = self.last_user() else {
            self.line_to(x1, y1);
            return;
        };
        let (x0, y0) = (p0.x, p0.y);
        // Coincident points or r = 0: a line to (x1, y1).
        let coincident = (x0 == x1 && y0 == y1) || (x1 == x2 && y1 == y2) || r == 0.0;
        let cross = (x1 - x0) * (y2 - y1) - (y1 - y0) * (x2 - x1);
        if coincident || cross.abs() < 1e-9 {
            self.line_to(x1, y1);
            return;
        }
        let (v1x, v1y) = (x0 - x1, y0 - y1);
        let (v2x, v2y) = (x2 - x1, y2 - y1);
        let (l1, l2) = ((v1x * v1x + v1y * v1y).sqrt(), (v2x * v2x + v2y * v2y).sqrt());
        let cos = ((v1x * v2x + v1y * v2y) / (l1 * l2)).clamp(-1.0, 1.0);
        let theta = cos.acos();
        let d = r / (theta / 2.0).tan();
        let (t1x, t1y) = (x1 + v1x / l1 * d, y1 + v1y / l1 * d);
        let (t2x, t2y) = (x1 + v2x / l2 * d, y1 + v2y / l2 * d);
        // The centre: perpendicular from the first tangent point, on the inner side.
        let (bx, by) = (v1x / l1 + v2x / l2, v1y / l1 + v2y / l2);
        let bl = (bx * bx + by * by).sqrt();
        let h = (d * d + r * r).sqrt();
        let (ccx, ccy) = (x1 + bx / bl * h, y1 + by / bl * h);
        let a0 = (t1y - ccy).atan2(t1x - ccx);
        let a1 = (t2y - ccy).atan2(t2x - ccx);
        let ccw = cross < 0.0;
        self.line_to(t1x, t1y);
        let mut sweep = a1 - a0;
        if ccw && sweep > 0.0 {
            sweep -= TAU;
        } else if !ccw && sweep < 0.0 {
            sweep += TAU;
        }
        self.ellipse(ccx, ccy, r, r, 0.0, a0, a0 + sweep, ccw);
    }
}

// ---- styles ----

#[derive(Clone, Debug, PartialEq)]
pub enum GradientKind {
    Linear { x0: f32, y0: f32, x1: f32, y1: f32 },
    Radial { x0: f32, y0: f32, r0: f32, x1: f32, y1: f32, r1: f32 },
    Conic { angle: f32, x: f32, y: f32 },
}

/// A CanvasGradient: its geometry and color stops (in order added, sorted by offset).
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    pub kind: GradientKind,
    pub stops: Vec<(f32, Color)>,
}

impl Gradient {
    /// Insert a stop (HTML addColorStop: after stops at the same offset).
    pub fn add_stop(&mut self, offset: f32, c: Color) {
        let i = self.stops.iter().position(|s| s.0 > offset).unwrap_or(self.stops.len());
        self.stops.insert(i, (offset, c));
    }

    /// The color at t (clamped to the first and last stops), unpremultiplied 0..255.
    fn color_at(&self, t: f32) -> [f32; 4] {
        let s = &self.stops;
        let c = |c: &Color| [c.r as f32, c.g as f32, c.b as f32, c.a as f32];
        if s.is_empty() {
            return [0.0; 4];
        }
        if t <= s[0].0 {
            return c(&s[0].1);
        }
        if t >= s[s.len() - 1].0 {
            return c(&s[s.len() - 1].1);
        }
        for w in s.windows(2) {
            if t >= w[0].0 && t < w[1].0 {
                let span = w[1].0 - w[0].0;
                if span <= 0.0 {
                    return c(&w[1].1);
                }
                let k = (t - w[0].0) / span;
                let (a, b) = (c(&w[0].1), c(&w[1].1));
                return [
                    a[0] + (b[0] - a[0]) * k,
                    a[1] + (b[1] - a[1]) * k,
                    a[2] + (b[2] - a[2]) * k,
                    a[3] + (b[3] - a[3]) * k,
                ];
            }
        }
        c(&s[s.len() - 1].1)
    }

    /// The gradient's color at user-space point p, premultiplied 0..255 (None: nothing).
    fn sample(&self, x: f32, y: f32) -> Option<[f32; 4]> {
        let t = match self.kind {
            GradientKind::Linear { x0, y0, x1, y1 } => {
                let (dx, dy) = (x1 - x0, y1 - y0);
                let l2 = dx * dx + dy * dy;
                if l2 == 0.0 {
                    return None;
                }
                ((x - x0) * dx + (y - y0) * dy) / l2
            }
            GradientKind::Radial { x0, y0, r0, x1, y1, r1 } => {
                if x0 == x1 && y0 == y1 && r0 == r1 {
                    return None;
                }
                radial_t(x, y, x0, y0, r0, x1, y1, r1)?
            }
            GradientKind::Conic { angle, x: cx, y: cy } => {
                let a = (y - cy).atan2(x - cx) - angle;
                (a / TAU).rem_euclid(1.0)
            }
        };
        let c = self.color_at(t);
        let a = c[3] / 255.0;
        Some([c[0] * a, c[1] * a, c[2] * a, c[3]])
    }
}

/// HTML createRadialGradient's rendering: the largest ω with r(ω) ≥ 0 whose circle passes
/// through the point.
#[allow(clippy::too_many_arguments)]
fn radial_t(x: f32, y: f32, x0: f32, y0: f32, r0: f32, x1: f32, y1: f32, r1: f32) -> Option<f32> {
    let (cdx, cdy, dr) = ((x1 - x0) as f64, (y1 - y0) as f64, (r1 - r0) as f64);
    let (px, py) = ((x - x0) as f64, (y - y0) as f64);
    let a = cdx * cdx + cdy * cdy - dr * dr;
    let b = px * cdx + py * cdy + r0 as f64 * dr;
    let c = px * px + py * py - (r0 as f64) * (r0 as f64);
    let ok = |w: f64| r0 as f64 + w * dr >= 0.0;
    if a.abs() < 1e-9 {
        if b.abs() < 1e-12 {
            return None;
        }
        let w = c / (2.0 * b);
        return ok(w).then_some(w as f32);
    }
    let disc = b * b - a * c;
    if disc < 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let (w1, w2) = ((b + sq) / a, (b - sq) / a);
    let (hi, lo) = if w1 > w2 { (w1, w2) } else { (w2, w1) };
    if ok(hi) {
        Some(hi as f32)
    } else if ok(lo) {
        Some(lo as f32)
    } else {
        None
    }
}

/// A CanvasPattern: an image, its repetition, and its transform.
#[derive(Clone, Debug)]
pub struct Pattern {
    pub image: Arc<Bitmap>,
    pub repeat_x: bool,
    pub repeat_y: bool,
    pub transform: Matrix,
}

#[derive(Clone, Debug)]
pub enum Style {
    Color(Color),
    Gradient(Arc<Gradient>),
    Pattern(Arc<Pattern>),
}

impl Default for Style {
    fn default() -> Self {
        Style::Color(Color::rgba(0, 0, 0, 255))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LineJoin {
    Round,
    Bevel,
    #[default]
    Miter,
}

/// globalCompositeOperation: Porter-Duff operators and blend modes (Compositing and Blending §10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Composite {
    #[default]
    SourceOver,
    Clear,
    SourceIn,
    SourceOut,
    SourceAtop,
    DestinationOver,
    DestinationIn,
    DestinationOut,
    DestinationAtop,
    Lighter,
    Copy,
    Xor,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl Composite {
    pub fn parse(s: &str) -> Option<Composite> {
        use Composite::*;
        Some(match s {
            "source-over" => SourceOver,
            "clear" => Clear,
            "source-in" => SourceIn,
            "source-out" => SourceOut,
            "source-atop" => SourceAtop,
            "destination-over" => DestinationOver,
            "destination-in" => DestinationIn,
            "destination-out" => DestinationOut,
            "destination-atop" => DestinationAtop,
            "lighter" => Lighter,
            "copy" => Copy,
            "xor" => Xor,
            "multiply" => Multiply,
            "screen" => Screen,
            "overlay" => Overlay,
            "darken" => Darken,
            "lighten" => Lighten,
            "color-dodge" => ColorDodge,
            "color-burn" => ColorBurn,
            "hard-light" => HardLight,
            "soft-light" => SoftLight,
            "difference" => Difference,
            "exclusion" => Exclusion,
            "hue" => Hue,
            "saturation" => Saturation,
            "color" => Color,
            "luminosity" => Luminosity,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        use Composite::*;
        match self {
            SourceOver => "source-over",
            Clear => "clear",
            SourceIn => "source-in",
            SourceOut => "source-out",
            SourceAtop => "source-atop",
            DestinationOver => "destination-over",
            DestinationIn => "destination-in",
            DestinationOut => "destination-out",
            DestinationAtop => "destination-atop",
            Lighter => "lighter",
            Copy => "copy",
            Xor => "xor",
            Multiply => "multiply",
            Screen => "screen",
            Overlay => "overlay",
            Darken => "darken",
            Lighten => "lighten",
            ColorDodge => "color-dodge",
            ColorBurn => "color-burn",
            HardLight => "hard-light",
            SoftLight => "soft-light",
            Difference => "difference",
            Exclusion => "exclusion",
            Hue => "hue",
            Saturation => "saturation",
            Color => "color",
            Luminosity => "luminosity",
        }
    }

    /// Operators that change pixels outside the source shape (HTML: compositing with a
    /// transparent source there).
    fn unbounded(self) -> bool {
        matches!(
            self,
            Composite::Clear
                | Composite::SourceIn
                | Composite::SourceOut
                | Composite::DestinationIn
                | Composite::DestinationAtop
                | Composite::Copy
        )
    }
}

/// Composite premultiplied `s` over `d` (0..1 floats) with operator `op`.
fn composite(op: Composite, s: [f32; 4], d: [f32; 4]) -> [f32; 4] {
    let (sa, da) = (s[3], d[3]);
    let pd = |fa: f32, fb: f32| {
        [s[0] * fa + d[0] * fb, s[1] * fa + d[1] * fb, s[2] * fa + d[2] * fb, sa * fa + da * fb]
    };
    use Composite::*;
    match op {
        SourceOver => pd(1.0, 1.0 - sa),
        // Clear: nothing where the shape is (the coverage lerps toward the destination).
        Clear => [d[0] * (1.0 - sa), d[1] * (1.0 - sa), d[2] * (1.0 - sa), da * (1.0 - sa)],
        SourceIn => pd(da, 0.0),
        SourceOut => pd(1.0 - da, 0.0),
        SourceAtop => pd(da, 1.0 - sa),
        DestinationOver => pd(1.0 - da, 1.0),
        DestinationIn => pd(0.0, sa),
        DestinationOut => pd(0.0, 1.0 - sa),
        DestinationAtop => pd(1.0 - da, sa),
        Lighter => {
            let r = pd(1.0, 1.0);
            [r[0].min(1.0), r[1].min(1.0), r[2].min(1.0), r[3].min(1.0)]
        }
        Copy => s,
        Xor => pd(1.0 - da, 1.0 - sa),
        _ => blend(op, s, d),
    }
}

/// Compositing and Blending §10: blend mode `op` with source-over compositing.
fn blend(op: Composite, s: [f32; 4], d: [f32; 4]) -> [f32; 4] {
    let (sa, da) = (s[3], d[3]);
    let un = |c: [f32; 4]| {
        if c[3] > 0.0 {
            [c[0] / c[3], c[1] / c[3], c[2] / c[3]]
        } else {
            [0.0; 3]
        }
    };
    let (cs, cb) = (un(s), un(d));
    let sep = |f: fn(f32, f32) -> f32| [f(cb[0], cs[0]), f(cb[1], cs[1]), f(cb[2], cs[2])];
    use Composite::*;
    let b = match op {
        Multiply => sep(|b, s| b * s),
        Screen => sep(|b, s| b + s - b * s),
        Overlay => sep(|b, s| hard_light(s, b)),
        Darken => sep(f32::min),
        Lighten => sep(f32::max),
        ColorDodge => sep(|b, s| {
            if b == 0.0 {
                0.0
            } else if s >= 1.0 {
                1.0
            } else {
                (b / (1.0 - s)).min(1.0)
            }
        }),
        ColorBurn => sep(|b, s| {
            if b >= 1.0 {
                1.0
            } else if s <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - b) / s).min(1.0)
            }
        }),
        HardLight => sep(hard_light),
        SoftLight => sep(|b, s| {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let dd = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
                b + (2.0 * s - 1.0) * (dd - b)
            }
        }),
        Difference => sep(|b, s| (b - s).abs()),
        Exclusion => sep(|b, s| b + s - 2.0 * b * s),
        Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        Color => set_lum(cs, lum(cb)),
        Luminosity => set_lum(cb, lum(cs)),
        _ => cs,
    };
    let mut out = [0.0; 4];
    for i in 0..3 {
        out[i] = s[i] * (1.0 - da) + d[i] * (1.0 - sa) + sa * da * b[i];
    }
    out[3] = sa + da * (1.0 - sa);
    out
}

fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b * 2.0 * s
    } else {
        let s2 = 2.0 * s - 1.0;
        b + s2 - b * s2
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut c = c;
    if n < 0.0 {
        for v in &mut c {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for v in &mut c {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    c
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mx = c[0].max(c[1]).max(c[2]);
    let mn = c[0].min(c[1]).min(c[2]);
    if mx <= mn {
        return [0.0; 3];
    }
    let mut out = [0.0; 3];
    for i in 0..3 {
        out[i] = if c[i] == mx {
            s
        } else if c[i] == mn {
            0.0
        } else {
            (c[i] - mn) * s / (mx - mn)
        };
    }
    out
}

// ---- the drawing state (HTML §4.12.5.1.2) ----

#[derive(Clone, Debug)]
pub struct State {
    pub transform: Matrix,
    pub fill: Style,
    pub stroke: Style,
    pub line_width: f32,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f32,
    pub dash: Vec<f32>,
    pub dash_offset: f32,
    pub alpha: f32,
    pub composite: Composite,
    pub shadow_color: Color,
    pub shadow_blur: f32,
    pub shadow_x: f32,
    pub shadow_y: f32,
    /// The clipping region as a full-canvas coverage mask (None: no clip).
    pub clip: Option<Arc<Mask>>,
    pub font: String,
    pub text_align: String,
    pub text_baseline: String,
    pub direction: String,
    pub smoothing: bool,
    pub letter_spacing: f32,
}

impl Default for State {
    fn default() -> Self {
        State {
            transform: Matrix::IDENTITY,
            fill: Style::default(),
            stroke: Style::default(),
            line_width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            alpha: 1.0,
            composite: Composite::SourceOver,
            shadow_color: Color::rgba(0, 0, 0, 0),
            shadow_blur: 0.0,
            shadow_x: 0.0,
            shadow_y: 0.0,
            clip: None,
            font: "10px sans-serif".to_string(),
            text_align: "start".to_string(),
            text_baseline: "alphabetic".to_string(),
            direction: "inherit".to_string(),
            smoothing: true,
            letter_spacing: 0.0,
        }
    }
}

/// A shape to draw: what covers which pixels, and how its paint is found.
enum Source<'a> {
    Style(&'a Style),
    Image { img: &'a Bitmap, inv: Matrix, bounds: (f32, f32, f32, f32), smooth: bool },
}

/// The context: its bitmap, state stack and current default path.
pub struct Context2D {
    pub bitmap: Bitmap,
    pub state: State,
    pub stack: Vec<State>,
    pub path: CanvasPath,
    /// The canvas has no alpha channel ({ alpha: false }): it starts opaque black.
    pub opaque: bool,
}

impl Context2D {
    pub fn new(w: u32, h: u32) -> Context2D {
        Context2D {
            bitmap: Bitmap::new(w, h),
            state: State::default(),
            stack: Vec::new(),
            path: CanvasPath::new(),
            opaque: false,
        }
    }

    /// Setting the canvas's width or height: a new transparent bitmap and the initial state.
    pub fn reset(&mut self, w: u32, h: u32) {
        self.bitmap = Bitmap::new(w, h);
        if self.opaque {
            for px in self.bitmap.data.chunks_mut(4) {
                px[3] = 255;
            }
        }
        self.state = State::default();
        self.stack.clear();
        self.path = CanvasPath::new();
    }

    pub fn save(&mut self) {
        if self.stack.len() < 1024 {
            self.stack.push(self.state.clone());
        }
    }

    pub fn restore(&mut self) {
        if let Some(s) = self.stack.pop() {
            self.state = s;
        }
    }

    /// The current default path's builder (points transformed by the current transform).
    pub fn builder(&mut self) -> PathBuilder<'_> {
        let m = self.state.transform;
        PathBuilder { path: &mut self.path, m }
    }

    fn clip_rect(&self) -> (i32, i32, i32, i32) {
        (0, 0, self.bitmap.w as i32, self.bitmap.h as i32)
    }

    fn tolerance(&self) -> f32 {
        0.2
    }

    /// Coverage of `path` (device coordinates) with a fill rule.
    fn coverage(&self, polys: &[Vec<Point>], even_odd: bool) -> Option<Mask> {
        rasterize_polys_rule(polys, self.clip_rect(), even_odd)
    }

    /// Fill `path` (device coordinates).
    pub fn fill_device(&mut self, path: &CanvasPath, even_odd: bool) {
        let polys: Vec<Vec<Point>> = path.flatten(self.tolerance()).into_iter().map(|(p, _)| p).collect();
        let style = self.state.fill.clone();
        let mask = self.coverage(&polys, even_odd);
        self.paint(mask, &Source::Style(&style));
    }

    pub fn fill(&mut self, path: Option<&CanvasPath>, even_odd: bool) {
        match path {
            None => {
                let p = self.path.clone();
                self.fill_device(&p, even_odd);
            }
            Some(p) => {
                let t = p.transformed(&self.state.transform);
                self.fill_device(&t, even_odd);
            }
        }
    }

    /// Stroke `path` (device coordinates if `device`, else user coordinates).
    pub fn stroke(&mut self, path: Option<&CanvasPath>) {
        let m = self.state.transform;
        // Stroke in user space, then transform the outline: line widths follow the transform.
        let user = match path {
            Some(p) => p.clone(),
            None => match m.invert() {
                Some(inv) => self.path.transformed(&inv),
                None => return,
            },
        };
        let polys = self.stroke_polys(&user, &m);
        let style = self.state.stroke.clone();
        let mask = self.coverage(&polys, false);
        self.paint(mask, &Source::Style(&style));
    }

    /// The stroke outline of `user` (user space) as device-space polygons.
    fn stroke_polys(&self, user: &CanvasPath, m: &Matrix) -> Vec<Vec<Point>> {
        let s = &self.state;
        // NaN widths too: only a positive width strokes.
        if s.line_width.is_nan() || s.line_width <= 0.0 || !m.is_finite() {
            return Vec::new();
        }
        let tol = self.tolerance() / m.scale();
        let lines = user.flatten(tol);
        let lines = if s.dash.is_empty() { lines } else { dash(&lines, &s.dash, s.dash_offset) };
        let out = stroke_outline(&lines, s.line_width, s.cap, s.join, s.miter_limit);
        out.into_iter().map(|poly| poly.into_iter().map(|p| m.apply(p.x, p.y)).collect()).collect()
    }

    /// Intersect the clip with `path` (device coordinates).
    pub fn clip(&mut self, path: Option<&CanvasPath>, even_odd: bool) {
        let p = match path {
            None => self.path.clone(),
            Some(p) => p.transformed(&self.state.transform),
        };
        let polys: Vec<Vec<Point>> = p.flatten(self.tolerance()).into_iter().map(|(p, _)| p).collect();
        let (w, h) = (self.bitmap.w, self.bitmap.h);
        let mut full = Mask { x: 0, y: 0, w, h, data: vec![0; (w * h) as usize] };
        if let Some(m) = self.coverage(&polys, even_odd) {
            for y in 0..m.h {
                for x in 0..m.w {
                    let (gx, gy) = (m.x + x as i32, m.y + y as i32);
                    full.data[(gy as u32 * w + gx as u32) as usize] = m.data[(y * m.w + x) as usize];
                }
            }
        }
        if let Some(old) = &self.state.clip {
            for (d, o) in full.data.iter_mut().zip(old.data.iter()) {
                *d = ((*d as u32 * *o as u32 + 127) / 255) as u8;
            }
        }
        self.state.clip = Some(Arc::new(full));
    }

    /// isPointInPath at canvas point (x, y).
    pub fn point_in_path(&self, path: Option<&CanvasPath>, x: f32, y: f32, even_odd: bool) -> bool {
        if !x.is_finite() || !y.is_finite() {
            return false;
        }
        let p = match path {
            None => self.path.clone(),
            Some(p) => p.transformed(&self.state.transform),
        };
        let polys: Vec<Vec<Point>> = p.flatten(self.tolerance()).into_iter().map(|(p, _)| p).collect();
        let w = winding(&polys, x, y);
        // Points on the boundary count as inside.
        if on_boundary(&polys, x, y) {
            return true;
        }
        if even_odd {
            w % 2 != 0
        } else {
            w != 0
        }
    }

    /// isPointInStroke at canvas point (x, y).
    pub fn point_in_stroke(&self, path: Option<&CanvasPath>, x: f32, y: f32) -> bool {
        if !x.is_finite() || !y.is_finite() {
            return false;
        }
        let m = self.state.transform;
        let user = match path {
            Some(p) => p.clone(),
            None => match m.invert() {
                Some(inv) => self.path.transformed(&inv),
                None => return false,
            },
        };
        let polys = self.stroke_polys(&user, &m);
        winding(&polys, x, y) != 0 || on_boundary(&polys, x, y)
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if ![x, y, w, h].iter().all(|v| v.is_finite()) || w == 0.0 || h == 0.0 {
            return;
        }
        let mut p = CanvasPath::new();
        PathBuilder { path: &mut p, m: self.state.transform }.rect(x, y, w, h);
        self.fill_device(&p, false);
    }

    pub fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if ![x, y, w, h].iter().all(|v| v.is_finite()) {
            return;
        }
        let mut p = CanvasPath::new();
        let mut b = PathBuilder { path: &mut p, m: Matrix::IDENTITY };
        if w == 0.0 && h == 0.0 {
            return;
        }
        if w == 0.0 || h == 0.0 {
            b.move_to(x, y);
            b.line_to(x + w, y + h);
        } else {
            b.rect(x, y, w, h);
        }
        self.stroke(Some(&p));
    }

    /// clearRect: transparent black inside the (transformed) rectangle and the clip.
    pub fn clear_rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if ![x, y, w, h].iter().all(|v| v.is_finite()) || w == 0.0 || h == 0.0 {
            return;
        }
        let mut p = CanvasPath::new();
        PathBuilder { path: &mut p, m: self.state.transform }.rect(x, y, w, h);
        let polys: Vec<Vec<Point>> = p.flatten(0.2).into_iter().map(|(p, _)| p).collect();
        let Some(mask) = self.coverage(&polys, false) else { return };
        let bw = self.bitmap.w;
        for my in 0..mask.h {
            for mx in 0..mask.w {
                let (gx, gy) = (mask.x + mx as i32, mask.y + my as i32);
                let mut c = mask.data[(my * mask.w + mx) as usize] as u32;
                if let Some(cl) = &self.state.clip {
                    c = c * cl.data[(gy as u32 * bw + gx as u32) as usize] as u32 / 255;
                }
                if c == 0 {
                    continue;
                }
                let i = ((gy as u32 * bw + gx as u32) * 4) as usize;
                let keep = 255 - c;
                for k in 0..4 {
                    self.bitmap.data[i + k] = (self.bitmap.data[i + k] as u32 * keep / 255) as u8;
                }
                if self.opaque {
                    self.bitmap.data[i + 3] = 255;
                }
            }
        }
    }

    /// drawImage: `img`'s source rectangle drawn into the destination rectangle.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_image(
        &mut self,
        img: &Bitmap,
        sx: f32,
        sy: f32,
        sw: f32,
        sh: f32,
        dx: f32,
        dy: f32,
        dw: f32,
        dh: f32,
    ) {
        if ![sx, sy, sw, sh, dx, dy, dw, dh].iter().all(|v| v.is_finite())
            || sw == 0.0
            || sh == 0.0
            || dw == 0.0
            || dh == 0.0
        {
            return;
        }
        // Normalize negative sizes.
        let (sx, sw) = if sw < 0.0 { (sx + sw, -sw) } else { (sx, sw) };
        let (sy, sh) = if sh < 0.0 { (sy + sh, -sh) } else { (sy, sh) };
        let (dx, dw) = if dw < 0.0 { (dx + dw, -dw) } else { (dx, dw) };
        let (dy, dh) = if dh < 0.0 { (dy + dh, -dh) } else { (dy, dh) };
        let mut p = CanvasPath::new();
        PathBuilder { path: &mut p, m: self.state.transform }.rect(dx, dy, dw, dh);
        let polys: Vec<Vec<Point>> = p.flatten(0.2).into_iter().map(|(p, _)| p).collect();
        let mask = self.coverage(&polys, false);
        // Device → source pixel space.
        let to_src = Matrix::new(sw / dw, 0.0, 0.0, sh / dh, sx - dx * sw / dw, sy - dy * sh / dh);
        let Some(inv) = self.state.transform.invert() else { return };
        let inv = to_src.mul(&inv);
        let bounds = (sx.max(0.0), sy.max(0.0), (sx + sw).min(img.w as f32), (sy + sh).min(img.h as f32));
        let smooth = self.state.smoothing;
        self.paint(mask, &Source::Image { img, inv, bounds, smooth });
    }

    /// getImageData: unpremultiplied RGBA of the rectangle (outside the bitmap: transparent).
    pub fn get_image_data(&self, sx: i32, sy: i32, sw: u32, sh: u32) -> Vec<u8> {
        let mut out = vec![0u8; (sw as usize) * (sh as usize) * 4];
        for y in 0..sh as i32 {
            for x in 0..sw as i32 {
                let (gx, gy) = (sx + x, sy + y);
                if gx < 0 || gy < 0 || gx >= self.bitmap.w as i32 || gy >= self.bitmap.h as i32 {
                    continue;
                }
                let i = ((gy as u32 * self.bitmap.w + gx as u32) * 4) as usize;
                let o = ((y as u32 * sw + x as u32) * 4) as usize;
                let a = self.bitmap.data[i + 3] as u32;
                if a == 0 {
                    continue;
                }
                for k in 0..3 {
                    out[o + k] = ((self.bitmap.data[i + k] as u32 * 255 + a / 2) / a).min(255) as u8;
                }
                out[o + 3] = a as u8;
            }
        }
        out
    }

    /// putImageData: the unpremultiplied `data` (w × h) written at (dx, dy), only the dirty
    /// rectangle (relative to the data), ignoring the state.
    #[allow(clippy::too_many_arguments)]
    pub fn put_image_data(
        &mut self,
        data: &[u8],
        w: u32,
        h: u32,
        dx: i32,
        dy: i32,
        dirty: (i32, i32, i32, i32),
    ) {
        let (mut x0, mut y0, mut x1, mut y1) = (dirty.0, dirty.1, dirty.0 + dirty.2, dirty.1 + dirty.3);
        if x0 > x1 {
            std::mem::swap(&mut x0, &mut x1);
        }
        if y0 > y1 {
            std::mem::swap(&mut y0, &mut y1);
        }
        let (x0, y0) = (x0.max(0), y0.max(0));
        let (x1, y1) = (x1.min(w as i32), y1.min(h as i32));
        for y in y0..y1 {
            for x in x0..x1 {
                let (gx, gy) = (dx + x, dy + y);
                if gx < 0 || gy < 0 || gx >= self.bitmap.w as i32 || gy >= self.bitmap.h as i32 {
                    continue;
                }
                let s = ((y as u32 * w + x as u32) * 4) as usize;
                let Some(px) = data.get(s..s + 4) else { continue };
                let a = px[3] as u32;
                let i = ((gy as u32 * self.bitmap.w + gx as u32) * 4) as usize;
                for k in 0..3 {
                    self.bitmap.data[i + k] = ((px[k] as u32 * a + 127) / 255) as u8;
                }
                self.bitmap.data[i + 3] = if self.opaque { 255 } else { a as u8 };
            }
        }
    }

    /// Paint a shape with coverage `mask` from `src`: shadow first, then the shape, through
    /// the clip, with global alpha and the composite operator.
    fn paint(&mut self, mask: Option<Mask>, src: &Source) {
        let s = &self.state;
        let shadow = s.shadow_color.a > 0 && (s.shadow_blur > 0.0 || s.shadow_x != 0.0 || s.shadow_y != 0.0);
        if shadow {
            if let Some(m) = &mask {
                self.paint_shadow(m, src);
            }
        }
        self.composite_mask(mask.as_ref(), |ctx, x, y| ctx.source_color(src, x, y));
    }

    fn paint_shadow(&mut self, m: &Mask, src: &Source) {
        let s = self.state.clone();
        // The shape's alpha (coverage × paint alpha), then blurred and offset.
        let mut alpha = Mask { x: m.x, y: m.y, w: m.w, h: m.h, data: vec![0; m.data.len()] };
        for y in 0..m.h {
            for x in 0..m.w {
                let c = m.data[(y * m.w + x) as usize] as f32;
                if c == 0.0 {
                    continue;
                }
                let p = self.source_color(src, m.x + x as i32, m.y + y as i32);
                alpha.data[(y * m.w + x) as usize] = (c * p[3]).round().clamp(0.0, 255.0) as u8;
            }
        }
        let sigma = s.shadow_blur / 2.0;
        let mut sh = if sigma > 0.0 {
            // Room for the blur around the shape.
            let pad = (sigma * 3.0).ceil() as i32;
            let mut padded = Mask {
                x: alpha.x - pad,
                y: alpha.y - pad,
                w: alpha.w + 2 * pad as u32,
                h: alpha.h + 2 * pad as u32,
                data: vec![0; ((alpha.w + 2 * pad as u32) * (alpha.h + 2 * pad as u32)) as usize],
            };
            for y in 0..alpha.h {
                for x in 0..alpha.w {
                    padded.data[((y + pad as u32) * padded.w + x + pad as u32) as usize] =
                        alpha.data[(y * alpha.w + x) as usize];
                }
            }
            blur_mask(&padded, sigma)
        } else {
            alpha
        };
        sh.x += s.shadow_x.round() as i32;
        sh.y += s.shadow_y.round() as i32;
        // Clip to the canvas.
        let c = s.shadow_color;
        let col = [
            c.r as f32 * c.a as f32 / 255.0,
            c.g as f32 * c.a as f32 / 255.0,
            c.b as f32 * c.a as f32 / 255.0,
            c.a as f32,
        ];
        let cropped = crop(&sh, self.bitmap.w, self.bitmap.h);
        self.composite_mask(cropped.as_ref(), |_, _, _| col);
    }

    /// The source's premultiplied color (0..255) at device pixel (x, y).
    fn source_color(&self, src: &Source, x: i32, y: i32) -> [f32; 4] {
        let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
        match src {
            Source::Style(Style::Color(c)) => {
                let a = c.a as f32 / 255.0;
                [c.r as f32 * a, c.g as f32 * a, c.b as f32 * a, c.a as f32]
            }
            Source::Style(Style::Gradient(g)) => {
                let Some(inv) = self.state.transform.invert() else { return [0.0; 4] };
                let p = inv.apply(cx, cy);
                g.sample(p.x, p.y).unwrap_or([0.0; 4])
            }
            Source::Style(Style::Pattern(pat)) => {
                let Some(inv) = self.state.transform.mul(&pat.transform).invert() else { return [0.0; 4] };
                let p = inv.apply(cx, cy);
                let (w, h) = (pat.image.w as f32, pat.image.h as f32);
                if w == 0.0 || h == 0.0 {
                    return [0.0; 4];
                }
                let (mut px, mut py) = (p.x, p.y);
                if pat.repeat_x {
                    px = px.rem_euclid(w);
                } else if px < 0.0 || px >= w {
                    return [0.0; 4];
                }
                if pat.repeat_y {
                    py = py.rem_euclid(h);
                } else if py < 0.0 || py >= h {
                    return [0.0; 4];
                }
                pat.image.sample(px, py, false, (0.0, 0.0, w, h))
            }
            Source::Image { img, inv, bounds, smooth } => {
                let p = inv.apply(cx, cy);
                img.sample(p.x, p.y, *smooth, *bounds)
            }
        }
    }

    /// Composite `color(x, y)` (premultiplied 0..255) through `mask` × clip × global alpha.
    fn composite_mask(&mut self, mask: Option<&Mask>, color: impl Fn(&Context2D, i32, i32) -> [f32; 4]) {
        let op = self.state.composite;
        let alpha = self.state.alpha.clamp(0.0, 1.0);
        let (bw, bh) = (self.bitmap.w as i32, self.bitmap.h as i32);
        // Unbounded operators touch the whole clip region; others only the mask.
        let (x0, y0, x1, y1) = if op.unbounded() {
            (0, 0, bw, bh)
        } else {
            match mask {
                Some(m) => (m.x.max(0), m.y.max(0), (m.x + m.w as i32).min(bw), (m.y + m.h as i32).min(bh)),
                None => return,
            }
        };
        let clip = self.state.clip.clone();
        for y in y0..y1 {
            for x in x0..x1 {
                let cov = mask.map_or(0, |m| m.get(x, y)) as f32 / 255.0;
                let clip_cov = clip.as_ref().map_or(1.0, |c| c.data[(y * bw + x) as usize] as f32 / 255.0);
                if clip_cov == 0.0 || (cov == 0.0 && !op.unbounded()) {
                    continue;
                }
                let k = cov * alpha;
                let sc = if k > 0.0 { color(self, x, y) } else { [0.0; 4] };
                let s = [sc[0] / 255.0 * k, sc[1] / 255.0 * k, sc[2] / 255.0 * k, sc[3] / 255.0 * k];
                let i = ((y * bw + x) * 4) as usize;
                let dpx = &self.bitmap.data[i..i + 4];
                let d = [
                    dpx[0] as f32 / 255.0,
                    dpx[1] as f32 / 255.0,
                    dpx[2] as f32 / 255.0,
                    dpx[3] as f32 / 255.0,
                ];
                let r = composite(op, s, d);
                // The clip lerps between the old and new pixel.
                for kk in 0..4 {
                    let v = d[kk] + (r[kk] - d[kk]) * clip_cov;
                    self.bitmap.data[i + kk] = (v * 255.0).round().clamp(0.0, 255.0) as u8;
                }
                if self.opaque {
                    self.bitmap.data[i + 3] = 255;
                }
            }
        }
    }

    // ---- text ----

    /// The current font resolved: (font list, size in px).
    fn font(&self) -> (bb_text::FontList, f32) {
        let f =
            parse_font(&self.state.font).unwrap_or_else(|| parse_font("10px sans-serif").unwrap_or_default());
        let mut fs = bb_text::FontSystem::lock();
        let list = fs.resolve(&f.families, f.weight, f.italic);
        (list, f.size)
    }

    /// Shape `text` in the current font: glyph runs, total width, and metrics.
    fn shape(&self, text: &str) -> (Arc<bb_text::ShapedText>, bb_text::Metrics, f32) {
        let (list, size) = self.font();
        let mut fs = bb_text::FontSystem::lock();
        let shaped = fs.shape(text, &list, size);
        let metrics = fs.metrics(list.primary());
        (shaped, metrics, size)
    }

    /// HTML measureText (TextMetrics): width, actual and font bounding boxes, baselines.
    pub fn measure_text(&self, text: &str) -> TextMetrics {
        let text: String =
            text.chars().map(|c| if matches!(c, '\t' | '\n' | '\x0C' | '\r') { ' ' } else { c }).collect();
        let (shaped, m, size) = self.shape(&text);
        let width = shaped.width + self.state.letter_spacing * text.chars().count() as f32;
        let (ascent, descent) = (m.ascent * size, m.descent * size);
        let anchor = self.anchor_x(width);
        let base = self.baseline_offset(ascent, descent);
        // The actual bounding box from the glyph outlines.
        let (mut minx, mut maxx, mut miny, mut maxy) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        let mut pen = 0.0;
        let mut fs = bb_text::FontSystem::lock();
        for g in &shaped.glyphs {
            if let Some(o) = fs.outline(g.font, g.id, size) {
                for cmd in &o.cmds {
                    let pts: &[(f32, f32)] = &match *cmd {
                        bb_text::OutlineCmd::MoveTo(x, y) | bb_text::OutlineCmd::LineTo(x, y) => vec![(x, y)],
                        bb_text::OutlineCmd::QuadTo(a, b, x, y) => vec![(a, b), (x, y)],
                        bb_text::OutlineCmd::CubicTo(a, b, c, d, x, y) => vec![(a, b), (c, d), (x, y)],
                        bb_text::OutlineCmd::Close => vec![],
                    };
                    for &(x, y) in pts {
                        let (x, y) = (x + pen + g.x_offset, y - g.y_offset);
                        minx = minx.min(x);
                        maxx = maxx.max(x);
                        miny = miny.min(y);
                        maxy = maxy.max(y);
                    }
                }
            }
            pen += g.x_advance + self.state.letter_spacing;
        }
        if minx > maxx {
            (minx, maxx, miny, maxy) = (0.0, 0.0, 0.0, 0.0);
        }
        TextMetrics {
            width,
            actual_left: anchor - minx,
            actual_right: maxx - anchor,
            actual_ascent: -miny - base,
            actual_descent: maxy + base,
            font_ascent: ascent - base,
            font_descent: descent + base,
            em_ascent: ascent / (ascent + descent).max(1e-6) * size - base,
            em_descent: descent / (ascent + descent).max(1e-6) * size + base,
            hanging: ascent * 0.8 - base,
            alphabetic: -base,
            ideographic: -descent - base,
        }
    }

    /// The x offset of the text's start from the anchor for textAlign (start/end follow the
    /// direction; "inherit" is left-to-right here).
    fn anchor_x(&self, width: f32) -> f32 {
        let rtl = self.state.direction == "rtl";
        match self.state.text_align.as_str() {
            "center" => width / 2.0,
            "right" => width,
            "end" if !rtl => width,
            "start" if rtl => width,
            _ => 0.0,
        }
    }

    /// The baseline's offset from the anchor for textBaseline (positive: anchor above).
    fn baseline_offset(&self, ascent: f32, descent: f32) -> f32 {
        match self.state.text_baseline.as_str() {
            "top" => ascent,
            "hanging" => ascent * 0.8,
            "middle" => (ascent - descent) / 2.0,
            "ideographic" | "bottom" => -descent,
            _ => 0.0,
        }
    }

    /// fillText/strokeText: the glyph outlines at (x, y), squeezed to `max_width`.
    pub fn text(&mut self, text: &str, x: f32, y: f32, max_width: Option<f32>, stroke: bool) {
        if !x.is_finite() || !y.is_finite() || max_width.is_some_and(|w| !w.is_finite() || w <= 0.0) {
            return;
        }
        let text: String =
            text.chars().map(|c| if matches!(c, '\t' | '\n' | '\x0C' | '\r') { ' ' } else { c }).collect();
        let (shaped, m, size) = self.shape(&text);
        let width = shaped.width + self.state.letter_spacing * text.chars().count() as f32;
        let squeeze = match max_width {
            Some(mw) if width > mw && width > 0.0 => mw / width,
            _ => 1.0,
        };
        let x0 = x - self.anchor_x(width) * squeeze;
        let base = y + self.baseline_offset(m.ascent * size, m.descent * size);
        let mut path = CanvasPath::new();
        let mut pen = 0.0;
        {
            let mut fs = bb_text::FontSystem::lock();
            for g in &shaped.glyphs {
                if let Some(o) = fs.outline(g.font, g.id, size) {
                    let gx = |px: f32| x0 + (pen + g.x_offset + px) * squeeze;
                    let gy = |py: f32| base + py - g.y_offset;
                    for cmd in &o.cmds {
                        match *cmd {
                            bb_text::OutlineCmd::MoveTo(a, b) => path.move_to(Point::new(gx(a), gy(b))),
                            bb_text::OutlineCmd::LineTo(a, b) => path.line_to(Point::new(gx(a), gy(b))),
                            bb_text::OutlineCmd::QuadTo(a, b, c, d) => {
                                path.quad_to(Point::new(gx(a), gy(b)), Point::new(gx(c), gy(d)))
                            }
                            bb_text::OutlineCmd::CubicTo(a, b, c, d, e, f) => path.cubic_to(
                                Point::new(gx(a), gy(b)),
                                Point::new(gx(c), gy(d)),
                                Point::new(gx(e), gy(f)),
                            ),
                            bb_text::OutlineCmd::Close => path.close(),
                        }
                    }
                }
                pen += g.x_advance + self.state.letter_spacing;
            }
        }
        if stroke {
            self.stroke(Some(&path));
        } else {
            let t = path.transformed(&self.state.transform);
            self.fill_device(&t, false);
        }
    }
}

fn crop(m: &Mask, w: u32, h: u32) -> Option<Mask> {
    let x0 = m.x.max(0);
    let y0 = m.y.max(0);
    let x1 = (m.x + m.w as i32).min(w as i32);
    let y1 = (m.y + m.h as i32).min(h as i32);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (cw, ch) = ((x1 - x0) as u32, (y1 - y0) as u32);
    let mut out = Mask { x: x0, y: y0, w: cw, h: ch, data: vec![0; (cw * ch) as usize] };
    for y in 0..ch {
        for x in 0..cw {
            out.data[(y * cw + x) as usize] = m.get(x0 + x as i32, y0 + y as i32);
        }
    }
    Some(out)
}

/// TextMetrics (HTML), in CSS px relative to the text's anchor point.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextMetrics {
    pub width: f32,
    pub actual_left: f32,
    pub actual_right: f32,
    pub actual_ascent: f32,
    pub actual_descent: f32,
    pub font_ascent: f32,
    pub font_descent: f32,
    pub em_ascent: f32,
    pub em_descent: f32,
    pub hanging: f32,
    pub alphabetic: f32,
    pub ideographic: f32,
}

/// A parsed CSS `font` shorthand (as canvas's font attribute takes it).
#[derive(Clone, Debug, Default)]
pub struct Font {
    pub italic: bool,
    pub weight: u16,
    pub size: f32,
    pub families: Vec<String>,
}

/// Parse the CSS font shorthand: [style] [variant] [weight] [stretch] size[/line-height]
/// family-list. Relative sizes resolve against canvas's default 10px.
pub fn parse_font(s: &str) -> Option<Font> {
    let s = s.trim();
    // Split off the family list at the size token.
    let mut italic = false;
    let mut weight = 400u16;
    let size: f32;
    let mut rest = s;
    loop {
        let t = rest.trim_start();
        let end = t.find(char::is_whitespace).unwrap_or(t.len());
        let tok = &t[..end];
        if tok.is_empty() {
            return None;
        }
        let lower = tok.to_ascii_lowercase();
        match lower.as_str() {
            "italic" | "oblique" => italic = true,
            "normal" | "small-caps" | "condensed" | "expanded" | "semi-condensed" | "semi-expanded"
            | "ultra-condensed" | "extra-condensed" | "extra-expanded" | "ultra-expanded" => {}
            "bold" => weight = 700,
            "bolder" => weight = 700,
            "lighter" => weight = 100,
            _ if lower.chars().all(|c| c.is_ascii_digit()) && lower.len() == 3 => {
                weight = lower.parse().ok()?;
            }
            _ => {
                // The size (with an optional /line-height).
                let sz = lower.split('/').next().unwrap_or("");
                size = parse_size(sz)?;
                rest = &t[end..];
                break;
            }
        }
        rest = &t[end..];
    }
    let families: Vec<String> = rest
        .split(',')
        .map(|f| f.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|f| !f.is_empty())
        .collect();
    if families.is_empty() {
        return None;
    }
    Some(Font { italic, weight, size, families })
}

fn parse_size(s: &str) -> Option<f32> {
    let num = |t: &str| t.parse::<f32>().ok().filter(|v| v.is_finite() && *v >= 0.0);
    let named = match s {
        "xx-small" => Some(9.0),
        "x-small" => Some(10.0),
        "small" => Some(13.0),
        "medium" => Some(16.0),
        "large" => Some(18.0),
        "x-large" => Some(24.0),
        "xx-large" => Some(32.0),
        "larger" => Some(12.0),
        "smaller" => Some(8.3),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    if let Some(v) = s.strip_suffix("px") {
        return num(v);
    }
    if let Some(v) = s.strip_suffix("pt") {
        return num(v).map(|v| v * 4.0 / 3.0);
    }
    if let Some(v) = s.strip_suffix("em").or_else(|| s.strip_suffix("rem")) {
        return num(v).map(|v| v * 10.0);
    }
    if let Some(v) = s.strip_suffix('%') {
        return num(v).map(|v| v / 10.0);
    }
    if let Some(v) = s.strip_suffix("in") {
        return num(v).map(|v| v * 96.0);
    }
    if let Some(v) = s.strip_suffix("cm") {
        return num(v).map(|v| v * 96.0 / 2.54);
    }
    if let Some(v) = s.strip_suffix("mm") {
        return num(v).map(|v| v * 96.0 / 25.4);
    }
    None
}

// ---- stroking ----

/// Split polylines into dashes (HTML "trace a path" dash steps).
fn dash(lines: &[(Vec<Point>, bool)], pattern: &[f32], offset: f32) -> Vec<(Vec<Point>, bool)> {
    let mut pat: Vec<f32> = pattern.to_vec();
    if pat.len() % 2 == 1 {
        pat.extend_from_slice(pattern);
    }
    let total: f32 = pat.iter().sum();
    if total.is_nan() || total <= 0.0 {
        return lines.to_vec();
    }
    let mut out = Vec::new();
    for (pts, closed) in lines {
        let mut pts = pts.clone();
        if *closed && pts.len() > 1 {
            pts.push(pts[0]);
        }
        // Where in the pattern this subpath starts.
        let mut pos = offset.rem_euclid(total);
        let mut idx = 0;
        while pos >= pat[idx] {
            pos -= pat[idx];
            idx = (idx + 1) % pat.len();
        }
        let mut left = pat[idx] - pos;
        let mut on = idx % 2 == 0;
        let mut cur: Vec<Point> = if on { vec![pts[0]] } else { Vec::new() };
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
            let mut t = 0.0;
            while len - t > left {
                t += left;
                let p = Point::new(a.x + (b.x - a.x) * t / len, a.y + (b.y - a.y) * t / len);
                if on {
                    cur.push(p);
                    out.push((std::mem::take(&mut cur), false));
                } else {
                    cur = vec![p];
                }
                on = !on;
                idx = (idx + 1) % pat.len();
                left = pat[idx];
            }
            left -= len - t;
            if on {
                cur.push(b);
            }
        }
        if on && cur.len() > 1 {
            out.push((cur, false));
        }
    }
    out
}

fn push_poly(out: &mut Vec<Vec<Point>>, pts: Vec<Point>) {
    // Wind every piece the same way, so overlaps never cancel under nonzero.
    let mut a = 0.0;
    for i in 0..pts.len() {
        let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
        a += p.x * q.y - q.x * p.y;
    }
    if a < 0.0 {
        out.push(pts.into_iter().rev().collect());
    } else if a > 0.0 {
        out.push(pts);
    }
}

fn circle(c: Point, r: f32) -> Vec<Point> {
    let n = ((r * 2.0).ceil() as usize).clamp(8, 64);
    (0..n)
        .map(|i| {
            let t = i as f32 / n as f32 * TAU;
            Point::new(c.x + r * t.cos(), c.y + r * t.sin())
        })
        .collect()
}

/// The outline of stroking `lines` (open or closed polylines), as positively wound polygons.
fn stroke_outline(
    lines: &[(Vec<Point>, bool)],
    width: f32,
    cap: LineCap,
    join: LineJoin,
    miter: f32,
) -> Vec<Vec<Point>> {
    let hw = width / 2.0;
    let mut out = Vec::new();
    for (pts, closed) in lines {
        // Drop repeated points.
        let mut p: Vec<Point> = Vec::with_capacity(pts.len());
        for &q in pts {
            if p.last().is_none_or(|l: &Point| (l.x - q.x).abs() > 1e-6 || (l.y - q.y).abs() > 1e-6) {
                p.push(q);
            }
        }
        let closed = *closed && p.len() > 2;
        if closed && p.len() > 1 {
            let (a, b) = (p[0], p[p.len() - 1]);
            if (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6 {
                p.pop();
            }
        }
        if p.len() == 1 {
            // A zero-length subpath: round caps draw a dot, square caps a square.
            match cap {
                LineCap::Round => push_poly(&mut out, circle(p[0], hw)),
                LineCap::Square => push_poly(
                    &mut out,
                    vec![
                        Point::new(p[0].x - hw, p[0].y - hw),
                        Point::new(p[0].x + hw, p[0].y - hw),
                        Point::new(p[0].x + hw, p[0].y + hw),
                        Point::new(p[0].x - hw, p[0].y + hw),
                    ],
                ),
                LineCap::Butt => {}
            }
            continue;
        }
        let n = p.len();
        let segs = if closed { n } else { n - 1 };
        let dir = |i: usize| {
            let (a, b) = (p[i % n], p[(i + 1) % n]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let l = (dx * dx + dy * dy).sqrt();
            (dx / l, dy / l)
        };
        for i in 0..segs {
            let (a, b) = (p[i % n], p[(i + 1) % n]);
            let (dx, dy) = dir(i);
            let (nx, ny) = (-dy * hw, dx * hw);
            push_poly(
                &mut out,
                vec![
                    Point::new(a.x + nx, a.y + ny),
                    Point::new(b.x + nx, b.y + ny),
                    Point::new(b.x - nx, b.y - ny),
                    Point::new(a.x - nx, a.y - ny),
                ],
            );
        }
        // Joins.
        let joints: Vec<usize> = if closed { (0..n).collect() } else { (1..n - 1).collect() };
        for j in joints {
            let (d0x, d0y) = dir((j + n - 1) % n);
            let (d1x, d1y) = dir(j);
            let v = p[j];
            let cross = d0x * d1y - d0y * d1x;
            if cross.abs() < 1e-9 && d0x * d1x + d0y * d1y > 0.0 {
                continue;
            }
            match join {
                LineJoin::Round => push_poly(&mut out, circle(v, hw)),
                _ => {
                    // The outer side's normals.
                    let s = if cross > 0.0 { -1.0 } else { 1.0 };
                    let n0 = (-d0y * hw * s, d0x * hw * s);
                    let n1 = (-d1y * hw * s, d1x * hw * s);
                    let p0 = Point::new(v.x + n0.0, v.y + n0.1);
                    let p1 = Point::new(v.x + n1.0, v.y + n1.1);
                    let cos = (d0x * d1x + d0y * d1y).clamp(-1.0, 1.0);
                    let theta = (PI - cos.acos()).max(1e-6);
                    let ratio = 1.0 / (theta / 2.0).sin();
                    if join == LineJoin::Miter && ratio <= miter {
                        // The miter tip along the bisector of the outer normals.
                        let (bx, by) = (n0.0 + n1.0, n0.1 + n1.1);
                        let bl = (bx * bx + by * by).sqrt().max(1e-9);
                        let len = hw * ratio;
                        let tip = Point::new(v.x + bx / bl * len, v.y + by / bl * len);
                        push_poly(&mut out, vec![v, p0, tip, p1]);
                    } else {
                        push_poly(&mut out, vec![v, p0, p1]);
                    }
                }
            }
        }
        // Caps.
        if !closed {
            for (end, d) in [
                (p[0], dir(0)),
                (p[n - 1], {
                    let (x, y) = dir(n - 2);
                    (-x, -y)
                }),
            ] {
                let (dx, dy) = d;
                match cap {
                    LineCap::Butt => {}
                    LineCap::Round => push_poly(&mut out, circle(end, hw)),
                    LineCap::Square => {
                        let (nx, ny) = (-dy * hw, dx * hw);
                        let back = Point::new(end.x - dx * hw, end.y - dy * hw);
                        push_poly(
                            &mut out,
                            vec![
                                Point::new(end.x + nx, end.y + ny),
                                Point::new(back.x + nx, back.y + ny),
                                Point::new(back.x - nx, back.y - ny),
                                Point::new(end.x - nx, end.y - ny),
                            ],
                        );
                    }
                }
            }
        }
    }
    out
}

/// The winding number of `polys` around (x, y).
fn winding(polys: &[Vec<Point>], x: f32, y: f32) -> i32 {
    let mut w = 0;
    for poly in polys {
        let n = poly.len();
        for i in 0..n {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            if a.y <= y {
                if b.y > y && (b.x - a.x) * (y - a.y) - (x - a.x) * (b.y - a.y) > 0.0 {
                    w += 1;
                }
            } else if b.y <= y && (b.x - a.x) * (y - a.y) - (x - a.x) * (b.y - a.y) < 0.0 {
                w -= 1;
            }
        }
    }
    w
}

fn on_boundary(polys: &[Vec<Point>], x: f32, y: f32) -> bool {
    for poly in polys {
        let n = poly.len();
        for i in 0..n {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            let cross = (b.x - a.x) * (y - a.y) - (x - a.x) * (b.y - a.y);
            if cross.abs() < 1e-4
                && x >= a.x.min(b.x) - 1e-4
                && x <= a.x.max(b.x) + 1e-4
                && y >= a.y.min(b.y) - 1e-4
                && y <= a.y.max(b.y) + 1e-4
            {
                return true;
            }
        }
    }
    false
}

// ---- encoding and SVG path data ----

/// A premultiplied channel back to straight alpha (0 where alpha is 0).
fn unpremul(c: u8, a: u32) -> u8 {
    (c as u32 * 255 + a / 2).checked_div(a).map_or(0, |v| v.min(255) as u8)
}

impl Bitmap {
    /// The bitmap as PNG (unpremultiplied RGBA), for toDataURL and toBlob.
    pub fn encode_png(&self) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(self.data.len());
        for px in self.data.chunks(4) {
            let a = px[3] as u32;
            for &c in &px[..3] {
                rgba.push(unpremul(c, a));
            }
            rgba.push(a as u8);
        }
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.w, self.h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            if let Ok(mut wr) = enc.write_header() {
                let _ = wr.write_image_data(&rgba);
            }
        }
        out
    }
}

/// SVG 2 path data (`new Path2D(d)`, SVG 2 §9.3 "Path data"): parsed up to the first error,
/// as SVG does.
pub fn parse_svg_path(d: &str) -> CanvasPath {
    let mut path = CanvasPath::new();
    let b: Vec<char> = d.chars().collect();
    let mut i = 0;
    let ws = |i: &mut usize| {
        while *i < b.len() && (b[*i].is_ascii_whitespace() || b[*i] == ',') {
            *i += 1;
        }
    };
    let number = |i: &mut usize| -> Option<f32> {
        ws(i);
        let start = *i;
        if *i < b.len() && (b[*i] == '+' || b[*i] == '-') {
            *i += 1;
        }
        let mut digits = false;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
            digits = true;
        }
        if *i < b.len() && b[*i] == '.' {
            *i += 1;
            while *i < b.len() && b[*i].is_ascii_digit() {
                *i += 1;
                digits = true;
            }
        }
        if !digits {
            *i = start;
            return None;
        }
        if *i < b.len() && (b[*i] == 'e' || b[*i] == 'E') {
            let save = *i;
            *i += 1;
            if *i < b.len() && (b[*i] == '+' || b[*i] == '-') {
                *i += 1;
            }
            let ds = *i;
            while *i < b.len() && b[*i].is_ascii_digit() {
                *i += 1;
            }
            if *i == ds {
                *i = save;
            }
        }
        b[start..*i].iter().collect::<String>().parse().ok()
    };
    let flag = |i: &mut usize| -> Option<bool> {
        ws(i);
        match b.get(*i) {
            Some('0') => {
                *i += 1;
                Some(false)
            }
            Some('1') => {
                *i += 1;
                Some(true)
            }
            _ => None,
        }
    };
    let (mut cx, mut cy, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    // The last control point, for S/T.
    let mut last_ctrl: Option<(char, f32, f32)> = None;
    let mut cmd: Option<char> = None;
    loop {
        ws(&mut i);
        if i >= b.len() {
            break;
        }
        let c = b[i];
        if c.is_ascii_alphabetic() {
            cmd = Some(c);
            i += 1;
        } else if cmd.is_none() {
            break;
        }
        let Some(op) = cmd else { break };
        let rel = op.is_ascii_lowercase();
        let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
        let mut pb = PathBuilder { path: &mut path, m: Matrix::IDENTITY };
        match op.to_ascii_uppercase() {
            'M' => {
                let (Some(x), Some(y)) = (number(&mut i), number(&mut i)) else { break };
                let (x, y) = (x + ox, y + oy);
                pb.move_to(x, y);
                (cx, cy, sx, sy) = (x, y, x, y);
                // Further pairs are line-tos.
                cmd = Some(if rel { 'l' } else { 'L' });
                last_ctrl = None;
            }
            'L' => {
                let (Some(x), Some(y)) = (number(&mut i), number(&mut i)) else { break };
                let (x, y) = (x + ox, y + oy);
                pb.line_to(x, y);
                (cx, cy) = (x, y);
                last_ctrl = None;
            }
            'H' => {
                let Some(x) = number(&mut i) else { break };
                let x = x + ox;
                pb.line_to(x, cy);
                cx = x;
                last_ctrl = None;
            }
            'V' => {
                let Some(y) = number(&mut i) else { break };
                let y = y + oy;
                pb.line_to(cx, y);
                cy = y;
                last_ctrl = None;
            }
            'C' => {
                let v: Vec<Option<f32>> = (0..6).map(|_| number(&mut i)).collect();
                let Some(v) = v.into_iter().collect::<Option<Vec<f32>>>() else { break };
                pb.cubic_to(v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy, v[4] + ox, v[5] + oy);
                last_ctrl = Some(('C', v[2] + ox, v[3] + oy));
                (cx, cy) = (v[4] + ox, v[5] + oy);
            }
            'S' => {
                let v: Vec<Option<f32>> = (0..4).map(|_| number(&mut i)).collect();
                let Some(v) = v.into_iter().collect::<Option<Vec<f32>>>() else { break };
                let (c1x, c1y) = match last_ctrl {
                    Some(('C', x, y)) => (2.0 * cx - x, 2.0 * cy - y),
                    _ => (cx, cy),
                };
                pb.cubic_to(c1x, c1y, v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy);
                last_ctrl = Some(('C', v[0] + ox, v[1] + oy));
                (cx, cy) = (v[2] + ox, v[3] + oy);
            }
            'Q' => {
                let v: Vec<Option<f32>> = (0..4).map(|_| number(&mut i)).collect();
                let Some(v) = v.into_iter().collect::<Option<Vec<f32>>>() else { break };
                pb.quad_to(v[0] + ox, v[1] + oy, v[2] + ox, v[3] + oy);
                last_ctrl = Some(('Q', v[0] + ox, v[1] + oy));
                (cx, cy) = (v[2] + ox, v[3] + oy);
            }
            'T' => {
                let (Some(x), Some(y)) = (number(&mut i), number(&mut i)) else { break };
                let (qx, qy) = match last_ctrl {
                    Some(('Q', x, y)) => (2.0 * cx - x, 2.0 * cy - y),
                    _ => (cx, cy),
                };
                pb.quad_to(qx, qy, x + ox, y + oy);
                last_ctrl = Some(('Q', qx, qy));
                (cx, cy) = (x + ox, y + oy);
            }
            'A' => {
                let (Some(rx), Some(ry), Some(rot)) = (number(&mut i), number(&mut i), number(&mut i)) else {
                    break;
                };
                let (Some(large), Some(sweep)) = (flag(&mut i), flag(&mut i)) else { break };
                let (Some(x), Some(y)) = (number(&mut i), number(&mut i)) else { break };
                let (x, y) = (x + ox, y + oy);
                svg_arc(&mut pb, (cx, cy), rx.abs(), ry.abs(), rot.to_radians(), large, sweep, (x, y));
                (cx, cy) = (x, y);
                last_ctrl = None;
            }
            'Z' => {
                pb.close();
                (cx, cy) = (sx, sy);
                // Z takes no arguments: a number after it is an error.
                cmd = None;
                last_ctrl = None;
            }
            _ => break,
        }
    }
    path
}

/// SVG 2 Appendix B.2.4 elliptical arc from endpoint to center parameterization.
#[allow(clippy::too_many_arguments)]
fn svg_arc(
    pb: &mut PathBuilder,
    p0: (f32, f32),
    rx: f32,
    ry: f32,
    phi: f32,
    large: bool,
    sweep: bool,
    p1: (f32, f32),
) {
    if p0 == p1 {
        return;
    }
    if rx == 0.0 || ry == 0.0 {
        pb.line_to(p1.0, p1.1);
        return;
    }
    let (cp, sp) = (phi.cos(), phi.sin());
    let (dx, dy) = ((p0.0 - p1.0) / 2.0, (p0.1 - p1.1) / 2.0);
    let (x1, y1) = (cp * dx + sp * dy, -sp * dx + cp * dy);
    let (mut rx, mut ry) = (rx, ry);
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut co = (num / den).max(0.0).sqrt();
    if large == sweep {
        co = -co;
    }
    let (cxp, cyp) = (co * rx * y1 / ry, -co * ry * x1 / rx);
    let (cx, cy) = (cp * cxp - sp * cyp + (p0.0 + p1.0) / 2.0, sp * cxp + cp * cyp + (p0.1 + p1.1) / 2.0);
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let th1 = ang(1.0, 0.0, (x1 - cxp) / rx, (y1 - cyp) / ry);
    let mut dth = ang((x1 - cxp) / rx, (y1 - cyp) / ry, (-x1 - cxp) / rx, (-y1 - cyp) / ry);
    if !sweep && dth > 0.0 {
        dth -= TAU;
    } else if sweep && dth < 0.0 {
        dth += TAU;
    }
    pb.ellipse(cx, cy, rx, ry, phi, th1, th1 + dth, !sweep);
}

//! Geometry: points, rects, paths and flattening.

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Point { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect { x, y, w: (r - x).max(0.0), h: (b - y).max(0.0) }
    }
    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect { x, y, w: self.right().max(o.right()) - x, h: self.bottom().max(o.bottom()) - y }
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        !self.intersect(o).is_empty()
    }
    pub fn inset(&self, t: f32, r: f32, b: f32, l: f32) -> Rect {
        Rect { x: self.x + l, y: self.y + t, w: (self.w - l - r).max(0.0), h: (self.h - t - b).max(0.0) }
    }
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect { x: self.x + dx, y: self.y + dy, ..*self }
    }
    pub fn scale(&self, s: f32) -> Rect {
        Rect { x: self.x * s, y: self.y * s, w: self.w * s, h: self.h * s }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathCmd {
    MoveTo(Point),
    LineTo(Point),
    QuadTo(Point, Point),
    CubicTo(Point, Point, Point),
    Close,
}

/// A vector path (nonzero fill rule).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Path {
    pub cmds: Vec<PathCmd>,
}

/// Corner radii: top-left, top-right, bottom-right, bottom-left (x, y each).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Radii {
    pub tl: (f32, f32),
    pub tr: (f32, f32),
    pub br: (f32, f32),
    pub bl: (f32, f32),
}

impl Radii {
    pub fn uniform(r: f32) -> Radii {
        Radii { tl: (r, r), tr: (r, r), br: (r, r), bl: (r, r) }
    }
    pub fn is_zero(&self) -> bool {
        [self.tl, self.tr, self.br, self.bl].iter().all(|(x, y)| *x <= 0.0 || *y <= 0.0)
    }
    /// CSS Backgrounds 3 §5.5: scale down overlapping radii.
    pub fn clamp_to(&self, r: &Rect) -> Radii {
        let mut f: f32 = 1.0;
        let check = |sum: f32, side: f32, f: &mut f32| {
            if sum > side && sum > 0.0 {
                *f = f.min(side / sum);
            }
        };
        check(self.tl.0 + self.tr.0, r.w, &mut f);
        check(self.bl.0 + self.br.0, r.w, &mut f);
        check(self.tl.1 + self.bl.1, r.h, &mut f);
        check(self.tr.1 + self.br.1, r.h, &mut f);
        let s = |(x, y): (f32, f32)| ((x * f).max(0.0), (y * f).max(0.0));
        Radii { tl: s(self.tl), tr: s(self.tr), br: s(self.br), bl: s(self.bl) }
    }
    /// Radii shrunk by border widths (for inner border edges / padding box).
    pub fn shrink(&self, t: f32, r: f32, b: f32, l: f32) -> Radii {
        let s = |(x, y): (f32, f32), dx: f32, dy: f32| ((x - dx).max(0.0), (y - dy).max(0.0));
        Radii { tl: s(self.tl, l, t), tr: s(self.tr, r, t), br: s(self.br, r, b), bl: s(self.bl, l, b) }
    }
}

/// Cubic Bézier approximation constant for quarter circles.
const KAPPA: f32 = 0.552_284_8;

impl Path {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn move_to(&mut self, x: f32, y: f32) -> &mut Self {
        self.cmds.push(PathCmd::MoveTo(Point::new(x, y)));
        self
    }
    pub fn line_to(&mut self, x: f32, y: f32) -> &mut Self {
        self.cmds.push(PathCmd::LineTo(Point::new(x, y)));
        self
    }
    pub fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) -> &mut Self {
        self.cmds.push(PathCmd::QuadTo(Point::new(cx, cy), Point::new(x, y)));
        self
    }
    pub fn cubic_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) -> &mut Self {
        self.cmds.push(PathCmd::CubicTo(Point::new(c1x, c1y), Point::new(c2x, c2y), Point::new(x, y)));
        self
    }
    pub fn close(&mut self) -> &mut Self {
        self.cmds.push(PathCmd::Close);
        self
    }

    pub fn rect(r: Rect) -> Path {
        let mut p = Path::new();
        p.move_to(r.x, r.y)
            .line_to(r.right(), r.y)
            .line_to(r.right(), r.bottom())
            .line_to(r.x, r.bottom())
            .close();
        p
    }

    /// Append a rounded rectangle (clockwise in screen space). `reverse` winds it the other way.
    pub fn add_rounded_rect(&mut self, r: Rect, radii: Radii, reverse: bool) {
        let rad = radii.clamp_to(&r);
        let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
        let mut pts: Vec<PathCmd> = Vec::new();
        let p = Point::new;
        pts.push(PathCmd::MoveTo(p(x0 + rad.tl.0, y0)));
        pts.push(PathCmd::LineTo(p(x1 - rad.tr.0, y0)));
        if rad.tr.0 > 0.0 && rad.tr.1 > 0.0 {
            pts.push(PathCmd::CubicTo(
                p(x1 - rad.tr.0 * (1.0 - KAPPA), y0),
                p(x1, y0 + rad.tr.1 * (1.0 - KAPPA)),
                p(x1, y0 + rad.tr.1),
            ));
        }
        pts.push(PathCmd::LineTo(p(x1, y1 - rad.br.1)));
        if rad.br.0 > 0.0 && rad.br.1 > 0.0 {
            pts.push(PathCmd::CubicTo(
                p(x1, y1 - rad.br.1 * (1.0 - KAPPA)),
                p(x1 - rad.br.0 * (1.0 - KAPPA), y1),
                p(x1 - rad.br.0, y1),
            ));
        }
        pts.push(PathCmd::LineTo(p(x0 + rad.bl.0, y1)));
        if rad.bl.0 > 0.0 && rad.bl.1 > 0.0 {
            pts.push(PathCmd::CubicTo(
                p(x0 + rad.bl.0 * (1.0 - KAPPA), y1),
                p(x0, y1 - rad.bl.1 * (1.0 - KAPPA)),
                p(x0, y1 - rad.bl.1),
            ));
        }
        pts.push(PathCmd::LineTo(p(x0, y0 + rad.tl.1)));
        if rad.tl.0 > 0.0 && rad.tl.1 > 0.0 {
            pts.push(PathCmd::CubicTo(
                p(x0, y0 + rad.tl.1 * (1.0 - KAPPA)),
                p(x0 + rad.tl.0 * (1.0 - KAPPA), y0),
                p(x0 + rad.tl.0, y0),
            ));
        }
        pts.push(PathCmd::Close);
        if reverse {
            // Flatten and reverse as a polygon (simplest correct reversal).
            let mut tmp = Path { cmds: pts };
            let polys = tmp.flatten(0.1);
            tmp.cmds.clear();
            for poly in polys {
                for (i, pt) in poly.iter().rev().enumerate() {
                    tmp.cmds.push(if i == 0 { PathCmd::MoveTo(*pt) } else { PathCmd::LineTo(*pt) });
                }
                tmp.cmds.push(PathCmd::Close);
            }
            self.cmds.extend(tmp.cmds);
        } else {
            self.cmds.extend(pts);
        }
    }

    pub fn rounded_rect(r: Rect, radii: Radii) -> Path {
        let mut p = Path::new();
        p.add_rounded_rect(r, radii, false);
        p
    }

    pub fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> Path {
        Path::rounded_rect(
            Rect::new(cx - rx, cy - ry, rx * 2.0, ry * 2.0),
            Radii { tl: (rx, ry), tr: (rx, ry), br: (rx, ry), bl: (rx, ry) },
        )
    }

    pub fn polygon(pts: &[Point]) -> Path {
        let mut p = Path::new();
        for (i, pt) in pts.iter().enumerate() {
            if i == 0 {
                p.move_to(pt.x, pt.y);
            } else {
                p.line_to(pt.x, pt.y);
            }
        }
        p.close();
        p
    }

    pub fn transform(&self, sx: f32, sy: f32, dx: f32, dy: f32) -> Path {
        let t = |p: Point| Point::new(p.x * sx + dx, p.y * sy + dy);
        Path {
            cmds: self
                .cmds
                .iter()
                .map(|c| match *c {
                    PathCmd::MoveTo(p) => PathCmd::MoveTo(t(p)),
                    PathCmd::LineTo(p) => PathCmd::LineTo(t(p)),
                    PathCmd::QuadTo(a, b) => PathCmd::QuadTo(t(a), t(b)),
                    PathCmd::CubicTo(a, b, c) => PathCmd::CubicTo(t(a), t(b), t(c)),
                    PathCmd::Close => PathCmd::Close,
                })
                .collect(),
        }
    }

    /// Apply a horizontal skew (synthetic italic): x' = x - y * k.
    pub fn skew_x(&self, k: f32, origin_y: f32) -> Path {
        let t = |p: Point| Point::new(p.x - (p.y - origin_y) * k, p.y);
        Path {
            cmds: self
                .cmds
                .iter()
                .map(|c| match *c {
                    PathCmd::MoveTo(p) => PathCmd::MoveTo(t(p)),
                    PathCmd::LineTo(p) => PathCmd::LineTo(t(p)),
                    PathCmd::QuadTo(a, b) => PathCmd::QuadTo(t(a), t(b)),
                    PathCmd::CubicTo(a, b, c) => PathCmd::CubicTo(t(a), t(b), t(c)),
                    PathCmd::Close => PathCmd::Close,
                })
                .collect(),
        }
    }

    /// Flatten curves into closed polygons with the given tolerance (px).
    pub fn flatten(&self, tol: f32) -> Vec<Vec<Point>> {
        let mut polys: Vec<Vec<Point>> = Vec::new();
        let mut cur: Vec<Point> = Vec::new();
        let mut last = Point::default();
        let mut start = Point::default();
        for c in &self.cmds {
            match *c {
                PathCmd::MoveTo(p) => {
                    if cur.len() > 1 {
                        polys.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                    cur.push(p);
                    last = p;
                    start = p;
                }
                PathCmd::LineTo(p) => {
                    if cur.is_empty() {
                        cur.push(last);
                    }
                    cur.push(p);
                    last = p;
                }
                PathCmd::QuadTo(c1, p) => {
                    if cur.is_empty() {
                        cur.push(last);
                    }
                    let dd =
                        ((last.x - 2.0 * c1.x + p.x).powi(2) + (last.y - 2.0 * c1.y + p.y).powi(2)).sqrt();
                    let n = ((dd / tol).sqrt().ceil() as usize).clamp(1, 64);
                    for i in 1..=n {
                        let t = i as f32 / n as f32;
                        let mt = 1.0 - t;
                        cur.push(Point::new(
                            mt * mt * last.x + 2.0 * mt * t * c1.x + t * t * p.x,
                            mt * mt * last.y + 2.0 * mt * t * c1.y + t * t * p.y,
                        ));
                    }
                    last = p;
                }
                PathCmd::CubicTo(c1, c2, p) => {
                    if cur.is_empty() {
                        cur.push(last);
                    }
                    let dd1 =
                        ((last.x - 2.0 * c1.x + c2.x).powi(2) + (last.y - 2.0 * c1.y + c2.y).powi(2)).sqrt();
                    let dd2 = ((c1.x - 2.0 * c2.x + p.x).powi(2) + (c1.y - 2.0 * c2.y + p.y).powi(2)).sqrt();
                    let dd = dd1.max(dd2) * 3.0;
                    let n = ((dd / tol).sqrt().ceil() as usize).clamp(1, 96);
                    for i in 1..=n {
                        let t = i as f32 / n as f32;
                        let mt = 1.0 - t;
                        let a = mt * mt * mt;
                        let b = 3.0 * mt * mt * t;
                        let c = 3.0 * mt * t * t;
                        let d = t * t * t;
                        cur.push(Point::new(
                            a * last.x + b * c1.x + c * c2.x + d * p.x,
                            a * last.y + b * c1.y + c * c2.y + d * p.y,
                        ));
                    }
                    last = p;
                }
                PathCmd::Close => {
                    if cur.len() > 1 {
                        polys.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                    last = start;
                }
            }
        }
        if cur.len() > 1 {
            polys.push(cur);
        }
        polys
    }

    /// Outline a stroke of `width` as fillable polygons (butt caps, round-ish joins).
    pub fn stroke(&self, width: f32) -> Path {
        let hw = width / 2.0;
        let mut out = Path::new();
        for poly in self.flatten(0.15) {
            let closed = poly.len() > 2 && {
                let (a, b) = (poly[0], poly[poly.len() - 1]);
                (a.x - b.x).abs() < 0.01 && (a.y - b.y).abs() < 0.01
            };
            let segs: Vec<(Point, Point)> = poly.windows(2).map(|w| (w[0], w[1])).collect();
            for (a, b) in &segs {
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let len = (dx * dx + dy * dy).sqrt();
                if len < 1e-4 {
                    continue;
                }
                let (nx, ny) = (-dy / len * hw, dx / len * hw);
                let quad = [
                    Point::new(a.x + nx, a.y + ny),
                    Point::new(b.x + nx, b.y + ny),
                    Point::new(b.x - nx, b.y - ny),
                    Point::new(a.x - nx, a.y - ny),
                ];
                push_positive(&mut out, &quad);
            }
            // Round joins (and caps on closed paths) as small polygons.
            let joints: Vec<Point> =
                if closed { poly.clone() } else { poly[1..poly.len().saturating_sub(1)].to_vec() };
            if width > 1.2 {
                for j in joints {
                    let n = 8;
                    let circle: Vec<Point> = (0..n)
                        .map(|i| {
                            let t = i as f32 / n as f32 * std::f32::consts::TAU;
                            Point::new(j.x + hw * t.cos(), j.y + hw * t.sin())
                        })
                        .collect();
                    push_positive(&mut out, &circle);
                }
            }
        }
        out
    }
}

/// Signed area (positive = clockwise in y-down screen space).
fn signed_area(pts: &[Point]) -> f32 {
    let mut a = 0.0;
    for i in 0..pts.len() {
        let p = pts[i];
        let q = pts[(i + 1) % pts.len()];
        a += p.x * q.y - q.x * p.y;
    }
    a / 2.0
}

/// Append a polygon wound positively, so overlapping pieces never cancel.
fn push_positive(out: &mut Path, pts: &[Point]) {
    let rev = signed_area(pts) < 0.0;
    let it: Box<dyn Iterator<Item = &Point>> =
        if rev { Box::new(pts.iter().rev()) } else { Box::new(pts.iter()) };
    for (i, p) in it.enumerate() {
        if i == 0 {
            out.move_to(p.x, p.y);
        } else {
            out.line_to(p.x, p.y);
        }
    }
    out.close();
}

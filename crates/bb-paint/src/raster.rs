//! Anti-aliased scanline coverage rasterizer.
//!
//! Exact-area coverage via signed-area accumulation (the technique popularized
//! by font-rs): each edge deposits its signed area into an accumulation
//! buffer; a running prefix sum per row yields coverage. The fill rule is
//! "nonzero, clamped": |winding| ≥ 1 is fully covered, which is correct for
//! nonzero fills whose sub-paths wind consistently.

use crate::path::{Path, Point};

/// An alpha coverage mask covering `[x, x+w) × [y, y+h)` in device pixels.
#[derive(Debug, Clone, Default)]
pub struct Mask {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

impl Mask {
    pub fn get(&self, px: i32, py: i32) -> u8 {
        let (lx, ly) = (px - self.x, py - self.y);
        if lx < 0 || ly < 0 || lx >= self.w as i32 || ly >= self.h as i32 {
            return 0;
        }
        self.data[(ly as u32 * self.w + lx as u32) as usize]
    }
}

struct Accum {
    w: usize,
    h: usize,
    a: Vec<f32>,
    ox: f32,
    oy: f32,
}

impl Accum {
    // font-rs style line accumulation in local (mask) coordinates.
    fn line(&mut self, p0: Point, p1: Point) {
        let (p0, p1) =
            (Point::new(p0.x - self.ox, p0.y - self.oy), Point::new(p1.x - self.ox, p1.y - self.oy));
        if (p0.y - p1.y).abs() < f32::EPSILON
            || !p0.y.is_finite()
            || !p1.y.is_finite()
            || !p0.x.is_finite()
            || !p1.x.is_finite()
        {
            return;
        }
        let (dir, p0, p1) = if p0.y < p1.y { (1.0f32, p0, p1) } else { (-1.0f32, p1, p0) };
        let dxdy = (p1.x - p0.x) / (p1.y - p0.y);
        let wmax = self.w as f32;
        let mut x = p0.x;
        let y_start = p0.y.max(0.0);
        if p0.y < 0.0 {
            x -= p0.y * dxdy;
        }
        let y_end = p1.y.min(self.h as f32);
        if y_start >= y_end {
            return;
        }
        let mut y = y_start.floor() as usize;
        while (y as f32) < y_end {
            let row = y * (self.w + 2);
            let top = (y as f32).max(p0.y);
            let bot = ((y + 1) as f32).min(p1.y);
            let dy = bot - top;
            let xnext = x + dxdy * dy;
            let d = dy * dir;
            let (xa, xb) = if x < xnext { (x, xnext) } else { (xnext, x) };
            // Clamp to the mask: area left of x=0 accumulates at column 0.
            let xa = xa.clamp(0.0, wmax);
            let xb = xb.clamp(0.0, wmax);
            let x0f = xa.floor();
            let x0i = x0f as usize;
            let x1c = xb.ceil();
            let x1i = x1c as usize;
            if x1i <= x0i + 1 {
                let xmf = 0.5 * (xa + xb) - x0f;
                self.a[row + x0i] += d - d * xmf;
                self.a[row + x0i + 1] += d * xmf;
            } else {
                let s = 1.0 / (xb - xa);
                let x0frac = xa - x0f;
                let a0 = 0.5 * s * (1.0 - x0frac) * (1.0 - x0frac);
                let x1frac = xb - x1c + 1.0;
                let am = 0.5 * s * x1frac * x1frac;
                self.a[row + x0i] += d * a0;
                if x1i == x0i + 2 {
                    self.a[row + x0i + 1] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - x0frac);
                    self.a[row + x0i + 1] += d * (a1 - a0);
                    for xi in x0i + 2..x1i - 1 {
                        self.a[row + xi] += d * s;
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    self.a[row + x1i - 1] += d * (1.0 - a2 - am);
                }
                self.a[row + x1i] += d * am;
            }
            x = xnext;
            y += 1;
        }
    }
}

/// Rasterize `path` (device coordinates) into a coverage mask clipped to `clip`
/// (x0, y0, x1, y1 in device pixels).
pub fn rasterize(path: &Path, clip: (i32, i32, i32, i32)) -> Option<Mask> {
    let polys = path.flatten(0.2);
    rasterize_polys(&polys, clip)
}

pub fn rasterize_polys(polys: &[Vec<Point>], clip: (i32, i32, i32, i32)) -> Option<Mask> {
    let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for poly in polys {
        for p in poly {
            if !p.x.is_finite() || !p.y.is_finite() {
                return None;
            }
            minx = minx.min(p.x);
            miny = miny.min(p.y);
            maxx = maxx.max(p.x);
            maxy = maxy.max(p.y);
        }
    }
    if minx > maxx {
        return None;
    }
    let x0 = (minx.floor() as i32).max(clip.0);
    let y0 = (miny.floor() as i32).max(clip.1);
    let x1 = (maxx.ceil() as i32).min(clip.2);
    let y1 = (maxy.ceil() as i32).min(clip.3);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    if w.saturating_mul(h) > 64 * 1024 * 1024 {
        return None;
    }
    let mut acc = Accum { w, h, a: vec![0.0; (w + 2) * h], ox: x0 as f32, oy: y0 as f32 };
    for poly in polys {
        let n = poly.len();
        for i in 0..n {
            acc.line(poly[i], poly[(i + 1) % n]);
        }
    }
    let mut data = vec![0u8; w * h];
    for y in 0..h {
        let mut sum = 0.0f32;
        let row = &acc.a[y * (w + 2)..y * (w + 2) + w + 2];
        for x in 0..w {
            sum += row[x];
            let c = sum.abs().min(1.0);
            data[y * w + x] = (c * 255.0 + 0.5) as u8;
        }
    }
    Some(Mask { x: x0, y: y0, w: w as u32, h: h as u32, data })
}

/// Separable box blur, applied three times (≈ Gaussian with σ = radius/2).
pub fn blur_mask(m: &Mask, sigma: f32) -> Mask {
    if sigma < 0.5 {
        return m.clone();
    }
    let pad = (sigma * 3.0).ceil() as i32;
    let (w, h) = (m.w as i32 + pad * 2, m.h as i32 + pad * 2);
    let mut buf = vec![0f32; (w * h) as usize];
    for y in 0..m.h as i32 {
        for x in 0..m.w as i32 {
            buf[((y + pad) * w + x + pad) as usize] = m.data[(y as u32 * m.w + x as u32) as usize] as f32;
        }
    }
    // Box size for 3 passes approximating a Gaussian.
    let r = ((sigma * sigma * 4.0 + 1.0).sqrt() / 2.0).round().max(1.0) as i32;
    let mut tmp = vec![0f32; buf.len()];
    for _ in 0..3 {
        // horizontal
        for y in 0..h {
            let mut s = 0.0;
            let row = (y * w) as usize;
            for x in -r..=r {
                s += buf[row + x.clamp(0, w - 1) as usize];
            }
            for x in 0..w {
                tmp[row + x as usize] = s / (2 * r + 1) as f32;
                s += buf[row + (x + r + 1).clamp(0, w - 1) as usize]
                    - buf[row + (x - r).clamp(0, w - 1) as usize];
            }
        }
        // vertical
        for x in 0..w {
            let mut s = 0.0;
            for y in -r..=r {
                s += tmp[(y.clamp(0, h - 1) * w + x) as usize];
            }
            for y in 0..h {
                buf[(y * w + x) as usize] = s / (2 * r + 1) as f32;
                s += tmp[((y + r + 1).clamp(0, h - 1) * w + x) as usize]
                    - tmp[((y - r).clamp(0, h - 1) * w + x) as usize];
            }
        }
    }
    Mask {
        x: m.x - pad,
        y: m.y - pad,
        w: w as u32,
        h: h as u32,
        data: buf.iter().map(|v| v.round().clamp(0.0, 255.0) as u8).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::{Path, Rect};

    #[test]
    fn rect_coverage_exact() {
        let m = rasterize(&Path::rect(Rect::new(1.0, 1.0, 2.0, 2.0)), (0, 0, 10, 10)).unwrap();
        assert_eq!(m.get(1, 1), 255);
        assert_eq!(m.get(2, 2), 255);
        assert_eq!(m.get(3, 3), 0);
        assert_eq!(m.get(0, 0), 0);
        // Half-pixel edges give ~50% coverage.
        let m = rasterize(&Path::rect(Rect::new(0.5, 0.0, 1.0, 1.0)), (0, 0, 4, 4)).unwrap();
        assert!((m.get(0, 0) as i32 - 128).abs() <= 1, "{}", m.get(0, 0));
        assert!((m.get(1, 0) as i32 - 128).abs() <= 1);
    }

    #[test]
    fn circle_area() {
        let m = rasterize(&Path::ellipse(50.0, 50.0, 20.0, 20.0), (0, 0, 100, 100)).unwrap();
        let area: f32 = m.data.iter().map(|&v| v as f32 / 255.0).sum();
        let expect = std::f32::consts::PI * 400.0;
        assert!((area - expect).abs() / expect < 0.01, "{area} vs {expect}");
    }

    #[test]
    fn ring_with_hole() {
        let mut p = Path::rect(Rect::new(0.0, 0.0, 10.0, 10.0));
        let mut inner = Path::new();
        inner.move_to(3.0, 3.0).line_to(3.0, 7.0).line_to(7.0, 7.0).line_to(7.0, 3.0).close();
        p.cmds.extend(inner.cmds);
        let m = rasterize(&p, (0, 0, 10, 10)).unwrap();
        assert_eq!(m.get(1, 1), 255);
        assert_eq!(m.get(5, 5), 0);
    }

    #[test]
    fn clipped_and_offscreen() {
        let m = rasterize(&Path::rect(Rect::new(-5.0, -5.0, 10.0, 10.0)), (0, 0, 3, 3)).unwrap();
        assert_eq!(m.get(0, 0), 255);
        assert_eq!(m.get(2, 2), 255);
        assert!(rasterize(&Path::rect(Rect::new(50.0, 50.0, 1.0, 1.0)), (0, 0, 3, 3)).is_none());
    }
}

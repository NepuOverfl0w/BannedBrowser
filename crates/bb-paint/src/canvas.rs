//! The `Canvas` trait and the reference `SoftwareCanvas` (RGBA8, premultiplied).

use crate::path::{Path, Radii, Rect};
use crate::raster::{blur_mask, rasterize, Mask};
use std::sync::Arc;

/// A straight-alpha sRGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }
    pub const fn hex(v: u32) -> Self {
        Color::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }
    pub fn with_alpha(self, a: f32) -> Self {
        Color { a: (self.a as f32 * a.clamp(0.0, 1.0)).round() as u8, ..self }
    }
    pub fn lerp(self, o: Color, t: f32) -> Color {
        let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8;
        Color { r: l(self.r, o.r), g: l(self.g, o.g), b: l(self.b, o.b), a: l(self.a, o.a) }
    }
    /// Premultiplied [r, g, b, a].
    pub fn premul(self) -> [u32; 4] {
        let a = self.a as u32;
        [(self.r as u32 * a + 127) / 255, (self.g as u32 * a + 127) / 255, (self.b as u32 * a + 127) / 255, a]
    }
}

/// A gradient color stop at offset 0..1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stop {
    pub offset: f32,
    pub color: Color,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Paint {
    Solid(Color),
    Linear { x0: f32, y0: f32, x1: f32, y1: f32, stops: Vec<Stop>, repeating: bool },
    Radial { cx: f32, cy: f32, rx: f32, ry: f32, stops: Vec<Stop>, repeating: bool },
}

/// A decoded image frame ready to draw (premultiplied RGBA8).
#[derive(Debug, Clone)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<Vec<u8>>,
}

/// The drawing interface shared by the software canvas (and a future GPU canvas).
pub trait Canvas {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn clear(&mut self, c: Color);
    fn fill_rect(&mut self, r: Rect, c: Color);
    fn fill_path(&mut self, p: &Path, paint: &Paint);
    /// Fill `c` through coverage mask `m` placed at offset (dx, dy).
    fn fill_mask(&mut self, m: &Mask, dx: i32, dy: i32, c: Color);
    fn draw_image(&mut self, img: &ImageData, dst: Rect, opacity: f32, smooth: bool);
    fn push_clip(&mut self, r: Rect, radii: Option<Radii>);
    fn pop_clip(&mut self);
    fn push_layer(&mut self, opacity: f32);
    fn pop_layer(&mut self);
    fn box_shadow(&mut self, r: Rect, radii: Radii, blur: f32, color: Color, clip_out: Option<(Rect, Radii)>);
}

struct Clip {
    rect: (i32, i32, i32, i32),
    mask: Option<Mask>,
}

struct Layer {
    pixels: Vec<u8>,
    opacity: f32,
}

/// CPU canvas: premultiplied RGBA8 in device pixels.
pub struct SoftwareCanvas {
    pub w: u32,
    pub h: u32,
    pub pixels: Vec<u8>,
    clips: Vec<Clip>,
    layers: Vec<Layer>,
}

#[inline]
fn blend_px(dst: &mut [u8], src: [u32; 4], cov: u32) {
    // src premultiplied, cov 0..255
    let a = src[3] * cov / 255;
    if a == 0 {
        return;
    }
    let inv = 255 - a;
    let s = |c: u32| c * cov / 255;
    dst[0] = (s(src[0]) + (dst[0] as u32 * inv + 127) / 255).min(255) as u8;
    dst[1] = (s(src[1]) + (dst[1] as u32 * inv + 127) / 255).min(255) as u8;
    dst[2] = (s(src[2]) + (dst[2] as u32 * inv + 127) / 255).min(255) as u8;
    dst[3] = (a + (dst[3] as u32 * inv + 127) / 255).min(255) as u8;
}

fn gradient_lut(stops: &[Stop]) -> Vec<[u32; 4]> {
    let mut lut = Vec::with_capacity(256);
    let mut s: Vec<Stop> = stops.to_vec();
    if s.is_empty() {
        s.push(Stop { offset: 0.0, color: Color::TRANSPARENT });
    }
    for i in 0..256 {
        let t = i as f32 / 255.0;
        let c = if t <= s[0].offset {
            s[0].color
        } else if t >= s[s.len() - 1].offset {
            s[s.len() - 1].color
        } else {
            let j = s.windows(2).position(|w| t >= w[0].offset && t <= w[1].offset).unwrap_or(0);
            let (a, b) = (s[j], s[j + 1]);
            let span = (b.offset - a.offset).max(1e-6);
            a.color.lerp(b.color, (t - a.offset) / span)
        };
        lut.push(c.premul());
    }
    lut
}

impl SoftwareCanvas {
    pub fn new(w: u32, h: u32) -> Self {
        let w = w.max(1);
        let h = h.max(1);
        SoftwareCanvas { w, h, pixels: vec![0; (w * h * 4) as usize], clips: Vec::new(), layers: Vec::new() }
    }

    fn buf(&mut self) -> &mut Vec<u8> {
        match self.layers.last_mut() {
            Some(l) => &mut l.pixels,
            None => &mut self.pixels,
        }
    }

    fn clip_rect(&self) -> (i32, i32, i32, i32) {
        self.clips.last().map(|c| c.rect).unwrap_or((0, 0, self.w as i32, self.h as i32))
    }

    /// Combined clip coverage at a pixel (1..255) for rounded clips.
    fn clip_cov(&self, x: i32, y: i32) -> u32 {
        let mut cov = 255u32;
        for c in &self.clips {
            if let Some(m) = &c.mask {
                cov = cov * m.get(x, y) as u32 / 255;
                if cov == 0 {
                    return 0;
                }
            }
        }
        cov
    }

    fn has_mask_clip(&self) -> bool {
        self.clips.iter().any(|c| c.mask.is_some())
    }

    /// Export as straight-alpha RGBA (for PNG encoding).
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = self.pixels.clone();
        for p in out.chunks_exact_mut(4) {
            let a = p[3] as u32;
            if a != 0 && a != 255 {
                p[0] = ((p[0] as u32 * 255 + a / 2) / a).min(255) as u8;
                p[1] = ((p[1] as u32 * 255 + a / 2) / a).min(255) as u8;
                p[2] = ((p[2] as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
        out
    }

    /// Pack as 0RGB u32 (softbuffer), compositing over `bg`.
    pub fn to_0rgb(&self, out: &mut [u32]) {
        for (o, p) in out.iter_mut().zip(self.pixels.chunks_exact(4)) {
            // Assume an opaque result; blend any leftover transparency over white.
            let inv = 255 - p[3] as u32;
            let r = p[0] as u32 + inv;
            let g = p[1] as u32 + inv;
            let b = p[2] as u32 + inv;
            *o = (r.min(255) << 16) | (g.min(255) << 8) | b.min(255);
        }
    }

    /// Encode as PNG.
    pub fn encode_png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.w, self.h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            if let Ok(mut wr) = enc.write_header() {
                let _ = wr.write_image_data(&self.to_rgba());
            }
        }
        out
    }

    fn paint_span(&mut self, paint: &Paint, lut: &Option<Vec<[u32; 4]>>, m: &Mask, dx: i32, dy: i32) {
        let (cx0, cy0, cx1, cy1) = self.clip_rect();
        let masked = self.has_mask_clip();
        let w = self.w as i32;
        let (mx, my) = (m.x + dx, m.y + dy);
        let x0 = mx.max(cx0).max(0);
        let x1 = (mx + m.w as i32).min(cx1).min(w);
        let y0 = my.max(cy0).max(0);
        let y1 = (my + m.h as i32).min(cy1).min(self.h as i32);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let solid = match paint {
            Paint::Solid(c) => Some(c.premul()),
            _ => None,
        };
        let mut covs: Vec<u32> = Vec::new();
        for y in y0..y1 {
            if masked {
                covs.clear();
                for x in x0..x1 {
                    covs.push(self.clip_cov(x, y));
                }
            }
            let buf = self.buf();
            let mrow = ((y - my) as u32 * m.w) as usize;
            for x in x0..x1 {
                let mut cov = m.data[mrow + (x - mx) as usize] as u32;
                if masked {
                    cov = cov * covs[(x - x0) as usize] / 255;
                }
                if cov == 0 {
                    continue;
                }
                let src = match (solid, paint) {
                    (Some(s), _) => s,
                    (None, Paint::Linear { x0: gx0, y0: gy0, x1: gx1, y1: gy1, repeating, .. }) => {
                        let (dx, dy) = (gx1 - gx0, gy1 - gy0);
                        let len2 = (dx * dx + dy * dy).max(1e-6);
                        let mut t = ((x as f32 + 0.5 - gx0) * dx + (y as f32 + 0.5 - gy0) * dy) / len2;
                        if *repeating {
                            t = t.rem_euclid(1.0);
                        }
                        lut.as_ref().map(|l| l[(t.clamp(0.0, 1.0) * 255.0) as usize]).unwrap_or([0; 4])
                    }
                    (None, Paint::Radial { cx, cy, rx, ry, repeating, .. }) => {
                        let ddx = (x as f32 + 0.5 - cx) / rx.max(1e-3);
                        let ddy = (y as f32 + 0.5 - cy) / ry.max(1e-3);
                        let mut t = (ddx * ddx + ddy * ddy).sqrt();
                        if *repeating {
                            t = t.rem_euclid(1.0);
                        }
                        lut.as_ref().map(|l| l[(t.clamp(0.0, 1.0) * 255.0) as usize]).unwrap_or([0; 4])
                    }
                    _ => [0; 4],
                };
                let i = ((y * w + x) * 4) as usize;
                blend_px(&mut buf[i..i + 4], src, cov);
            }
        }
    }
}

impl Canvas for SoftwareCanvas {
    fn width(&self) -> u32 {
        self.w
    }
    fn height(&self) -> u32 {
        self.h
    }

    fn clear(&mut self, c: Color) {
        let p = c.premul();
        let px = [p[0] as u8, p[1] as u8, p[2] as u8, p[3] as u8];
        for chunk in self.buf().chunks_exact_mut(4) {
            chunk.copy_from_slice(&px);
        }
    }

    fn fill_rect(&mut self, r: Rect, c: Color) {
        if c.a == 0 || r.is_empty() {
            return;
        }
        // Fast path for pixel-aligned rects without mask clips.
        let aligned = r.x.fract() == 0.0 && r.y.fract() == 0.0 && r.w.fract() == 0.0 && r.h.fract() == 0.0;
        if aligned && !self.has_mask_clip() {
            let (cx0, cy0, cx1, cy1) = self.clip_rect();
            let x0 = (r.x as i32).max(cx0);
            let y0 = (r.y as i32).max(cy0);
            let x1 = (r.right() as i32).min(cx1);
            let y1 = (r.bottom() as i32).min(cy1);
            let src = c.premul();
            let w = self.w as i32;
            let opaque = c.a == 255;
            let buf = self.buf();
            for y in y0.max(0)..y1 {
                for x in x0.max(0)..x1 {
                    let i = ((y * w + x) * 4) as usize;
                    if opaque {
                        buf[i] = src[0] as u8;
                        buf[i + 1] = src[1] as u8;
                        buf[i + 2] = src[2] as u8;
                        buf[i + 3] = 255;
                    } else {
                        blend_px(&mut buf[i..i + 4], src, 255);
                    }
                }
            }
            return;
        }
        self.fill_path(&Path::rect(r), &Paint::Solid(c));
    }

    fn fill_path(&mut self, p: &Path, paint: &Paint) {
        let clip = self.clip_rect();
        let Some(m) = rasterize(p, clip) else { return };
        let lut = match paint {
            Paint::Linear { stops, .. } | Paint::Radial { stops, .. } => Some(gradient_lut(stops)),
            Paint::Solid(_) => None,
        };
        self.paint_span(paint, &lut, &m, 0, 0);
    }

    fn fill_mask(&mut self, m: &Mask, dx: i32, dy: i32, c: Color) {
        if c.a == 0 {
            return;
        }
        self.paint_span(&Paint::Solid(c), &None, m, dx, dy);
    }

    fn draw_image(&mut self, img: &ImageData, dst: Rect, opacity: f32, smooth: bool) {
        if dst.is_empty() || img.width == 0 || img.height == 0 {
            return;
        }
        let (cx0, cy0, cx1, cy1) = self.clip_rect();
        let x0 = (dst.x.floor() as i32).max(cx0).max(0);
        let y0 = (dst.y.floor() as i32).max(cy0).max(0);
        let x1 = (dst.right().ceil() as i32).min(cx1).min(self.w as i32);
        let y1 = (dst.bottom().ceil() as i32).min(cy1).min(self.h as i32);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let sx = img.width as f32 / dst.w;
        let sy = img.height as f32 / dst.h;
        let op = (opacity.clamp(0.0, 1.0) * 255.0) as u32;
        let masked = self.has_mask_clip();
        let w = self.w as i32;
        let (iw, ih) = (img.width as i32, img.height as i32);
        let px = &img.pixels;
        let fetch = |x: i32, y: i32| -> [u32; 4] {
            let x = x.clamp(0, iw - 1);
            let y = y.clamp(0, ih - 1);
            let i = ((y * iw + x) * 4) as usize;
            [px[i] as u32, px[i + 1] as u32, px[i + 2] as u32, px[i + 3] as u32]
        };
        let upscale = sx < 1.0 || sy < 1.0;
        for y in y0..y1 {
            // Edge coverage for fractional dst bounds.
            let fy = y as f32 + 0.5;
            if fy < dst.y || fy > dst.bottom() {
                continue;
            }
            for x in x0..x1 {
                let fx = x as f32 + 0.5;
                if fx < dst.x || fx > dst.right() {
                    continue;
                }
                let u = (fx - dst.x) * sx - 0.5;
                let v = (fy - dst.y) * sy - 0.5;
                let s = if smooth && (upscale || (sx - 1.0).abs() > 0.01 || (sy - 1.0).abs() > 0.01) {
                    if !upscale && (sx > 2.0 || sy > 2.0) {
                        // Box-filter downscale: average a small footprint.
                        let (fx0, fy0) =
                            ((u - sx / 2.0).floor() as i32 + 1, (v - sy / 2.0).floor() as i32 + 1);
                        let (nx, ny) = (sx.ceil().min(8.0) as i32, sy.ceil().min(8.0) as i32);
                        let mut acc = [0u32; 4];
                        for j in 0..ny {
                            for i in 0..nx {
                                let p = fetch(
                                    fx0 + i * (sx / nx as f32).max(1.0) as i32,
                                    fy0 + j * (sy / ny as f32).max(1.0) as i32,
                                );
                                for k in 0..4 {
                                    acc[k] += p[k];
                                }
                            }
                        }
                        let n = (nx * ny) as u32;
                        [acc[0] / n, acc[1] / n, acc[2] / n, acc[3] / n]
                    } else {
                        let (ux, vy) = (u.floor(), v.floor());
                        let (tx, ty) = (u - ux, v - vy);
                        let (ix, iy) = (ux as i32, vy as i32);
                        let a = fetch(ix, iy);
                        let b = fetch(ix + 1, iy);
                        let c = fetch(ix, iy + 1);
                        let d = fetch(ix + 1, iy + 1);
                        let mut o = [0u32; 4];
                        for k in 0..4 {
                            let top = a[k] as f32 * (1.0 - tx) + b[k] as f32 * tx;
                            let bot = c[k] as f32 * (1.0 - tx) + d[k] as f32 * tx;
                            o[k] = (top * (1.0 - ty) + bot * ty + 0.5) as u32;
                        }
                        o
                    }
                } else {
                    fetch((u + 0.5).floor() as i32, (v + 0.5).floor() as i32)
                };
                let mut cov = op;
                if masked {
                    cov = cov * self.clip_cov(x, y) / 255;
                }
                if cov == 0 {
                    continue;
                }
                let i = ((y * w + x) * 4) as usize;
                let buf = self.buf();
                blend_px(&mut buf[i..i + 4], s, cov);
            }
        }
    }

    fn push_clip(&mut self, r: Rect, radii: Option<Radii>) {
        let (px0, py0, px1, py1) = self.clip_rect();
        let x0 = (r.x.floor() as i32).max(px0);
        let y0 = (r.y.floor() as i32).max(py0);
        let x1 = (r.right().ceil() as i32).min(px1).max(x0);
        let y1 = (r.bottom().ceil() as i32).min(py1).max(y0);
        let mask = match radii {
            Some(rad) if !rad.is_zero() => rasterize(&Path::rounded_rect(r, rad), (x0, y0, x1, y1))
                .or(Some(Mask { x: x0, y: y0, w: 0, h: 0, data: Vec::new() })),
            _ => {
                // Fractional rect edges become a mask too, for exact anti-aliasing.
                None
            }
        };
        self.clips.push(Clip { rect: (x0, y0, x1, y1), mask });
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn push_layer(&mut self, opacity: f32) {
        self.layers.push(Layer { pixels: vec![0; (self.w * self.h * 4) as usize], opacity });
    }

    fn pop_layer(&mut self) {
        let Some(layer) = self.layers.pop() else { return };
        let op = (layer.opacity.clamp(0.0, 1.0) * 255.0) as u32;
        let dst = self.buf();
        for (d, s) in dst.chunks_exact_mut(4).zip(layer.pixels.chunks_exact(4)) {
            if s[3] == 0 {
                continue;
            }
            blend_px(d, [s[0] as u32, s[1] as u32, s[2] as u32, s[3] as u32], op);
        }
    }

    fn box_shadow(
        &mut self,
        r: Rect,
        radii: Radii,
        blur: f32,
        color: Color,
        clip_out: Option<(Rect, Radii)>,
    ) {
        let pad = (blur * 1.5).ceil() as i32 + 2;
        let (cx0, cy0, cx1, cy1) = self.clip_rect();
        let Some(m) = rasterize(&Path::rounded_rect(r, radii), (cx0 - pad, cy0 - pad, cx1 + pad, cy1 + pad))
        else {
            return;
        };
        let mut m = if blur > 0.0 { blur_mask(&m, blur / 2.0) } else { m };
        if let Some((cr, crad)) = clip_out {
            // Shadows are not drawn under the box itself (CSS Backgrounds 3 §7.1.1).
            if let Some(inner) =
                rasterize(&Path::rounded_rect(cr, crad), (m.x, m.y, m.x + m.w as i32, m.y + m.h as i32))
            {
                for y in 0..inner.h as i32 {
                    for x in 0..inner.w as i32 {
                        let (gx, gy) = (inner.x + x, inner.y + y);
                        let (lx, ly) = (gx - m.x, gy - m.y);
                        if lx >= 0 && ly >= 0 && lx < m.w as i32 && ly < m.h as i32 {
                            let idx = (ly as u32 * m.w + lx as u32) as usize;
                            let ic = inner.data[(y as u32 * inner.w + x as u32) as usize] as u32;
                            m.data[idx] = (m.data[idx] as u32 * (255 - ic) / 255) as u8;
                        }
                    }
                }
            }
        }
        self.fill_mask(&m, 0, 0, color);
    }
}

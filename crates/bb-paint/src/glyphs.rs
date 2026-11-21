//! Glyph rasterization and caching.

use crate::canvas::{Canvas, Color, ImageData};
use crate::path::{Path, PathCmd, Point, Rect};
use crate::raster::{rasterize, Mask};

use bb_text::{FontId, FontSystem, OutlineCmd};
use rustc_hash::FxHashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Subpixel horizontal positions per pixel.
const SUBPIXEL: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    font: FontId,
    glyph: u16,
    size: u32,
    sub: u8,
    bold: bool,
    italic: bool,
}

enum CachedGlyph {
    Mask(Mask),
    Bitmap { img: ImageData, x: f32, y: f32, w: f32, h: f32 },
    Empty,
}

struct GlyphCache {
    map: FxHashMap<GlyphKey, Arc<CachedGlyph>>,
}

static CACHE: OnceLock<Mutex<GlyphCache>> = OnceLock::new();

fn cache() -> std::sync::MutexGuard<'static, GlyphCache> {
    let m = CACHE.get_or_init(|| Mutex::new(GlyphCache { map: FxHashMap::default() }));
    match m.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Coverage contrast curve: slightly darkens AA edges so small text reads
/// crisply without hinting.
fn gamma_lut() -> &'static [u8; 256] {
    static LUT: OnceLock<[u8; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut l = [0u8; 256];
        for (i, v) in l.iter_mut().enumerate() {
            *v = ((i as f32 / 255.0).powf(1.0 / 1.25) * 255.0).round() as u8;
        }
        l
    })
}

fn outline_to_path(cmds: &[OutlineCmd], dx: f32, dy: f32) -> Path {
    let p = |x: f32, y: f32| Point::new(x + dx, y + dy);
    Path {
        cmds: cmds
            .iter()
            .map(|c| match *c {
                OutlineCmd::MoveTo(x, y) => PathCmd::MoveTo(p(x, y)),
                OutlineCmd::LineTo(x, y) => PathCmd::LineTo(p(x, y)),
                OutlineCmd::QuadTo(a, b, x, y) => PathCmd::QuadTo(p(a, b), p(x, y)),
                OutlineCmd::CubicTo(a, b, c, d, x, y) => PathCmd::CubicTo(p(a, b), p(c, d), p(x, y)),
                OutlineCmd::Close => PathCmd::Close,
            })
            .collect(),
    }
}

fn render_glyph(key: GlyphKey, size: f32) -> CachedGlyph {
    let mut fs = FontSystem::lock();
    if let Some(r) = fs.raster_glyph(key.font, key.glyph, size) {
        drop(fs);
        if let Ok(img) = bb_image::decode(&r.png) {
            return CachedGlyph::Bitmap {
                img: ImageData { width: img.width, height: img.height, pixels: img.frames[0].pixels.clone() },
                x: r.x,
                y: r.y,
                w: r.width,
                h: r.height,
            };
        }
        return CachedGlyph::Empty;
    }
    let Some(outline) = fs.outline(key.font, key.glyph, size) else { return CachedGlyph::Empty };
    drop(fs);
    if outline.cmds.is_empty() {
        return CachedGlyph::Empty;
    }
    let dx = key.sub as f32 / SUBPIXEL as f32;
    let mut path = outline_to_path(&outline.cmds, dx, 0.0);
    if key.italic {
        path = path.skew_x(0.2, 0.0);
    }
    let big = (-4096, -4096, 4096, 4096);
    let Some(mut m) = rasterize(&path, big) else { return CachedGlyph::Empty };
    if key.bold {
        // Synthetic bold: smear the glyph horizontally.
        let shift = (size / 28.0).clamp(0.5, 3.0);
        let p2 = path.transform(1.0, 1.0, shift, 0.0);
        if let Some(m2) = rasterize(&p2, big) {
            let x0 = m.x.min(m2.x);
            let y0 = m.y.min(m2.y);
            let x1 = (m.x + m.w as i32).max(m2.x + m2.w as i32);
            let y1 = (m.y + m.h as i32).max(m2.y + m2.h as i32);
            let (w, h) = ((x1 - x0) as u32, (y1 - y0) as u32);
            let mut data = vec![0u8; (w * h) as usize];
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    data[(y as u32 * w + x as u32) as usize] =
                        m.get(x0 + x, y0 + y).max(m2.get(x0 + x, y0 + y));
                }
            }
            m = Mask { x: x0, y: y0, w, h, data };
        }
    }
    let lut = gamma_lut();
    for v in m.data.iter_mut() {
        *v = lut[*v as usize];
    }
    CachedGlyph::Mask(m)
}

/// A glyph positioned in device pixels (pen position on the baseline).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedGlyph {
    pub font: FontId,
    pub id: u16,
    pub x: f32,
    pub y: f32,
}

/// Draw glyphs at device positions with `size` in device px.
pub fn draw_glyphs<C: Canvas + ?Sized>(
    canvas: &mut C,
    glyphs: &[PlacedGlyph],
    size: f32,
    color: Color,
    bold: bool,
    italic: bool,
) {
    if color.a == 0 || size <= 0.0 || size > 2000.0 {
        return;
    }
    let size_q = (size * 8.0).round() as u32;
    let size = size_q as f32 / 8.0;
    for g in glyphs {
        let xf = g.x.floor();
        let sub = ((g.x - xf) * SUBPIXEL as f32).floor() as u8 % SUBPIXEL as u8;
        let key = GlyphKey { font: g.font, glyph: g.id, size: size_q, sub, bold, italic };
        let cached = {
            let c = cache();
            c.map.get(&key).cloned()
        };
        let cached = match cached {
            Some(c) => c,
            None => {
                let r = Arc::new(render_glyph(key, size));
                let mut c = cache();
                if c.map.len() > 20_000 {
                    c.map.clear();
                }
                c.map.insert(key, r.clone());
                r
            }
        };
        let by = g.y.round();
        match &*cached {
            CachedGlyph::Mask(m) => {
                canvas.fill_mask(m, xf as i32, by as i32, color);
            }
            CachedGlyph::Bitmap { img, x, y, w, h } => {
                canvas.draw_image(img, Rect::new(g.x + x, g.y + y, *w, *h), color.a as f32 / 255.0, true);
            }
            CachedGlyph::Empty => {}
        }
    }
}

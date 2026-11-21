//! UI text helpers: shaping with the platform UI font chain.

use bb_text::{FontId, FontSystem};

/// The UI font chain: Segoe UI Variable / Segoe UI (Windows), SF Pro /
/// system-ui (macOS), Inter / Noto Sans / DejaVu Sans (Linux).
pub fn ui_families() -> Vec<String> {
    vec!["system-ui".to_string(), "sans-serif".to_string()]
}

/// Shape `s` at `size` px with the UI font; returns glyphs (font, id, x, y), width, synthetic bold.
pub fn shape_ui(s: &str, size: f32, weight: u16) -> (Vec<(FontId, u16, f32, f32)>, f32, bool) {
    let mut fs = FontSystem::lock();
    let list = fs.resolve(&ui_families(), weight, false);
    let shaped = fs.shape(s, &list, size);
    let mut x = 0.0;
    let mut out = Vec::with_capacity(shaped.glyphs.len());
    for g in &shaped.glyphs {
        out.push((g.font, g.id, x + g.x_offset, g.y_offset));
        x += g.x_advance;
    }
    (out, shaped.width, shaped.synthetic_bold)
}

pub fn measure(s: &str, size: f32, weight: u16) -> f32 {
    let mut fs = FontSystem::lock();
    let list = fs.resolve(&ui_families(), weight, false);
    fs.shape(s, &list, size).width
}

/// x offset of each char boundary (byte index → x), for caret placement.
pub fn boundaries(s: &str, size: f32) -> Vec<(usize, f32)> {
    let mut out = Vec::new();
    let mut x = 0.0;
    out.push((0, 0.0));
    for (i, c) in s.char_indices() {
        let mut b = [0u8; 4];
        x += measure(c.encode_utf8(&mut b), size, 400);
        out.push((i + c.len_utf8(), x));
    }
    // Scale to the true shaped width (kerning).
    let total = measure(s, size, 400);
    if x > 0.0 {
        let k = total / x;
        for p in out.iter_mut() {
            p.1 *= k;
        }
    }
    out
}

/// Ascent of the UI font at `size` (for vertical centering).
pub fn ascent(size: f32) -> f32 {
    let mut fs = FontSystem::lock();
    let list = fs.resolve(&ui_families(), 400, false);
    let m = fs.metrics(list.primary());
    m.ascent * size
}

/// Baseline that vertically centers a line of text in a box of height `h` at `y`.
pub fn centered_baseline(y: f32, h: f32, size: f32) -> f32 {
    let mut fs = FontSystem::lock();
    let list = fs.resolve(&ui_families(), 400, false);
    let m = fs.metrics(list.primary());
    let (a, d) = (m.ascent * size, m.descent * size);
    y + (h - (a + d)) / 2.0 + a
}

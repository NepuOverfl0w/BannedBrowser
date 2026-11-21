//! `bb-image` — image decoding to premultiplied RGBA8.
//!
//! Wraps the allowed codecs (`png`, `zune-jpeg`, `gif`, `image-webp`) and
//! implements BMP and ICO decoding itself. All decoders are limited in size
//! and never panic on malformed input.
#![forbid(unsafe_code)]

use std::io::Cursor;
use std::sync::Arc;

/// Maximum decoded pixel count (protects memory).
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;
pub const MAX_DIM: u32 = 16384;

/// One decoded frame: premultiplied RGBA8, row-major.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub pixels: Arc<Vec<u8>>,
    /// Display time in milliseconds (animated images).
    pub delay_ms: u32,
}

/// A decoded image.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<Frame>,
}

impl Image {
    /// The first (or only) frame's premultiplied RGBA pixels.
    pub fn pixels(&self) -> &[u8] {
        &self.frames[0].pixels
    }

    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }

    /// Build from straight-alpha RGBA.
    pub fn from_rgba(width: u32, height: u32, mut rgba: Vec<u8>) -> Option<Image> {
        if rgba.len() != (width as usize) * (height as usize) * 4 || width == 0 || height == 0 {
            return None;
        }
        premultiply(&mut rgba);
        Some(Image { width, height, frames: vec![Frame { pixels: Arc::new(rgba), delay_ms: 0 }] })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    UnknownFormat,
    TooLarge,
    Corrupt(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::UnknownFormat => f.write_str("unknown image format"),
            DecodeError::TooLarge => f.write_str("image too large"),
            DecodeError::Corrupt(s) => write!(f, "corrupt image: {s}"),
        }
    }
}

impl std::error::Error for DecodeError {}

pub fn premultiply(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a != 255 {
            px[0] = ((px[0] as u32 * a + 127) / 255) as u8;
            px[1] = ((px[1] as u32 * a + 127) / 255) as u8;
            px[2] = ((px[2] as u32 * a + 127) / 255) as u8;
        }
    }
}

fn check_dims(w: u32, h: u32) -> Result<(), DecodeError> {
    if w == 0 || h == 0 {
        return Err(DecodeError::Corrupt("zero-sized image".into()));
    }
    if w > MAX_DIM || h > MAX_DIM || (w as u64) * (h as u64) > MAX_PIXELS {
        return Err(DecodeError::TooLarge);
    }
    Ok(())
}

/// Sniff and decode any supported format.
pub fn decode(bytes: &[u8]) -> Result<Image, DecodeError> {
    let r = std::panic::catch_unwind(|| decode_inner(bytes));
    match r {
        Ok(r) => r,
        Err(_) => Err(DecodeError::Corrupt("decoder panicked".into())),
    }
}

fn decode_inner(bytes: &[u8]) -> Result<Image, DecodeError> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        decode_jpeg(bytes)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        decode_gif(bytes)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        decode_webp(bytes)
    } else if bytes.starts_with(b"BM") {
        decode_bmp(bytes, false)
    } else if bytes.starts_with(&[0, 0, 1, 0]) || bytes.starts_with(&[0, 0, 2, 0]) {
        decode_ico(bytes)
    } else {
        Err(DecodeError::UnknownFormat)
    }
}

pub fn decode_png(bytes: &[u8]) -> Result<Image, DecodeError> {
    let mut dec = png::Decoder::new(Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(|e| DecodeError::Corrupt(e.to_string()))?;
    let (w, h) = (reader.info().width, reader.info().height);
    check_dims(w, h)?;
    let size = reader.output_buffer_size().ok_or(DecodeError::TooLarge)?;
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).map_err(|e| DecodeError::Corrupt(e.to_string()))?;
    let n = (w * h) as usize;
    let mut rgba = Vec::with_capacity(n * 4);
    let line = info.line_size;
    for y in 0..h as usize {
        let row = &buf[y * line..(y + 1) * line];
        match info.color_type {
            png::ColorType::Rgba => rgba.extend_from_slice(&row[..w as usize * 4]),
            png::ColorType::Rgb => {
                for p in row[..w as usize * 3].chunks_exact(3) {
                    rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
                }
            }
            png::ColorType::GrayscaleAlpha => {
                for p in row[..w as usize * 2].chunks_exact(2) {
                    rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
                }
            }
            png::ColorType::Grayscale => {
                for &g in &row[..w as usize] {
                    rgba.extend_from_slice(&[g, g, g, 255]);
                }
            }
            png::ColorType::Indexed => return Err(DecodeError::Corrupt("unexpanded palette".into())),
        }
    }
    Image::from_rgba(w, h, rgba).ok_or_else(|| DecodeError::Corrupt("size mismatch".into()))
}

pub fn decode_jpeg(bytes: &[u8]) -> Result<Image, DecodeError> {
    use zune_core::colorspace::ColorSpace;
    use zune_core::options::DecoderOptions;
    let opts = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGBA)
        .set_max_width(MAX_DIM as usize)
        .set_max_height(MAX_DIM as usize);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(Cursor::new(bytes), opts);
    dec.decode_headers().map_err(|e| DecodeError::Corrupt(format!("{e:?}")))?;
    let info = dec.info().ok_or_else(|| DecodeError::Corrupt("no header".into()))?;
    let (w, h) = (info.width as u32, info.height as u32);
    check_dims(w, h)?;
    let px = dec.decode().map_err(|e| DecodeError::Corrupt(format!("{e:?}")))?;
    let out_cs = dec.output_colorspace().unwrap_or(ColorSpace::RGBA);
    let rgba = match out_cs {
        ColorSpace::RGBA => px,
        ColorSpace::RGB => px.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        ColorSpace::Luma => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        _ => return Err(DecodeError::Corrupt(format!("unsupported JPEG colorspace {out_cs:?}"))),
    };
    Image::from_rgba(w, h, rgba).ok_or_else(|| DecodeError::Corrupt("size mismatch".into()))
}

/// GIF with all frames composited per the disposal methods (capped at 500 frames).
pub fn decode_gif(bytes: &[u8]) -> Result<Image, DecodeError> {
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);
    let mut dec = opts.read_info(Cursor::new(bytes)).map_err(|e| DecodeError::Corrupt(e.to_string()))?;
    let (sw, sh) = (dec.width() as u32, dec.height() as u32);
    let mut frames: Vec<Frame> = Vec::new();
    let mut canvas: Vec<u8> = Vec::new();
    let mut prev: Option<Vec<u8>> = None;
    let mut w = sw;
    let mut h = sh;
    loop {
        let frame = match dec.read_next_frame() {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                if frames.is_empty() {
                    return Err(DecodeError::Corrupt(e.to_string()));
                }
                break; // keep the frames decoded so far
            }
        };
        if canvas.is_empty() {
            // Some GIFs have a zero logical screen; use the first frame's extent.
            w = w.max(frame.left as u32 + frame.width as u32);
            h = h.max(frame.top as u32 + frame.height as u32);
            check_dims(w, h)?;
            canvas = vec![0u8; (w * h * 4) as usize];
        }
        if frame.dispose == gif::DisposalMethod::Previous {
            prev = Some(canvas.clone());
        }
        let (fx, fy, fw, fh) = (frame.left as u32, frame.top as u32, frame.width as u32, frame.height as u32);
        for y in 0..fh {
            let cy = fy + y;
            if cy >= h {
                break;
            }
            for x in 0..fw {
                let cx = fx + x;
                if cx >= w {
                    break;
                }
                let si = ((y * fw + x) * 4) as usize;
                let di = ((cy * w + cx) * 4) as usize;
                if si + 3 < frame.buffer.len() && frame.buffer[si + 3] != 0 {
                    canvas[di..di + 4].copy_from_slice(&frame.buffer[si..si + 4]);
                }
            }
        }
        let mut out = canvas.clone();
        premultiply(&mut out);
        let delay = (frame.delay as u32 * 10).max(if frame.delay <= 1 { 100 } else { 20 });
        frames.push(Frame { pixels: Arc::new(out), delay_ms: delay });
        match frame.dispose {
            gif::DisposalMethod::Background => {
                for y in fy..(fy + fh).min(h) {
                    for x in fx..(fx + fw).min(w) {
                        let di = ((y * w + x) * 4) as usize;
                        canvas[di..di + 4].copy_from_slice(&[0, 0, 0, 0]);
                    }
                }
            }
            gif::DisposalMethod::Previous => {
                if let Some(p) = prev.take() {
                    canvas = p;
                }
            }
            _ => {}
        }
        if frames.len() >= 500 {
            break;
        }
    }
    if frames.is_empty() {
        return Err(DecodeError::Corrupt("no frames".into()));
    }
    Ok(Image { width: w, height: h, frames })
}

pub fn decode_webp(bytes: &[u8]) -> Result<Image, DecodeError> {
    let mut dec =
        image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|e| DecodeError::Corrupt(e.to_string()))?;
    let (w, h) = dec.dimensions();
    check_dims(w, h)?;
    let size = dec.output_buffer_size().ok_or(DecodeError::TooLarge)?;
    let mut buf = vec![0u8; size];
    dec.read_image(&mut buf).map_err(|e| DecodeError::Corrupt(e.to_string()))?;
    let rgba = if dec.has_alpha() {
        buf
    } else {
        buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
    };
    Image::from_rgba(w, h, rgba).ok_or_else(|| DecodeError::Corrupt("size mismatch".into()))
}

fn le16(b: &[u8], i: usize) -> Option<u32> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32)
}

fn le32(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?]))
}

/// BMP (BITMAPINFOHEADER and later; 1/4/8/24/32 bpp, uncompressed or BI_BITFIELDS).
/// `ico` = the DIB is embedded in an ICO (no file header, doubled height + AND mask).
pub fn decode_bmp(bytes: &[u8], ico: bool) -> Result<Image, DecodeError> {
    let bad = || DecodeError::Corrupt("bad BMP".into());
    let (dib, pixel_offset_hint) =
        if ico { (0usize, None) } else { (14usize, Some(le32(bytes, 10).ok_or_else(bad)? as usize)) };
    let header_size = le32(bytes, dib).ok_or_else(bad)? as usize;
    if header_size < 40 {
        return Err(DecodeError::Corrupt("OS/2 BMP headers unsupported".into()));
    }
    let width = le32(bytes, dib + 4).ok_or_else(bad)? as i32;
    let mut height = le32(bytes, dib + 8).ok_or_else(bad)? as i32;
    let bpp = le16(bytes, dib + 14).ok_or_else(bad)?;
    let compression = le32(bytes, dib + 16).ok_or_else(bad)?;
    let colors_used = le32(bytes, dib + 32).ok_or_else(bad)?;
    if ico {
        height /= 2;
    }
    let top_down = height < 0;
    let (w, h) = (width.unsigned_abs(), height.unsigned_abs());
    check_dims(w, h)?;
    if !(compression == 0 || compression == 3) {
        return Err(DecodeError::Corrupt("compressed BMP unsupported".into()));
    }
    let palette_start = dib + header_size + if compression == 3 && header_size == 40 { 12 } else { 0 };
    let palette_len = if bpp <= 8 {
        if colors_used == 0 {
            1 << bpp
        } else {
            colors_used.min(256)
        }
    } else {
        0
    } as usize;
    let palette: Vec<[u8; 4]> = (0..palette_len)
        .map(|i| {
            let p = palette_start + i * 4;
            [
                *bytes.get(p + 2).unwrap_or(&0),
                *bytes.get(p + 1).unwrap_or(&0),
                *bytes.get(p).unwrap_or(&0),
                255,
            ]
        })
        .collect();
    let pixels_start = pixel_offset_hint.unwrap_or(palette_start + palette_len * 4);
    let stride = ((w as usize * bpp as usize).div_ceil(32)) * 4;
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let mut any_alpha = false;
    for row in 0..h as usize {
        let src_row = if top_down { row } else { h as usize - 1 - row };
        let base = pixels_start + src_row * stride;
        for x in 0..w as usize {
            let px: [u8; 4] = match bpp {
                32 => {
                    let i = base + x * 4;
                    let a = *bytes.get(i + 3).unwrap_or(&255);
                    if a != 0 {
                        any_alpha = true;
                    }
                    [
                        *bytes.get(i + 2).unwrap_or(&0),
                        *bytes.get(i + 1).unwrap_or(&0),
                        *bytes.get(i).unwrap_or(&0),
                        a,
                    ]
                }
                24 => {
                    let i = base + x * 3;
                    [
                        *bytes.get(i + 2).unwrap_or(&0),
                        *bytes.get(i + 1).unwrap_or(&0),
                        *bytes.get(i).unwrap_or(&0),
                        255,
                    ]
                }
                8 | 4 | 1 => {
                    let bit = x * bpp as usize;
                    let byte = *bytes.get(base + bit / 8).unwrap_or(&0);
                    let shift = 8 - bpp as usize - (bit % 8);
                    let idx = ((byte >> shift) & ((1u16 << bpp) - 1) as u8) as usize;
                    *palette.get(idx).unwrap_or(&[0, 0, 0, 255])
                }
                _ => return Err(DecodeError::Corrupt(format!("{bpp}-bit BMP unsupported"))),
            };
            let o = (row * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&px);
        }
    }
    if bpp == 32 && !any_alpha {
        for p in rgba.chunks_exact_mut(4) {
            p[3] = 255;
        }
    }
    if ico && bpp != 32 {
        // Apply the 1-bpp AND mask after the XOR bitmap.
        let mask_start = pixels_start + stride * h as usize;
        let mstride = (w as usize).div_ceil(32) * 4;
        for row in 0..h as usize {
            let src_row = if top_down { row } else { h as usize - 1 - row };
            for x in 0..w as usize {
                let byte = *bytes.get(mask_start + src_row * mstride + x / 8).unwrap_or(&0);
                if (byte >> (7 - x % 8)) & 1 == 1 {
                    rgba[(row * w as usize + x) * 4 + 3] = 0;
                }
            }
        }
    }
    Image::from_rgba(w, h, rgba).ok_or_else(bad)
}

/// ICO / CUR: pick the largest entry (PNG or BMP payload).
pub fn decode_ico(bytes: &[u8]) -> Result<Image, DecodeError> {
    let bad = || DecodeError::Corrupt("bad ICO".into());
    let count = le16(bytes, 4).ok_or_else(bad)? as usize;
    let mut best: Option<(u32, usize, usize)> = None;
    for i in 0..count.min(64) {
        let e = 6 + i * 16;
        let w = *bytes.get(e).ok_or_else(bad)? as u32;
        let w = if w == 0 { 256 } else { w };
        let size = le32(bytes, e + 8).ok_or_else(bad)? as usize;
        let offset = le32(bytes, e + 12).ok_or_else(bad)? as usize;
        if offset.checked_add(size).is_none_or(|end| end > bytes.len()) {
            continue;
        }
        if best.is_none_or(|(bw, _, _)| w > bw) {
            best = Some((w, offset, size));
        }
    }
    let (_, off, size) = best.ok_or_else(bad)?;
    let data = &bytes[off..off + size];
    if data.starts_with(b"\x89PNG") {
        decode_png(data)
    } else {
        decode_bmp(data, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(rgba).unwrap();
        }
        out
    }

    #[test]
    fn png_roundtrip_premultiplies() {
        let data = png_bytes(2, 1, &[255, 0, 0, 255, 0, 255, 0, 128]);
        let img = decode(&data).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(&img.pixels()[..4], &[255, 0, 0, 255]);
        assert_eq!(&img.pixels()[4..], &[0, 128, 0, 128]);
    }

    #[test]
    fn gif_decodes() {
        let mut out = Vec::new();
        {
            let mut enc = gif::Encoder::new(&mut out, 2, 2, &[0, 0, 0, 255, 255, 255]).unwrap();
            let f = gif::Frame {
                width: 2,
                height: 2,
                buffer: std::borrow::Cow::Borrowed(&[0, 1, 1, 0]),
                ..Default::default()
            };
            enc.write_frame(&f).unwrap();
        }
        let img = decode(&out).unwrap();
        assert_eq!(img.width, 2);
        assert_eq!(&img.pixels()[4..8], &[255, 255, 255, 255]);
    }

    #[test]
    fn bmp_24bit() {
        // 1x1 BMP, blue pixel.
        let mut b = vec![b'B', b'M'];
        b.extend_from_slice(&(58u32).to_le_bytes());
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(&(54u32).to_le_bytes());
        b.extend_from_slice(&(40u32).to_le_bytes());
        b.extend_from_slice(&(1i32).to_le_bytes());
        b.extend_from_slice(&(1i32).to_le_bytes());
        b.extend_from_slice(&(1u16).to_le_bytes());
        b.extend_from_slice(&(24u16).to_le_bytes());
        b.extend_from_slice(&[0; 24]);
        b.extend_from_slice(&[255, 0, 0, 0]);
        let img = decode(&b).unwrap();
        assert_eq!(img.pixels(), &[0, 0, 255, 255]);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert_eq!(decode(b"nope"), Err(DecodeError::UnknownFormat));
        let mut seed = 7u32;
        for len in [8usize, 30, 100, 500] {
            for prefix in [
                &b"\x89PNG\r\n\x1a\n"[..],
                b"GIF89a",
                &[0xFF, 0xD8, 0xFF],
                b"BM",
                &[0, 0, 1, 0],
                b"RIFF\0\0\0\0WEBP",
            ] {
                let mut v = prefix.to_vec();
                while v.len() < len {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    v.push((seed >> 24) as u8);
                }
                let _ = decode(&v);
            }
        }
    }
}

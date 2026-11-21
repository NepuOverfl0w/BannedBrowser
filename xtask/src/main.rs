//! Project automation: `cargo run -p xtask -- icons` renders the original
//! BannedBrowser logo (from `bb_ui::icons::logo_cells`) with our own
//! rasterizer into PNGs (Linux/README), .icns-ready sizes (macOS) and a
//! multi-size .ico (Windows).

use bb_paint::{Canvas, Color, Paint, Path, Radii, Rect, SoftwareCanvas};

fn render_icon(size: u32, dark_bg: bool) -> SoftwareCanvas {
    let mut c = SoftwareCanvas::new(size, size);
    c.clear(Color::TRANSPARENT);
    let s = size as f32;
    // Rounded-square tile.
    let bg = if dark_bg { Color::hex(0x15141A) } else { Color::hex(0x1C1B22) };
    c.fill_path(&Path::rounded_rect(Rect::new(0.0, 0.0, s, s), Radii::uniform(s * 0.22)), &Paint::Solid(bg));
    let cell = s / 16.0;
    let cells = bb_ui::icons::logo_cells();
    // Cyan glitch echo, offset down-left.
    for &(x, y, k) in &cells {
        if k == 0 {
            let r = Rect::new((x as f32 - 0.35) * cell, (y as f32 + 0.3) * cell, cell, cell);
            c.fill_rect(r, Color::hex(0x3DF2E0).with_alpha(0.55));
        }
    }
    for &(x, y, k) in &cells {
        let col = match k {
            0 => Color::hex(0x7C5CFF),
            1 => Color::hex(0x3DF2E0),
            _ => Color::hex(0xFBFBFE),
        };
        let inset = if size >= 64 { cell * 0.06 } else { 0.0 };
        c.fill_rect(
            Rect::new(
                x as f32 * cell + inset,
                y as f32 * cell + inset,
                cell - 2.0 * inset,
                cell - 2.0 * inset,
            ),
            col,
        );
    }
    c
}

/// Wordmark banner for the README hero (logo + name), light or dark text.
fn render_banner(dark_text_bg: bool) -> SoftwareCanvas {
    let (w, h) = (720u32, 160u32);
    let mut c = SoftwareCanvas::new(w, h);
    c.clear(Color::TRANSPARENT);
    let icon = render_icon(128, true);
    let img =
        bb_paint::ImageData { width: 128, height: 128, pixels: std::sync::Arc::new(icon.pixels.clone()) };
    c.draw_image(&img, Rect::new(16.0, 16.0, 128.0, 128.0), 1.0, false);
    let mut dl = bb_paint::DisplayList::default();
    let mut p = bb_ui::Painter::new(&mut dl);
    let text_col = if dark_text_bg { Color::hex(0xFBFBFE) } else { Color::hex(0x15141A) };
    let x = 168.0;
    let w1 = p.text("Banned", x, 102.0, 64.0, 700, Color::hex(0x7C5CFF));
    let w2 = p.text("Browser", x + w1, 102.0, 64.0, 700, text_col);
    p.text("_", x + w1 + w2 + 4.0, 102.0, 64.0, 700, Color::hex(0x3DF2E0));
    p.text(
        "no engines borrowed · no rules followed",
        x + 4.0,
        136.0,
        20.0,
        400,
        if dark_text_bg { Color::hex(0xBFBFC9) } else { Color::hex(0x5B5B66) },
    );
    bb_paint::render(&dl, &mut c, (0.0, 0.0), 1.0);
    c
}

fn write_ico(path: &str, sizes: &[u32]) -> std::io::Result<()> {
    let pngs: Vec<Vec<u8>> = sizes.iter().map(|&s| render_icon(s, false).encode_png()).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (s, png) in sizes.iter().zip(&pngs) {
        let b = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[b, b, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in pngs {
        out.extend_from_slice(&png);
    }
    std::fs::write(path, out)
}

fn main() -> std::io::Result<()> {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    match cmd.as_str() {
        "icons" => {
            std::fs::create_dir_all("resources/icons")?;
            std::fs::create_dir_all("docs/assets")?;
            for s in [16, 32, 48, 64, 128, 256, 512, 1024] {
                std::fs::write(
                    format!("resources/icons/bannedbrowser-{s}.png"),
                    render_icon(s, false).encode_png(),
                )?;
            }
            write_ico("resources/icons/bannedbrowser.ico", &[16, 24, 32, 48, 64, 256])?;
            std::fs::write("docs/assets/logo-light.png", render_banner(false).encode_png())?;
            std::fs::write("docs/assets/logo-dark.png", render_banner(true).encode_png())?;
            std::fs::write("docs/assets/icon-256.png", render_icon(256, false).encode_png())?;
            println!("icons written to resources/icons/ and docs/assets/");
        }
        _ => {
            eprintln!("usage: cargo run -p xtask -- icons");
            std::process::exit(2);
        }
    }
    Ok(())
}

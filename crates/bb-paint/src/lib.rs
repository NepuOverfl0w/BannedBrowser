//! `bb-paint` — display lists and the software rasterizer.
//!
//! The [`Canvas`] trait is the single paint path for web content *and* the
//! browser chrome. [`SoftwareCanvas`] (CPU, premultiplied RGBA8) is the
//! reference implementation used by every test.

pub mod canvas;
pub mod canvas2d;
pub mod display;
pub mod glyphs;
pub mod path;
pub mod raster;

pub use canvas::{Canvas, Color, ImageData, Paint, SoftwareCanvas, Stop};
pub use display::{
    render, render_with, BorderSide, BorderStyle, DisplayList, GlyphRun, Item, SubDocumentPlacement,
};
pub use path::{Path, Point, Radii, Rect};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_and_blend() {
        let mut c = SoftwareCanvas::new(10, 10);
        c.clear(Color::WHITE);
        c.fill_rect(Rect::new(0.0, 0.0, 5.0, 5.0), Color::rgb(255, 0, 0));
        assert_eq!(&c.pixels[0..4], &[255, 0, 0, 255]);
        c.fill_rect(Rect::new(0.0, 0.0, 5.0, 5.0), Color::rgba(0, 0, 255, 128));
        let p = &c.pixels[0..4];
        assert!(p[0] > 120 && p[0] < 135 && p[2] > 120 && p[2] < 135, "{p:?}");
    }

    #[test]
    fn clip_and_layer() {
        let mut c = SoftwareCanvas::new(10, 10);
        c.clear(Color::WHITE);
        c.push_clip(Rect::new(2.0, 2.0, 3.0, 3.0), None);
        c.fill_rect(Rect::new(0.0, 0.0, 10.0, 10.0), Color::BLACK);
        c.pop_clip();
        assert_eq!(c.pixels[0], 255);
        assert_eq!(c.pixels[((3 * 10 + 3) * 4) as usize], 0);
        c.push_layer(0.5);
        c.fill_rect(Rect::new(0.0, 0.0, 1.0, 1.0), Color::BLACK);
        c.pop_layer();
        assert!((c.pixels[0] as i32 - 128).abs() <= 2);
    }

    #[test]
    fn text_renders_ink() {
        let mut fs = bb_text::FontSystem::lock();
        let list = fs.resolve(&["sans-serif".to_string()], 400, false);
        let shaped = fs.shape("Hi", &list, 32.0);
        drop(fs);
        let mut dl = DisplayList::default();
        let mut x = 4.0;
        let glyphs = shaped
            .glyphs
            .iter()
            .map(|g| {
                let r = (g.font, g.id, x + g.x_offset, 36.0);
                x += g.x_advance;
                r
            })
            .collect();
        dl.push(Item::Text(GlyphRun { glyphs, size: 32.0, color: Color::BLACK, bold: false, italic: false }));
        let mut c = SoftwareCanvas::new(64, 48);
        c.clear(Color::WHITE);
        render(&dl, &mut c, (0.0, 0.0), 1.0);
        let dark = c.pixels.chunks_exact(4).filter(|p| p[0] < 100).count();
        assert!(dark > 50, "glyphs should produce ink, got {dark} dark pixels");
    }

    #[test]
    fn borders_and_rounded() {
        let mut dl = DisplayList::default();
        let side = Some(BorderSide { width: 2.0, color: Color::BLACK, style: BorderStyle::Solid });
        dl.push(Item::Border {
            rect: Rect::new(1.0, 1.0, 20.0, 20.0),
            radii: Radii::default(),
            sides: [side; 4],
        });
        dl.push(Item::Border {
            rect: Rect::new(30.0, 1.0, 20.0, 20.0),
            radii: Radii::uniform(6.0),
            sides: [side; 4],
        });
        let mut c = SoftwareCanvas::new(60, 30);
        c.clear(Color::WHITE);
        render(&dl, &mut c, (0.0, 0.0), 1.0);
        let px = |x: u32, y: u32| c.pixels[((y * 60 + x) * 4) as usize];
        assert_eq!(px(1, 10), 0);
        assert_eq!(px(10, 10), 255);
        assert_eq!(px(30, 1), 255, "rounded corner is clear");
        assert_eq!(px(40, 1), 0);
    }
}

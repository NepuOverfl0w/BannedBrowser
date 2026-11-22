//! `bb-ui` — the widget toolkit for BannedBrowser's chrome.
//!
//! Everything paints through `bb-paint` display lists (the same path as web
//! content), in logical pixels; the shell renders them at the window's scale
//! factor. Icons are original vector path data in a Proton-like line style.

pub mod icons;
pub mod text;
pub mod textfield;

use bb_paint::{Color, DisplayList, GlyphRun, Item, Paint, Path, Radii, Rect};

/// Chrome color theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub dark: bool,
    pub private: bool,
    /// Tab strip / frame background.
    pub frame: Color,
    /// Selected tab and toolbar background.
    pub toolbar: Color,
    pub tab_hover: Color,
    pub field: Color,
    pub field_border: Color,
    pub text: Color,
    pub text_muted: Color,
    pub button_hover: Color,
    pub button_active: Color,
    pub separator: Color,
    pub accent: Color,
    pub glitch: Color,
    pub popup: Color,
    pub shadow: Color,
}

/// Brand accent: electric violet.
pub const ACCENT: Color = Color::hex(0x7C5CFF);
/// Cyan glitch highlight.
pub const GLITCH: Color = Color::hex(0x3DF2E0);

impl Theme {
    pub fn light() -> Theme {
        Theme {
            dark: false,
            private: false,
            frame: Color::hex(0xF0F0F4),
            toolbar: Color::hex(0xF9F9FB),
            tab_hover: Color::rgba(0x15, 0x14, 0x1A, 0x14),
            field: Color::hex(0xFFFFFF),
            field_border: Color::rgba(0x15, 0x14, 0x1A, 0x30),
            text: Color::hex(0x15141A),
            text_muted: Color::hex(0x5B5B66),
            button_hover: Color::rgba(0x15, 0x14, 0x1A, 0x18),
            button_active: Color::rgba(0x15, 0x14, 0x1A, 0x30),
            separator: Color::rgba(0x15, 0x14, 0x1A, 0x1E),
            accent: ACCENT,
            glitch: GLITCH,
            popup: Color::hex(0xFFFFFF),
            shadow: Color::rgba(0, 0, 0, 0x40),
        }
    }

    pub fn dark() -> Theme {
        Theme {
            dark: true,
            private: false,
            frame: Color::hex(0x1C1B22),
            toolbar: Color::hex(0x2B2A33),
            tab_hover: Color::rgba(0xFB, 0xFB, 0xFE, 0x16),
            field: Color::hex(0x1C1B22),
            field_border: Color::rgba(0xFB, 0xFB, 0xFE, 0x30),
            text: Color::hex(0xFBFBFE),
            text_muted: Color::hex(0xBFBFC9),
            button_hover: Color::rgba(0xFB, 0xFB, 0xFE, 0x1A),
            button_active: Color::rgba(0xFB, 0xFB, 0xFE, 0x30),
            separator: Color::rgba(0xFB, 0xFB, 0xFE, 0x1E),
            accent: Color::hex(0x9A80FF),
            glitch: GLITCH,
            popup: Color::hex(0x42414D),
            shadow: Color::rgba(0, 0, 0, 0x80),
        }
    }

    /// Private windows: a distinct dark-violet frame.
    pub fn private() -> Theme {
        Theme {
            private: true,
            frame: Color::hex(0x25003E),
            toolbar: Color::hex(0x3A1B5C),
            field: Color::hex(0x25003E),
            tab_hover: Color::rgba(0xFF, 0xFF, 0xFF, 0x18),
            ..Theme::dark()
        }
    }
}

/// Paint helpers that append to a display list (logical px).
pub struct Painter<'a> {
    pub dl: &'a mut DisplayList,
}

impl<'a> Painter<'a> {
    pub fn new(dl: &'a mut DisplayList) -> Self {
        Painter { dl }
    }

    pub fn rect(&mut self, r: Rect, c: Color) {
        self.dl.push(Item::Rect { rect: r, color: c });
    }

    pub fn rounded(&mut self, r: Rect, radius: f32, c: Color) {
        self.dl.push(Item::RoundedRect { rect: r, radii: Radii::uniform(radius), paint: Paint::Solid(c) });
    }

    pub fn shadow(&mut self, r: Rect, radius: f32, blur: f32, c: Color) {
        self.dl.push(Item::Shadow {
            rect: r.translate(0.0, blur / 4.0),
            radii: Radii::uniform(radius),
            blur,
            color: c,
            box_rect: r,
            box_radii: Radii::uniform(radius),
        });
    }

    pub fn path(&mut self, p: Path, c: Color) {
        self.dl.push(Item::Path { path: p, paint: Paint::Solid(c) });
    }

    pub fn clip(&mut self, r: Rect) {
        self.dl.push(Item::PushClip { rect: r, radii: None });
    }

    pub fn unclip(&mut self) {
        self.dl.push(Item::PopClip);
    }

    /// Draw single-line text with its baseline at `y` (logical px).
    pub fn text(&mut self, s: &str, x: f32, baseline: f32, size: f32, weight: u16, c: Color) -> f32 {
        let (glyphs, w, bold) = text::shape_ui(s, size, weight);
        let gl = glyphs.into_iter().map(|(f, id, gx, gy)| (f, id, x + gx, baseline + gy)).collect();
        self.dl.push(Item::Text(GlyphRun { glyphs: gl, size, color: c, bold, italic: false }));
        w
    }

    /// Text that fades out (instead of an ellipsis) when wider than `max_w`.
    pub fn text_faded(&mut self, s: &str, x: f32, baseline: f32, size: f32, max_w: f32, c: Color, bg: Color) {
        let w = text::measure(s, size, 400);
        self.clip(Rect::new(x, baseline - size * 1.5, max_w, size * 2.5));
        self.text(s, x, baseline, size, 400, c);
        self.unclip();
        if w > max_w {
            let fade = 24.0f32.min(max_w);
            let r = Rect::new(x + max_w - fade, baseline - size * 1.5, fade, size * 2.5);
            let paint = Paint::Linear {
                x0: r.x,
                y0: 0.0,
                x1: r.right(),
                y1: 0.0,
                stops: vec![
                    bb_paint::Stop { offset: 0.0, color: bg.with_alpha(0.0) },
                    bb_paint::Stop { offset: 1.0, color: bg },
                ],
                repeating: false,
            };
            self.dl.push(Item::Gradient { rect: r, radii: Radii::default(), paint });
        }
    }

    /// A toolbar-style icon button. Returns nothing; hit testing uses `r`.
    pub fn icon_button(
        &mut self,
        r: Rect,
        icon: icons::Icon,
        theme: &Theme,
        hovered: bool,
        pressed: bool,
        enabled: bool,
    ) {
        if enabled && (hovered || pressed) {
            self.rounded(r, 4.0, if pressed { theme.button_active } else { theme.button_hover });
        }
        let c = if enabled { theme.text } else { theme.text.with_alpha(0.35) };
        let s = 16.0;
        let ix = r.x + (r.w - s) / 2.0;
        let iy = r.y + (r.h - s) / 2.0;
        self.path(icons::path(icon, ix, iy, s), c);
    }
}

/// Simple hit-testing helper.
pub fn hit(r: &Rect, x: f32, y: f32) -> bool {
    r.contains(x, y)
}

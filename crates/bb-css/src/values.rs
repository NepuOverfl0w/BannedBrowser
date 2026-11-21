//! CSS Values & Units / CSS Color parsing helpers.

use crate::parser::Cv;
use crate::tokenizer::Token;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    Px,
    Em,
    Rem,
    Ex,
    Ch,
    Vw,
    Vh,
    Vmin,
    Vmax,
    Pt,
    Pc,
    In,
    Cm,
    Mm,
    Q,
}

/// A dimension with a length unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Length {
    pub value: f32,
    pub unit: Unit,
}

impl Length {
    pub const fn px(v: f32) -> Self {
        Length { value: v, unit: Unit::Px }
    }

    /// Absolute units only (and em/rem against `font_px`); `None` for viewport units.
    pub fn to_px_static(&self, font_px: f32) -> Option<f32> {
        Some(match self.unit {
            Unit::Px => self.value,
            Unit::Em | Unit::Rem => self.value * font_px,
            Unit::Ex => self.value * font_px * 0.5,
            Unit::Ch => self.value * font_px * 0.5,
            Unit::Pt => self.value * 96.0 / 72.0,
            Unit::Pc => self.value * 16.0,
            Unit::In => self.value * 96.0,
            Unit::Cm => self.value * 96.0 / 2.54,
            Unit::Mm => self.value * 96.0 / 25.4,
            Unit::Q => self.value * 96.0 / 101.6,
            Unit::Vw | Unit::Vh | Unit::Vmin | Unit::Vmax => return None,
        })
    }
}

pub fn unit_from_str(u: &str) -> Option<Unit> {
    Some(match u.to_ascii_lowercase().as_str() {
        "px" => Unit::Px,
        "em" => Unit::Em,
        "rem" => Unit::Rem,
        "ex" => Unit::Ex,
        "ch" => Unit::Ch,
        "vw" | "svw" | "lvw" | "dvw" => Unit::Vw,
        "vh" | "svh" | "lvh" | "dvh" => Unit::Vh,
        "vmin" => Unit::Vmin,
        "vmax" => Unit::Vmax,
        "pt" => Unit::Pt,
        "pc" => Unit::Pc,
        "in" => Unit::In,
        "cm" => Unit::Cm,
        "mm" => Unit::Mm,
        "q" => Unit::Q,
        _ => return None,
    })
}

/// Parse a `<length>` from a single component value.
pub fn parse_length(cv: &Cv) -> Option<Length> {
    match cv {
        Cv::Token(Token::Dimension { value, unit }) => {
            Some(Length { value: *value, unit: unit_from_str(unit)? })
        }
        _ => None,
    }
}

/// An sRGB color with straight alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba { r: 0, g: 0, b: 0, a: 0 };
    pub const BLACK: Rgba = Rgba { r: 0, g: 0, b: 0, a: 255 };
    pub const WHITE: Rgba = Rgba { r: 255, g: 255, b: 255, a: 255 };

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Rgba { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Rgba { r, g, b, a }
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Rgba { a: (self.a as f32 * a.clamp(0.0, 1.0)).round() as u8, ..self }
    }

    pub fn from_hex(h: &str) -> Option<Rgba> {
        let v: Vec<u8> =
            h.bytes().map(|b| (b as char).to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
        Some(match v.len() {
            3 => Rgba::rgb(v[0] * 17, v[1] * 17, v[2] * 17),
            4 => Rgba::rgba(v[0] * 17, v[1] * 17, v[2] * 17, v[3] * 17),
            6 => Rgba::rgb(v[0] * 16 + v[1], v[2] * 16 + v[3], v[4] * 16 + v[5]),
            8 => Rgba::rgba(v[0] * 16 + v[1], v[2] * 16 + v[3], v[4] * 16 + v[5], v[6] * 16 + v[7]),
            _ => return None,
        })
    }

    /// Relative luminance-ish brightness (0..1), for dark/light decisions.
    pub fn brightness(&self) -> f32 {
        (0.2126 * self.r as f32 + 0.7152 * self.g as f32 + 0.0722 * self.b as f32) / 255.0
    }
}

/// A specified color: resolved against `currentColor` / color scheme at
/// computed-value time.
#[derive(Debug, Clone, PartialEq)]
pub enum ColorValue {
    Rgba(Rgba),
    CurrentColor,
    /// `light-dark(light, dark)`.
    LightDark(Box<ColorValue>, Box<ColorValue>),
    /// CSS system colors (`Canvas`, `CanvasText`, `LinkText`, …), lowercased.
    System(String),
}

impl ColorValue {
    /// Resolve to RGBA given the current color and whether the used color
    /// scheme is dark.
    pub fn resolve(&self, current: Rgba, dark: bool) -> Rgba {
        match self {
            ColorValue::Rgba(c) => *c,
            ColorValue::CurrentColor => current,
            ColorValue::LightDark(l, d) => {
                if dark {
                    d.resolve(current, dark)
                } else {
                    l.resolve(current, dark)
                }
            }
            ColorValue::System(s) => system_color(s, dark),
        }
    }
}

/// CSS Color 4 §6.2 system colors (light / dark palettes).
pub fn system_color(name: &str, dark: bool) -> Rgba {
    match (name, dark) {
        ("canvas", false) | ("field", false) | ("buttonhighlight", false) => Rgba::WHITE,
        ("canvas", true) | ("field", true) => Rgba::rgb(0x1c, 0x1b, 0x22),
        ("canvastext", false) | ("fieldtext", false) | ("buttontext", false) => Rgba::BLACK,
        ("canvastext", true) | ("fieldtext", true) | ("buttontext", true) => Rgba::rgb(0xfb, 0xfb, 0xfe),
        ("linktext", false) => Rgba::rgb(0, 0, 0xee),
        ("linktext", true) => Rgba::rgb(0x8c, 0x8c, 0xff),
        ("visitedtext", false) => Rgba::rgb(0x55, 0x1a, 0x8b),
        ("visitedtext", true) => Rgba::rgb(0xff, 0xad, 0xff),
        ("activetext", _) => Rgba::rgb(0xee, 0, 0),
        ("buttonface", false) | ("buttonborder", true) => Rgba::rgb(0xe9, 0xe9, 0xed),
        ("buttonface", true) | ("buttonhighlight", true) => Rgba::rgb(0x2b, 0x2a, 0x33),
        ("buttonborder", false) => Rgba::rgb(0x8f, 0x8f, 0x9d),
        ("graytext", _) => Rgba::rgb(0x6d, 0x6d, 0x6d),
        ("highlight", _) | ("selecteditem", _) | ("accentcolor", _) => Rgba::rgb(0x7c, 0x5c, 0xff),
        ("highlighttext", _) | ("selecteditemtext", _) | ("accentcolortext", _) => Rgba::WHITE,
        ("mark", _) => Rgba::rgb(0xff, 0xff, 0),
        ("marktext", _) => Rgba::BLACK,
        (_, false) => Rgba::BLACK,
        (_, true) => Rgba::rgb(0xfb, 0xfb, 0xfe),
    }
}

const SYSTEM_COLORS: &[&str] = &[
    "canvas",
    "canvastext",
    "linktext",
    "visitedtext",
    "activetext",
    "buttonface",
    "buttontext",
    "buttonborder",
    "field",
    "fieldtext",
    "highlight",
    "highlighttext",
    "selecteditem",
    "selecteditemtext",
    "mark",
    "marktext",
    "graytext",
    "accentcolor",
    "accentcolortext",
    "buttonhighlight",
    "window",
    "windowtext",
];

pub fn named_color(name: &str) -> Option<Rgba> {
    let n = name.to_ascii_lowercase();
    let i = NAMED_COLORS.binary_search_by(|(k, _)| k.cmp(&n.as_str())).ok()?;
    let v = NAMED_COLORS[i].1;
    Some(Rgba::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

fn num_or_pct(cv: &Cv, pct_scale: f32) -> Option<f32> {
    match cv {
        Cv::Token(Token::Number { value, .. }) => Some(*value),
        Cv::Token(Token::Percentage(p)) => Some(p / 100.0 * pct_scale),
        Cv::Token(Token::Ident(i)) if i.eq_ignore_ascii_case("none") => Some(0.0),
        Cv::Function(f, args) if f == "calc" => crate::calc::parse_calc(args).and_then(|c| c.eval_number()),
        _ => None,
    }
}

fn hue(cv: &Cv) -> Option<f32> {
    match cv {
        Cv::Token(Token::Number { value, .. }) => Some(*value),
        Cv::Token(Token::Dimension { value, unit }) => Some(match unit.to_ascii_lowercase().as_str() {
            "deg" => *value,
            "rad" => value.to_degrees(),
            "grad" => value * 0.9,
            "turn" => value * 360.0,
            _ => return None,
        }),
        Cv::Token(Token::Ident(i)) if i.eq_ignore_ascii_case("none") => Some(0.0),
        _ => None,
    }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    // CSS Color 4 §7.1
    let h = h.rem_euclid(360.0);
    let f = |n: f32| {
        let k = (n + h / 30.0) % 12.0;
        let a = s * l.min(1.0 - l);
        l - a * (-1.0f32).max((k - 3.0).min(9.0 - k).min(1.0))
    };
    (f(0.0), f(8.0), f(4.0))
}

/// Split function arguments into values, accepting both the legacy
/// comma syntax and the modern space / slash syntax.
fn color_args(args: &[Cv]) -> Option<(Vec<&Cv>, Option<&Cv>)> {
    let items: Vec<&Cv> = args.iter().filter(|c| !c.is_ws()).collect();
    let has_comma = items.iter().any(|c| c.is_comma());
    if has_comma {
        let vals: Vec<&Cv> = items.iter().copied().filter(|c| !c.is_comma()).collect();
        return match vals.len() {
            3 => Some((vals, None)),
            4 => Some((vals[..3].to_vec(), Some(vals[3]))),
            _ => None,
        };
    }
    let slash = items.iter().position(|c| c.is_delim('/'));
    match slash {
        Some(i) => Some((items[..i].to_vec(), items.get(i + 1).copied())),
        None => Some((items, None)),
    }
}

fn alpha_val(a: Option<&Cv>) -> Option<f32> {
    match a {
        None => Some(1.0),
        Some(cv) => num_or_pct(cv, 1.0).map(|v| v.clamp(0.0, 1.0)),
    }
}

fn to_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// Parse a `<color>` from component values (whitespace allowed around).
pub fn parse_color(cvs: &[Cv]) -> Option<ColorValue> {
    let items: Vec<&Cv> = cvs.iter().filter(|c| !c.is_ws()).collect();
    if items.len() != 1 {
        return None;
    }
    parse_color_cv(items[0])
}

pub fn parse_color_cv(cv: &Cv) -> Option<ColorValue> {
    match cv {
        Cv::Token(Token::Hash(h, _)) => Rgba::from_hex(h).map(ColorValue::Rgba),
        Cv::Token(Token::Ident(name)) => {
            let n = name.to_ascii_lowercase();
            match n.as_str() {
                "currentcolor" => Some(ColorValue::CurrentColor),
                "transparent" => Some(ColorValue::Rgba(Rgba::TRANSPARENT)),
                _ if SYSTEM_COLORS.contains(&n.as_str()) => Some(ColorValue::System(match n.as_str() {
                    "window" => "canvas".into(),
                    "windowtext" => "canvastext".into(),
                    _ => n,
                })),
                _ => named_color(&n).map(ColorValue::Rgba),
            }
        }
        Cv::Function(f, args) => match f.as_str() {
            "rgb" | "rgba" => {
                let (vals, a) = color_args(args)?;
                if vals.len() != 3 {
                    return None;
                }
                let c: Vec<f32> = vals.iter().map(|v| num_or_pct(v, 255.0)).collect::<Option<_>>()?;
                let a = alpha_val(a)?;
                Some(ColorValue::Rgba(Rgba::rgba(to_u8(c[0]), to_u8(c[1]), to_u8(c[2]), to_u8(a * 255.0))))
            }
            "hsl" | "hsla" => {
                let (vals, a) = color_args(args)?;
                if vals.len() != 3 {
                    return None;
                }
                let h = hue(vals[0])?;
                let s = num_or_pct(vals[1], 100.0)? / 100.0;
                let l = num_or_pct(vals[2], 100.0)? / 100.0;
                let (r, g, b) = hsl_to_rgb(h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
                let a = alpha_val(a)?;
                Some(ColorValue::Rgba(Rgba::rgba(
                    to_u8(r * 255.0),
                    to_u8(g * 255.0),
                    to_u8(b * 255.0),
                    to_u8(a * 255.0),
                )))
            }
            "hwb" => {
                let (vals, a) = color_args(args)?;
                if vals.len() != 3 {
                    return None;
                }
                let h = hue(vals[0])?;
                let mut w = num_or_pct(vals[1], 100.0)? / 100.0;
                let mut bl = num_or_pct(vals[2], 100.0)? / 100.0;
                if w + bl >= 1.0 {
                    let g = w / (w + bl);
                    w = g;
                    bl = 1.0 - g;
                }
                let (r, g, b) = hsl_to_rgb(h, 1.0, 0.5);
                let f = |c: f32| c * (1.0 - w - bl) + w;
                let a = alpha_val(a)?;
                Some(ColorValue::Rgba(Rgba::rgba(
                    to_u8(f(r) * 255.0),
                    to_u8(f(g) * 255.0),
                    to_u8(f(b) * 255.0),
                    to_u8(a * 255.0),
                )))
            }
            "light-dark" => {
                let parts: Vec<&[Cv]> = args.split(|c| c.is_comma()).collect();
                if parts.len() != 2 {
                    return None;
                }
                Some(ColorValue::LightDark(
                    Box::new(parse_color(parts[0])?),
                    Box::new(parse_color(parts[1])?),
                ))
            }
            // TODO(css-color-4 §8-10): lab(), lch(), oklab(), oklch(), color(), color-mix()
            _ => None,
        },
        _ => None,
    }
}

/// HTML §2.3.6 "rules for parsing a legacy colour value" (bgcolor, color=…).
pub fn parse_legacy_color(input: &str) -> Option<Rgba> {
    let s = input.trim_matches(|c: char| c.is_ascii_whitespace());
    if s.is_empty() || s.eq_ignore_ascii_case("transparent") {
        return None;
    }
    if let Some(c) = named_color(s) {
        return Some(c);
    }
    let b: Vec<char> = s.chars().collect();
    if b.len() == 4 && b[0] == '#' && b[1..].iter().all(|c| c.is_ascii_hexdigit()) {
        let d = |c: char| c.to_digit(16).unwrap_or(0) as u8 * 17;
        return Some(Rgba::rgb(d(b[1]), d(b[2]), d(b[3])));
    }
    // Replace non-BMP chars with "00", truncate to 128, strip '#', replace non-hex with '0'.
    let mut t: String =
        s.chars().map(|c| if c as u32 > 0xFFFF { "00".to_string() } else { c.to_string() }).collect();
    t = t.chars().take(128).collect();
    if t.starts_with('#') {
        t.remove(0);
    }
    let mut t: Vec<char> = t.chars().map(|c| if c.is_ascii_hexdigit() { c } else { '0' }).collect();
    while t.is_empty() || t.len() % 3 != 0 {
        t.push('0');
    }
    let len = t.len() / 3;
    let comps: Vec<Vec<char>> = (0..3).map(|i| t[i * len..(i + 1) * len].to_vec()).collect();
    let mut comps = comps;
    if len > 8 {
        for c in comps.iter_mut() {
            *c = c[len - 8..].to_vec();
        }
    }
    let mut l = comps[0].len();
    while l > 2 && comps.iter().all(|c| c[comps[0].len() - l] == '0') {
        l -= 1;
    }
    let start = comps[0].len() - l;
    let val = |c: &Vec<char>| {
        let s: String = c[start..start + l.min(2)].iter().collect();
        u8::from_str_radix(if s.is_empty() { "0" } else { &s }, 16).unwrap_or(0)
    };
    Some(Rgba::rgb(val(&comps[0]), val(&comps[1]), val(&comps[2])))
}

/// CSS named colors, sorted.
pub static NAMED_COLORS: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_component_values;

    fn col(s: &str) -> Option<Rgba> {
        parse_color(&parse_component_values(s)).map(|c| c.resolve(Rgba::BLACK, false))
    }

    #[test]
    fn colors() {
        assert_eq!(col("#f00"), Some(Rgba::rgb(255, 0, 0)));
        assert_eq!(col("#00ff0080"), Some(Rgba::rgba(0, 255, 0, 128)));
        assert_eq!(col("RebeccaPurple"), Some(Rgba::rgb(0x66, 0x33, 0x99)));
        assert_eq!(col("rgb(1, 2, 3)"), Some(Rgba::rgb(1, 2, 3)));
        assert_eq!(col("rgb(100% 0% 0% / 50%)"), Some(Rgba::rgba(255, 0, 0, 128)));
        assert_eq!(col("rgba(0,0,0,.5)"), Some(Rgba::rgba(0, 0, 0, 128)));
        assert_eq!(col("hsl(120 100% 50%)"), Some(Rgba::rgb(0, 255, 0)));
        assert_eq!(col("hsl(0, 100%, 25%)"), Some(Rgba::rgb(128, 0, 0)));
        assert!(col("rgb(1,2)").is_none());
        assert!(col("notacolor").is_none());
        let ld = parse_color(&parse_component_values("light-dark(#eee, #222)")).unwrap();
        assert_eq!(ld.resolve(Rgba::BLACK, false), Rgba::rgb(0xee, 0xee, 0xee));
        assert_eq!(ld.resolve(Rgba::BLACK, true), Rgba::rgb(0x22, 0x22, 0x22));
    }

    #[test]
    fn legacy_colors() {
        assert_eq!(parse_legacy_color("lime"), Some(Rgba::rgb(0, 255, 0)));
        assert_eq!(parse_legacy_color("#FFFF00"), Some(Rgba::rgb(255, 255, 0)));
        assert_eq!(parse_legacy_color("FFFF00"), Some(Rgba::rgb(255, 255, 0)));
        assert_eq!(parse_legacy_color("#abc"), Some(Rgba::rgb(0xaa, 0xbb, 0xcc)));
        // The famous "chucknorris" is red.
        assert_eq!(parse_legacy_color("chucknorris"), Some(Rgba::rgb(0xc0, 0, 0)));
        assert_eq!(parse_legacy_color("grey"), Some(Rgba::rgb(128, 128, 128)));
        assert_eq!(parse_legacy_color(""), None);
    }

    #[test]
    fn named_sorted() {
        assert!(NAMED_COLORS.windows(2).all(|w| w[0].0 < w[1].0));
    }
}

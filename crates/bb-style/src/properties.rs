//! Parsing declared values into computed values, property by property.
//!
//! Declarations are applied in cascade order onto a [`ComputedStyle`]
//! that starts as "inherit from parent"; later declarations overwrite
//! earlier ones, which also gives correct shorthand/longhand interplay.

use crate::computed::*;
use bb_css::calc::{parse_math_function, Calc};
use bb_css::values::{parse_color, parse_color_cv, unit_from_str, ColorValue};
use bb_css::{Cv, Length, Rgba, Token, Unit};
use std::rc::Rc;

/// Context for computing values of one element.
pub struct Ctx<'a> {
    pub parent: &'a ComputedStyle,
    pub root_font_size: f32,
    pub viewport: (f32, f32),
    /// The OS/user prefers a dark color scheme.
    pub prefers_dark: bool,
    pub quirks: bool,
}

fn items(v: &[Cv]) -> Vec<&Cv> {
    v.iter().filter(|c| !c.is_ws()).collect()
}

fn single(v: &[Cv]) -> Option<&Cv> {
    let it = items(v);
    if it.len() == 1 {
        Some(it[0])
    } else {
        None
    }
}

fn ident(v: &[Cv]) -> Option<String> {
    single(v).and_then(|c| c.ident()).map(|s| s.to_ascii_lowercase())
}

/// Resolve a length to px for an element whose font size is `font_px`.
pub fn length_px(l: &Length, font_px: f32, ctx: &Ctx) -> f32 {
    match l.unit {
        Unit::Rem => l.value * ctx.root_font_size,
        Unit::Vw => l.value * ctx.viewport.0 / 100.0,
        Unit::Vh => l.value * ctx.viewport.1 / 100.0,
        Unit::Vmin => l.value * ctx.viewport.0.min(ctx.viewport.1) / 100.0,
        Unit::Vmax => l.value * ctx.viewport.0.max(ctx.viewport.1) / 100.0,
        _ => l.to_px_static(font_px).unwrap_or(0.0),
    }
}

/// Replace length leaves in a calc tree by px numbers.
fn resolve_calc(c: &Calc, font_px: f32, ctx: &Ctx) -> Calc {
    let r = |x: &Calc| Box::new(resolve_calc(x, font_px, ctx));
    match c {
        Calc::Length(l) => Calc::Number(length_px(l, font_px, ctx)),
        Calc::Number(_) | Calc::Percent(_) => c.clone(),
        Calc::Add(a, b) => Calc::Add(r(a), r(b)),
        Calc::Sub(a, b) => Calc::Sub(r(a), r(b)),
        Calc::Mul(a, b) => Calc::Mul(r(a), r(b)),
        Calc::Div(a, b) => Calc::Div(r(a), r(b)),
        Calc::Min(v) => Calc::Min(v.iter().map(|x| resolve_calc(x, font_px, ctx)).collect()),
        Calc::Max(v) => Calc::Max(v.iter().map(|x| resolve_calc(x, font_px, ctx)).collect()),
        Calc::Clamp(a, b, d) => Calc::Clamp(r(a), r(b), r(d)),
    }
}

/// Parse a `<length-percentage>`. `quirky` allows unitless numbers (HTML quirks mode).
pub fn parse_lp(cv: &Cv, font_px: f32, ctx: &Ctx, quirky: bool) -> Option<LP> {
    match cv {
        Cv::Token(Token::Dimension { value, unit }) => {
            let u = unit_from_str(unit)?;
            Some(LP::Px(length_px(&Length { value: *value, unit: u }, font_px, ctx)))
        }
        Cv::Token(Token::Percentage(p)) => Some(LP::Pct(*p)),
        Cv::Token(Token::Number { value, .. }) if *value == 0.0 => Some(LP::Px(0.0)),
        Cv::Token(Token::Number { value, .. }) if quirky && ctx.quirks => Some(LP::Px(*value)),
        Cv::Function(name, args) => {
            let c = parse_math_function(name, args)?;
            let c = resolve_calc(&c, font_px, ctx);
            if c.has_percent() {
                Some(LP::Calc(Rc::new(c)))
            } else {
                c.eval_number().map(LP::Px)
            }
        }
        _ => None,
    }
}

fn parse_lpa(cv: &Cv, font_px: f32, ctx: &Ctx, quirky: bool) -> Option<LPA> {
    if cv.is_ident("auto") {
        return Some(LPA::Auto);
    }
    parse_lp(cv, font_px, ctx, quirky).map(LPA::LP)
}

fn parse_len_px(cv: &Cv, font_px: f32, ctx: &Ctx, quirky: bool) -> Option<f32> {
    match parse_lp(cv, font_px, ctx, quirky)? {
        LP::Px(v) => Some(v),
        _ => None,
    }
}

fn number(cv: &Cv) -> Option<f32> {
    match cv {
        Cv::Token(Token::Number { value, .. }) => Some(*value),
        Cv::Function(n, a) => parse_math_function(n, a).and_then(|c| c.eval_number()),
        _ => None,
    }
}

fn color_of(cv: &Cv, style: &ComputedStyle) -> Option<Rgba> {
    parse_color_cv(cv).map(|c| c.resolve(style.color, style.color_scheme_dark))
}

fn border_width_px(cv: &Cv, font_px: f32, ctx: &Ctx) -> Option<f32> {
    if let Some(i) = cv.ident() {
        return match i.to_ascii_lowercase().as_str() {
            "thin" => Some(1.0),
            "medium" => Some(3.0),
            "thick" => Some(5.0),
            _ => None,
        };
    }
    parse_len_px(cv, font_px, ctx, true).map(|v| v.max(0.0))
}

fn border_style(cv: &Cv) -> Option<BorderStyle> {
    Some(match cv.ident()?.to_ascii_lowercase().as_str() {
        "none" => BorderStyle::None,
        "hidden" => BorderStyle::Hidden,
        "solid" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        "groove" => BorderStyle::Groove,
        "ridge" => BorderStyle::Ridge,
        "inset" => BorderStyle::Inset,
        "outset" => BorderStyle::Outset,
        _ => return None,
    })
}

/// Expand 1–4 values to [top, right, bottom, left].
fn four<T: Clone>(v: Vec<T>) -> Option<[T; 4]> {
    Some(match v.len() {
        1 => [v[0].clone(), v[0].clone(), v[0].clone(), v[0].clone()],
        2 => [v[0].clone(), v[1].clone(), v[0].clone(), v[1].clone()],
        3 => [v[0].clone(), v[1].clone(), v[2].clone(), v[1].clone()],
        4 => [v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone()],
        _ => return None,
    })
}

const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

/// CSS Fonts 4 §2.5 absolute-size keywords (Firefox's 16px scale).
pub fn font_size_keyword(k: &str) -> Option<f32> {
    Some(match k {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "xxx-large" => 48.0,
        _ => return None,
    })
}

fn font_weight(cv: &Cv, parent: u16) -> Option<u16> {
    if let Some(i) = cv.ident() {
        return Some(match i.to_ascii_lowercase().as_str() {
            "normal" => 400,
            "bold" => 700,
            "bolder" => match parent {
                0..=349 => 400,
                350..=549 => 700,
                _ => 900,
            },
            "lighter" => match parent {
                0..=549 => 100,
                550..=749 => 400,
                _ => 700,
            },
            _ => return None,
        });
    }
    let n = number(cv)?;
    if (1.0..=1000.0).contains(&n) {
        Some(n as u16)
    } else {
        None
    }
}

fn parse_font_family(v: &[Cv]) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for part in v.split(|c| c.is_comma()) {
        let it = items(part);
        if it.is_empty() {
            return None;
        }
        if let [Cv::Token(Token::String(s))] = it.as_slice() {
            out.push(s.clone());
            continue;
        }
        let mut name = Vec::new();
        for c in it {
            name.push(c.ident()?.to_string());
        }
        let n = name.join(" ");
        let lower = n.to_ascii_lowercase();
        let generic = matches!(
            lower.as_str(),
            "serif"
                | "sans-serif"
                | "monospace"
                | "cursive"
                | "fantasy"
                | "system-ui"
                | "ui-serif"
                | "ui-sans-serif"
                | "ui-monospace"
                | "ui-rounded"
                | "emoji"
                | "math"
                | "-apple-system"
                | "blinkmacsystemfont"
        );
        out.push(if generic { lower } else { n });
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn parse_font_size(cv: &Cv, style: &ComputedStyle, ctx: &Ctx) -> Option<f32> {
    let pf = ctx.parent.font_size;
    if let Some(i) = cv.ident() {
        let k = i.to_ascii_lowercase();
        if let Some(px) = font_size_keyword(&k) {
            // Monospace "medium" is 13px (Firefox default).
            let mono = style.font_family.len() == 1 && style.font_family[0] == "monospace";
            return Some(if mono { px * 13.0 / 16.0 } else { px });
        }
        return match k.as_str() {
            "smaller" => Some(pf / 1.2),
            "larger" => Some(pf * 1.2),
            _ => None,
        };
    }
    match parse_lp(cv, pf, ctx, true)? {
        LP::Px(v) => Some(v.max(0.0)),
        LP::Pct(p) => Some(pf * p / 100.0),
        LP::Calc(c) => Some(c.eval_px(&PxOnly, pf).max(0.0)),
    }
}

fn parse_line_height(cv: &Cv, font_px: f32, ctx: &Ctx) -> Option<LineHeight> {
    if cv.is_ident("normal") {
        return Some(LineHeight::Normal);
    }
    if let Some(n) = number(cv) {
        return Some(LineHeight::Number(n.max(0.0)));
    }
    match parse_lp(cv, font_px, ctx, false)? {
        LP::Px(v) => Some(LineHeight::Px(v.max(0.0))),
        LP::Pct(p) => Some(LineHeight::Px(font_px * p / 100.0)),
        LP::Calc(c) => Some(LineHeight::Px(c.eval_px(&PxOnly, font_px))),
    }
}

fn parse_display(v: &[Cv]) -> Option<Display> {
    let ids: Vec<String> =
        items(v).iter().map(|c| c.ident().map(|s| s.to_ascii_lowercase())).collect::<Option<_>>()?;
    let ids: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
    Some(match ids.as_slice() {
        ["none"] => Display::None,
        ["contents"] => Display::Contents,
        ["inline"] | ["inline", "flow"] => Display::Inline,
        ["block"] | ["block", "flow"] => Display::Block,
        ["inline-block"] | ["inline", "flow-root"] => Display::InlineBlock,
        ["flow-root"] | ["block", "flow-root"] => Display::FlowRoot,
        ["list-item"] | ["block", "list-item"] | ["list-item", "block"] => Display::ListItem,
        ["table"] | ["block", "table"] => Display::Table,
        ["inline-table"] | ["inline", "table"] => Display::InlineTable,
        ["table-row-group"] => Display::TableRowGroup,
        ["table-header-group"] => Display::TableHeaderGroup,
        ["table-footer-group"] => Display::TableFooterGroup,
        ["table-row"] => Display::TableRow,
        ["table-cell"] => Display::TableCell,
        ["table-column"] => Display::TableColumn,
        ["table-column-group"] => Display::TableColumnGroup,
        ["table-caption"] => Display::TableCaption,
        ["flex"] | ["block", "flex"] | ["-webkit-box"] | ["-webkit-flex"] | ["-moz-box"] => Display::Flex,
        ["inline-flex"] | ["inline", "flex"] | ["-webkit-inline-box"] | ["-webkit-inline-flex"] => {
            Display::InlineFlex
        }
        ["grid"] | ["block", "grid"] => Display::Grid,
        ["inline-grid"] | ["inline", "grid"] => Display::InlineGrid,
        _ => return None,
    })
}

fn list_style_type(s: &str) -> Option<ListStyleType> {
    Some(match s {
        "none" => ListStyleType::None,
        "disc" => ListStyleType::Disc,
        "circle" => ListStyleType::Circle,
        "square" => ListStyleType::Square,
        "decimal" => ListStyleType::Decimal,
        "decimal-leading-zero" => ListStyleType::DecimalLeadingZero,
        "lower-alpha" | "lower-latin" => ListStyleType::LowerAlpha,
        "upper-alpha" | "upper-latin" => ListStyleType::UpperAlpha,
        "lower-roman" => ListStyleType::LowerRoman,
        "upper-roman" => ListStyleType::UpperRoman,
        "lower-greek" => ListStyleType::LowerGreek,
        "disclosure-open" => ListStyleType::DisclosureOpen,
        "disclosure-closed" => ListStyleType::DisclosureClosed,
        _ => return None,
    })
}

fn url_of(cv: &Cv) -> Option<String> {
    match cv {
        Cv::Token(Token::Url(u)) => Some(u.clone()),
        Cv::Function(f, args) if f == "url" || f == "src" => args.iter().find_map(|a| match a {
            Cv::Token(Token::String(s)) => Some(s.clone()),
            _ => None,
        }),
        _ => None,
    }
}

fn parse_gradient(name: &str, args: &[Cv], style: &ComputedStyle, ctx: &Ctx) -> Option<Image> {
    let (base, repeating) = match name.strip_prefix("repeating-") {
        Some(b) => (b, true),
        None => (name, false),
    };
    let base = base.trim_start_matches("-webkit-").trim_start_matches("-moz-");
    let parts: Vec<&[Cv]> = args.split(|c| c.is_comma()).collect();
    let mut angle = 180.0;
    let mut first_stop = 0;
    if base == "linear-gradient" {
        let p0 = items(parts.first()?);
        if let Some(Cv::Token(Token::Dimension { value, unit })) = p0.first() {
            angle = match unit.to_ascii_lowercase().as_str() {
                "deg" => *value,
                "rad" => value.to_degrees(),
                "turn" => value * 360.0,
                "grad" => value * 0.9,
                _ => return None,
            };
            first_stop = 1;
        } else if p0.first().is_some_and(|c| c.is_ident("to")) {
            let dirs: Vec<String> =
                p0[1..].iter().filter_map(|c| c.ident().map(|s| s.to_ascii_lowercase())).collect();
            let has = |d: &str| dirs.iter().any(|x| x == d);
            angle = match (has("top"), has("bottom"), has("left"), has("right")) {
                (true, _, false, false) => 0.0,
                (_, true, false, false) => 180.0,
                (false, false, true, _) => 270.0,
                (false, false, _, true) => 90.0,
                (true, _, true, _) => 315.0,
                (true, _, _, true) => 45.0,
                (_, true, true, _) => 225.0,
                (_, true, _, true) => 135.0,
                _ => 180.0,
            };
            first_stop = 1;
        }
    } else if base == "radial-gradient" {
        // Skip a shape/position prelude if the first part isn't a color stop.
        if parts.first().is_some_and(|p| items(p).first().and_then(|c| parse_color_cv(c)).is_none()) {
            first_stop = 1;
        }
    } else {
        return None;
    }
    let mut stops = Vec::new();
    for p in &parts[first_stop..] {
        let it = items(p);
        let Some(color) = it.first().and_then(|c| color_of(c, style)) else {
            // A lone position (color hint) — skip.
            continue;
        };
        let pos = it.get(1).and_then(|c| parse_lp(c, style.font_size, ctx, false));
        stops.push(GradientStop { color, pos: pos.clone() });
        if let Some(p2) = it.get(2).and_then(|c| parse_lp(c, style.font_size, ctx, false)) {
            stops.push(GradientStop { color, pos: Some(p2) });
        }
    }
    if stops.is_empty() {
        return None;
    }
    if base == "linear-gradient" {
        Some(Image::LinearGradient { angle, stops, repeating })
    } else {
        Some(Image::RadialGradient { stops, repeating })
    }
}

fn parse_image(cv: &Cv, style: &ComputedStyle, ctx: &Ctx) -> Option<Option<Image>> {
    if cv.is_ident("none") {
        return Some(None);
    }
    if let Some(u) = url_of(cv) {
        return Some(Some(Image::Url(u)));
    }
    if let Cv::Function(f, args) = cv {
        return parse_gradient(f, args, style, ctx).map(Some);
    }
    None
}

fn repeat_kw(s: &str) -> Option<BgRepeat> {
    Some(match s {
        "repeat" => BgRepeat::Repeat,
        "no-repeat" => BgRepeat::NoRepeat,
        "space" => BgRepeat::Space,
        "round" => BgRepeat::Round,
        _ => return None,
    })
}

fn position_kw(s: &str) -> Option<(char, f32)> {
    Some(match s {
        "left" => ('x', 0.0),
        "right" => ('x', 100.0),
        "top" => ('y', 0.0),
        "bottom" => ('y', 100.0),
        "center" => ('c', 50.0),
        _ => return None,
    })
}

/// Parse a background-position value list into (x, y).
fn parse_position(vals: &[&Cv], font_px: f32, ctx: &Ctx) -> Option<(LP, LP)> {
    let mut x: Option<LP> = None;
    let mut y: Option<LP> = None;
    let mut pending_center = 0;
    let mut lengths: Vec<LP> = Vec::new();
    for v in vals {
        if let Some(i) = v.ident() {
            let (axis, pct) = position_kw(&i.to_ascii_lowercase())?;
            match axis {
                'x' => x = Some(LP::Pct(pct)),
                'y' => y = Some(LP::Pct(pct)),
                _ => pending_center += 1,
            }
        } else {
            let lp = parse_lp(v, font_px, ctx, false)?;
            lengths.push(lp);
        }
    }
    // Simple two-value forms: <len> <len>, <kw> <len>, <len> <kw>.
    match (lengths.len(), x.is_some(), y.is_some()) {
        (2, false, false) => {
            x = Some(lengths[0].clone());
            y = Some(lengths[1].clone());
        }
        (1, false, false) => {
            x = Some(lengths[0].clone());
            if pending_center == 0 {
                y = Some(LP::Pct(50.0));
            }
        }
        (1, true, false) => y = Some(lengths[0].clone()),
        (1, false, true) => x = Some(lengths[0].clone()),
        _ => {}
    }
    Some((x.unwrap_or(LP::Pct(50.0)), y.unwrap_or(LP::Pct(50.0))))
}

fn parse_bg_layer(
    vals: &[&Cv],
    style: &ComputedStyle,
    ctx: &Ctx,
    color: &mut Option<Rgba>,
) -> Option<BgLayer> {
    let mut layer = BgLayer::default();
    let mut pos_vals: Vec<&Cv> = Vec::new();
    let mut i = 0;
    let mut size_mode = false;
    let mut size_vals: Vec<&Cv> = Vec::new();
    while i < vals.len() {
        let v = vals[i];
        if v.is_delim('/') {
            size_mode = true;
            i += 1;
            continue;
        }
        if size_mode {
            if v.ident()
                .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "cover" | "contain" | "auto"))
                || parse_lp(v, style.font_size, ctx, false).is_some()
            {
                size_vals.push(v);
                i += 1;
                continue;
            }
            size_mode = false;
        }
        if let Some(img) = parse_image(v, style, ctx) {
            layer.image = img;
        } else if let Some(id) = v.ident().map(|s| s.to_ascii_lowercase()) {
            if let Some(r) = repeat_kw(&id) {
                layer.repeat_x = r;
                layer.repeat_y = r;
            } else if id == "repeat-x" {
                layer.repeat_x = BgRepeat::Repeat;
                layer.repeat_y = BgRepeat::NoRepeat;
            } else if id == "repeat-y" {
                layer.repeat_x = BgRepeat::NoRepeat;
                layer.repeat_y = BgRepeat::Repeat;
            } else if id == "fixed" {
                layer.fixed = true;
            } else if id == "scroll" || id == "local" {
                layer.fixed = false;
            } else if position_kw(&id).is_some() {
                pos_vals.push(v);
            } else if matches!(id.as_str(), "border-box" | "padding-box" | "content-box" | "text") {
                // background-origin / clip: TODO(css-backgrounds-3 §3.7)
            } else {
                let c = color_of(v, style)?;
                *color = Some(c);
            }
        } else if let Some(c) = color_of(v, style) {
            *color = Some(c);
        } else if parse_lp(v, style.font_size, ctx, false).is_some() {
            pos_vals.push(v);
        } else {
            return None;
        }
        i += 1;
    }
    if !pos_vals.is_empty() {
        let (x, y) = parse_position(&pos_vals, style.font_size, ctx)?;
        layer.pos_x = x;
        layer.pos_y = y;
    }
    if !size_vals.is_empty() {
        layer.size = parse_bg_size(&size_vals, style.font_size, ctx)?;
    }
    Some(layer)
}

fn parse_bg_size(vals: &[&Cv], font_px: f32, ctx: &Ctx) -> Option<BgSize> {
    if vals.len() == 1 {
        if vals[0].is_ident("cover") {
            return Some(BgSize::Cover);
        }
        if vals[0].is_ident("contain") {
            return Some(BgSize::Contain);
        }
    }
    let w = parse_lpa(vals.first()?, font_px, ctx, false)?;
    let h = match vals.get(1) {
        Some(v) => parse_lpa(v, font_px, ctx, false)?,
        None => LPA::Auto,
    };
    Some(BgSize::Auto(w, h))
}

fn parse_shadow(v: &[Cv], style: &ComputedStyle, ctx: &Ctx) -> Option<Vec<Shadow>> {
    if ident(v).as_deref() == Some("none") {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for part in v.split(|c| c.is_comma()) {
        let mut lens = Vec::new();
        let mut color = style.color;
        let mut inset = false;
        for c in items(part) {
            if c.is_ident("inset") {
                inset = true;
            } else if let Some(px) = parse_len_px(c, style.font_size, ctx, false) {
                lens.push(px);
            } else {
                let col = color_of(c, style)?;
                color = col;
            }
        }
        if lens.len() < 2 {
            return None;
        }
        out.push(Shadow {
            x: lens[0],
            y: lens[1],
            blur: lens.get(2).copied().unwrap_or(0.0).max(0.0),
            spread: lens.get(3).copied().unwrap_or(0.0),
            color,
            inset,
        });
    }
    Some(out)
}

fn parse_content(v: &[Cv]) -> Option<Option<Vec<ContentItem>>> {
    let it = items(v);
    if it.len() == 1 && (it[0].is_ident("normal") || it[0].is_ident("none")) {
        return Some(None);
    }
    let mut out = Vec::new();
    for c in it {
        match c {
            Cv::Token(Token::String(s)) => out.push(ContentItem::Text(s.clone())),
            Cv::Function(f, args) if f == "attr" => {
                let name = args.iter().find_map(|a| a.ident())?.to_ascii_lowercase();
                out.push(ContentItem::Attr(name));
            }
            Cv::Function(f, args) if f == "counter" => {
                let parts: Vec<&[Cv]> = args.split(|c| c.is_comma()).collect();
                let name = parts.first().and_then(|p| ident(p))?;
                let ty = parts
                    .get(1)
                    .and_then(|p| ident(p))
                    .and_then(|s| list_style_type(&s))
                    .unwrap_or(ListStyleType::Decimal);
                out.push(ContentItem::Counter(name, ty));
            }
            c if c.is_ident("open-quote") => out.push(ContentItem::OpenQuote),
            c if c.is_ident("close-quote") => out.push(ContentItem::CloseQuote),
            c if c.is_ident("no-open-quote") || c.is_ident("no-close-quote") => {}
            c => {
                let u = url_of(c)?;
                out.push(ContentItem::Url(u));
            }
        }
    }
    Some(Some(out))
}

fn parse_counters(v: &[Cv], default: i32) -> Option<Vec<(String, i32)>> {
    let it = items(v);
    if it.len() == 1 && it[0].is_ident("none") {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < it.len() {
        let name = it[i].ident()?.to_string();
        let mut val = default;
        if let Some(Cv::Token(Token::Number { value, .. })) = it.get(i + 1) {
            val = *value as i32;
            i += 1;
        }
        out.push((name, val));
        i += 1;
    }
    Some(out)
}

fn align_kw(s: &str) -> Option<Align> {
    Some(match s {
        "auto" => Align::Auto,
        "normal" => Align::Normal,
        "stretch" => Align::Stretch,
        "start" | "flex-start" | "self-start" | "left" => Align::Start,
        "end" | "flex-end" | "self-end" | "right" => Align::End,
        "center" => Align::Center,
        "baseline" | "first" | "last" => Align::Baseline,
        "space-between" => Align::SpaceBetween,
        "space-around" => Align::SpaceAround,
        "space-evenly" => Align::SpaceEvenly,
        _ => return None,
    })
}

fn parse_align(v: &[Cv]) -> Option<Align> {
    // Ignore safe/unsafe/first/last prefixes.
    let ids: Vec<String> =
        items(v).iter().filter_map(|c| c.ident().map(|s| s.to_ascii_lowercase())).collect();
    let last = ids.iter().rev().find(|s| !matches!(s.as_str(), "safe" | "unsafe"))?;
    align_kw(last)
}

/// Properties that inherit (for `inherit`/`unset`/`initial` handling).
pub fn is_inherited(name: &str) -> bool {
    matches!(
        name,
        "color"
            | "font-family"
            | "font-size"
            | "font-weight"
            | "font-style"
            | "font-variant"
            | "line-height"
            | "text-align"
            | "text-indent"
            | "text-transform"
            | "white-space"
            | "letter-spacing"
            | "word-spacing"
            | "visibility"
            | "list-style-type"
            | "list-style-position"
            | "list-style-image"
            | "border-collapse"
            | "border-spacing"
            | "caption-side"
            | "empty-cells"
            | "cursor"
            | "direction"
            | "color-scheme"
            | "quotes"
            | "font"
            | "list-style"
    ) || name.starts_with("--")
}

/// Copy one property's computed value from `src` into `dst`.
fn copy_property(name: &str, dst: &mut ComputedStyle, src: &ComputedStyle) {
    macro_rules! cp {
        ($($f:ident),*) => {{ $( dst.$f = src.$f.clone(); )* }};
    }
    match name {
        "color" => cp!(color),
        "font-family" => cp!(font_family),
        "font-size" => cp!(font_size),
        "font-weight" => cp!(font_weight),
        "font-style" => cp!(font_style),
        "font-variant" | "font-variant-caps" => cp!(small_caps),
        "font" => cp!(font_family, font_size, font_weight, font_style, small_caps, line_height),
        "line-height" => cp!(line_height),
        "text-align" => cp!(text_align),
        "text-indent" => cp!(text_indent),
        "text-transform" => cp!(text_transform),
        "white-space" => cp!(white_space),
        "letter-spacing" => cp!(letter_spacing),
        "word-spacing" => cp!(word_spacing),
        "visibility" => cp!(visibility),
        "list-style-type" => cp!(list_style_type),
        "list-style-position" => cp!(list_style_position),
        "list-style-image" => cp!(list_style_image),
        "list-style" => cp!(list_style_type, list_style_position, list_style_image),
        "border-collapse" => cp!(border_collapse),
        "border-spacing" => cp!(border_spacing),
        "caption-side" => cp!(caption_side),
        "empty-cells" => cp!(empty_cells_show),
        "cursor" => cp!(cursor),
        "direction" => cp!(direction_rtl),
        "display" => cp!(display),
        "position" => cp!(position),
        "float" => cp!(float),
        "clear" => cp!(clear),
        "top" => cp!(top),
        "right" => cp!(right),
        "bottom" => cp!(bottom),
        "left" => cp!(left),
        "inset" => cp!(top, right, bottom, left),
        "width" => cp!(width),
        "height" => cp!(height),
        "min-width" => cp!(min_width),
        "min-height" => cp!(min_height),
        "max-width" => cp!(max_width),
        "max-height" => cp!(max_height),
        "margin" => cp!(margin),
        "padding" => cp!(padding),
        "border" => cp!(border_width, border_style, border_color),
        "border-width" => cp!(border_width),
        "border-style" => cp!(border_style),
        "border-color" => cp!(border_color),
        "border-radius" => cp!(border_radius),
        "box-sizing" => cp!(box_sizing),
        "overflow" => cp!(overflow_x, overflow_y),
        "overflow-x" => cp!(overflow_x),
        "overflow-y" => cp!(overflow_y),
        "z-index" => cp!(z_index),
        "opacity" => cp!(opacity),
        "background" => cp!(background_color, background),
        "background-color" => cp!(background_color),
        "background-image" | "background-repeat" | "background-position" | "background-size" => {
            cp!(background)
        }
        "box-shadow" => cp!(box_shadow),
        "text-decoration" | "text-decoration-line" => cp!(text_decoration_line, text_decoration_color),
        "text-decoration-color" => cp!(text_decoration_color),
        "vertical-align" => cp!(vertical_align),
        "table-layout" => cp!(table_layout),
        "content" => cp!(content),
        "outline" => cp!(outline_width, outline_style, outline_color),
        "flex-direction" => cp!(flex_direction),
        "flex-wrap" => cp!(flex_wrap),
        "flex" => cp!(flex_grow, flex_shrink, flex_basis),
        "flex-grow" => cp!(flex_grow),
        "flex-shrink" => cp!(flex_shrink),
        "flex-basis" => cp!(flex_basis),
        "justify-content" => cp!(justify_content),
        "align-items" => cp!(align_items),
        "align-self" => cp!(align_self),
        "align-content" => cp!(align_content),
        "gap" => cp!(row_gap, column_gap),
        "row-gap" => cp!(row_gap),
        "column-gap" => cp!(column_gap),
        "order" => cp!(order),
        "object-fit" => cp!(object_fit),
        "color-scheme" => cp!(color_scheme_dark, color_scheme_allows_dark),
        _ => {
            for (i, s) in SIDES.iter().enumerate() {
                if name == format!("margin-{s}") {
                    dst.margin[i] = src.margin[i].clone();
                } else if name == format!("padding-{s}") {
                    dst.padding[i] = src.padding[i].clone();
                } else if name == format!("border-{s}") {
                    dst.border_width[i] = src.border_width[i];
                    dst.border_style[i] = src.border_style[i];
                    dst.border_color[i] = src.border_color[i];
                } else if name == format!("border-{s}-width") {
                    dst.border_width[i] = src.border_width[i];
                } else if name == format!("border-{s}-style") {
                    dst.border_style[i] = src.border_style[i];
                } else if name == format!("border-{s}-color") {
                    dst.border_color[i] = src.border_color[i];
                }
            }
        }
    }
}

/// Properties computed in the first pass (others may depend on them).
pub fn is_early(name: &str) -> bool {
    matches!(name, "font-size" | "font" | "font-family" | "color" | "color-scheme")
}

/// Apply one declaration. Returns `false` if the value was invalid (ignored).
pub fn apply(style: &mut ComputedStyle, name: &str, value: &[Cv], ctx: &Ctx) -> bool {
    // CSS-wide keywords.
    if let Some(kw) = ident(value) {
        match kw.as_str() {
            "inherit" => {
                copy_property(name, style, ctx.parent);
                return true;
            }
            "initial" => {
                copy_property(name, style, &ComputedStyle::initial());
                return true;
            }
            "unset" | "revert" | "revert-layer" => {
                if is_inherited(name) {
                    copy_property(name, style, ctx.parent);
                } else {
                    copy_property(name, style, &ComputedStyle::initial());
                }
                return true;
            }
            _ => {}
        }
    }
    apply_value(style, name, value, ctx).is_some()
}

fn apply_value(s: &mut ComputedStyle, name: &str, v: &[Cv], ctx: &Ctx) -> Option<()> {
    let fs = s.font_size;
    match name {
        "color" => {
            let c = parse_color(v)?;
            // currentColor on `color` means the parent's color.
            s.color = c.resolve(ctx.parent.color, s.color_scheme_dark);
        }
        "color-scheme" => {
            let ids: Vec<String> =
                items(v).iter().filter_map(|c| c.ident().map(|x| x.to_ascii_lowercase())).collect();
            let allows_dark = ids.iter().any(|x| x == "dark");
            let allows_light = ids.iter().any(|x| x == "light") || ids.iter().any(|x| x == "normal");
            s.color_scheme_allows_dark = allows_dark;
            s.color_scheme_dark = allows_dark && (ctx.prefers_dark || !allows_light);
        }
        "font-size" => s.font_size = parse_font_size(single(v)?, s, ctx)?,
        "font-family" => s.font_family = Rc::new(parse_font_family(v)?),
        "font-weight" => s.font_weight = font_weight(single(v)?, ctx.parent.font_weight)?,
        "font-style" => {
            s.font_style = match items(v).first()?.ident()?.to_ascii_lowercase().as_str() {
                "normal" => FontStyle::Normal,
                "italic" => FontStyle::Italic,
                "oblique" => FontStyle::Oblique,
                _ => return None,
            }
        }
        "font-variant" | "font-variant-caps" => {
            s.small_caps = match ident(v)?.as_str() {
                "small-caps" | "all-small-caps" => true,
                "normal" | "none" => false,
                _ => return None,
            }
        }
        "font" => apply_font_shorthand(s, v, ctx)?,
        "line-height" => s.line_height = parse_line_height(single(v)?, fs, ctx)?,
        "text-align" => {
            s.text_align = match ident(v)?.as_str() {
                "start" => TextAlign::Start,
                "end" => TextAlign::End,
                "left" => TextAlign::Left,
                "right" => TextAlign::Right,
                "center" => TextAlign::Center,
                "justify" => TextAlign::Justify,
                "-webkit-center" | "-moz-center" => TextAlign::LegacyCenter,
                "-webkit-left" | "-moz-left" => TextAlign::LegacyLeft,
                "-webkit-right" | "-moz-right" => TextAlign::LegacyRight,
                "match-parent" => ctx.parent.text_align,
                _ => return None,
            }
        }
        "text-indent" => s.text_indent = parse_lp(items(v).first()?, fs, ctx, true)?,
        "text-transform" => {
            s.text_transform = match ident(v)?.as_str() {
                "none" => TextTransform::None,
                "uppercase" => TextTransform::Uppercase,
                "lowercase" => TextTransform::Lowercase,
                "capitalize" => TextTransform::Capitalize,
                _ => return None,
            }
        }
        "white-space" => {
            let ids: Vec<String> =
                items(v).iter().filter_map(|c| c.ident().map(|x| x.to_ascii_lowercase())).collect();
            s.white_space = match ids.iter().map(|x| x.as_str()).collect::<Vec<_>>().as_slice() {
                ["normal"] => WhiteSpace::Normal,
                ["pre"] => WhiteSpace::Pre,
                ["nowrap"] => WhiteSpace::Nowrap,
                ["pre-wrap"] | ["-moz-pre-wrap"] => WhiteSpace::PreWrap,
                ["pre-line"] => WhiteSpace::PreLine,
                ["break-spaces"] => WhiteSpace::BreakSpaces,
                _ => return None,
            }
        }
        "text-wrap" | "text-wrap-mode" => match ident(v)?.as_str() {
            "nowrap" => {
                s.white_space =
                    if s.white_space.preserves_spaces() { WhiteSpace::Pre } else { WhiteSpace::Nowrap }
            }
            "wrap" | "balance" | "pretty" | "stable" => {}
            _ => return None,
        },
        "letter-spacing" => {
            s.letter_spacing = if ident(v).as_deref() == Some("normal") {
                0.0
            } else {
                parse_len_px(single(v)?, fs, ctx, false)?
            }
        }
        "word-spacing" => {
            s.word_spacing = if ident(v).as_deref() == Some("normal") {
                0.0
            } else {
                parse_len_px(single(v)?, fs, ctx, false)?
            }
        }
        "visibility" => {
            s.visibility = match ident(v)?.as_str() {
                "visible" => Visibility::Visible,
                "hidden" => Visibility::Hidden,
                "collapse" => Visibility::Collapse,
                _ => return None,
            }
        }
        "list-style-type" => s.list_style_type = list_style_type(&ident(v)?)?,
        "list-style-position" => {
            s.list_style_position = match ident(v)?.as_str() {
                "inside" => ListStylePosition::Inside,
                "outside" => ListStylePosition::Outside,
                _ => return None,
            }
        }
        "list-style-image" => {
            let c = single(v)?;
            s.list_style_image = if c.is_ident("none") { None } else { Some(url_of(c)?) };
        }
        "list-style" => {
            let mut ty = None;
            let mut pos = None;
            let mut img = None;
            let mut nones = 0;
            for c in items(v) {
                if c.is_ident("none") {
                    nones += 1;
                } else if let Some(t) = c.ident().and_then(|i| list_style_type(&i.to_ascii_lowercase())) {
                    ty = Some(t);
                } else if c.is_ident("inside") {
                    pos = Some(ListStylePosition::Inside);
                } else if c.is_ident("outside") {
                    pos = Some(ListStylePosition::Outside);
                } else {
                    let u = url_of(c)?;
                    img = Some(u);
                }
            }
            if nones > 0 && ty.is_none() {
                ty = Some(ListStyleType::None);
            }
            s.list_style_type = ty.unwrap_or(ListStyleType::Disc);
            s.list_style_position = pos.unwrap_or(ListStylePosition::Outside);
            s.list_style_image = img;
        }
        "border-collapse" => {
            s.border_collapse = match ident(v)?.as_str() {
                "collapse" => BorderCollapse::Collapse,
                "separate" => BorderCollapse::Separate,
                _ => return None,
            }
        }
        "border-spacing" => {
            let vals: Vec<f32> =
                items(v).iter().map(|c| parse_len_px(c, fs, ctx, true)).collect::<Option<_>>()?;
            s.border_spacing = match vals.as_slice() {
                [a] => (*a, *a),
                [a, b] => (*a, *b),
                _ => return None,
            };
        }
        "caption-side" => {
            s.caption_side = match ident(v)?.as_str() {
                "top" => CaptionSide::Top,
                "bottom" => CaptionSide::Bottom,
                _ => return None,
            }
        }
        "empty-cells" => s.empty_cells_show = ident(v)? != "hide",
        "cursor" => {
            // Take the first keyword after any url() fallbacks.
            let kw = items(v).iter().rev().find_map(|c| c.ident()).map(|s| s.to_ascii_lowercase())?;
            s.cursor = match kw.as_str() {
                "auto" => Cursor::Auto,
                "default" | "context-menu" => Cursor::Default,
                "pointer" | "hand" => Cursor::Pointer,
                "text" | "vertical-text" => Cursor::Text,
                "move" | "all-scroll" => Cursor::Move,
                "not-allowed" | "no-drop" => Cursor::NotAllowed,
                "wait" => Cursor::Wait,
                "progress" => Cursor::Progress,
                "help" => Cursor::Help,
                "crosshair" | "cell" => Cursor::Crosshair,
                "grab" | "grabbing" => Cursor::Grab,
                _ => Cursor::Auto,
            }
        }
        "direction" => s.direction_rtl = ident(v)? == "rtl",
        "display" => s.display = parse_display(v)?,
        "position" => {
            s.position = match ident(v)?.as_str() {
                "static" => Position::Static,
                "relative" => Position::Relative,
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                "sticky" | "-webkit-sticky" => Position::Sticky,
                _ => return None,
            }
        }
        "float" => {
            s.float = match ident(v)?.as_str() {
                "none" => Float::None,
                "left" | "inline-start" => Float::Left,
                "right" | "inline-end" => Float::Right,
                _ => return None,
            }
        }
        "clear" => {
            s.clear = match ident(v)?.as_str() {
                "none" => Clear::None,
                "left" | "inline-start" => Clear::Left,
                "right" | "inline-end" => Clear::Right,
                "both" | "all" => Clear::Both,
                _ => return None,
            }
        }
        "top" => s.top = parse_lpa(single(v)?, fs, ctx, true)?,
        "right" => s.right = parse_lpa(single(v)?, fs, ctx, true)?,
        "bottom" => s.bottom = parse_lpa(single(v)?, fs, ctx, true)?,
        "left" => s.left = parse_lpa(single(v)?, fs, ctx, true)?,
        "inset" => {
            let vals: Vec<LPA> =
                items(v).iter().map(|c| parse_lpa(c, fs, ctx, false)).collect::<Option<_>>()?;
            let [t, r, b, l] = four(vals)?;
            s.top = t;
            s.right = r;
            s.bottom = b;
            s.left = l;
        }
        "width" | "height" | "min-width" | "min-height" => {
            let c = single(v)?;
            let val = if let Some(id) = c.ident().map(|x| x.to_ascii_lowercase()) {
                match id.as_str() {
                    "auto" => LPA::Auto,
                    // Intrinsic keywords: approximated as auto for now.
                    "min-content"
                    | "max-content"
                    | "fit-content"
                    | "-moz-fit-content"
                    | "-webkit-fill-available"
                    | "stretch"
                    | "-moz-available" => LPA::Auto,
                    _ => return None,
                }
            } else {
                let lp = parse_lp(c, fs, ctx, true)?;
                if matches!(lp, LP::Px(x) if x < 0.0) || matches!(lp, LP::Pct(x) if x < 0.0) {
                    return None;
                }
                LPA::LP(lp)
            };
            match name {
                "width" => s.width = val,
                "height" => s.height = val,
                "min-width" => s.min_width = val,
                _ => s.min_height = val,
            }
        }
        "max-width" | "max-height" => {
            let c = single(v)?;
            let val = if c.ident().is_some() {
                match c.ident()?.to_ascii_lowercase().as_str() {
                    "none" | "max-content" | "min-content" | "fit-content" | "-moz-available" | "stretch" => {
                        None
                    }
                    _ => return None,
                }
            } else {
                Some(parse_lp(c, fs, ctx, true)?)
            };
            if name == "max-width" {
                s.max_width = val;
            } else {
                s.max_height = val;
            }
        }
        "margin" => {
            let vals: Vec<LPA> =
                items(v).iter().map(|c| parse_lpa(c, fs, ctx, true)).collect::<Option<_>>()?;
            s.margin = four(vals)?;
        }
        "padding" => {
            let vals: Vec<LP> = items(v).iter().map(|c| parse_lp(c, fs, ctx, true)).collect::<Option<_>>()?;
            if vals.iter().any(|x| matches!(x, LP::Px(p) if *p < 0.0)) {
                return None;
            }
            s.padding = four(vals)?;
        }
        "margin-block" | "margin-inline" | "padding-block" | "padding-inline" => {
            let (a, b) = if name.ends_with("block") { (0, 2) } else { (3, 1) };
            if name.starts_with("margin") {
                let vals: Vec<LPA> =
                    items(v).iter().map(|c| parse_lpa(c, fs, ctx, false)).collect::<Option<_>>()?;
                s.margin[a] = vals.first()?.clone();
                s.margin[b] = vals.get(1).unwrap_or(&vals[0]).clone();
            } else {
                let vals: Vec<LP> =
                    items(v).iter().map(|c| parse_lp(c, fs, ctx, false)).collect::<Option<_>>()?;
                s.padding[a] = vals.first()?.clone();
                s.padding[b] = vals.get(1).unwrap_or(&vals[0]).clone();
            }
        }
        "border" => {
            let (w, st, c) = parse_border_side(v, s, ctx)?;
            s.border_width = [w; 4];
            s.border_style = [st; 4];
            s.border_color = [c; 4];
        }
        "border-width" => {
            let vals: Vec<f32> =
                items(v).iter().map(|c| border_width_px(c, fs, ctx)).collect::<Option<_>>()?;
            s.border_width = four(vals)?;
        }
        "border-style" => {
            let vals: Vec<BorderStyle> = items(v).iter().map(|c| border_style(c)).collect::<Option<_>>()?;
            s.border_style = four(vals)?;
        }
        "border-color" => {
            let vals: Vec<Rgba> = items(v).iter().map(|c| color_of(c, s)).collect::<Option<_>>()?;
            s.border_color = four(vals)?;
        }
        "border-radius" => {
            let it = items(v);
            let end = it.iter().position(|c| c.is_delim('/')).unwrap_or(it.len());
            let vals: Vec<LP> =
                it[..end].iter().map(|c| parse_lp(c, fs, ctx, false)).collect::<Option<_>>()?;
            s.border_radius = four(vals)?;
        }
        "border-top-left-radius"
        | "border-top-right-radius"
        | "border-bottom-right-radius"
        | "border-bottom-left-radius" => {
            let idx = match name {
                "border-top-left-radius" => 0,
                "border-top-right-radius" => 1,
                "border-bottom-right-radius" => 2,
                _ => 3,
            };
            s.border_radius[idx] = parse_lp(items(v).first()?, fs, ctx, false)?;
        }
        "box-sizing" | "-moz-box-sizing" | "-webkit-box-sizing" => {
            s.box_sizing = match ident(v)?.as_str() {
                "content-box" => BoxSizing::ContentBox,
                "border-box" => BoxSizing::BorderBox,
                _ => return None,
            }
        }
        "overflow" | "overflow-x" | "overflow-y" => {
            let vals: Vec<Overflow> = items(v)
                .iter()
                .map(|c| {
                    Some(match c.ident()?.to_ascii_lowercase().as_str() {
                        "visible" => Overflow::Visible,
                        "hidden" => Overflow::Hidden,
                        "clip" => Overflow::Clip,
                        "scroll" => Overflow::Scroll,
                        "auto" | "overlay" => Overflow::Auto,
                        _ => return None,
                    })
                })
                .collect::<Option<_>>()?;
            match name {
                "overflow-x" => s.overflow_x = *vals.first()?,
                "overflow-y" => s.overflow_y = *vals.first()?,
                _ => {
                    s.overflow_x = *vals.first()?;
                    s.overflow_y = *vals.get(1).unwrap_or(&vals[0]);
                }
            }
        }
        "z-index" => {
            let c = single(v)?;
            s.z_index = if c.is_ident("auto") { None } else { Some(number(c)? as i32) };
        }
        "opacity" => {
            let c = single(v)?;
            let o = match c {
                Cv::Token(Token::Percentage(p)) => p / 100.0,
                c => number(c)?,
            };
            s.opacity = o.clamp(0.0, 1.0);
        }
        "background-color" => s.background_color = color_of(single(v)?, s)?,
        "background" => {
            let layers_src: Vec<&[Cv]> = v.split(|c| c.is_comma()).collect();
            let mut layers = Vec::new();
            let mut color = None;
            for (i, l) in layers_src.iter().enumerate() {
                let mut c = None;
                let layer = parse_bg_layer(&items(l), s, ctx, &mut c)?;
                if c.is_some() && i + 1 != layers_src.len() {
                    return None;
                }
                color = color.or(c);
                layers.push(layer);
            }
            s.background_color = color.unwrap_or(Rgba::TRANSPARENT);
            s.background = if layers.iter().all(|l| l.image.is_none()) { Vec::new() } else { layers };
        }
        "background-image" => {
            let mut imgs = Vec::new();
            for part in v.split(|c| c.is_comma()) {
                imgs.push(parse_image(single(part)?, s, ctx)?);
            }
            let old = std::mem::take(&mut s.background);
            s.background = imgs
                .into_iter()
                .enumerate()
                .map(|(i, img)| {
                    let mut l = old.get(i).cloned().unwrap_or_default();
                    l.image = img;
                    l
                })
                .collect();
            if s.background.iter().all(|l| l.image.is_none()) {
                s.background.clear();
            }
        }
        "background-repeat" => {
            let it = items(v);
            let (rx, ry) = match it.as_slice() {
                [c] => match c.ident()?.to_ascii_lowercase().as_str() {
                    "repeat-x" => (BgRepeat::Repeat, BgRepeat::NoRepeat),
                    "repeat-y" => (BgRepeat::NoRepeat, BgRepeat::Repeat),
                    k => {
                        let r = repeat_kw(k)?;
                        (r, r)
                    }
                },
                [a, b] => (
                    repeat_kw(&a.ident()?.to_ascii_lowercase())?,
                    repeat_kw(&b.ident()?.to_ascii_lowercase())?,
                ),
                _ => return None,
            };
            for l in s.background.iter_mut() {
                l.repeat_x = rx;
                l.repeat_y = ry;
            }
            if s.background.is_empty() {
                s.background.push(BgLayer { repeat_x: rx, repeat_y: ry, ..Default::default() });
            }
        }
        "background-position" => {
            let (x, y) = parse_position(&items(v.split(|c| c.is_comma()).next()?), fs, ctx)?;
            if s.background.is_empty() {
                s.background.push(BgLayer::default());
            }
            for l in s.background.iter_mut() {
                l.pos_x = x.clone();
                l.pos_y = y.clone();
            }
        }
        "background-size" => {
            let size = parse_bg_size(&items(v.split(|c| c.is_comma()).next()?), fs, ctx)?;
            if s.background.is_empty() {
                s.background.push(BgLayer::default());
            }
            for l in s.background.iter_mut() {
                l.size = size.clone();
            }
        }
        "background-attachment" => {
            let fixed = ident(v)? == "fixed";
            for l in s.background.iter_mut() {
                l.fixed = fixed;
            }
        }
        "box-shadow" => s.box_shadow = parse_shadow(v, s, ctx)?,
        "text-decoration" | "text-decoration-line" => {
            let mut line = TextDecorationLine::default();
            let mut color = None;
            for c in items(v) {
                match c.ident().map(|x| x.to_ascii_lowercase()).as_deref() {
                    Some("none") => {}
                    Some("underline") => line.underline = true,
                    Some("overline") => line.overline = true,
                    Some("line-through") => line.line_through = true,
                    Some("blink") => {}
                    Some("solid" | "double" | "dotted" | "dashed" | "wavy") if name == "text-decoration" => {}
                    _ => {
                        if name == "text-decoration" {
                            if let Some(col) = color_of(c, s) {
                                color = Some(col);
                                continue;
                            }
                            if parse_lp(c, fs, ctx, false).is_some()
                                || c.is_ident("auto")
                                || c.is_ident("from-font")
                            {
                                continue;
                            }
                        }
                        return None;
                    }
                }
            }
            s.text_decoration_line = line;
            if name == "text-decoration" {
                s.text_decoration_color = color;
            }
        }
        "text-decoration-color" => s.text_decoration_color = Some(color_of(single(v)?, s)?),
        "vertical-align" => {
            let c = single(v)?;
            s.vertical_align = if let Some(id) = c.ident() {
                match id.to_ascii_lowercase().as_str() {
                    "baseline" => VerticalAlign::Baseline,
                    "sub" => VerticalAlign::Sub,
                    "super" => VerticalAlign::Super,
                    "text-top" => VerticalAlign::TextTop,
                    "text-bottom" => VerticalAlign::TextBottom,
                    "middle" => VerticalAlign::Middle,
                    "top" => VerticalAlign::Top,
                    "bottom" => VerticalAlign::Bottom,
                    _ => return None,
                }
            } else {
                match parse_lp(c, fs, ctx, false)? {
                    LP::Pct(p) => VerticalAlign::Length(LP::Px(p / 100.0 * s.line_height_px(fs * 1.2))),
                    lp => VerticalAlign::Length(lp),
                }
            };
        }
        "table-layout" => {
            s.table_layout = match ident(v)?.as_str() {
                "auto" => TableLayout::Auto,
                "fixed" => TableLayout::Fixed,
                _ => return None,
            }
        }
        "content" => s.content = parse_content(v)?,
        "counter-reset" => s.counter_reset = parse_counters(v, 0)?,
        "counter-increment" => s.counter_increment = parse_counters(v, 1)?,
        "outline" => {
            if ident(v).as_deref() == Some("none") || ident(v).as_deref() == Some("0") {
                s.outline_style = BorderStyle::None;
                return Some(());
            }
            let (w, st, c) = parse_border_side(v, s, ctx)?;
            s.outline_width = w;
            s.outline_style = st;
            s.outline_color = Some(c);
        }
        "outline-width" => s.outline_width = border_width_px(single(v)?, fs, ctx)?,
        "outline-style" => {
            s.outline_style =
                border_style(single(v)?).or_else(|| (ident(v)? == "auto").then_some(BorderStyle::Solid))?
        }
        "outline-color" => s.outline_color = Some(color_of(single(v)?, s)?),
        "flex-direction" => {
            s.flex_direction = match ident(v)?.as_str() {
                "row" => FlexDirection::Row,
                "row-reverse" => FlexDirection::RowReverse,
                "column" => FlexDirection::Column,
                "column-reverse" => FlexDirection::ColumnReverse,
                _ => return None,
            }
        }
        "flex-wrap" => {
            s.flex_wrap = match ident(v)?.as_str() {
                "nowrap" => FlexWrap::NoWrap,
                "wrap" => FlexWrap::Wrap,
                "wrap-reverse" => FlexWrap::WrapReverse,
                _ => return None,
            }
        }
        "flex-flow" => {
            for c in items(v) {
                let one = [c.clone()];
                if !(apply(s, "flex-direction", &one, ctx) || apply(s, "flex-wrap", &one, ctx)) {
                    return None;
                }
            }
        }
        "flex" => {
            let it = items(v);
            if it.len() == 1 && it[0].is_ident("none") {
                s.flex_grow = 0.0;
                s.flex_shrink = 0.0;
                s.flex_basis = LPA::Auto;
                return Some(());
            }
            if it.len() == 1 && it[0].is_ident("auto") {
                s.flex_grow = 1.0;
                s.flex_shrink = 1.0;
                s.flex_basis = LPA::Auto;
                return Some(());
            }
            let mut nums = Vec::new();
            let mut basis = None;
            for c in it {
                if let Some(n) = number(c) {
                    nums.push(n);
                } else {
                    basis = Some(parse_lpa(c, fs, ctx, false)?);
                }
            }
            s.flex_grow = *nums.first().unwrap_or(&1.0);
            s.flex_shrink = *nums.get(1).unwrap_or(&1.0);
            s.flex_basis = basis.unwrap_or(LPA::LP(LP::Px(0.0)));
        }
        "flex-grow" => s.flex_grow = number(single(v)?)?.max(0.0),
        "flex-shrink" => s.flex_shrink = number(single(v)?)?.max(0.0),
        "flex-basis" => {
            let c = single(v)?;
            s.flex_basis = if c.is_ident("content") { LPA::Auto } else { parse_lpa(c, fs, ctx, false)? };
        }
        "justify-content" => s.justify_content = parse_align(v)?,
        "align-items" => s.align_items = parse_align(v)?,
        "align-self" => s.align_self = parse_align(v)?,
        "align-content" => s.align_content = parse_align(v)?,
        "gap" | "grid-gap" => {
            let vals: Vec<LP> = items(v)
                .iter()
                .map(|c| if c.is_ident("normal") { Some(LP::ZERO) } else { parse_lp(c, fs, ctx, false) })
                .collect::<Option<_>>()?;
            s.row_gap = vals.first()?.clone();
            s.column_gap = vals.get(1).unwrap_or(&vals[0]).clone();
        }
        "row-gap" | "grid-row-gap" => {
            s.row_gap = parse_lp(single(v)?, fs, ctx, false)
                .or_else(|| (ident(v)? == "normal").then_some(LP::ZERO))?
        }
        "column-gap" | "grid-column-gap" => {
            s.column_gap = parse_lp(single(v)?, fs, ctx, false)
                .or_else(|| (ident(v)? == "normal").then_some(LP::ZERO))?
        }
        "order" => s.order = number(single(v)?)? as i32,
        "object-fit" => {
            s.object_fit = match ident(v)?.as_str() {
                "fill" => ObjectFit::Fill,
                "contain" => ObjectFit::Contain,
                "cover" => ObjectFit::Cover,
                "none" => ObjectFit::None,
                "scale-down" => ObjectFit::ScaleDown,
                _ => return None,
            }
        }
        "transform" => {
            // Only translate() is supported so far (TODO(css-transforms-1): full 2D matrices).
            let it = items(v);
            if it.len() == 1 && it[0].is_ident("none") {
                s.translate = (LP::ZERO, LP::ZERO);
                return Some(());
            }
            let mut tx = LP::ZERO;
            let mut ty = LP::ZERO;
            for c in it {
                match c {
                    Cv::Function(f, args) if f == "translate" || f == "translatex" || f == "translatey" => {
                        let parts: Vec<&[Cv]> = args.split(|c| c.is_comma()).collect();
                        let a = parse_lp(single(parts.first()?)?, fs, ctx, false)?;
                        match f.as_str() {
                            "translatex" => tx = a,
                            "translatey" => ty = a,
                            _ => {
                                tx = a;
                                if let Some(p) = parts.get(1) {
                                    ty = parse_lp(single(p)?, fs, ctx, false)?;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            s.translate = (tx, ty);
        }
        _ => {
            for (i, side) in SIDES.iter().enumerate() {
                let n = name;
                if n == format!("margin-{side}") {
                    s.margin[i] = parse_lpa(single(v)?, fs, ctx, true)?;
                    return Some(());
                }
                if n == format!("padding-{side}") {
                    let p = parse_lp(single(v)?, fs, ctx, true)?;
                    if matches!(p, LP::Px(x) if x < 0.0) {
                        return None;
                    }
                    s.padding[i] = p;
                    return Some(());
                }
                if n == format!("border-{side}") {
                    let (w, st, c) = parse_border_side(v, s, ctx)?;
                    s.border_width[i] = w;
                    s.border_style[i] = st;
                    s.border_color[i] = c;
                    return Some(());
                }
                if n == format!("border-{side}-width") {
                    s.border_width[i] = border_width_px(single(v)?, fs, ctx)?;
                    return Some(());
                }
                if n == format!("border-{side}-style") {
                    s.border_style[i] = border_style(single(v)?)?;
                    return Some(());
                }
                if n == format!("border-{side}-color") {
                    s.border_color[i] = color_of(single(v)?, s)?;
                    return Some(());
                }
            }
            // Logical properties for horizontal-tb, ltr.
            let logical = [
                ("margin-block-start", "margin-top"),
                ("margin-block-end", "margin-bottom"),
                ("margin-inline-start", "margin-left"),
                ("margin-inline-end", "margin-right"),
                ("padding-block-start", "padding-top"),
                ("padding-block-end", "padding-bottom"),
                ("padding-inline-start", "padding-left"),
                ("padding-inline-end", "padding-right"),
                ("inline-size", "width"),
                ("block-size", "height"),
                ("max-inline-size", "max-width"),
                ("max-block-size", "max-height"),
                ("min-inline-size", "min-width"),
                ("min-block-size", "min-height"),
                ("border-inline-start", "border-left"),
                ("border-inline-end", "border-right"),
                ("border-block-start", "border-top"),
                ("border-block-end", "border-bottom"),
                ("inset-inline-start", "left"),
                ("inset-inline-end", "right"),
                ("inset-block-start", "top"),
                ("inset-block-end", "bottom"),
            ];
            if let Some((_, phys)) = logical.iter().find(|(l, _)| *l == name) {
                return apply_value(s, phys, v, ctx);
            }
            return None; // unknown property
        }
    }
    Some(())
}

fn parse_border_side(v: &[Cv], s: &ComputedStyle, ctx: &Ctx) -> Option<(f32, BorderStyle, Rgba)> {
    let mut w = None;
    let mut st = None;
    let mut c = None;
    for cv in items(v) {
        if st.is_none() {
            if let Some(x) = border_style(cv) {
                st = Some(x);
                continue;
            }
        }
        if w.is_none() {
            if let Some(x) = border_width_px(cv, s.font_size, ctx) {
                w = Some(x);
                continue;
            }
        }
        if c.is_none() {
            if let Some(x) = color_of(cv, s) {
                c = Some(x);
                continue;
            }
        }
        return None;
    }
    Some((w.unwrap_or(3.0), st.unwrap_or(BorderStyle::None), c.unwrap_or(s.color)))
}

/// CSS Fonts 4 §2.8 `font` shorthand.
fn apply_font_shorthand(s: &mut ComputedStyle, v: &[Cv], ctx: &Ctx) -> Option<()> {
    let it = items(v);
    // System fonts.
    if it.len() == 1 {
        if let Some(k) = it[0].ident().map(|x| x.to_ascii_lowercase()) {
            if matches!(
                k.as_str(),
                "caption"
                    | "icon"
                    | "menu"
                    | "message-box"
                    | "small-caption"
                    | "status-bar"
                    | "-moz-field"
                    | "-moz-button"
            ) {
                s.font_family = Rc::new(vec!["system-ui".into()]);
                s.font_size = 13.0;
                s.font_weight = 400;
                s.font_style = FontStyle::Normal;
                s.line_height = LineHeight::Normal;
                return Some(());
            }
        }
    }
    let mut style = FontStyle::Normal;
    let mut weight = 400;
    let mut small_caps = false;
    let mut i = 0;
    // Optional style/variant/weight/stretch before the size.
    while i < it.len() {
        let c = it[i];
        if let Some(k) = c.ident().map(|x| x.to_ascii_lowercase()) {
            match k.as_str() {
                "normal" => {}
                "italic" => style = FontStyle::Italic,
                "oblique" => style = FontStyle::Oblique,
                "small-caps" => small_caps = true,
                "bold" | "bolder" | "lighter" => weight = font_weight(c, ctx.parent.font_weight)?,
                "condensed" | "expanded" | "semi-condensed" | "semi-expanded" | "ultra-condensed"
                | "extra-condensed" | "extra-expanded" | "ultra-expanded" => {}
                _ => break,
            }
            i += 1;
        } else if let Cv::Token(Token::Number { value, .. }) = c {
            weight = *value as u16;
            i += 1;
        } else {
            break;
        }
    }
    let size_cv = it.get(i)?;
    // Family must be resolved first for the monospace size adjustment.
    let mut j = i + 1;
    let mut lh = LineHeight::Normal;
    if it.get(j).is_some_and(|c| c.is_delim('/')) {
        lh = parse_line_height(it.get(j + 1)?, 0.0, ctx)?;
        j += 2;
    }
    // Family: the remaining raw component values (keep commas).
    let start_idx = {
        let mut seen = 0;
        let mut idx = 0;
        for (k, cv) in v.iter().enumerate() {
            if !cv.is_ws() {
                if seen == j {
                    idx = k;
                    break;
                }
                seen += 1;
            }
            idx = k + 1;
        }
        idx
    };
    let family = parse_font_family(&v[start_idx..])?;
    s.font_family = Rc::new(family);
    s.font_size = parse_font_size(size_cv, s, ctx)?;
    if let LineHeight::Px(_) = lh {
        lh = parse_line_height(it[i + 2], s.font_size, ctx)?;
    }
    s.font_style = style;
    s.font_weight = weight;
    s.small_caps = small_caps;
    s.line_height = lh;
    Some(())
}

/// Resolve `var()` references in a declared value (CSS Variables 1 §3).
/// Returns `None` if a reference is invalid at computed-value time.
pub fn substitute_vars(value: &[Cv], style: &ComputedStyle, depth: usize) -> Option<Vec<Cv>> {
    if depth > 16 {
        return None;
    }
    let mut out = Vec::with_capacity(value.len());
    for cv in value {
        match cv {
            Cv::Function(f, args) if f == "var" => {
                let mut parts = args.splitn(2, |c| c.is_comma());
                let name_part = parts.next().unwrap_or(&[]);
                let name = name_part.iter().find_map(|c| c.ident())?.to_string();
                match style.custom_property(&name) {
                    Some(val) => out.extend(substitute_vars(val, style, depth + 1)?),
                    None => {
                        let fallback = parts.next()?;
                        out.extend(substitute_vars(fallback, style, depth + 1)?);
                    }
                }
            }
            Cv::Function(f, args) => out.push(Cv::Function(f.clone(), substitute_vars(args, style, depth)?)),
            Cv::Block(c, inner) => out.push(Cv::Block(*c, substitute_vars(inner, style, depth)?)),
            other => out.push(other.clone()),
        }
    }
    Some(out)
}

pub fn contains_var(value: &[Cv]) -> bool {
    value.iter().any(|cv| match cv {
        Cv::Function(f, args) => f == "var" || contains_var(args),
        Cv::Block(_, inner) => contains_var(inner),
        _ => false,
    })
}

/// Turn a `ColorValue` (from HTML hints etc.) into RGBA for this element.
pub fn resolve_color(c: &ColorValue, s: &ComputedStyle) -> Rgba {
    c.resolve(s.color, s.color_scheme_dark)
}

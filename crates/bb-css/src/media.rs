//! Media Queries Level 4 (subset): parsing and evaluation.

use crate::parser::Cv;
use crate::tokenizer::Token;
use crate::values::{parse_length, Length};

/// The environment media queries are evaluated against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MediaContext {
    /// Viewport width / height in CSS px.
    pub width: f32,
    pub height: f32,
    pub dark: bool,
    pub print: bool,
    /// Device pixels per CSS px.
    pub dppx: f32,
}

impl Default for MediaContext {
    fn default() -> Self {
        MediaContext { width: 1024.0, height: 768.0, dark: false, print: false, dppx: 1.0 }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Cond {
    Feature(Feature),
    Not(Box<Cond>),
    And(Vec<Cond>),
    Or(Vec<Cond>),
    /// Unknown / unparseable: evaluates to false.
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
enum Cmp {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

#[derive(Debug, Clone, PartialEq)]
struct Feature {
    name: String,
    /// `None` for boolean context (`(color)`).
    tests: Vec<(Cmp, Vec<Cv>)>,
}

#[derive(Debug, Clone, PartialEq)]
struct Query {
    not: bool,
    /// `None` = all.
    media_type: Option<String>,
    cond: Option<Cond>,
}

/// A comma-separated media query list. Empty matches everything.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MediaQueryList {
    queries: Vec<Query>,
}

impl MediaQueryList {
    pub fn parse(cvs: &[Cv]) -> MediaQueryList {
        let mut queries = Vec::new();
        if cvs.iter().all(|c| c.is_ws()) {
            return MediaQueryList { queries };
        }
        for part in cvs.split(|c| c.is_comma()) {
            let items: Vec<&Cv> = part.iter().filter(|c| !c.is_ws()).collect();
            queries.push(parse_query(&items).unwrap_or(Query {
                not: true,
                media_type: None,
                cond: None, // "not all": never matches
            }));
        }
        MediaQueryList { queries }
    }

    pub fn parse_str(s: &str) -> MediaQueryList {
        Self::parse(&crate::parser::parse_component_values(s))
    }

    pub fn is_all(&self) -> bool {
        self.queries.is_empty()
    }

    pub fn evaluate(&self, ctx: &MediaContext) -> bool {
        self.queries.is_empty() || self.queries.iter().any(|q| eval_query(q, ctx))
    }
}

fn parse_query(items: &[&Cv]) -> Option<Query> {
    let mut i = 0;
    let mut not = false;
    if items.first().is_some_and(|c| matches!(c, Cv::Block('(', _)))
        || items.first().is_some_and(|c| c.is_ident("not") && matches!(items.get(1), Some(Cv::Block('(', _))))
    {
        let cond = parse_condition(items)?;
        return Some(Query { not: false, media_type: None, cond: Some(cond) });
    }
    if items.get(i).is_some_and(|c| c.is_ident("not")) {
        not = true;
        i += 1;
    } else if items.get(i).is_some_and(|c| c.is_ident("only")) {
        i += 1;
    }
    let media_type = match items.get(i) {
        Some(Cv::Token(Token::Ident(t))) => {
            i += 1;
            Some(t.to_ascii_lowercase())
        }
        _ => return None,
    };
    if i == items.len() {
        return Some(Query { not, media_type, cond: None });
    }
    if !items[i].is_ident("and") {
        return None;
    }
    let cond = parse_condition(&items[i + 1..])?;
    Some(Query { not, media_type, cond: Some(cond) })
}

fn parse_condition(items: &[&Cv]) -> Option<Cond> {
    if items.is_empty() {
        return None;
    }
    if items[0].is_ident("not") {
        return Some(Cond::Not(Box::new(parse_in_parens(items.get(1)?)?)));
    }
    let first = parse_in_parens(items[0])?;
    if items.len() == 1 {
        return Some(first);
    }
    let joiner = items[1].ident()?.to_ascii_lowercase();
    let mut parts = vec![first];
    let mut i = 1;
    while i < items.len() {
        if items[i].ident().map(|s| s.to_ascii_lowercase()) != Some(joiner.clone()) {
            return None;
        }
        parts.push(parse_in_parens(items.get(i + 1)?)?);
        i += 2;
    }
    match joiner.as_str() {
        "and" => Some(Cond::And(parts)),
        "or" => Some(Cond::Or(parts)),
        _ => None,
    }
}

fn parse_in_parens(cv: &Cv) -> Option<Cond> {
    let Cv::Block('(', inner) = cv else { return Some(Cond::Unknown) };
    let items: Vec<&Cv> = inner.iter().filter(|c| !c.is_ws()).collect();
    if items.first().is_some_and(|c| matches!(c, Cv::Block('(', _)) || c.is_ident("not")) {
        return parse_condition(&items).or(Some(Cond::Unknown));
    }
    Some(parse_feature(&items).map(Cond::Feature).unwrap_or(Cond::Unknown))
}

fn parse_cmp(items: &[&Cv], i: &mut usize) -> Option<Cmp> {
    let c = items.get(*i)?;
    let eq_next = items.get(*i + 1).is_some_and(|n| n.is_delim('='));
    let r = if c.is_delim('<') {
        if eq_next {
            *i += 1;
            Cmp::Le
        } else {
            Cmp::Lt
        }
    } else if c.is_delim('>') {
        if eq_next {
            *i += 1;
            Cmp::Ge
        } else {
            Cmp::Gt
        }
    } else if c.is_delim('=') {
        Cmp::Eq
    } else {
        return None;
    };
    *i += 1;
    Some(r)
}

fn flip(c: Cmp) -> Cmp {
    match c {
        Cmp::Lt => Cmp::Gt,
        Cmp::Le => Cmp::Ge,
        Cmp::Gt => Cmp::Lt,
        Cmp::Ge => Cmp::Le,
        Cmp::Eq => Cmp::Eq,
    }
}

fn parse_feature(items: &[&Cv]) -> Option<Feature> {
    // `name`, `name: value`, `name op value`, `value op name`, `v op name op v`
    if let Some(Cv::Token(Token::Ident(name))) = items.first() {
        let name = name.to_ascii_lowercase();
        if items.len() == 1 {
            return Some(Feature { name, tests: Vec::new() });
        }
        if matches!(items[1], Cv::Token(Token::Colon)) {
            let value: Vec<Cv> = items[2..].iter().map(|c| (*c).clone()).collect();
            let (base, cmp) = if let Some(b) = name.strip_prefix("min-") {
                (b.to_string(), Cmp::Ge)
            } else if let Some(b) = name.strip_prefix("max-") {
                (b.to_string(), Cmp::Le)
            } else {
                (name.clone(), Cmp::Eq)
            };
            return Some(Feature { name: base, tests: vec![(cmp, value)] });
        }
        let mut i = 1;
        let cmp = parse_cmp(items, &mut i)?;
        let value: Vec<Cv> = items[i..].iter().map(|c| (*c).clone()).collect();
        return Some(Feature { name, tests: vec![(cmp, value)] });
    }
    // value op name [op value]
    let name_pos = items.iter().position(|c| c.ident().is_some())?;
    let left: Vec<Cv> = items[..name_pos].iter().map(|c| (*c).clone()).collect();
    let mut j = 0;
    let lhs_end = left.iter().position(|c| c.is_delim('<') || c.is_delim('>') || c.is_delim('='))?;
    let refs: Vec<&Cv> = left.iter().collect();
    j += lhs_end;
    let cmp1 = parse_cmp(&refs, &mut j)?;
    let v1: Vec<Cv> = left[..lhs_end].to_vec();
    let name = items[name_pos].ident()?.to_ascii_lowercase();
    let mut tests = vec![(flip(cmp1), v1)];
    let mut k = name_pos + 1;
    if k < items.len() {
        let cmp2 = parse_cmp(items, &mut k)?;
        tests.push((cmp2, items[k..].iter().map(|c| (*c).clone()).collect()));
    }
    Some(Feature { name, tests })
}

fn eval_query(q: &Query, ctx: &MediaContext) -> bool {
    let type_ok = match q.media_type.as_deref() {
        None | Some("all") => true,
        Some("screen") => !ctx.print,
        Some("print") => ctx.print,
        _ => false,
    };
    let cond_ok = q.cond.as_ref().is_none_or(|c| eval_cond(c, ctx));
    let r = type_ok && cond_ok;
    if q.not {
        !r
    } else {
        r
    }
}

fn eval_cond(c: &Cond, ctx: &MediaContext) -> bool {
    match c {
        Cond::Feature(f) => eval_feature(f, ctx),
        Cond::Not(c) => !eval_cond(c, ctx),
        Cond::And(v) => v.iter().all(|c| eval_cond(c, ctx)),
        Cond::Or(v) => v.iter().any(|c| eval_cond(c, ctx)),
        Cond::Unknown => false,
    }
}

fn length_px(v: &[Cv]) -> Option<f32> {
    let cv = v.iter().find(|c| !c.is_ws())?;
    if let Cv::Token(Token::Number { value, .. }) = cv {
        if *value == 0.0 {
            return Some(0.0);
        }
    }
    let l: Length = parse_length(cv)?;
    // Media queries resolve relative units against the initial font size (16px).
    l.to_px_static(16.0)
}

fn cmp_num(a: f32, op: &Cmp, b: f32) -> bool {
    match op {
        Cmp::Lt => a < b,
        Cmp::Le => a <= b + 0.001,
        Cmp::Eq => (a - b).abs() < 0.01,
        Cmp::Ge => a + 0.001 >= b,
        Cmp::Gt => a > b,
    }
}

fn ident_of(v: &[Cv]) -> Option<String> {
    v.iter().find_map(|c| c.ident()).map(|s| s.to_ascii_lowercase())
}

fn eval_feature(f: &Feature, ctx: &MediaContext) -> bool {
    let num_feature = |actual: f32, parse: &dyn Fn(&[Cv]) -> Option<f32>| -> bool {
        if f.tests.is_empty() {
            return actual != 0.0;
        }
        f.tests.iter().all(|(op, v)| parse(v).is_some_and(|x| cmp_num(actual, op, x)))
    };
    let ident_feature = |actual: &str, boolean: bool| -> bool {
        if f.tests.is_empty() {
            return boolean;
        }
        f.tests.iter().all(|(op, v)| *op == Cmp::Eq && ident_of(v).as_deref() == Some(actual))
    };
    match f.name.as_str() {
        "width" | "device-width" => num_feature(ctx.width, &length_px),
        "height" | "device-height" => num_feature(ctx.height, &length_px),
        "aspect-ratio" | "device-aspect-ratio" => {
            num_feature(ctx.width / ctx.height.max(1.0), &|v: &[Cv]| {
                let nums: Vec<f32> = v
                    .iter()
                    .filter_map(|c| match c {
                        Cv::Token(Token::Number { value, .. }) => Some(*value),
                        _ => None,
                    })
                    .collect();
                match nums.as_slice() {
                    [a, b] if *b != 0.0 => Some(a / b),
                    [a] => Some(*a),
                    _ => None,
                }
            })
        }
        "orientation" => ident_feature(if ctx.height >= ctx.width { "portrait" } else { "landscape" }, true),
        "prefers-color-scheme" => ident_feature(if ctx.dark { "dark" } else { "light" }, true),
        "prefers-reduced-motion" | "prefers-reduced-transparency" | "prefers-reduced-data" => {
            ident_feature("no-preference", false)
        }
        "prefers-contrast" => ident_feature("no-preference", false),
        "forced-colors" | "inverted-colors" => ident_feature("none", false),
        "hover" | "any-hover" => ident_feature("hover", true),
        "pointer" | "any-pointer" => ident_feature("fine", true),
        "scripting" => ident_feature("none", false),
        "display-mode" => ident_feature("browser", true),
        "update" => ident_feature("fast", true),
        "color" => num_feature(8.0, &|v: &[Cv]| match v.iter().find(|c| !c.is_ws()) {
            Some(Cv::Token(Token::Number { value, .. })) => Some(*value),
            _ => None,
        }),
        "monochrome" => num_feature(0.0, &|v: &[Cv]| match v.iter().find(|c| !c.is_ws()) {
            Some(Cv::Token(Token::Number { value, .. })) => Some(*value),
            _ => None,
        }),
        "resolution" | "-webkit-device-pixel-ratio" => {
            num_feature(ctx.dppx, &|v: &[Cv]| match v.iter().find(|c| !c.is_ws()) {
                Some(Cv::Token(Token::Dimension { value, unit })) => match unit.to_ascii_lowercase().as_str()
                {
                    "dppx" | "x" => Some(*value),
                    "dpi" => Some(*value / 96.0),
                    "dpcm" => Some(*value * 2.54 / 96.0),
                    _ => None,
                },
                Some(Cv::Token(Token::Number { value, .. })) => Some(*value),
                _ => None,
            })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(q: &str, ctx: MediaContext) -> bool {
        MediaQueryList::parse_str(q).evaluate(&ctx)
    }

    #[test]
    fn queries() {
        let c = MediaContext { width: 800.0, height: 600.0, ..Default::default() };
        assert!(eval("", c));
        assert!(eval("screen", c));
        assert!(!eval("print", c));
        assert!(eval("screen and (min-width: 600px)", c));
        assert!(!eval("(max-width: 600px)", c));
        assert!(eval("(max-width: 600px), (orientation: landscape)", c));
        assert!(eval("(width >= 800px)", c));
        assert!(eval("(400px < width <= 800px)", c));
        assert!(!eval("not screen", c));
        assert!(eval("not print", c));
        assert!(eval("(prefers-color-scheme: light)", c));
        assert!(eval("(min-width: 40em)", c));
        assert!(!eval("(unknown-feature)", c));
        assert!(eval("only screen and (min-device-pixel-ratio: 0), all", c));
        let d = MediaContext { dark: true, ..c };
        assert!(eval("(prefers-color-scheme: dark)", d));
        assert!(eval("not (prefers-color-scheme: light)", d));
    }
}

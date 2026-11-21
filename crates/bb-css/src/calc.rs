//! CSS Values 4 §10 math functions: `calc()`, `min()`, `max()`, `clamp()`.

use crate::parser::Cv;
use crate::tokenizer::Token;
use crate::values::{unit_from_str, Length, Unit};

/// A calc expression tree. Leaves are numbers, lengths or percentages.
#[derive(Debug, Clone, PartialEq)]
pub enum Calc {
    Number(f32),
    Length(Length),
    Percent(f32),
    Add(Box<Calc>, Box<Calc>),
    Sub(Box<Calc>, Box<Calc>),
    Mul(Box<Calc>, Box<Calc>),
    Div(Box<Calc>, Box<Calc>),
    Min(Vec<Calc>),
    Max(Vec<Calc>),
    Clamp(Box<Calc>, Box<Calc>, Box<Calc>),
}

/// Context needed to resolve lengths inside calc().
pub trait LengthResolver {
    fn length_px(&self, l: &Length) -> f32;
}

impl Calc {
    /// Evaluate to px, with percentages resolved against `pct_base`.
    pub fn eval_px(&self, r: &dyn LengthResolver, pct_base: f32) -> f32 {
        match self {
            Calc::Number(n) => *n,
            Calc::Length(l) => r.length_px(l),
            Calc::Percent(p) => p / 100.0 * pct_base,
            Calc::Add(a, b) => a.eval_px(r, pct_base) + b.eval_px(r, pct_base),
            Calc::Sub(a, b) => a.eval_px(r, pct_base) - b.eval_px(r, pct_base),
            Calc::Mul(a, b) => a.eval_px(r, pct_base) * b.eval_px(r, pct_base),
            Calc::Div(a, b) => {
                let d = b.eval_px(r, pct_base);
                if d == 0.0 {
                    0.0
                } else {
                    a.eval_px(r, pct_base) / d
                }
            }
            Calc::Min(v) => v.iter().map(|c| c.eval_px(r, pct_base)).fold(f32::INFINITY, f32::min),
            Calc::Max(v) => v.iter().map(|c| c.eval_px(r, pct_base)).fold(f32::NEG_INFINITY, f32::max),
            Calc::Clamp(lo, v, hi) => {
                let lo = lo.eval_px(r, pct_base);
                let hi = hi.eval_px(r, pct_base);
                v.eval_px(r, pct_base).min(hi).max(lo)
            }
        }
    }

    /// Evaluate a pure-number expression.
    pub fn eval_number(&self) -> Option<f32> {
        struct NoLen;
        impl LengthResolver for NoLen {
            fn length_px(&self, _: &Length) -> f32 {
                f32::NAN
            }
        }
        if self.has_percent() {
            return None;
        }
        let v = self.eval_px(&NoLen, 0.0);
        if v.is_nan() {
            None
        } else {
            Some(v)
        }
    }

    pub fn has_percent(&self) -> bool {
        match self {
            Calc::Percent(_) => true,
            Calc::Number(_) | Calc::Length(_) => false,
            Calc::Add(a, b) | Calc::Sub(a, b) | Calc::Mul(a, b) | Calc::Div(a, b) => {
                a.has_percent() || b.has_percent()
            }
            Calc::Min(v) | Calc::Max(v) => v.iter().any(Calc::has_percent),
            Calc::Clamp(a, b, c) => a.has_percent() || b.has_percent() || c.has_percent(),
        }
    }
}

/// Parse the arguments of a `calc()` function.
pub fn parse_calc(args: &[Cv]) -> Option<Calc> {
    let items: Vec<&Cv> = args.iter().collect();
    let mut pos = 0;
    let r = parse_sum(&items, &mut pos)?;
    skip_ws(&items, &mut pos);
    if pos != items.len() {
        return None;
    }
    Some(r)
}

/// Parse a math function (`calc`, `min`, `max`, `clamp`) given its name and args.
pub fn parse_math_function(name: &str, args: &[Cv]) -> Option<Calc> {
    match name {
        "calc" | "-webkit-calc" | "-moz-calc" => parse_calc(args),
        "min" | "max" => {
            let parts: Vec<Calc> = args.split(|c| c.is_comma()).map(parse_calc).collect::<Option<_>>()?;
            if parts.is_empty() {
                return None;
            }
            Some(if name == "min" { Calc::Min(parts) } else { Calc::Max(parts) })
        }
        "clamp" => {
            let parts: Vec<Calc> = args.split(|c| c.is_comma()).map(parse_calc).collect::<Option<_>>()?;
            if parts.len() != 3 {
                return None;
            }
            let mut it = parts.into_iter();
            Some(Calc::Clamp(Box::new(it.next()?), Box::new(it.next()?), Box::new(it.next()?)))
        }
        _ => None,
    }
}

fn skip_ws(items: &[&Cv], pos: &mut usize) {
    while items.get(*pos).is_some_and(|c| c.is_ws()) {
        *pos += 1;
    }
}

fn parse_sum(items: &[&Cv], pos: &mut usize) -> Option<Calc> {
    let mut left = parse_product(items, pos)?;
    loop {
        skip_ws(items, pos);
        match items.get(*pos) {
            Some(c) if c.is_delim('+') => {
                *pos += 1;
                let right = parse_product(items, pos)?;
                left = Calc::Add(Box::new(left), Box::new(right));
            }
            Some(c) if c.is_delim('-') => {
                *pos += 1;
                let right = parse_product(items, pos)?;
                left = Calc::Sub(Box::new(left), Box::new(right));
            }
            // "a -1px" tokenizes as a negative number: treat as subtraction.
            Some(Cv::Token(Token::Number { value, .. })) if *value < 0.0 => {
                let right = parse_product(items, pos)?;
                left = Calc::Add(Box::new(left), Box::new(right));
            }
            Some(Cv::Token(Token::Dimension { value, .. })) if *value < 0.0 => {
                let right = parse_product(items, pos)?;
                left = Calc::Add(Box::new(left), Box::new(right));
            }
            _ => return Some(left),
        }
    }
}

fn parse_product(items: &[&Cv], pos: &mut usize) -> Option<Calc> {
    let mut left = parse_value(items, pos)?;
    loop {
        skip_ws(items, pos);
        match items.get(*pos) {
            Some(c) if c.is_delim('*') => {
                *pos += 1;
                let right = parse_value(items, pos)?;
                left = Calc::Mul(Box::new(left), Box::new(right));
            }
            Some(c) if c.is_delim('/') => {
                *pos += 1;
                let right = parse_value(items, pos)?;
                left = Calc::Div(Box::new(left), Box::new(right));
            }
            _ => return Some(left),
        }
    }
}

fn parse_value(items: &[&Cv], pos: &mut usize) -> Option<Calc> {
    skip_ws(items, pos);
    let cv = *items.get(*pos)?;
    *pos += 1;
    Some(match cv {
        Cv::Token(Token::Number { value, .. }) => Calc::Number(*value),
        Cv::Token(Token::Percentage(p)) => Calc::Percent(*p),
        Cv::Token(Token::Dimension { value, unit }) => {
            Calc::Length(Length { value: *value, unit: unit_from_str(unit).unwrap_or(Unit::Px) })
        }
        Cv::Block('(', inner) => parse_calc(inner)?,
        Cv::Function(name, args) => parse_math_function(name, args)?,
        Cv::Token(Token::Ident(i)) => match i.to_ascii_lowercase().as_str() {
            "pi" => Calc::Number(std::f32::consts::PI),
            "e" => Calc::Number(std::f32::consts::E),
            _ => return None,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_component_values;

    struct R;
    impl LengthResolver for R {
        fn length_px(&self, l: &Length) -> f32 {
            l.to_px_static(10.0).unwrap_or(0.0)
        }
    }

    fn calc(s: &str) -> Calc {
        let cvs = parse_component_values(s);
        match &cvs[0] {
            Cv::Function(n, a) => parse_math_function(n, a).unwrap(),
            _ => panic!(),
        }
    }

    #[test]
    fn evaluate() {
        assert_eq!(calc("calc(100% - 20px)").eval_px(&R, 200.0), 180.0);
        assert_eq!(calc("calc(2em + 3px * 2)").eval_px(&R, 0.0), 26.0);
        assert_eq!(calc("calc((1px + 2px) * 3)").eval_px(&R, 0.0), 9.0);
        assert_eq!(calc("min(10px, 5%)").eval_px(&R, 100.0), 5.0);
        assert_eq!(calc("clamp(10px, 50%, 30px)").eval_px(&R, 100.0), 30.0);
        assert_eq!(calc("calc(10px / 0)").eval_px(&R, 0.0), 0.0);
    }
}

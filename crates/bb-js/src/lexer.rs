//! Lexical grammar (ECMA-262 §12): white space, line terminators, comments, identifiers and
//! reserved words, punctuators, numeric (incl. BigInt, separators, legacy octal), string and
//! template literals (cooked as UTF-16 code units, with raw text), regular expression literals.
//!
//! The parser drives the lexer: whether a `/` starts a regular expression and where a template
//! continues depends on the syntactic context (§12 "goal symbols").

/// A JS string: UTF-16 code units (lone surrogates allowed).
pub type JsStr = Vec<u16>;

#[derive(Debug, Clone, PartialEq)]
pub enum TokKind {
    /// An identifier name (reserved words too; the parser decides). `escaped` = written with
    /// `\u` escapes (escaped reserved words are never keywords).
    Ident {
        name: String,
        escaped: bool,
    },
    /// `#name`
    Private(String),
    Punct(&'static str),
    Num(f64),
    BigInt(String),
    Str(JsStr),
    /// A template part: `cooked` is `None` for an invalid escape (allowed in tagged templates);
    /// `tail` = this part ends the template (`` ` ``) rather than `${`.
    Template {
        cooked: Option<JsStr>,
        raw: String,
        tail: bool,
    },
    Regex {
        body: String,
        flags: String,
    },
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokKind,
    pub start: usize,
    pub end: usize,
    /// A line terminator came between the previous token and this one (ASI, restricted
    /// productions).
    pub nl_before: bool,
    /// A string or template used a legacy octal escape or `\8`/`\9`, or the number is a legacy
    /// octal / decimal-with-leading-zero literal (early errors in strict mode).
    pub legacy_octal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub pos: usize,
    pub msg: String,
}

pub fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// §12.2 WhiteSpace: TAB, VT, FF, ZWNBSP and Unicode "Space_Separator" (Zs).
pub fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\u{b}' | '\u{c}' | ' ' | '\u{a0}' | '\u{feff}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

fn in_table(c: char, table: &[(u32, u32)]) -> bool {
    let c = c as u32;
    table
        .binary_search_by(|&(a, b)| {
            if b < c {
                std::cmp::Ordering::Less
            } else if a > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// IdentifierStartChar: ID_Start (Unicode 17), `$`, `_`.
pub fn is_id_start(c: char) -> bool {
    c == '$'
        || c == '_'
        || (c.is_ascii() && c.is_ascii_alphabetic())
        || (!c.is_ascii() && in_table(c, crate::unicode_ids::ID_START))
}

/// IdentifierPartChar: ID_Continue (Unicode 17), `$`, ZWNJ, ZWJ.
pub fn is_id_continue(c: char) -> bool {
    c == '$'
        || c == '\u{200c}'
        || c == '\u{200d}'
        || (c.is_ascii() && (c.is_ascii_alphanumeric() || c == '_'))
        || (!c.is_ascii() && in_table(c, crate::unicode_ids::ID_CONTINUE))
}

/// Punctuators, longest first (§12.8).
const PUNCTS: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=", "<=",
    ">=", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "%=", "&=", "|=", "^=", "/=", "<<", ">>",
    "**", "{", "}", "(", ")", "[", "]", ".", ";", ",", "<", ">", "+", "-", "*", "%", "&", "|", "^", "!", "~",
    "?", ":", "=", "@", "/",
];

pub struct Lexer {
    src: Vec<char>,
    pub pos: usize,
    /// Source text with lone surrogates (eval of a JavaScript string) has them mapped to a
    /// block of 0x800 private-use code points from this base, none of which occur in the
    /// text itself (`encode_source`); string and regular expression literals map them
    /// back.
    pub surrogates: Option<u32>,
}

/// Source text from UTF-16 that may hold lone surrogates: a Rust string with them mapped
/// to private-use code points (`Lexer::surrogates`), and that block's base if any.
pub fn encode_source(units: &[u16]) -> (String, Option<u32>) {
    if String::from_utf16(units).is_ok() {
        return (String::from_utf16_lossy(units), None);
    }
    let cps: Vec<u32> = char::decode_utf16(units.iter().copied())
        .map(|r| r.map(|c| c as u32).unwrap_or_else(|e| e.unpaired_surrogate() as u32))
        .collect();
    // A block in planes 15–16 that the text doesn't use.
    let base = (0xF0000..0x10F800)
        .step_by(0x800)
        .find(|b| !cps.iter().any(|c| (*b..b + 0x800).contains(c)))
        .unwrap_or(0xF0000);
    let s = cps
        .iter()
        .map(|&c| match c {
            0xD800..=0xDFFF => char::from_u32(base + c - 0xD800).unwrap_or('\u{FFFD}'),
            c => char::from_u32(c).unwrap_or('\u{FFFD}'),
        })
        .collect();
    (s, Some(base))
}

fn push_cp(out: &mut JsStr, cp: u32) {
    if let Some(c) = char::from_u32(cp) {
        let mut b = [0u16; 2];
        out.extend_from_slice(c.encode_utf16(&mut b));
    } else {
        // A lone surrogate (from \uD800 etc.).
        out.push(cp as u16);
    }
}

impl Lexer {
    /// A source character into a literal's value (a mapped lone surrogate back to itself).
    fn push_src(&self, out: &mut JsStr, c: char) {
        match self.surrogates {
            Some(b) if (b..b + 0x800).contains(&(c as u32)) => out.push((0xD800 + c as u32 - b) as u16),
            _ => push_cp(out, c as u32),
        }
    }

    /// A source text slice (a regular expression body) as UTF-16, lone surrogates restored.
    pub fn units(&self, s: &str) -> JsStr {
        let mut out = Vec::with_capacity(s.len());
        for c in s.chars() {
            self.push_src(&mut out, c);
        }
        out
    }

    pub fn new(src: &str) -> Lexer {
        Lexer { src: src.chars().collect(), pos: 0, surrogates: None }
    }

    pub fn source(&self) -> &[char] {
        &self.src
    }

    fn peek(&self) -> Option<char> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.src.get(self.pos + n).copied()
    }

    fn err<T>(&self, msg: impl Into<String>) -> Result<T, LexError> {
        Err(LexError { pos: self.pos, msg: msg.into() })
    }

    /// Skip white space and comments; returns whether a line terminator was crossed.
    /// `html_comments`: Annex B `<!--` and `-->` comments (scripts only).
    pub fn skip_trivia(&mut self, html_comments: bool, mut at_line_start: bool) -> Result<bool, LexError> {
        let mut nl = false;
        loop {
            let Some(c) = self.peek() else { return Ok(nl) };
            if is_whitespace(c) {
                self.pos += 1;
            } else if is_line_terminator(c) {
                self.pos += 1;
                nl = true;
                at_line_start = true;
            } else if c == '/' && self.peek_at(1) == Some('/') {
                self.skip_line();
            } else if c == '/' && self.peek_at(1) == Some('*') {
                self.pos += 2;
                loop {
                    match self.peek() {
                        None => return self.err("unterminated comment"),
                        Some('*') if self.peek_at(1) == Some('/') => {
                            self.pos += 2;
                            break;
                        }
                        Some(c) => {
                            if is_line_terminator(c) {
                                nl = true;
                                at_line_start = true;
                            }
                            self.pos += 1;
                        }
                    }
                }
            } else if html_comments
                && ((c == '<' && self.src[self.pos..].starts_with(&['<', '!', '-', '-']))
                    || ((nl || at_line_start)
                        && c == '-'
                        && self.src[self.pos..].starts_with(&['-', '-', '>'])))
            {
                self.skip_line();
            } else {
                return Ok(nl);
            }
        }
    }

    fn skip_line(&mut self) {
        while let Some(c) = self.peek() {
            if is_line_terminator(c) {
                break;
            }
            self.pos += 1;
        }
    }

    /// The next token. `regex_ok`: a `/` here starts a regular expression literal.
    pub fn next(&mut self, regex_ok: bool, html_comments: bool) -> Result<Token, LexError> {
        let at_start = self.pos == 0;
        // §12.5 Hashbang comment: `#!` at the very start of a script or module.
        if at_start && self.src.starts_with(&['#', '!']) {
            self.skip_line();
        }
        let nl = self.skip_trivia(html_comments, at_start)?;
        let start = self.pos;
        let mut legacy_octal = false;
        let kind = match self.peek() {
            None => TokKind::Eof,
            Some(c) if c == '"' || c == '\'' => {
                let (s, oct) = self.string(c)?;
                legacy_octal = oct;
                TokKind::Str(s)
            }
            Some('`') => {
                self.pos += 1;
                self.template_part()?
            }
            Some(c)
                if c.is_ascii_digit()
                    || (c == '.' && self.peek_at(1).is_some_and(|d| d.is_ascii_digit())) =>
            {
                let (k, oct) = self.number()?;
                legacy_octal = oct;
                k
            }
            Some('#') => {
                self.pos += 1;
                let (name, _) = self
                    .identifier()?
                    .ok_or_else(|| LexError { pos: self.pos, msg: "bad private name".into() })?;
                TokKind::Private(name)
            }
            Some(c) if is_id_start(c) || c == '\\' => match self.identifier()? {
                Some((name, escaped)) => TokKind::Ident { name, escaped },
                None => return self.err("invalid identifier"),
            },
            Some('/') if regex_ok => self.regex()?,
            Some(_) => {
                let rest = &self.src[self.pos..];
                // Punctuators are ASCII: compare in place, no allocation per candidate.
                let p = PUNCTS
                    .iter()
                    .find(|p| p.len() <= rest.len() && p.bytes().zip(rest).all(|(a, &b)| a as char == b));
                match p {
                    // `?.` followed by a digit is `?` then a number (`a?.5:b`).
                    Some(&"?.") if rest.get(2).is_some_and(|d| d.is_ascii_digit()) => {
                        self.pos += 1;
                        TokKind::Punct("?")
                    }
                    Some(p) => {
                        self.pos += p.len();
                        TokKind::Punct(p)
                    }
                    None => return self.err(format!("unexpected character {:?}", self.src[self.pos])),
                }
            }
        };
        Ok(Token { kind, start, end: self.pos, nl_before: nl, legacy_octal })
    }

    /// IdentifierName with `\u` escapes; `None` if it isn't one.
    fn identifier(&mut self) -> Result<Option<(String, bool)>, LexError> {
        let mut name = String::new();
        let mut escaped = false;
        let mut first = true;
        loop {
            let c = match self.peek() {
                Some('\\') => {
                    if self.peek_at(1) != Some('u') {
                        return self.err("bad escape in identifier");
                    }
                    self.pos += 2;
                    let cp = self.unicode_escape_body()?;
                    escaped = true;
                    let c = char::from_u32(cp)
                        .ok_or_else(|| LexError { pos: self.pos, msg: "bad escape".into() })?;
                    let ok = if first { is_id_start(c) } else { is_id_continue(c) };
                    if !ok {
                        return self.err("escaped character not allowed in identifier");
                    }
                    name.push(c);
                    first = false;
                    continue;
                }
                Some(c) if (first && is_id_start(c)) || (!first && is_id_continue(c)) => c,
                _ => break,
            };
            name.push(c);
            self.pos += 1;
            first = false;
        }
        Ok(if name.is_empty() { None } else { Some((name, escaped)) })
    }

    /// After `\u`: `XXXX` or `{X…}`.
    fn unicode_escape_body(&mut self) -> Result<u32, LexError> {
        if self.peek() == Some('{') {
            self.pos += 1;
            let mut v: u32 = 0;
            let mut n = 0;
            while let Some(d) = self.peek().and_then(|c| c.to_digit(16)) {
                v = v.saturating_mul(16).saturating_add(d);
                self.pos += 1;
                n += 1;
            }
            if n == 0 || self.peek() != Some('}') || v > 0x10ffff {
                return self.err("bad \\u{} escape");
            }
            self.pos += 1;
            Ok(v)
        } else {
            let mut v = 0;
            for _ in 0..4 {
                let d = self
                    .peek()
                    .and_then(|c| c.to_digit(16))
                    .ok_or_else(|| LexError { pos: self.pos, msg: "bad \\u escape".into() })?;
                v = v * 16 + d;
                self.pos += 1;
            }
            Ok(v)
        }
    }

    fn string(&mut self, quote: char) -> Result<(JsStr, bool), LexError> {
        self.pos += 1;
        let mut out = Vec::new();
        let mut octal = false;
        loop {
            let Some(c) = self.peek() else { return self.err("unterminated string") };
            self.pos += 1;
            if c == quote {
                return Ok((out, octal));
            }
            if c == '\\' {
                match self.escape(false)? {
                    Escape::Cp(cp) => push_cp(&mut out, cp),
                    Escape::None => {}
                    Escape::LegacyOctal(cp) => {
                        octal = true;
                        push_cp(&mut out, cp);
                    }
                }
            } else if c == '\n' || c == '\r' {
                return self.err("line break in string");
            } else {
                self.push_src(&mut out, c);
            }
        }
    }

    /// After a backslash. `template`: octal escapes are errors (§12.9.6), not legacy octal.
    fn escape(&mut self, template: bool) -> Result<Escape, LexError> {
        let Some(c) = self.peek() else { return self.err("unterminated escape") };
        self.pos += 1;
        Ok(match c {
            'n' => Escape::Cp(10),
            't' => Escape::Cp(9),
            'r' => Escape::Cp(13),
            'b' => Escape::Cp(8),
            'f' => Escape::Cp(12),
            'v' => Escape::Cp(11),
            '\r' => {
                if self.peek() == Some('\n') {
                    self.pos += 1;
                }
                Escape::None
            }
            '\n' | '\u{2028}' | '\u{2029}' => Escape::None,
            'x' => {
                let h = |c: Option<char>| c.and_then(|c| c.to_digit(16));
                match (h(self.peek()), h(self.peek_at(1))) {
                    (Some(a), Some(b)) => {
                        self.pos += 2;
                        Escape::Cp(a * 16 + b)
                    }
                    _ => return self.err("bad \\x escape"),
                }
            }
            'u' => Escape::Cp(self.unicode_escape_body()?),
            '0' if !self.peek().is_some_and(|d| d.is_ascii_digit()) => Escape::Cp(0),
            '0'..='7' => {
                if template {
                    return self.err("octal escape in template");
                }
                // LegacyOctalEscapeSequence: up to 3 digits, value ≤ 0o377.
                let mut v = c.to_digit(8).unwrap_or(0);
                let max_digits = if c <= '3' { 3 } else { 2 };
                let mut n = 1;
                while n < max_digits {
                    match self.peek().and_then(|d| d.to_digit(8)) {
                        Some(d) => {
                            v = v * 8 + d;
                            self.pos += 1;
                            n += 1;
                        }
                        None => break,
                    }
                }
                Escape::LegacyOctal(v)
            }
            '8' | '9' => {
                if template {
                    return self.err("\\8 or \\9 in template");
                }
                Escape::LegacyOctal(c as u32)
            }
            other => Escape::Cp(other as u32),
        })
    }

    /// A template part after `` ` `` or `}` (the parser calls this for continuations).
    pub fn template_part(&mut self) -> Result<TokKind, LexError> {
        let mut cooked: Option<JsStr> = Some(Vec::new());
        let mut raw = String::new();
        loop {
            let Some(c) = self.peek() else { return self.err("unterminated template") };
            if c == '`' {
                self.pos += 1;
                return Ok(TokKind::Template { cooked, raw, tail: true });
            }
            if c == '$' && self.peek_at(1) == Some('{') {
                self.pos += 2;
                return Ok(TokKind::Template { cooked, raw, tail: false });
            }
            if c == '\\' {
                let start = self.pos;
                self.pos += 1;
                let saved = self.pos;
                match self.escape(true) {
                    Ok(Escape::Cp(cp)) => {
                        if let Some(s) = cooked.as_mut() {
                            push_cp(s, cp)
                        }
                    }
                    Ok(Escape::None) => {}
                    Ok(Escape::LegacyOctal(_)) | Err(_) => {
                        // NotEscapeSequence: the cooked value is undefined (tagged templates).
                        cooked = None;
                        self.pos = saved;
                    }
                }
                // Raw text: line terminators are normalized (CR LF and CR → LF).
                for ch in &self.src[start..self.pos] {
                    raw.push(*ch);
                }
                raw = raw.replace("\r\n", "\n").replace('\r', "\n");
                continue;
            }
            self.pos += 1;
            if c == '\r' {
                if self.peek() == Some('\n') {
                    self.pos += 1;
                }
                raw.push('\n');
                if let Some(s) = cooked.as_mut() {
                    s.push(10);
                }
                continue;
            }
            raw.push(c);
            if let Some(s) = cooked.as_mut() {
                match self.surrogates {
                    Some(b) if (b..b + 0x800).contains(&(c as u32)) => s.push((0xD800 + c as u32 - b) as u16),
                    _ => push_cp(s, c as u32),
                }
            }
        }
    }

    fn digits(&mut self, radix: u32, out: &mut String, allow_sep: bool) -> Result<(), LexError> {
        let mut last_sep = true;
        let mut any = false;
        loop {
            match self.peek() {
                Some('_') if allow_sep => {
                    if last_sep || !self.peek_at(1).is_some_and(|c| c.is_digit(radix)) {
                        return self.err("bad numeric separator");
                    }
                    self.pos += 1;
                    last_sep = true;
                }
                Some(c) if c.is_digit(radix) => {
                    out.push(c);
                    self.pos += 1;
                    last_sep = false;
                    any = true;
                }
                _ => break,
            }
        }
        if !any && radix != 10 {
            return self.err("missing digits");
        }
        Ok(())
    }

    fn number(&mut self) -> Result<(TokKind, bool), LexError> {
        let c = self.peek().unwrap_or('0');
        let radix = match (c, self.peek_at(1).map(|c| c.to_ascii_lowercase())) {
            ('0', Some('x')) => 16,
            ('0', Some('o')) => 8,
            ('0', Some('b')) => 2,
            _ => 10,
        };
        let mut legacy = false;
        let kind = if radix != 10 {
            self.pos += 2;
            let mut d = String::new();
            self.digits(radix, &mut d, true)?;
            if self.peek() == Some('n') {
                self.pos += 1;
                TokKind::BigInt(parse_radix_big(&d, radix))
            } else {
                TokKind::Num(parse_radix(&d, radix))
            }
        } else if c == '0' && self.peek_at(1).is_some_and(|d| d.is_ascii_digit()) {
            // Legacy octal (0777) or a decimal with a leading zero (089); no separators.
            legacy = true;
            let mut d = String::new();
            while let Some(c) = self.peek().filter(|c| c.is_ascii_digit()) {
                d.push(c);
                self.pos += 1;
            }
            if d.chars().all(|c| c < '8') {
                TokKind::Num(parse_radix(&d, 8))
            } else {
                // NonOctalDecimalIntegerLiteral, may continue with a fraction/exponent.
                let mut s = d;
                self.fraction_exponent(&mut s)?;
                TokKind::Num(s.parse().unwrap_or(f64::NAN))
            }
        } else {
            if c == '0' && self.peek_at(1) == Some('_') {
                return self.err("numeric separator after a leading 0");
            }
            let mut s = String::new();
            self.digits(10, &mut s, true)?;
            if self.peek() == Some('n') {
                if s.len() > 1 && s.starts_with('0') {
                    return self.err("bad BigInt");
                }
                self.pos += 1;
                if s.is_empty() {
                    return self.err("bad BigInt");
                }
                let v = s.trim_start_matches('0');
                TokKind::BigInt(if v.is_empty() { "0".into() } else { v.into() })
            } else {
                self.fraction_exponent(&mut s)?;
                TokKind::Num(s.parse().unwrap_or(f64::NAN))
            }
        };
        // §12.9.3: an IdentifierStart or digit can't follow a numeric literal.
        if self.peek().is_some_and(|c| is_id_start(c) || c.is_ascii_digit() || c == '\\') {
            return self.err("identifier directly after number");
        }
        Ok((kind, legacy))
    }

    fn fraction_exponent(&mut self, s: &mut String) -> Result<(), LexError> {
        if self.peek() == Some('.') {
            self.pos += 1;
            s.push('.');
            if self.peek() == Some('_') {
                return self.err("bad numeric separator");
            }
            self.digits(10, s, true)?;
        }
        if s.is_empty() || s == "." {
            return self.err("bad number");
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            self.pos += 1;
            s.push('e');
            if let Some(sign @ ('+' | '-')) = self.peek() {
                s.push(sign);
                self.pos += 1;
            }
            let before = s.len();
            self.digits(10, s, true)?;
            if s.len() == before {
                return self.err("missing exponent");
            }
        }
        if s.starts_with('.') {
            s.insert(0, '0');
        }
        Ok(())
    }

    fn regex(&mut self) -> Result<TokKind, LexError> {
        self.pos += 1;
        let mut body = String::new();
        let mut class = false;
        loop {
            let Some(c) = self.peek() else { return self.err("unterminated regular expression") };
            if is_line_terminator(c) {
                return self.err("line break in regular expression");
            }
            self.pos += 1;
            match c {
                '\\' => {
                    body.push(c);
                    match self.peek() {
                        Some(n) if !is_line_terminator(n) => {
                            body.push(n);
                            self.pos += 1;
                        }
                        _ => return self.err("bad escape in regular expression"),
                    }
                    continue;
                }
                '[' => class = true,
                ']' => class = false,
                '/' if !class => break,
                _ => {}
            }
            body.push(c);
        }
        let mut flags = String::new();
        while let Some(c) = self.peek().filter(|&c| is_id_continue(c) || c == '\\') {
            if c == '\\' {
                return self.err("escape in regular expression flags");
            }
            flags.push(c);
            self.pos += 1;
        }
        Ok(TokKind::Regex { body, flags })
    }
}

enum Escape {
    Cp(u32),
    None,
    LegacyOctal(u32),
}

/// Integer digits in a radix to the nearest f64.
fn parse_radix(d: &str, radix: u32) -> f64 {
    let mut v: f64 = 0.0;
    // Exact while below 2^53; u128 keeps more digits exact before the final rounding.
    if let Ok(n) = u128::from_str_radix(d, radix) {
        return n as f64;
    }
    for c in d.chars() {
        v = v * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0));
    }
    v
}

/// Integer digits in a radix as a decimal string (BigInt literals).
fn parse_radix_big(d: &str, radix: u32) -> String {
    // Schoolbook conversion into base 1e9 limbs.
    let mut limbs: Vec<u64> = vec![0];
    for c in d.chars() {
        let mut carry = u64::from(c.to_digit(radix).unwrap_or(0));
        for l in limbs.iter_mut() {
            let v = *l * u64::from(radix) + carry;
            *l = v % 1_000_000_000;
            carry = v / 1_000_000_000;
        }
        while carry > 0 {
            limbs.push(carry % 1_000_000_000);
            carry /= 1_000_000_000;
        }
    }
    let mut s = limbs.last().copied().unwrap_or(0).to_string();
    for l in limbs.iter().rev().skip(1) {
        s.push_str(&format!("{l:09}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<TokKind> {
        let mut l = Lexer::new(src);
        let mut out = Vec::new();
        loop {
            let t = l.next(false, true).unwrap();
            if t.kind == TokKind::Eof {
                return out;
            }
            out.push(t.kind);
        }
    }

    fn s(x: &str) -> JsStr {
        x.encode_utf16().collect()
    }

    #[test]
    fn numbers() {
        assert_eq!(
            toks("0x1F 0o17 0b101 1_000 .5 1e3 1.5E-2 017 089 10n 0x10n"),
            vec![
                TokKind::Num(31.0),
                TokKind::Num(15.0),
                TokKind::Num(5.0),
                TokKind::Num(1000.0),
                TokKind::Num(0.5),
                TokKind::Num(1000.0),
                TokKind::Num(0.015),
                TokKind::Num(15.0),
                TokKind::Num(89.0),
                TokKind::BigInt("10".into()),
                TokKind::BigInt("16".into()),
            ]
        );
        for bad in ["1__0", "1_", "0x", "3in", "1e", "0_1", "01n"] {
            assert!(Lexer::new(bad).next(false, true).is_err(), "{bad}");
        }
    }

    #[test]
    fn strings_and_templates() {
        assert_eq!(
            toks(r#"'a\n\x41B\u{1F600}\101' "\uD800""#),
            vec![
                TokKind::Str({
                    let mut v = s("a\nAB😀A");
                    v.truncate(v.len());
                    v
                }),
                TokKind::Str(vec![0xd800]),
            ]
        );
        let t = toks("`a${");
        assert_eq!(t[0], TokKind::Template { cooked: Some(s("a")), raw: "a".into(), tail: false });
        let t = toks("`\\unicode`");
        assert_eq!(t[0], TokKind::Template { cooked: None, raw: "\\unicode".into(), tail: true });
    }

    #[test]
    fn punctuators_comments_regex() {
        assert_eq!(
            toks("a >>>= b /* x */ ?. // y\n c?.5:d"),
            vec![
                TokKind::Ident { name: "a".into(), escaped: false },
                TokKind::Punct(">>>="),
                TokKind::Ident { name: "b".into(), escaped: false },
                TokKind::Punct("?."),
                TokKind::Ident { name: "c".into(), escaped: false },
                TokKind::Punct("?"),
                TokKind::Num(0.5),
                TokKind::Punct(":"),
                TokKind::Ident { name: "d".into(), escaped: false },
            ]
        );
        let mut l = Lexer::new("/a[/]b\\/c/gi");
        assert_eq!(
            l.next(true, true).unwrap().kind,
            TokKind::Regex { body: "a[/]b\\/c".into(), flags: "gi".into() }
        );
        let mut l = Lexer::new("\\u0061bc");
        assert_eq!(l.next(false, true).unwrap().kind, TokKind::Ident { name: "abc".into(), escaped: true });
    }
}

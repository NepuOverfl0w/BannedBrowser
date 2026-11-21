//! CSS Syntax Level 3 §4 tokenizer.

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Ident(String),
    Function(String),
    AtKeyword(String),
    /// `#name`; `true` if it is a valid identifier ("id" type flag).
    Hash(String, bool),
    String(String),
    BadString,
    Url(String),
    BadUrl,
    Delim(char),
    Number {
        value: f32,
        int: bool,
    },
    Percentage(f32),
    Dimension {
        value: f32,
        unit: String,
    },
    Whitespace,
    Cdo,
    Cdc,
    Colon,
    Semicolon,
    Comma,
    LeftSquare,
    RightSquare,
    LeftParen,
    RightParen,
    LeftCurly,
    RightCurly,
}

pub struct Tokenizer {
    chars: Vec<char>,
    pos: usize,
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c as u32 >= 0x80
}

fn is_name(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == '-'
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n')
}

impl Tokenizer {
    /// Preprocess (CSS Syntax §3.3) and create a tokenizer.
    pub fn new(input: &str) -> Self {
        let mut chars = Vec::with_capacity(input.len());
        let mut it = input.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '\r' => {
                    if it.peek() == Some(&'\n') {
                        it.next();
                    }
                    chars.push('\n');
                }
                '\x0C' => chars.push('\n'),
                '\0' => chars.push('\u{FFFD}'),
                c => chars.push(c),
            }
        }
        Tokenizer { chars, pos: 0 }
    }

    fn peek(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).copied()
    }

    fn valid_escape(a: Option<char>, b: Option<char>) -> bool {
        a == Some('\\') && b.is_some_and(|c| c != '\n')
    }

    fn starts_ident(a: Option<char>, b: Option<char>, c: Option<char>) -> bool {
        match a {
            Some('-') => b.is_some_and(|b| is_name_start(b) || b == '-') || Self::valid_escape(b, c),
            Some(x) if is_name_start(x) => true,
            Some('\\') => Self::valid_escape(a, b),
            _ => false,
        }
    }

    fn starts_number(a: Option<char>, b: Option<char>, c: Option<char>) -> bool {
        match a {
            Some('+') | Some('-') => {
                b.is_some_and(|b| b.is_ascii_digit())
                    || (b == Some('.') && c.is_some_and(|c| c.is_ascii_digit()))
            }
            Some('.') => b.is_some_and(|b| b.is_ascii_digit()),
            Some(x) => x.is_ascii_digit(),
            None => false,
        }
    }

    /// Tokenize everything (whitespace and comments collapse as per spec).
    pub fn tokenize(mut self) -> Vec<Token> {
        let mut out = Vec::new();
        while let Some(t) = self.next_token() {
            out.push(t);
        }
        out
    }

    fn consume_comments(&mut self) {
        while self.peek(0) == Some('/') && self.peek(1) == Some('*') {
            self.pos += 2;
            loop {
                match self.peek(0) {
                    None => return,
                    Some('*') if self.peek(1) == Some('/') => {
                        self.pos += 2;
                        break;
                    }
                    _ => self.pos += 1,
                }
            }
        }
    }

    // CSS Syntax §4.3.1 consume a token
    pub fn next_token(&mut self) -> Option<Token> {
        self.consume_comments();
        let c = self.peek(0)?;
        self.pos += 1;
        Some(match c {
            c if is_ws(c) => {
                while self.peek(0).is_some_and(is_ws) {
                    self.pos += 1;
                }
                Token::Whitespace
            }
            '"' | '\'' => self.consume_string(c),
            '#' => {
                if self.peek(0).is_some_and(is_name) || Self::valid_escape(self.peek(0), self.peek(1)) {
                    let is_id = Self::starts_ident(self.peek(0), self.peek(1), self.peek(2));
                    Token::Hash(self.consume_name(), is_id)
                } else {
                    Token::Delim('#')
                }
            }
            '(' => Token::LeftParen,
            ')' => Token::RightParen,
            '+' | '.' => {
                if Self::starts_number(Some(c), self.peek(0), self.peek(1)) {
                    self.pos -= 1;
                    self.consume_numeric()
                } else {
                    Token::Delim(c)
                }
            }
            ',' => Token::Comma,
            '-' => {
                if Self::starts_number(Some(c), self.peek(0), self.peek(1)) {
                    self.pos -= 1;
                    self.consume_numeric()
                } else if self.peek(0) == Some('-') && self.peek(1) == Some('>') {
                    self.pos += 2;
                    Token::Cdc
                } else if Self::starts_ident(Some(c), self.peek(0), self.peek(1)) {
                    self.pos -= 1;
                    self.consume_ident_like()
                } else {
                    Token::Delim('-')
                }
            }
            ':' => Token::Colon,
            ';' => Token::Semicolon,
            '<' => {
                if self.peek(0) == Some('!') && self.peek(1) == Some('-') && self.peek(2) == Some('-') {
                    self.pos += 3;
                    Token::Cdo
                } else {
                    Token::Delim('<')
                }
            }
            '@' => {
                if Self::starts_ident(self.peek(0), self.peek(1), self.peek(2)) {
                    Token::AtKeyword(self.consume_name())
                } else {
                    Token::Delim('@')
                }
            }
            '[' => Token::LeftSquare,
            '\\' => {
                if Self::valid_escape(Some('\\'), self.peek(0)) {
                    self.pos -= 1;
                    self.consume_ident_like()
                } else {
                    Token::Delim('\\')
                }
            }
            ']' => Token::RightSquare,
            '{' => Token::LeftCurly,
            '}' => Token::RightCurly,
            c if c.is_ascii_digit() => {
                self.pos -= 1;
                self.consume_numeric()
            }
            c if is_name_start(c) => {
                self.pos -= 1;
                self.consume_ident_like()
            }
            c => Token::Delim(c),
        })
    }

    // §4.3.7 consume an escaped code point (after the backslash)
    fn consume_escape(&mut self) -> char {
        let Some(c) = self.peek(0) else { return '\u{FFFD}' };
        self.pos += 1;
        if c.is_ascii_hexdigit() {
            let mut v = c.to_digit(16).unwrap_or(0);
            let mut n = 1;
            while n < 6 {
                match self.peek(0).and_then(|c| c.to_digit(16)) {
                    Some(d) => {
                        v = v * 16 + d;
                        self.pos += 1;
                        n += 1;
                    }
                    None => break,
                }
            }
            if self.peek(0).is_some_and(is_ws) {
                self.pos += 1;
            }
            if v == 0 || (0xD800..=0xDFFF).contains(&v) || v > 0x10FFFF {
                return '\u{FFFD}';
            }
            return char::from_u32(v).unwrap_or('\u{FFFD}');
        }
        c
    }

    // §4.3.11 consume an ident sequence
    fn consume_name(&mut self) -> String {
        let mut s = String::new();
        loop {
            match self.peek(0) {
                Some(c) if is_name(c) => {
                    s.push(c);
                    self.pos += 1;
                }
                Some('\\') if Self::valid_escape(Some('\\'), self.peek(1)) => {
                    self.pos += 1;
                    s.push(self.consume_escape());
                }
                _ => return s,
            }
        }
    }

    // §4.3.12 consume a number
    fn consume_number(&mut self) -> (f32, bool) {
        let start = self.pos;
        let mut int = true;
        if matches!(self.peek(0), Some('+') | Some('-')) {
            self.pos += 1;
        }
        while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek(0) == Some('.') && self.peek(1).is_some_and(|c| c.is_ascii_digit()) {
            int = false;
            self.pos += 1;
            while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(0), Some('e') | Some('E')) {
            let has_sign = matches!(self.peek(1), Some('+') | Some('-'));
            let d = if has_sign { self.peek(2) } else { self.peek(1) };
            if d.is_some_and(|c| c.is_ascii_digit()) {
                int = false;
                self.pos += if has_sign { 2 } else { 1 };
                while self.peek(0).is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
        }
        let s: String = self.chars[start..self.pos].iter().collect();
        (s.parse::<f32>().unwrap_or(0.0), int)
    }

    // §4.3.3 consume a numeric token
    fn consume_numeric(&mut self) -> Token {
        let (value, int) = self.consume_number();
        if Self::starts_ident(self.peek(0), self.peek(1), self.peek(2)) {
            let unit = self.consume_name();
            Token::Dimension { value, unit }
        } else if self.peek(0) == Some('%') {
            self.pos += 1;
            Token::Percentage(value)
        } else {
            Token::Number { value, int }
        }
    }

    // §4.3.4 consume an ident-like token
    fn consume_ident_like(&mut self) -> Token {
        let name = self.consume_name();
        if name.eq_ignore_ascii_case("url") && self.peek(0) == Some('(') {
            self.pos += 1;
            while self.peek(0).is_some_and(is_ws) && self.peek(1).is_some_and(is_ws) {
                self.pos += 1;
            }
            let (a, b) = (self.peek(0), self.peek(1));
            let quote = |c: Option<char>| matches!(c, Some('"') | Some('\''));
            if quote(a) || (a.is_some_and(is_ws) && quote(b)) {
                return Token::Function(name);
            }
            return self.consume_url();
        }
        if self.peek(0) == Some('(') {
            self.pos += 1;
            return Token::Function(name);
        }
        Token::Ident(name)
    }

    // §4.3.5 consume a string token
    fn consume_string(&mut self, end: char) -> Token {
        let mut s = String::new();
        loop {
            match self.peek(0) {
                None => return Token::String(s),
                Some(c) if c == end => {
                    self.pos += 1;
                    return Token::String(s);
                }
                Some('\n') => return Token::BadString,
                Some('\\') => {
                    self.pos += 1;
                    match self.peek(0) {
                        None => {}
                        Some('\n') => self.pos += 1,
                        Some(_) => s.push(self.consume_escape()),
                    }
                }
                Some(c) => {
                    s.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    // §4.3.6 consume a url token
    fn consume_url(&mut self) -> Token {
        let mut s = String::new();
        while self.peek(0).is_some_and(is_ws) {
            self.pos += 1;
        }
        loop {
            match self.peek(0) {
                None => return Token::Url(s),
                Some(')') => {
                    self.pos += 1;
                    return Token::Url(s);
                }
                Some(c) if is_ws(c) => {
                    while self.peek(0).is_some_and(is_ws) {
                        self.pos += 1;
                    }
                    match self.peek(0) {
                        None => return Token::Url(s),
                        Some(')') => {
                            self.pos += 1;
                            return Token::Url(s);
                        }
                        _ => {
                            self.consume_bad_url();
                            return Token::BadUrl;
                        }
                    }
                }
                Some('"') | Some('\'') | Some('(') => {
                    self.consume_bad_url();
                    return Token::BadUrl;
                }
                Some(c) if (c as u32) < 0x20 && c != '\t' || c == '\u{7F}' => {
                    self.consume_bad_url();
                    return Token::BadUrl;
                }
                Some('\\') => {
                    if Self::valid_escape(Some('\\'), self.peek(1)) {
                        self.pos += 1;
                        s.push(self.consume_escape());
                    } else {
                        self.consume_bad_url();
                        return Token::BadUrl;
                    }
                }
                Some(c) => {
                    s.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn consume_bad_url(&mut self) {
        loop {
            match self.peek(0) {
                None => return,
                Some(')') => {
                    self.pos += 1;
                    return;
                }
                Some('\\') if Self::valid_escape(Some('\\'), self.peek(1)) => {
                    self.pos += 1;
                    self.consume_escape();
                }
                _ => self.pos += 1,
            }
        }
    }
}

/// Serialize tokens back to CSS text (used for custom properties and debug dumps).
pub fn serialize_tokens(tokens: &[Token]) -> String {
    let mut s = String::new();
    for t in tokens {
        match t {
            Token::Ident(i) => s.push_str(i),
            Token::Function(f) => {
                s.push_str(f);
                s.push('(');
            }
            Token::AtKeyword(a) => {
                s.push('@');
                s.push_str(a);
            }
            Token::Hash(h, _) => {
                s.push('#');
                s.push_str(h);
            }
            Token::String(st) => {
                s.push('"');
                s.push_str(&st.replace('\\', "\\\\").replace('"', "\\\""));
                s.push('"');
            }
            Token::BadString => s.push_str("\"\n"),
            Token::Url(u) => {
                s.push_str("url(");
                s.push_str(u);
                s.push(')');
            }
            Token::BadUrl => s.push_str("url()"),
            Token::Delim(c) => s.push(*c),
            Token::Number { value, .. } => s.push_str(&fmt_num(*value)),
            Token::Percentage(v) => {
                s.push_str(&fmt_num(*v));
                s.push('%');
            }
            Token::Dimension { value, unit } => {
                s.push_str(&fmt_num(*value));
                s.push_str(unit);
            }
            Token::Whitespace => s.push(' '),
            Token::Cdo => s.push_str("<!--"),
            Token::Cdc => s.push_str("-->"),
            Token::Colon => s.push(':'),
            Token::Semicolon => s.push(';'),
            Token::Comma => s.push(','),
            Token::LeftSquare => s.push('['),
            Token::RightSquare => s.push(']'),
            Token::LeftParen => s.push('('),
            Token::RightParen => s.push(')'),
            Token::LeftCurly => s.push('{'),
            Token::RightCurly => s.push('}'),
        }
    }
    s
}

pub(crate) fn fmt_num(v: f32) -> String {
    if v.fract() == 0.0 && v.abs() < 1e9 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Token> {
        Tokenizer::new(s).tokenize()
    }

    #[test]
    fn basics() {
        assert_eq!(
            toks("a{color:#fff;width:10.5px;margin:-2em 50%}"),
            vec![
                Token::Ident("a".into()),
                Token::LeftCurly,
                Token::Ident("color".into()),
                Token::Colon,
                Token::Hash("fff".into(), true),
                Token::Semicolon,
                Token::Ident("width".into()),
                Token::Colon,
                Token::Dimension { value: 10.5, unit: "px".into() },
                Token::Semicolon,
                Token::Ident("margin".into()),
                Token::Colon,
                Token::Dimension { value: -2.0, unit: "em".into() },
                Token::Whitespace,
                Token::Percentage(50.0),
                Token::RightCurly,
            ]
        );
    }

    #[test]
    fn urls_strings_escapes() {
        assert_eq!(toks("url( a.png )"), vec![Token::Url("a.png".into())]);
        assert_eq!(toks("url('a.png')")[0], Token::Function("url".into()));
        assert_eq!(toks("'a\\'b'"), vec![Token::String("a'b".into())]);
        assert_eq!(toks("\\31 0"), vec![Token::Ident("10".into())]);
        assert_eq!(toks("/* c */x"), vec![Token::Ident("x".into())]);
        assert_eq!(toks("--my-var"), vec![Token::Ident("--my-var".into())]);
        assert_eq!(toks("1e3"), vec![Token::Number { value: 1000.0, int: false }]);
    }
}

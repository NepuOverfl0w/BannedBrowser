//! CSS Syntax Level 3 §5 parsing: component values, rules, declarations,
//! and BannedBrowser's stylesheet object model.

use crate::media::MediaQueryList;
use crate::selectors::{parse_selector_list, SelectorList};
use crate::tokenizer::{Token, Tokenizer};

/// A component value (CSS Syntax §5.?): a preserved token, a function or a block.
#[derive(Debug, Clone, PartialEq)]
pub enum Cv {
    Token(Token),
    Function(String, Vec<Cv>),
    /// A simple block; `char` is the opening bracket (`[`, `(`, `{`).
    Block(char, Vec<Cv>),
}

impl Cv {
    pub fn is_ws(&self) -> bool {
        matches!(self, Cv::Token(Token::Whitespace))
    }
    pub fn ident(&self) -> Option<&str> {
        match self {
            Cv::Token(Token::Ident(s)) => Some(s),
            _ => None,
        }
    }
    pub fn is_ident(&self, s: &str) -> bool {
        self.ident().is_some_and(|i| i.eq_ignore_ascii_case(s))
    }
    pub fn is_comma(&self) -> bool {
        matches!(self, Cv::Token(Token::Comma))
    }
    pub fn is_delim(&self, c: char) -> bool {
        matches!(self, Cv::Token(Token::Delim(d)) if *d == c)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    /// Lowercased property name (custom properties keep their case).
    pub name: String,
    pub value: Vec<Cv>,
    pub important: bool,
}

#[derive(Debug, Clone)]
pub struct StyleRule {
    pub selectors: SelectorList,
    pub declarations: Vec<Declaration>,
}

#[derive(Debug, Clone)]
pub enum Rule {
    Style(StyleRule),
    Media {
        query: MediaQueryList,
        rules: Vec<Rule>,
    },
    Supports {
        supported: bool,
        rules: Vec<Rule>,
    },
    Import {
        url: String,
        media: MediaQueryList,
    },
    FontFace(Vec<Declaration>),
    /// `@layer` blocks are flattened (layer ordering: TODO(css-cascade-5 §6.4)).
    Layer(Vec<Rule>),
}

#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

impl Stylesheet {
    pub fn parse(css: &str) -> Stylesheet {
        let tokens = Tokenizer::new(css).tokenize();
        let mut p = Parser { tokens, pos: 0 };
        let rules = p.consume_rule_list(true);
        Stylesheet { rules: convert_rules(rules, 0) }
    }

    /// Number of style rules (recursively), for diagnostics.
    pub fn style_rule_count(&self) -> usize {
        fn count(r: &[Rule]) -> usize {
            r.iter()
                .map(|r| match r {
                    Rule::Style(_) => 1,
                    Rule::Media { rules, .. } | Rule::Supports { rules, .. } | Rule::Layer(rules) => {
                        count(rules)
                    }
                    _ => 0,
                })
                .sum()
        }
        count(&self.rules)
    }
}

/// Parse a declaration list, e.g. a `style=""` attribute.
/// Serialize component values back to CSS text (CSSOM §6.7.2 "serialize a CSS value",
/// for declared values as written), trimmed.
pub fn serialize_cvs(cvs: &[Cv]) -> String {
    fn go(cvs: &[Cv], out: &mut String) {
        for cv in cvs {
            match cv {
                Cv::Token(t) => out.push_str(&crate::tokenizer::serialize_tokens(std::slice::from_ref(t))),
                Cv::Function(name, args) => {
                    out.push_str(name);
                    out.push('(');
                    go(args, out);
                    out.push(')');
                }
                Cv::Block(open, inner) => {
                    out.push(*open);
                    go(inner, out);
                    out.push(match open {
                        '[' => ']',
                        '(' => ')',
                        _ => '}',
                    });
                }
            }
        }
    }
    let mut s = String::new();
    go(cvs, &mut s);
    s.trim().to_string()
}

pub fn parse_declaration_list(css: &str) -> Vec<Declaration> {
    let tokens = Tokenizer::new(css).tokenize();
    let mut p = Parser { tokens, pos: 0 };
    let cvs = p.consume_all_component_values();
    parse_declarations_from(&cvs)
}

/// Parse a list of component values from a string (for property values).
pub fn parse_component_values(css: &str) -> Vec<Cv> {
    let tokens = Tokenizer::new(css).tokenize();
    let mut p = Parser { tokens, pos: 0 };
    p.consume_all_component_values()
}

enum RawRule {
    Qualified { prelude: Vec<Cv>, block: Vec<Cv> },
    At { name: String, prelude: Vec<Cv>, block: Option<Vec<Cv>> },
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    // §5.4.1 consume a list of rules
    fn consume_rule_list(&mut self, top_level: bool) -> Vec<RawRule> {
        let mut rules = Vec::new();
        while let Some(t) = self.peek().cloned() {
            match t {
                Token::Whitespace => {
                    self.pos += 1;
                }
                Token::Cdo | Token::Cdc if top_level => {
                    self.pos += 1;
                }
                Token::AtKeyword(_) => {
                    if let Some(r) = self.consume_at_rule() {
                        rules.push(r);
                    }
                }
                _ => {
                    if let Some(r) = self.consume_qualified_rule() {
                        rules.push(r);
                    }
                }
            }
        }
        rules
    }

    // §5.4.2 consume an at-rule
    fn consume_at_rule(&mut self) -> Option<RawRule> {
        let name = match self.next()? {
            Token::AtKeyword(n) => n.to_ascii_lowercase(),
            _ => return None,
        };
        let mut prelude = Vec::new();
        loop {
            match self.peek() {
                None => return Some(RawRule::At { name, prelude, block: None }),
                Some(Token::Semicolon) => {
                    self.pos += 1;
                    return Some(RawRule::At { name, prelude, block: None });
                }
                Some(Token::LeftCurly) => {
                    self.pos += 1;
                    let block = self.consume_block_contents('{');
                    return Some(RawRule::At { name, prelude, block: Some(block) });
                }
                _ => prelude.push(self.consume_component_value()?),
            }
        }
    }

    // §5.4.3 consume a qualified rule
    fn consume_qualified_rule(&mut self) -> Option<RawRule> {
        let mut prelude = Vec::new();
        loop {
            match self.peek() {
                None => return None,
                Some(Token::LeftCurly) => {
                    self.pos += 1;
                    let block = self.consume_block_contents('{');
                    return Some(RawRule::Qualified { prelude, block });
                }
                _ => prelude.push(self.consume_component_value()?),
            }
        }
    }

    fn consume_block_contents(&mut self, open: char) -> Vec<Cv> {
        let close = match open {
            '{' => Token::RightCurly,
            '[' => Token::RightSquare,
            _ => Token::RightParen,
        };
        let mut v = Vec::new();
        loop {
            match self.peek() {
                None => return v,
                Some(t) if *t == close => {
                    self.pos += 1;
                    return v;
                }
                _ => match self.consume_component_value() {
                    Some(cv) => v.push(cv),
                    None => return v,
                },
            }
        }
    }

    // §5.4.7 consume a component value
    fn consume_component_value(&mut self) -> Option<Cv> {
        let t = self.next()?;
        Some(match t {
            Token::LeftCurly => Cv::Block('{', self.consume_block_contents('{')),
            Token::LeftSquare => Cv::Block('[', self.consume_block_contents('[')),
            Token::LeftParen => Cv::Block('(', self.consume_block_contents('(')),
            Token::Function(name) => {
                Cv::Function(name.to_ascii_lowercase(), self.consume_block_contents('('))
            }
            t => Cv::Token(t),
        })
    }

    fn consume_all_component_values(&mut self) -> Vec<Cv> {
        let mut v = Vec::new();
        while let Some(cv) = self.consume_component_value() {
            v.push(cv);
        }
        v
    }
}

/// CSS Syntax §5.4.5 "consume a list of declarations" over component values.
pub fn parse_declarations_from(cvs: &[Cv]) -> Vec<Declaration> {
    let mut out = Vec::new();
    for part in cvs.split(|cv| matches!(cv, Cv::Token(Token::Semicolon))) {
        let mut it = part.iter().skip_while(|c| c.is_ws());
        let Some(Cv::Token(Token::Ident(name))) = it.next() else {
            continue; // parse error: skip to the next ';'
        };
        let rest: Vec<&Cv> = it.skip_while(|c| c.is_ws()).collect();
        if !matches!(rest.first(), Some(Cv::Token(Token::Colon))) {
            continue;
        }
        let mut value: Vec<Cv> = rest[1..].iter().map(|c| (*c).clone()).collect();
        // Trim whitespace, detect !important.
        while value.last().is_some_and(|c| c.is_ws()) {
            value.pop();
        }
        let mut important = false;
        let n = value.len();
        if n >= 2 && value[n - 1].is_ident("important") {
            let mut k = n - 2;
            while k > 0 && value[k].is_ws() {
                k -= 1;
            }
            if value[k].is_delim('!') {
                important = true;
                value.truncate(k);
            }
        }
        while value.last().is_some_and(|c| c.is_ws()) {
            value.pop();
        }
        while value.first().is_some_and(|c| c.is_ws()) {
            value.remove(0);
        }
        let name = if name.starts_with("--") { name.clone() } else { name.to_ascii_lowercase() };
        out.push(Declaration { name, value, important });
    }
    out
}

fn prelude_text(prelude: &[Cv]) -> String {
    let mut toks = Vec::new();
    flatten(prelude, &mut toks);
    crate::tokenizer::serialize_tokens(&toks)
}

/// Flatten component values back into tokens.
pub fn flatten(cvs: &[Cv], out: &mut Vec<Token>) {
    for cv in cvs {
        match cv {
            Cv::Token(t) => out.push(t.clone()),
            Cv::Function(name, args) => {
                out.push(Token::Function(name.clone()));
                flatten(args, out);
                out.push(Token::RightParen);
            }
            Cv::Block(open, inner) => {
                let (o, c) = match open {
                    '{' => (Token::LeftCurly, Token::RightCurly),
                    '[' => (Token::LeftSquare, Token::RightSquare),
                    _ => (Token::LeftParen, Token::RightParen),
                };
                out.push(o);
                flatten(inner, out);
                out.push(c);
            }
        }
    }
}

/// Parse the contents of a `{}` block that may contain nested rules (for
/// `@media` etc.): re-tokenize as a rule list.
fn nested_rules(block: &[Cv], depth: usize) -> Vec<Rule> {
    let mut toks = Vec::new();
    flatten(block, &mut toks);
    let mut p = Parser { tokens: toks, pos: 0 };
    convert_rules(p.consume_rule_list(false), depth + 1)
}

fn convert_rules(raw: Vec<RawRule>, depth: usize) -> Vec<Rule> {
    if depth > 32 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for r in raw {
        match r {
            RawRule::Qualified { prelude, block } => {
                let Some(selectors) = parse_selector_list(&prelude) else {
                    log::debug!("dropping rule with invalid selector {:?}", prelude_text(&prelude));
                    continue;
                };
                // Nested rules inside style blocks (CSS Nesting) are not supported yet:
                // declarations are taken from the top-level of the block only.
                out.push(Rule::Style(StyleRule { selectors, declarations: parse_declarations_from(&block) }));
            }
            RawRule::At { name, prelude, block } => match name.as_str() {
                "media" => {
                    if let Some(block) = block {
                        out.push(Rule::Media {
                            query: MediaQueryList::parse(&prelude),
                            rules: nested_rules(&block, depth),
                        });
                    }
                }
                "supports" => {
                    if let Some(block) = block {
                        let supported = crate::supports::evaluate(&prelude);
                        out.push(Rule::Supports { supported, rules: nested_rules(&block, depth) });
                    }
                }
                "layer" => {
                    if let Some(block) = block {
                        out.push(Rule::Layer(nested_rules(&block, depth)));
                    }
                }
                "import" => {
                    let mut it = prelude.iter().filter(|c| !c.is_ws());
                    let url = match it.next() {
                        Some(Cv::Token(Token::String(s))) | Some(Cv::Token(Token::Url(s))) => Some(s.clone()),
                        Some(Cv::Function(f, args)) if f == "url" => args.iter().find_map(|a| match a {
                            Cv::Token(Token::String(s)) => Some(s.clone()),
                            _ => None,
                        }),
                        _ => None,
                    };
                    if let Some(url) = url {
                        let rest: Vec<Cv> = prelude
                            .iter()
                            .skip_while(|c| c.is_ws())
                            .skip(1)
                            .filter(|c| !matches!(c, Cv::Function(f, _) if f == "layer" || f == "supports"))
                            .filter(|c| !c.is_ident("layer"))
                            .cloned()
                            .collect();
                        out.push(Rule::Import { url, media: MediaQueryList::parse(&rest) });
                    }
                }
                "font-face" => {
                    if let Some(block) = block {
                        out.push(Rule::FontFace(parse_declarations_from(&block)));
                    }
                }
                _ => {} // @charset, @namespace, @keyframes, @page…: ignored for now
            },
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rules() {
        let ss = Stylesheet::parse(
            "@charset 'utf-8'; body { color: red !important; margin: 0 } \
             @media (max-width: 600px) { p { x: y } } bad { a: b ; ; c } .x{}",
        );
        assert_eq!(ss.style_rule_count(), 4);
        let Rule::Style(r) = &ss.rules[0] else { panic!() };
        assert_eq!(r.declarations.len(), 2);
        assert!(r.declarations[0].important);
        assert_eq!(r.declarations[0].name, "color");
    }

    #[test]
    fn style_attribute() {
        let d = parse_declaration_list("COLOR: blue; width: 10px !IMPORTANT; --Var: { x }; junk");
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].name, "color");
        assert!(d[1].important);
        assert_eq!(d[2].name, "--Var");
    }

    #[test]
    fn invalid_selector_dropped() {
        let ss = Stylesheet::parse("a:::b { color: red } p { color: blue }");
        assert_eq!(ss.style_rule_count(), 1);
    }

    #[test]
    fn imports() {
        let ss = Stylesheet::parse("@import url(a.css) screen; @import 'b.css';");
        assert!(matches!(&ss.rules[0], Rule::Import { url, .. } if url == "a.css"));
        assert!(matches!(&ss.rules[1], Rule::Import { url, .. } if url == "b.css"));
    }
}

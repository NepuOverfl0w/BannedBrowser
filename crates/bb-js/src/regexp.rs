//! Regular expression pattern syntax (ECMA-262 §22.2.1) and its early errors, in the three
//! modes: Annex B (no `u`/`v` flag: lenient legacy syntax), `u` (Unicode) and `v` (Unicode sets:
//! class set operations, nested classes, `\q{…}`). Matching comes with the interpreter.

use crate::regexp_props::{GENERAL_CATEGORY, SCRIPT};

/// ECMA-262 Table 67: binary Unicode properties (name and alias).
const BINARY: &[&str] = &[
    "ASCII",
    "ASCII_Hex_Digit",
    "AHex",
    "Alphabetic",
    "Alpha",
    "Any",
    "Assigned",
    "Bidi_Control",
    "Bidi_C",
    "Bidi_Mirrored",
    "Bidi_M",
    "Case_Ignorable",
    "CI",
    "Cased",
    "Changes_When_Casefolded",
    "CWCF",
    "Changes_When_Casemapped",
    "CWCM",
    "Changes_When_Lowercased",
    "CWL",
    "Changes_When_NFKC_Casefolded",
    "CWKCF",
    "Changes_When_Titlecased",
    "CWT",
    "Changes_When_Uppercased",
    "CWU",
    "Dash",
    "Default_Ignorable_Code_Point",
    "DI",
    "Deprecated",
    "Dep",
    "Diacritic",
    "Dia",
    "Emoji",
    "Emoji_Component",
    "EComp",
    "Emoji_Modifier",
    "EMod",
    "Emoji_Modifier_Base",
    "EBase",
    "Emoji_Presentation",
    "EPres",
    "Extended_Pictographic",
    "ExtPict",
    "Extender",
    "Ext",
    "Grapheme_Base",
    "Gr_Base",
    "Grapheme_Extend",
    "Gr_Ext",
    "Hex_Digit",
    "Hex",
    "IDS_Binary_Operator",
    "IDSB",
    "IDS_Trinary_Operator",
    "IDST",
    "ID_Continue",
    "IDC",
    "ID_Start",
    "IDS",
    "Ideographic",
    "Ideo",
    "Join_Control",
    "Join_C",
    "Logical_Order_Exception",
    "LOE",
    "Lowercase",
    "Lower",
    "Math",
    "Noncharacter_Code_Point",
    "NChar",
    "Pattern_Syntax",
    "Pat_Syn",
    "Pattern_White_Space",
    "Pat_WS",
    "Quotation_Mark",
    "QMark",
    "Radical",
    "Regional_Indicator",
    "RI",
    "Sentence_Terminal",
    "STerm",
    "Soft_Dotted",
    "SD",
    "Terminal_Punctuation",
    "Term",
    "Unified_Ideograph",
    "UIdeo",
    "Uppercase",
    "Upper",
    "Variation_Selector",
    "VS",
    "White_Space",
    "space",
    "XID_Continue",
    "XIDC",
    "XID_Start",
    "XIDS",
];

/// ECMA-262 Table 69: binary properties of strings (`v` flag only).
const STRING_PROPS: &[&str] = &[
    "Basic_Emoji",
    "Emoji_Keycap_Sequence",
    "RGI_Emoji_Modifier_Sequence",
    "RGI_Emoji_Flag_Sequence",
    "RGI_Emoji_Tag_Sequence",
    "RGI_Emoji_ZWJ_Sequence",
    "RGI_Emoji",
];

struct P<'a> {
    s: &'a [char],
    i: usize,
    u: bool,
    v: bool,
    /// The pattern has named groups (makes `\k` a group reference in Annex B mode).
    named: bool,
    groups: u32,
    /// Group names per alternative path, for duplicate detection.
    names: Vec<Vec<String>>,
    all_names: Vec<String>,
    backrefs: Vec<String>,
    max_backref: u32,
    /// A union operand was seen in the current `v`-mode class (operators can't follow it).
    union_seen: bool,
}

type R<T> = Result<T, String>;

/// What a class atom stands for: one code point, or a set (\d, \p{…}, nested class).
#[derive(Clone, Copy, PartialEq)]
enum ClassAtom {
    Char(u32),
    Set,
    /// A set that may contain strings (`v` flag).
    Strings,
}

fn is_syntax(c: char) -> bool {
    "^$\\.*+?()[]{}|/".contains(c)
}

impl P<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn at(&self, n: usize) -> Option<char> {
        self.s.get(self.i + n).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn annex_b(&self) -> bool {
        !self.u && !self.v
    }

    fn disjunction(&mut self) -> R<()> {
        // Each alternative gets a fresh copy of the names declared before the disjunction.
        let base = self.names.last().cloned().unwrap_or_default();
        let mut merged = base.clone();
        loop {
            self.names.push(base.clone());
            self.alternative()?;
            let alt = self.names.pop().unwrap_or_default();
            for n in alt {
                if !merged.contains(&n) {
                    merged.push(n);
                }
            }
            if !self.eat('|') {
                break;
            }
        }
        if let Some(top) = self.names.last_mut() {
            *top = merged;
        }
        Ok(())
    }

    fn alternative(&mut self) -> R<()> {
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            self.term()?;
        }
        Ok(())
    }

    fn term(&mut self) -> R<()> {
        let c = self.peek().ok_or("unexpected end")?;
        // Assertions.
        match c {
            '^' | '$' => {
                self.i += 1;
                return self.no_quantifier();
            }
            '\\' if matches!(self.at(1), Some('b') | Some('B')) => {
                self.i += 2;
                return self.no_quantifier();
            }
            '(' if self.at(1) == Some('?') => {
                let la = matches!(self.at(2), Some('=') | Some('!'));
                let lb = self.at(2) == Some('<') && matches!(self.at(3), Some('=') | Some('!'));
                if la || lb {
                    self.i += if la { 3 } else { 4 };
                    self.disjunction()?;
                    if !self.eat(')') {
                        return Err("unterminated group".into());
                    }
                    // Annex B: lookaheads may be quantified without the u/v flags.
                    if la && self.annex_b() {
                        return self.quantifier();
                    }
                    return self.no_quantifier();
                }
            }
            _ => {}
        }
        self.atom()?;
        self.quantifier()
    }

    fn no_quantifier(&mut self) -> R<()> {
        match self.peek() {
            Some('*' | '+' | '?') => Err("nothing to repeat".into()),
            Some('{') if self.quantifier_braces_ahead() => Err("nothing to repeat".into()),
            _ => Ok(()),
        }
    }

    /// Whether `{` starts a syntactically valid `{n}`, `{n,}` or `{n,m}`.
    fn quantifier_braces_ahead(&self) -> bool {
        let mut j = self.i + 1;
        let digits = |j: &mut usize| {
            let s = *j;
            while self.s.get(*j).is_some_and(|c| c.is_ascii_digit()) {
                *j += 1;
            }
            *j > s
        };
        if !digits(&mut j) {
            return false;
        }
        if self.s.get(j) == Some(&',') {
            j += 1;
            digits(&mut j);
        }
        self.s.get(j) == Some(&'}')
    }

    fn quantifier(&mut self) -> R<()> {
        match self.peek() {
            Some('*' | '+' | '?') => self.i += 1,
            Some('{') => {
                if self.quantifier_braces_ahead() {
                    self.i += 1;
                    let a = self.number();
                    let b = if self.eat(',') {
                        if self.peek() == Some('}') {
                            None
                        } else {
                            Some(self.number())
                        }
                    } else {
                        Some(a)
                    };
                    self.i += 1; // }
                    if let Some(b) = b {
                        if b < a {
                            return Err("numbers out of order in {} quantifier".into());
                        }
                    }
                } else if self.annex_b() {
                    return Ok(()); // a literal `{`
                } else {
                    return Err("lone quantifier brackets".into());
                }
            }
            _ => return Ok(()),
        }
        self.eat('?');
        match self.peek() {
            Some('*' | '+' | '?') => Err("nothing to repeat".into()),
            Some('{') if self.quantifier_braces_ahead() => Err("nothing to repeat".into()),
            _ => Ok(()),
        }
    }

    fn number(&mut self) -> u64 {
        let mut v: u64 = 0;
        while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
            v = v.saturating_mul(10).saturating_add(u64::from(d));
            self.i += 1;
        }
        v
    }

    fn atom(&mut self) -> R<()> {
        let c = self.peek().ok_or("unexpected end")?;
        match c {
            '.' => {
                self.i += 1;
                Ok(())
            }
            '(' => self.group(),
            '[' => {
                self.i += 1;
                if self.v {
                    self.class_set_expression().map(|_| ())
                } else {
                    self.class_ranges()
                }
            }
            '\\' => {
                self.i += 1;
                self.atom_escape(false).map(|_| ())
            }
            '*' | '+' | '?' => Err("nothing to repeat".into()),
            '{' => {
                if self.annex_b() {
                    if self.quantifier_braces_ahead() {
                        return Err("nothing to repeat".into());
                    }
                    self.i += 1;
                    Ok(())
                } else {
                    Err("lone quantifier brackets".into())
                }
            }
            '}' | ']' => {
                if self.annex_b() {
                    self.i += 1;
                    Ok(())
                } else {
                    Err(format!("lone '{c}'"))
                }
            }
            ')' => Err("unmatched ')'".into()),
            _ => {
                self.i += 1;
                Ok(())
            }
        }
    }

    fn group(&mut self) -> R<()> {
        self.i += 1; // (
        if self.eat('?') {
            match self.peek() {
                Some(':') => {
                    self.i += 1;
                }
                Some('<') => {
                    self.i += 1;
                    let name = self.group_name()?;
                    let dup = self.names.last().is_some_and(|n| n.contains(&name));
                    if dup {
                        return Err(format!("duplicate group name '{name}'"));
                    }
                    if let Some(n) = self.names.last_mut() {
                        n.push(name.clone());
                    }
                    self.all_names.push(name);
                    self.groups += 1;
                }
                _ => {
                    // Modifiers: (?ims-ims:…)
                    let mut add = String::new();
                    while let Some(c) = self.peek().filter(|c| c.is_ascii_alphabetic()) {
                        add.push(c);
                        self.i += 1;
                    }
                    let mut remove = String::new();
                    let dash = self.eat('-');
                    if dash {
                        while let Some(c) = self.peek().filter(|c| c.is_ascii_alphabetic()) {
                            remove.push(c);
                            self.i += 1;
                        }
                    }
                    if !self.eat(':') {
                        return Err("invalid group".into());
                    }
                    let ok = |f: &str| {
                        let mut seen = String::new();
                        f.chars().all(|c| {
                            let fine = "ims".contains(c) && !seen.contains(c);
                            seen.push(c);
                            fine
                        })
                    };
                    if !ok(&add)
                        || !ok(&remove)
                        || add.chars().any(|c| remove.contains(c))
                        || (dash && add.is_empty() && remove.is_empty())
                    {
                        return Err("invalid regular expression modifiers".into());
                    }
                }
            }
        } else {
            self.groups += 1;
        }
        self.disjunction()?;
        if !self.eat(')') {
            return Err("unterminated group".into());
        }
        Ok(())
    }

    /// RegExpIdentifierName after `<`, up to `>`.
    fn group_name(&mut self) -> R<String> {
        let mut name = String::new();
        loop {
            let c = match self.peek() {
                Some('>') => {
                    self.i += 1;
                    break;
                }
                Some('\\') => {
                    self.i += 1;
                    if !self.eat('u') {
                        return Err("invalid group name".into());
                    }
                    let cp = self.unicode_escape_in_name()?;
                    char::from_u32(cp).ok_or("invalid group name")?
                }
                Some(c) => {
                    self.i += 1;
                    // A surrogate pair written directly is fine (chars are code points here).
                    c
                }
                None => return Err("invalid group name".into()),
            };
            let ok =
                if name.is_empty() { crate::lexer::is_id_start(c) } else { crate::lexer::is_id_continue(c) };
            if !ok {
                return Err("invalid group name".into());
            }
            name.push(c);
        }
        if name.is_empty() {
            return Err("invalid group name".into());
        }
        Ok(name)
    }

    /// `\u` in a group name: XXXX (a surrogate pair of two escapes joins), or {…} (any mode).
    fn unicode_escape_in_name(&mut self) -> R<u32> {
        if self.eat('{') {
            let v = self.hex_digits_until('}')?;
            return Ok(v);
        }
        let hi = self.hex4().ok_or("invalid escape in group name")?;
        if (0xd800..0xdc00).contains(&hi) && self.peek() == Some('\\') && self.at(1) == Some('u') {
            let save = self.i;
            self.i += 2;
            if let Some(lo) = self.hex4().filter(|lo| (0xdc00..0xe000).contains(lo)) {
                return Ok(0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00));
            }
            self.i = save;
        }
        Ok(hi)
    }

    fn hex4(&mut self) -> Option<u32> {
        let mut v = 0;
        for k in 0..4 {
            v = v * 16 + self.at(k)?.to_digit(16)?;
        }
        self.i += 4;
        Some(v)
    }

    fn hex_digits_until(&mut self, end: char) -> R<u32> {
        let mut v: u32 = 0;
        let mut n = 0;
        while let Some(d) = self.peek().and_then(|c| c.to_digit(16)) {
            v = v.saturating_mul(16).saturating_add(d);
            self.i += 1;
            n += 1;
        }
        if n == 0 || !self.eat(end) || v > 0x10ffff {
            return Err("invalid unicode escape".into());
        }
        Ok(v)
    }

    /// After `\`. `in_class`: inside a character class (`\b` is backspace, `\-` allowed in u).
    fn atom_escape(&mut self, in_class: bool) -> R<ClassAtom> {
        let c = self.peek().ok_or("\\ at end of pattern")?;
        self.i += 1;
        Ok(match c {
            'd' | 'D' | 's' | 'S' | 'w' | 'W' => ClassAtom::Set,
            'p' | 'P' if !self.annex_b() => {
                let strings = self.property(c == 'P')?;
                if strings {
                    ClassAtom::Strings
                } else {
                    ClassAtom::Set
                }
            }
            'b' if in_class => ClassAtom::Char(8),
            '-' if in_class && self.u => ClassAtom::Char('-' as u32),
            'k' if !in_class && (!self.annex_b() || self.named) => {
                if !self.eat('<') {
                    return Err("invalid named reference".into());
                }
                let name = self.group_name()?;
                self.backrefs.push(name);
                ClassAtom::Set
            }
            '1'..='9' if !in_class => {
                self.i -= 1;
                let start = self.i;
                let n = self.number();
                // Annex B: a number above the group count is a legacy octal / identity escape,
                // so only the u/v modes check it (after parsing, against the group count).
                let _ = start;
                if !self.annex_b() {
                    self.max_backref = self.max_backref.max(u32::try_from(n).unwrap_or(u32::MAX));
                }
                ClassAtom::Set
            }
            _ => {
                self.i -= 1;
                ClassAtom::Char(self.character_escape(in_class)?)
            }
        })
    }

    /// CharacterEscape (and Annex B's extras) at `self.i` (after the backslash).
    fn character_escape(&mut self, in_class: bool) -> R<u32> {
        let c = self.peek().ok_or("\\ at end of pattern")?;
        self.i += 1;
        Ok(match c {
            't' => 9,
            'n' => 10,
            'v' => 11,
            'f' => 12,
            'r' => 13,
            'c' => match self.peek() {
                Some(l) if l.is_ascii_alphabetic() => {
                    self.i += 1;
                    (l as u32) % 32
                }
                Some(d) if self.annex_b() && in_class && (d.is_ascii_digit() || d == '_') => {
                    self.i += 1;
                    (d as u32) % 32
                }
                _ if self.annex_b() => {
                    // `\c` stays a backslash; the `c` is read again as a literal.
                    self.i -= 1;
                    '\\' as u32
                }
                _ => return Err("invalid \\c escape".into()),
            },
            '0' if !self.peek().is_some_and(|d| d.is_ascii_digit()) => 0,
            '0'..='7' if self.annex_b() => {
                // Legacy octal escape.
                let mut v = c.to_digit(8).unwrap_or(0);
                let max = if c <= '3' { 3 } else { 2 };
                let mut n = 1;
                while n < max {
                    match self.peek().and_then(|d| d.to_digit(8)) {
                        Some(d) => {
                            v = v * 8 + d;
                            self.i += 1;
                            n += 1;
                        }
                        None => break,
                    }
                }
                v
            }
            'x' => {
                let h = |k: usize| self.at(k).and_then(|c| c.to_digit(16));
                match (h(0), h(1)) {
                    (Some(a), Some(b)) => {
                        self.i += 2;
                        a * 16 + b
                    }
                    _ if self.annex_b() => 'x' as u32,
                    _ => return Err("invalid \\x escape".into()),
                }
            }
            'u' => {
                if self.u || self.v {
                    if self.eat('{') {
                        return self.hex_digits_until('}');
                    }
                    let hi = self.hex4().ok_or("invalid \\u escape")?;
                    if (0xd800..0xdc00).contains(&hi) && self.peek() == Some('\\') && self.at(1) == Some('u')
                    {
                        let save = self.i;
                        self.i += 2;
                        if let Some(lo) = self.hex4().filter(|lo| (0xdc00..0xe000).contains(lo)) {
                            return Ok(0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00));
                        }
                        self.i = save;
                    }
                    hi
                } else {
                    self.hex4().unwrap_or('u' as u32)
                }
            }
            _ => {
                // IdentityEscape.
                if self.u || self.v {
                    let allowed = is_syntax(c)
                        || (c == '-' && in_class)
                        || (self.v && in_class && "&-!#%,:;<=>@`~".contains(c));
                    if allowed {
                        c as u32
                    } else {
                        return Err(format!("invalid escape \\{c}"));
                    }
                } else if c == 'k' && self.named {
                    return Err("invalid named reference".into());
                } else {
                    c as u32
                }
            }
        })
    }

    /// `\p{…}` after the `p`/`P`; returns whether it is a property of strings.
    fn property(&mut self, negated: bool) -> R<bool> {
        if !self.eat('{') {
            return Err("invalid property name".into());
        }
        let mut body = String::new();
        loop {
            match self.peek() {
                Some('}') => {
                    self.i += 1;
                    break;
                }
                Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == '=' => {
                    body.push(c);
                    self.i += 1;
                }
                _ => return Err("invalid property name".into()),
            }
        }
        if let Some((name, value)) = body.split_once('=') {
            let ok = match name {
                "General_Category" | "gc" => GENERAL_CATEGORY.contains(&value),
                "Script" | "sc" | "Script_Extensions" | "scx" => SCRIPT.contains(&value),
                _ => false,
            };
            return if ok { Ok(false) } else { Err(format!("invalid property {body}")) };
        }
        if GENERAL_CATEGORY.contains(&body.as_str()) || BINARY.contains(&body.as_str()) {
            return Ok(false);
        }
        if self.v && STRING_PROPS.contains(&body.as_str()) {
            if negated {
                return Err("negated property of strings".into());
            }
            return Ok(true);
        }
        Err(format!("invalid property {body}"))
    }

    /// `[` … `]` without the `v` flag (the `[` is consumed).
    fn class_ranges(&mut self) -> R<()> {
        self.eat('^');
        loop {
            match self.peek() {
                None => return Err("unterminated character class".into()),
                Some(']') => {
                    self.i += 1;
                    return Ok(());
                }
                _ => {}
            }
            let a = self.class_atom()?;
            if self.peek() == Some('-') && self.at(1) != Some(']') && self.at(1).is_some() {
                self.i += 1;
                let b = self.class_atom()?;
                match (a, b) {
                    (ClassAtom::Char(x), ClassAtom::Char(y)) => {
                        if x > y {
                            return Err("range out of order in character class".into());
                        }
                    }
                    _ if self.annex_b() => {}
                    _ => return Err("invalid character class range".into()),
                }
            }
        }
    }

    fn class_atom(&mut self) -> R<ClassAtom> {
        let c = self.peek().ok_or("unterminated character class")?;
        self.i += 1;
        if c == '\\' {
            return self.atom_escape(true);
        }
        Ok(ClassAtom::Char(c as u32))
    }

    /// `v`-mode ClassSetExpression after `[`; returns whether it may contain strings.
    fn class_set_expression(&mut self) -> R<bool> {
        let negated = self.eat('^');
        let mut strings = false;
        let mut op: Option<&str> = None;
        let mut first = true;
        loop {
            match self.peek() {
                None => return Err("unterminated character class".into()),
                Some(']') => {
                    self.i += 1;
                    break;
                }
                _ => {}
            }
            // Operators.
            let two: String = self.s[self.i..].iter().take(2).collect();
            if two == "&&" || two == "--" {
                if first {
                    return Err("operator at the start of a class".into());
                }
                let this = if two == "&&" { "&&" } else { "--" };
                if op.is_some_and(|o| o != this) || op.is_none() && !first && self.union_seen {
                    return Err("mixed class set operators".into());
                }
                op = Some(this);
                self.i += 2;
                if self.peek() == Some('&') && this == "&&" {
                    return Err("&&& in a class".into());
                }
                let operand = self.class_set_operand(true)?;
                if this == "&&" {
                    strings = strings && operand.1;
                }
                continue;
            }
            if op.is_some() {
                return Err("mixed class set operators".into());
            }
            let (a, s) = self.class_set_operand(false)?;
            // Ranges a-b (single characters only).
            if self.peek() == Some('-') && self.at(1) != Some('-') {
                self.i += 1;
                let (b, _) = self.class_set_operand(false)?;
                match (a, b) {
                    (ClassAtom::Char(x), ClassAtom::Char(y)) if x <= y => {}
                    (ClassAtom::Char(_), ClassAtom::Char(_)) => return Err("range out of order".into()),
                    _ => return Err("invalid class set range".into()),
                }
                self.union_seen = !first || self.union_seen;
            } else {
                strings |= s;
                if !first {
                    self.union_seen = true;
                }
            }
            first = false;
        }
        self.union_seen = false;
        if negated && strings {
            return Err("negated class with strings".into());
        }
        Ok(strings)
    }

    /// A ClassSetOperand: a nested class, `\q{…}`, an escape or a single character.
    fn class_set_operand(&mut self, after_op: bool) -> R<(ClassAtom, bool)> {
        let c = self.peek().ok_or("unterminated character class")?;
        match c {
            '[' => {
                self.i += 1;
                let saved = self.union_seen;
                self.union_seen = false;
                let s = self.class_set_expression()?;
                self.union_seen = saved;
                Ok((ClassAtom::Set, s))
            }
            '\\' => {
                self.i += 1;
                if self.peek() == Some('q') && self.at(1) == Some('{') {
                    self.i += 2;
                    let mut strings = false;
                    let mut len = 0;
                    loop {
                        match self.peek() {
                            None => return Err("unterminated \\q".into()),
                            Some('}') => {
                                self.i += 1;
                                break;
                            }
                            Some('|') => {
                                if len != 1 {
                                    strings = true;
                                }
                                len = 0;
                                self.i += 1;
                            }
                            Some('\\') => {
                                self.i += 1;
                                self.character_escape(true)?;
                                len += 1;
                            }
                            Some(ch) if "()[]{}/-|".contains(ch) => {
                                return Err("unescaped syntax character in \\q".into())
                            }
                            Some(_) => {
                                self.i += 1;
                                len += 1;
                            }
                        }
                    }
                    if len != 1 {
                        strings = true;
                    }
                    return Ok((ClassAtom::Set, strings));
                }
                let a = self.atom_escape(true)?;
                let strings = a == ClassAtom::Strings;
                Ok((a, strings))
            }
            _ => {
                let two: String = self.s[self.i..].iter().take(2).collect();
                const RESERVED: &[&str] = &[
                    "&&", "!!", "##", "$$", "%%", "**", "++", ",,", "..", "::", ";;", "<<", "==", ">>", "??",
                    "@@", "^^", "``", "~~",
                ];
                if RESERVED.contains(&two.as_str()) {
                    return Err("reserved double punctuator in class".into());
                }
                if "()[]{}/-|".contains(c) && !(c == '-' && false) {
                    return Err(format!("unescaped '{c}' in class"));
                }
                let _ = after_op;
                self.i += 1;
                Ok((ClassAtom::Char(c as u32), false))
            }
        }
    }
}

/// Validate a pattern. `unicode`: the `u` or `v` flag; `sets`: the `v` flag.
pub fn validate(body: &str, unicode: bool, sets: bool) -> Result<(), String> {
    let chars: Vec<char> = body.chars().collect();
    let named = has_named_groups(&chars);
    let mut p = P {
        s: &chars,
        i: 0,
        u: unicode && !sets,
        v: sets,
        named,
        groups: 0,
        names: vec![Vec::new()],
        all_names: Vec::new(),
        backrefs: Vec::new(),
        max_backref: 0,
        union_seen: false,
    };
    p.disjunction()?;
    if p.i < chars.len() {
        return Err(if chars[p.i] == ')' { "unmatched ')'".into() } else { "unexpected character".into() });
    }
    for b in &p.backrefs {
        if !p.all_names.contains(b) {
            return Err(format!("no group named '{b}'"));
        }
    }
    if !p.annex_b() && p.max_backref > p.groups {
        return Err("backreference to a missing group".into());
    }
    Ok(())
}

/// A `(?<name>` anywhere outside classes.
fn has_named_groups(s: &[char]) -> bool {
    let mut i = 0;
    let mut class = false;
    while i < s.len() {
        match s[i] {
            '\\' => i += 1,
            '[' => class = true,
            ']' => class = false,
            '(' if !class
                && s.get(i + 1) == Some(&'?')
                && s.get(i + 2) == Some(&'<')
                && !matches!(s.get(i + 3), Some('=') | Some('!')) =>
            {
                return true
            }
            _ => {}
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::validate;

    #[test]
    fn modes() {
        for ok in
            ["a|b", "(?:a)+", "a{1,2}?", "[a-z\\d]", "(?<x>a)\\k<x>", "\\1(a)", "(?i:a)", "(?<a>x)|(?<a>y)"]
        {
            assert!(validate(ok, false, false).is_ok(), "{ok}");
            assert!(validate(ok, true, false).is_ok(), "{ok} /u");
        }
        // Annex B leniency.
        for annex_b in ["a{", "}", "]", "\\c1", "\\8", "(?=a)*", "\\p", "[\\d-z]", "\\k"] {
            assert!(validate(annex_b, false, false).is_ok(), "{annex_b}");
            assert!(validate(annex_b, true, false).is_err(), "{annex_b} /u");
        }
        for bad in [
            "a**",
            "(",
            ")",
            "[b-a]",
            "a{2,1}",
            "(?<a>x)(?<a>y)",
            "(?ii:a)",
            "(?-:a)",
            "\\k<nope>(?<x>a)",
            "*",
        ] {
            assert!(validate(bad, false, false).is_err(), "{bad}");
        }
        assert!(validate("\\p{Script=Latin}\\p{gc=Lu}\\p{ASCII}\\P{L}", true, false).is_ok());
        assert!(validate("\\p{Script=Nope}", true, false).is_err());
        assert!(validate("\\p{ASCII=Y}", true, false).is_err());
        assert!(validate("\\p{RGI_Emoji}", true, true).is_ok());
        assert!(validate("\\p{RGI_Emoji}", true, false).is_err());
        assert!(validate("[^\\p{RGI_Emoji}]", true, true).is_err());
        assert!(validate("[\\w--\\d]", true, true).is_ok());
        assert!(validate("[\\w--\\d&&a]", true, true).is_err());
        assert!(validate("[\\q{abc|d}a]", true, true).is_ok());
        assert!(validate("[(]", true, true).is_err());
        assert!(validate("\\2(a)", true, false).is_err());
    }
}

//! Our regular expression engine (ECMA-262 §22.2): a parser for the pattern grammar (with the
//! Annex B.1.2 extensions outside u/v mode), a compiler to a small instruction set, and a
//! backtracking matcher with an explicit stack (no recursion over the input, so long subjects
//! can't overflow the native stack) and a step budget.
//!
//! Patterns are validated by `crate::regexp::validate` before they get here, so the parser is
//! lenient about errors.

pub mod unicode;

use unicode::{simple_fold, upper_canon};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Flags {
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    /// `u` or `v`: match by code points.
    pub unicode: bool,
    pub sets: bool,
}

/// A character set: sorted, merged ranges, plus strings (v-mode classes).
#[derive(Clone, Debug, Default)]
struct Set {
    ranges: Vec<(u32, u32)>,
    strings: Vec<Vec<u32>>,
}

fn normalize(mut r: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    r.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(r.len());
    for (a, b) in r {
        if let Some(last) = out.last_mut() {
            if a <= last.1.saturating_add(1) {
                last.1 = last.1.max(b);
                continue;
            }
        }
        out.push((a, b));
    }
    out
}

fn complement(r: &[(u32, u32)], max: u32) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut next = 0u32;
    for &(a, b) in r {
        if a > next {
            out.push((next, a - 1));
        }
        next = b.saturating_add(1);
    }
    if next <= max {
        out.push((next, max));
    }
    out
}

fn intersect(a: &[(u32, u32)], b: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let lo = a[i].0.max(b[j].0);
        let hi = a[i].1.min(b[j].1);
        if lo <= hi {
            out.push((lo, hi));
        }
        if a[i].1 < b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

fn contains(r: &[(u32, u32)], c: u32) -> bool {
    r.binary_search_by(|&(a, b)| {
        if c < a {
            std::cmp::Ordering::Greater
        } else if c > b {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Equal
        }
    })
    .is_ok()
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    Char(u32),
    Any,
    Set(usize, bool),
    Start,
    End,
    WordB(bool),
    Group(Box<Node>, Option<usize>),
    Alt(Vec<Node>),
    Seq(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        first_group: usize,
        groups: usize,
    },
    BackRef(Vec<usize>),
    Look {
        node: Box<Node>,
        ahead: bool,
        negate: bool,
    },
    /// `(?ims-ims:…)` modifiers.
    Modified(Box<Node>, Flags),
}

struct Parser<'a> {
    p: &'a [u32],
    i: usize,
    flags: Flags,
    sets: Vec<Set>,
    ngroups: usize,
    names: Vec<(Vec<u32>, usize)>,
    /// The pattern has named groups (enables `\k` outside u mode, B.1.2).
    has_names: bool,
}

const WORD: &[(u32, u32)] = &[(0x30, 0x39), (0x41, 0x5A), (0x5F, 0x5F), (0x61, 0x7A)];
const DIGIT: &[(u32, u32)] = &[(0x30, 0x39)];
const SPACE: &[(u32, u32)] = &[
    (0x09, 0x0D),
    (0x20, 0x20),
    (0xA0, 0xA0),
    (0x1680, 0x1680),
    (0x2000, 0x200A),
    (0x2028, 0x2029),
    (0x202F, 0x202F),
    (0x205F, 0x205F),
    (0x3000, 0x3000),
    (0xFEFF, 0xFEFF),
];

impl Parser<'_> {
    fn peek(&self) -> Option<u32> {
        self.p.get(self.i).copied()
    }
    fn peek_at(&self, k: usize) -> Option<u32> {
        self.p.get(self.i + k).copied()
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c as u32) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn max_char(&self) -> u32 {
        if self.flags.unicode {
            0x10FFFF
        } else {
            0xFFFF
        }
    }
    fn add_set(&mut self, s: Set) -> usize {
        self.sets.push(s);
        self.sets.len() - 1
    }

    fn disjunction(&mut self) -> Node {
        let mut alts = vec![self.alternative()];
        while self.eat('|') {
            alts.push(self.alternative());
        }
        if alts.len() == 1 {
            alts.pop().unwrap_or(Node::Empty)
        } else {
            Node::Alt(alts)
        }
    }

    fn alternative(&mut self) -> Node {
        let mut seq = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' as u32 || c == ')' as u32 {
                break;
            }
            let groups_before = self.ngroups;
            let Some(atom) = self.term() else { continue };
            let atom = self.quantifier(atom, groups_before);
            seq.push(atom);
        }
        match seq.len() {
            0 => Node::Empty,
            1 => seq.pop().unwrap_or(Node::Empty),
            _ => Node::Seq(seq),
        }
    }

    /// `{n}`, `{n,}`, `{n,m}` at the cursor (B.1.2: otherwise `{` is a literal).
    fn braces(&mut self) -> Option<(u32, Option<u32>)> {
        let save = self.i;
        if !self.eat('{') {
            return None;
        }
        let num = |p: &mut Self| -> Option<u32> {
            let st = p.i;
            let mut v: u64 = 0;
            while let Some(c) = p.peek().filter(|c| (0x30..=0x39).contains(c)) {
                v = (v * 10 + (c - 0x30) as u64).min(u32::MAX as u64);
                p.i += 1;
            }
            (p.i > st).then_some(v as u32)
        };
        let Some(min) = num(self) else {
            self.i = save;
            return None;
        };
        let max = if self.eat(',') {
            if self.peek() == Some('}' as u32) {
                None
            } else {
                match num(self) {
                    Some(m) => Some(m),
                    None => {
                        self.i = save;
                        return None;
                    }
                }
            }
        } else {
            Some(min)
        };
        if !self.eat('}') {
            self.i = save;
            return None;
        }
        Some((min, max))
    }

    fn quantifier(&mut self, atom: Node, groups_before: usize) -> Node {
        if matches!(atom, Node::Start | Node::End | Node::WordB(_))
            || matches!(atom, Node::Look { ahead: false, .. })
        {
            return atom;
        }
        let (min, max) = match self.peek() {
            Some(0x2A) => {
                self.i += 1;
                (0, None)
            }
            Some(0x2B) => {
                self.i += 1;
                (1, None)
            }
            Some(0x3F) => {
                self.i += 1;
                (0, Some(1))
            }
            Some(0x7B) => match self.braces() {
                Some(q) => q,
                None => return atom,
            },
            _ => return atom,
        };
        let greedy = !self.eat('?');
        Node::Repeat {
            node: Box::new(atom),
            min,
            max,
            greedy,
            first_group: groups_before,
            groups: self.ngroups - groups_before,
        }
    }

    fn term(&mut self) -> Option<Node> {
        let c = self.peek()?;
        self.i += 1;
        Some(match char::from_u32(c).unwrap_or('\0') {
            '^' => Node::Start,
            '$' => Node::End,
            '.' => Node::Any,
            '(' => self.group(),
            '[' => {
                let s = self.class();
                let has_strings = !s.strings.is_empty();
                let idx = self.add_set(s);
                if has_strings {
                    self.string_alternatives(idx)
                } else {
                    Node::Set(idx, false)
                }
            }
            '\\' => self.atom_escape(),
            _ => Node::Char(self.surrogate_pair(c)),
        })
    }

    /// In u mode a pattern is already code points; this is the identity.
    fn surrogate_pair(&self, c: u32) -> u32 {
        c
    }

    fn group(&mut self) -> Node {
        if self.eat('?') {
            if self.eat(':') {
                let n = self.disjunction();
                self.eat(')');
                return Node::Group(Box::new(n), None);
            }
            if self.eat('=') || self.peek() == Some('!' as u32) {
                let negate = self.eat('!');
                let n = self.disjunction();
                self.eat(')');
                return Node::Look { node: Box::new(n), ahead: true, negate };
            }
            if self.peek() == Some('<' as u32) && matches!(self.peek_at(1), Some(0x3D) | Some(0x21)) {
                self.i += 1;
                let negate = self.eat('!');
                if !negate {
                    self.eat('=');
                }
                let n = self.disjunction();
                self.eat(')');
                return Node::Look { node: Box::new(n), ahead: false, negate };
            }
            if self.eat('<') {
                let mut name = Vec::new();
                while let Some(c) = self.peek() {
                    self.i += 1;
                    if c == '>' as u32 {
                        break;
                    }
                    if c == '\\' as u32 && self.eat('u') {
                        name.extend(self.unicode_escape_in_name());
                        continue;
                    }
                    name.push(c);
                }
                self.ngroups += 1;
                let idx = self.ngroups;
                self.names.push((name, idx));
                let n = self.disjunction();
                self.eat(')');
                return Node::Group(Box::new(n), Some(idx));
            }
            // Modifiers: (?ims-ims:…)
            let mut f = self.flags;
            let mut on = true;
            while let Some(c) = self.peek() {
                self.i += 1;
                match char::from_u32(c).unwrap_or('\0') {
                    'i' => f.ignore_case = on,
                    'm' => f.multiline = on,
                    's' => f.dot_all = on,
                    '-' => on = false,
                    _ => break,
                }
            }
            let saved = self.flags;
            self.flags = f;
            let n = self.disjunction();
            self.flags = saved;
            self.eat(')');
            return Node::Modified(Box::new(n), f);
        }
        self.ngroups += 1;
        let idx = self.ngroups;
        let n = self.disjunction();
        self.eat(')');
        Node::Group(Box::new(n), Some(idx))
    }

    fn unicode_escape_in_name(&mut self) -> Vec<u32> {
        let v = self.unicode_escape_value().unwrap_or(0);
        vec![v]
    }

    fn hex(&mut self, n: usize) -> Option<u32> {
        let mut v = 0;
        for k in 0..n {
            let d = char::from_u32(self.peek_at(k)?)?.to_digit(16)?;
            v = v * 16 + d;
        }
        self.i += n;
        Some(v)
    }

    /// After `\u`: `XXXX`, a surrogate pair `\uXXXX\uXXXX` (u mode), or `{X…}` (u mode).
    fn unicode_escape_value(&mut self) -> Option<u32> {
        if self.peek() == Some('{' as u32) && (self.flags.unicode || true) {
            let save = self.i;
            self.i += 1;
            let mut v: u32 = 0;
            let st = self.i;
            while let Some(d) = self.peek().and_then(char::from_u32).and_then(|c| c.to_digit(16)) {
                v = v.saturating_mul(16).saturating_add(d);
                self.i += 1;
            }
            if self.i > st && self.eat('}') && self.flags.unicode {
                return Some(v);
            }
            self.i = save;
            return None;
        }
        let v = self.hex(4)?;
        if self.flags.unicode
            && (0xD800..0xDC00).contains(&v)
            && self.peek() == Some('\\' as u32)
            && self.peek_at(1) == Some('u' as u32)
        {
            let save = self.i;
            self.i += 2;
            if let Some(lo) = self.hex(4) {
                if (0xDC00..0xE000).contains(&lo) {
                    return Some(0x10000 + ((v - 0xD800) << 10) + (lo - 0xDC00));
                }
            }
            self.i = save;
        }
        Some(v)
    }

    fn atom_escape(&mut self) -> Node {
        let Some(c) = self.peek() else { return Node::Char('\\' as u32) };
        let ch = char::from_u32(c).unwrap_or('\0');
        // Back references.
        if ch.is_ascii_digit() && ch != '0' {
            let save = self.i;
            let mut n: usize = 0;
            while let Some(d) = self.peek().and_then(char::from_u32).and_then(|c| c.to_digit(10)) {
                n = n.saturating_mul(10).saturating_add(d as usize);
                self.i += 1;
            }
            if self.flags.unicode || n <= self.total_groups() {
                return Node::BackRef(vec![n]);
            }
            // B.1.2: a legacy octal escape, or an identity escape for 8 and 9.
            self.i = save;
            if ch >= '8' {
                self.i += 1;
                return Node::Char(c);
            }
            return Node::Char(self.legacy_octal());
        }
        if ch == 'k' && (self.flags.unicode || self.has_names) {
            self.i += 1;
            if self.eat('<') {
                let mut name = Vec::new();
                while let Some(c) = self.peek() {
                    self.i += 1;
                    if c == '>' as u32 {
                        break;
                    }
                    if c == '\\' as u32 && self.eat('u') {
                        name.extend(self.unicode_escape_in_name());
                        continue;
                    }
                    name.push(c);
                }
                return Node::BackRef(self.group_indices_of(&name));
            }
            return Node::Char('k' as u32);
        }
        match ch {
            'b' => {
                self.i += 1;
                Node::WordB(true)
            }
            'B' => {
                self.i += 1;
                Node::WordB(false)
            }
            _ => {
                let s = self.class_escape(false);
                match s {
                    Escape::Char(c) => Node::Char(c),
                    Escape::Set(set) => {
                        let has_strings = !set.strings.is_empty();
                        let idx = self.add_set(set);
                        if has_strings {
                            self.string_alternatives(idx)
                        } else {
                            Node::Set(idx, false)
                        }
                    }
                }
            }
        }
    }

    fn legacy_octal(&mut self) -> u32 {
        let mut v = 0u32;
        let mut n = 0;
        while let Some(d) = self.peek().and_then(char::from_u32).and_then(|c| c.to_digit(8)) {
            if n == 3 || v * 8 + d > 0o377 {
                break;
            }
            v = v * 8 + d;
            self.i += 1;
            n += 1;
        }
        v
    }

    /// The number of capturing groups in the whole pattern (for `\N` decisions).
    fn total_groups(&self) -> usize {
        count_groups(self.p)
    }

    fn group_indices_of(&self, name: &[u32]) -> Vec<usize> {
        // Names may be declared later in the pattern: scan the whole pattern.
        all_group_names(self.p).into_iter().filter(|(n, _)| n == name).map(|(_, i)| i).collect()
    }

    /// An escape inside or outside a class: a character or a set.
    fn class_escape(&mut self, in_class: bool) -> Escape {
        let Some(c) = self.peek() else { return Escape::Char('\\' as u32) };
        self.i += 1;
        let ch = char::from_u32(c).unwrap_or('\0');
        let set = |r: &[(u32, u32)]| Escape::Set(Set { ranges: r.to_vec(), strings: Vec::new() });
        match ch {
            'd' => set(DIGIT),
            'D' => Escape::Set(Set { ranges: complement(DIGIT, self.max_char()), strings: Vec::new() }),
            's' => set(SPACE),
            'S' => Escape::Set(Set { ranges: complement(SPACE, self.max_char()), strings: Vec::new() }),
            'w' => Escape::Set(Set { ranges: self.word_set(), strings: Vec::new() }),
            'W' => Escape::Set(Set {
                ranges: complement(&self.word_set(), self.max_char()),
                strings: Vec::new(),
            }),
            'p' | 'P' if self.flags.unicode => {
                let negate = ch == 'P';
                let mut body = String::new();
                if self.eat('{') {
                    while let Some(c) = self.peek() {
                        self.i += 1;
                        if c == '}' as u32 {
                            break;
                        }
                        body.push(char::from_u32(c).unwrap_or('\0'));
                    }
                }
                let (name, value) = match body.split_once('=') {
                    Some((n, v)) => (n.to_string(), Some(v.to_string())),
                    None => (body.clone(), None),
                };
                if value.is_none() && self.flags.sets {
                    if let Some(strs) = unicode::string_property(&name) {
                        let mut ranges = Vec::new();
                        let mut strings = Vec::new();
                        for s in strs {
                            if s.len() == 1 {
                                ranges.push((s[0], s[0]));
                            } else {
                                strings.push(s);
                            }
                        }
                        return Escape::Set(Set { ranges: normalize(ranges), strings });
                    }
                }
                let r = unicode::property(&name, value.as_deref()).unwrap_or_default();
                let r = if self.flags.ignore_case && self.flags.sets { fold_closure(&r) } else { r };
                Escape::Set(Set {
                    ranges: if negate { complement(&r, 0x10FFFF) } else { r },
                    strings: Vec::new(),
                })
            }
            'f' => Escape::Char(0x0C),
            'n' => Escape::Char(0x0A),
            'r' => Escape::Char(0x0D),
            't' => Escape::Char(0x09),
            'v' => Escape::Char(0x0B),
            'b' if in_class => Escape::Char(0x08),
            '-' if in_class => Escape::Char('-' as u32),
            'c' => {
                if let Some(l) =
                    self.peek().filter(|&l| char::from_u32(l).is_some_and(|ch| ch.is_ascii_alphabetic()))
                {
                    self.i += 1;
                    return Escape::Char(l % 32);
                }
                if in_class && !self.flags.unicode {
                    // B.1.2 ClassControlLetter: digits and `_`.
                    if let Some(l) = self
                        .peek()
                        .filter(|&l| char::from_u32(l).is_some_and(|ch| ch.is_ascii_digit() || ch == '_'))
                    {
                        self.i += 1;
                        return Escape::Char(l % 32);
                    }
                }
                // `\c` alone is a literal backslash then `c`.
                self.i -= 1;
                Escape::Char('\\' as u32)
            }
            '0' if !self.peek().is_some_and(|d| (0x30..=0x39).contains(&d)) => Escape::Char(0),
            '0'..='7' if !self.flags.unicode => {
                self.i -= 1;
                Escape::Char(self.legacy_octal())
            }
            'x' => match self.hex(2) {
                Some(v) => Escape::Char(v),
                None => Escape::Char('x' as u32),
            },
            'u' => match self.unicode_escape_value() {
                Some(v) => Escape::Char(v),
                None => Escape::Char('u' as u32),
            },
            _ => Escape::Char(c),
        }
    }

    fn word_set(&self) -> Vec<(u32, u32)> {
        if self.flags.unicode && self.flags.ignore_case {
            // §22.2.2.9.4 WordCharacters: plus ſ (U+017F) and K (U+212A).
            normalize(WORD.iter().copied().chain([(0x17F, 0x17F), (0x212A, 0x212A)]).collect())
        } else {
            WORD.to_vec()
        }
    }

    /// `[…]` after the `[`.
    fn class(&mut self) -> Set {
        if self.flags.sets {
            return self.class_v();
        }
        let negate = self.eat('^');
        let mut ranges = Vec::new();
        let mut first = true;
        while let Some(c) = self.peek() {
            if c == ']' as u32 && !(first && false) {
                self.i += 1;
                break;
            }
            first = false;
            let a = self.class_atom();
            if self.peek() == Some('-' as u32) && self.peek_at(1).is_some_and(|n| n != ']' as u32) {
                let save = self.i;
                self.i += 1;
                let b = self.class_atom();
                match (&a, &b) {
                    (Escape::Char(x), Escape::Char(y)) => {
                        ranges.push((*x, *y));
                        continue;
                    }
                    _ => {
                        // B.1.2: `\d-x` is a set, `-` and `x`.
                        self.i = save;
                    }
                }
            }
            match a {
                Escape::Char(x) => ranges.push((x, x)),
                Escape::Set(s) => ranges.extend(s.ranges),
            }
        }
        let r = normalize(ranges);
        Set { ranges: if negate { complement(&r, self.max_char()) } else { r }, strings: Vec::new() }
    }

    fn class_atom(&mut self) -> Escape {
        let Some(c) = self.peek() else { return Escape::Char(0) };
        self.i += 1;
        if c == '\\' as u32 {
            return self.class_escape(true);
        }
        Escape::Char(c)
    }

    /// A v-mode class (§22.2.1 ClassSetExpression): unions, `&&`, `--`, nested classes, `\q{}`.
    fn class_v(&mut self) -> Set {
        let negate = self.eat('^');
        let mut acc: Option<Set> = None;
        let mut op: u8 = 0; // 0 union, 1 intersection, 2 difference
        while let Some(c) = self.peek() {
            if c == ']' as u32 {
                self.i += 1;
                break;
            }
            if c == '&' as u32 && self.peek_at(1) == Some('&' as u32) {
                self.i += 2;
                op = 1;
                continue;
            }
            if c == '-' as u32 && self.peek_at(1) == Some('-' as u32) {
                self.i += 2;
                op = 2;
                continue;
            }
            let operand = self.class_v_operand();
            acc = Some(match (acc, op) {
                (None, _) => operand,
                (Some(a), 0) => Set {
                    ranges: normalize(a.ranges.into_iter().chain(operand.ranges).collect()),
                    strings: a.strings.into_iter().chain(operand.strings).collect(),
                },
                (Some(a), 1) => Set {
                    ranges: intersect(&a.ranges, &operand.ranges),
                    strings: a.strings.into_iter().filter(|s| operand.strings.contains(s)).collect(),
                },
                (Some(a), _) => Set {
                    ranges: intersect(&a.ranges, &complement(&operand.ranges, 0x10FFFF)),
                    strings: a.strings.into_iter().filter(|s| !operand.strings.contains(s)).collect(),
                },
            });
        }
        let mut s = acc.unwrap_or_default();
        if self.flags.ignore_case {
            s.ranges = fold_closure(&s.ranges);
        }
        if negate {
            s.ranges = complement(&s.ranges, 0x10FFFF);
            s.strings.clear();
        }
        s
    }

    fn class_v_operand(&mut self) -> Set {
        let Some(c) = self.peek() else { return Set::default() };
        if c == '[' as u32 {
            self.i += 1;
            return self.class_v();
        }
        if c == '\\' as u32 && self.peek_at(1) == Some('q' as u32) && self.peek_at(2) == Some('{' as u32) {
            self.i += 3;
            let mut strings = Vec::new();
            let mut cur = Vec::new();
            while let Some(c) = self.peek() {
                self.i += 1;
                if c == '}' as u32 {
                    strings.push(std::mem::take(&mut cur));
                    break;
                }
                if c == '|' as u32 {
                    strings.push(std::mem::take(&mut cur));
                    continue;
                }
                if c == '\\' as u32 {
                    if let Escape::Char(x) = self.class_escape(true) {
                        cur.push(x);
                    }
                    continue;
                }
                cur.push(c);
            }
            let mut ranges = Vec::new();
            let mut strs = Vec::new();
            for s in strings {
                if s.len() == 1 {
                    ranges.push((s[0], s[0]));
                } else {
                    strs.push(s);
                }
            }
            return Set { ranges: normalize(ranges), strings: strs };
        }
        let a = self.class_atom();
        if let Escape::Char(x) = a {
            if self.peek() == Some('-' as u32) && self.peek_at(1) != Some('-' as u32) {
                self.i += 1;
                if let Escape::Char(y) = self.class_atom() {
                    return Set { ranges: vec![(x, y)], strings: Vec::new() };
                }
            }
            return Set { ranges: vec![(x, x)], strings: Vec::new() };
        }
        match a {
            Escape::Set(s) => s,
            Escape::Char(_) => Set::default(),
        }
    }

    /// A class with strings matches the longest string first (§22.2.2.7 CompileAtom).
    fn string_alternatives(&mut self, idx: usize) -> Node {
        let mut strings = self.sets[idx].strings.clone();
        strings.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let mut alts: Vec<Node> =
            strings.into_iter().map(|s| Node::Seq(s.into_iter().map(Node::Char).collect())).collect();
        alts.push(Node::Set(idx, false));
        Node::Group(Box::new(Node::Alt(alts)), None)
    }
}

enum Escape {
    Char(u32),
    Set(Set),
}

/// Add every character with the same simple case folding (v mode with i).
fn fold_closure(r: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let t = unicode::tables();
    let mut out = r.to_vec();
    for (&c, others) in &t.fold_class {
        if contains(r, c) {
            for &o in others {
                out.push((o, o));
            }
        }
    }
    normalize(out)
}

fn count_groups(p: &[u32]) -> usize {
    all_group_names_and_count(p).1
}

fn all_group_names(p: &[u32]) -> Vec<(Vec<u32>, usize)> {
    all_group_names_and_count(p).0
}

/// A quick scan for capturing groups (skipping escapes and classes).
fn all_group_names_and_count(p: &[u32]) -> (Vec<(Vec<u32>, usize)>, usize) {
    let mut i = 0;
    let mut n = 0;
    let mut names = Vec::new();
    let mut in_class = false;
    while i < p.len() {
        let c = p[i];
        if c == '\\' as u32 {
            i += 2;
            continue;
        }
        if in_class {
            if c == ']' as u32 {
                in_class = false;
            }
            i += 1;
            continue;
        }
        if c == '[' as u32 {
            in_class = true;
        } else if c == '(' as u32 {
            if p.get(i + 1) != Some(&('?' as u32)) {
                n += 1;
            } else if p.get(i + 2) == Some(&('<' as u32)) && !matches!(p.get(i + 3), Some(0x3D) | Some(0x21))
            {
                n += 1;
                let mut j = i + 3;
                let mut name = Vec::new();
                while j < p.len() && p[j] != '>' as u32 {
                    name.push(p[j]);
                    j += 1;
                }
                names.push((name, n));
            }
        }
        i += 1;
    }
    (names, n)
}

// ---- compiled program ----

#[derive(Clone, Debug)]
enum Inst {
    Char(u32),
    Any,
    Set(usize, bool),
    Start,
    End,
    WordB(bool),
    Split(usize, usize),
    Jmp(usize),
    Save(usize),
    BackRef(Vec<usize>),
    /// Body at pc+1 up to the LookEnd; continue at the field.
    Look {
        ahead: bool,
        negate: bool,
        next: usize,
    },
    LookEnd,
    /// Clear captures of groups `a..a+n` (each loop iteration, §22.2.2.3.1 step 4).
    ClearCaps(usize, usize),
    /// Loop counter register r = 0.
    CountInit(usize),
    /// Loop head: (register, min, max, greedy, body, exit).
    Loop {
        r: usize,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        body: usize,
        exit: usize,
    },
    /// Record the position for the empty check of loop register r.
    PosSave(usize),
    /// Fail if no progress since PosSave (once the minimum is met); then count++.
    LoopEnd {
        r: usize,
        min: u32,
        head: usize,
    },
    /// Switch the flags in effect (modifiers).
    SetFlags(Flags),
    /// A quantified single-character matcher (the instruction at pc+1), forward and without
    /// groups: scans in one step and backtracks a character at a time; continues at pc+2.
    Star {
        min: u32,
        max: Option<u32>,
        greedy: bool,
    },
    Match,
}

struct Compiler {
    prog: Vec<Inst>,
    regs: usize,
}

impl Compiler {
    fn emit(&mut self, i: Inst) -> usize {
        self.prog.push(i);
        self.prog.len() - 1
    }

    fn node(&mut self, n: &Node, back: bool, flags: Flags) {
        match n {
            Node::Empty => {}
            Node::Char(c) => {
                self.emit(Inst::Char(*c));
            }
            Node::Any => {
                self.emit(Inst::Any);
            }
            Node::Set(i, neg) => {
                self.emit(Inst::Set(*i, *neg));
            }
            Node::Start => {
                self.emit(Inst::Start);
            }
            Node::End => {
                self.emit(Inst::End);
            }
            Node::WordB(b) => {
                self.emit(Inst::WordB(*b));
            }
            Node::Group(inner, idx) => {
                if let Some(g) = idx {
                    self.emit(Inst::Save(if back { 2 * g + 1 } else { 2 * g }));
                }
                self.node(inner, back, flags);
                if let Some(g) = idx {
                    self.emit(Inst::Save(if back { 2 * g } else { 2 * g + 1 }));
                }
            }
            Node::Seq(items) => {
                if back {
                    for it in items.iter().rev() {
                        self.node(it, back, flags);
                    }
                } else {
                    for it in items {
                        self.node(it, back, flags);
                    }
                }
            }
            Node::Alt(alts) => {
                let mut jumps = Vec::new();
                for (k, a) in alts.iter().enumerate() {
                    if k + 1 < alts.len() {
                        let split = self.emit(Inst::Split(0, 0));
                        self.node(a, back, flags);
                        jumps.push(self.emit(Inst::Jmp(0)));
                        let next = self.prog.len();
                        self.prog[split] = Inst::Split(split + 1, next);
                    } else {
                        self.node(a, back, flags);
                    }
                }
                let end = self.prog.len();
                for j in jumps {
                    self.prog[j] = Inst::Jmp(end);
                }
            }
            Node::Repeat { node, min, max, greedy, groups: 0, .. }
                if !back && matches!(**node, Node::Char(_) | Node::Any | Node::Set(..)) =>
            {
                self.emit(Inst::Star { min: *min, max: *max, greedy: *greedy });
                self.node(node, back, flags);
            }
            Node::Repeat { node, min, max, greedy, first_group, groups } => {
                let r = self.regs;
                self.regs += 2;
                self.emit(Inst::CountInit(r));
                let head =
                    self.emit(Inst::Loop { r, min: *min, max: *max, greedy: *greedy, body: 0, exit: 0 });
                let body = self.prog.len();
                self.emit(Inst::PosSave(r + 1));
                if *groups > 0 {
                    self.emit(Inst::ClearCaps(first_group + 1, *groups));
                }
                self.node(node, back, flags);
                self.emit(Inst::LoopEnd { r, min: *min, head });
                let exit = self.prog.len();
                self.prog[head] = Inst::Loop { r, min: *min, max: *max, greedy: *greedy, body, exit };
            }
            Node::BackRef(g) => {
                self.emit(Inst::BackRef(g.clone()));
            }
            Node::Look { node, ahead, negate } => {
                let at = self.emit(Inst::Look { ahead: *ahead, negate: *negate, next: 0 });
                self.node(node, !*ahead, flags);
                self.emit(Inst::LookEnd);
                let next = self.prog.len();
                self.prog[at] = Inst::Look { ahead: *ahead, negate: *negate, next };
            }
            Node::Modified(inner, f) => {
                self.emit(Inst::SetFlags(*f));
                self.node(inner, back, *f);
                self.emit(Inst::SetFlags(flags));
            }
        }
    }
}

/// A compiled regular expression.
pub struct Regex {
    prog: Vec<Inst>,
    sets: Vec<Set>,
    pub flags: Flags,
    pub groups: usize,
    /// (name, group index) in pattern order (duplicates allowed in different alternatives).
    pub names: Vec<(Vec<u16>, usize)>,
    regs: usize,
}

/// The match: capture slots (code unit offsets), `None` for groups that didn't participate.
pub type Captures = Vec<Option<(usize, usize)>>;

/// Ran out of steps (catastrophic backtracking): the caller turns this into termination.
#[derive(Debug)]
pub struct Exhausted;

impl Regex {
    pub fn new(pattern: &[u16], flags: Flags) -> Regex {
        // Work in code points in u/v mode, code units otherwise.
        let p: Vec<u32> = if flags.unicode {
            char::decode_utf16(pattern.iter().copied())
                .map(|r| r.map(|c| c as u32).unwrap_or_else(|e| e.unpaired_surrogate() as u32))
                .collect()
        } else {
            pattern.iter().map(|&u| u as u32).collect()
        };
        let has_names = !all_group_names(&p).is_empty();
        let mut parser =
            Parser { p: &p, i: 0, flags, sets: Vec::new(), ngroups: 0, names: Vec::new(), has_names };
        let mut node = parser.disjunction();
        // A stray `)` can't happen in a valid pattern; consume to the end regardless.
        while parser.i < p.len() {
            parser.i += 1;
            let rest = parser.disjunction();
            node = Node::Seq(vec![node, rest]);
        }
        let mut c = Compiler { prog: Vec::new(), regs: 0 };
        c.node(&node, false, flags);
        c.emit(Inst::Match);
        let names = parser
            .names
            .iter()
            .map(|(n, i)| {
                let s: String = n.iter().filter_map(|&c| char::from_u32(c)).collect();
                (s.encode_utf16().collect(), *i)
            })
            .collect();
        Regex { prog: c.prog, sets: parser.sets, flags, groups: parser.ngroups, names, regs: c.regs }
    }

    /// RegExpBuiltinExec's matcher at `start` (§22.2.7.2 step 13): no searching.
    pub fn match_at(
        &self,
        text: &[u16],
        start: usize,
        budget: &mut u64,
    ) -> Result<Option<Captures>, Exhausted> {
        let mut m = Matcher {
            re: self,
            text,
            caps: vec![None; 2 * (self.groups + 1)],
            regs: vec![0; self.regs],
            budget,
            flags: self.flags,
        };
        if let Some(end) = m.run(0, start, false)? {
            m.caps[0] = Some(start);
            m.caps[1] = Some(end);
            let mut out = Vec::with_capacity(self.groups + 1);
            for g in 0..=self.groups {
                out.push(match (m.caps[2 * g], m.caps[2 * g + 1]) {
                    (Some(a), Some(b)) => Some((a.min(b), a.max(b))),
                    _ => None,
                });
            }
            return Ok(Some(out));
        }
        Ok(None)
    }
}

enum Undo {
    Choice(usize, usize),
    /// A Star's state: (its pc, start position, position, count).
    Star(usize, usize, usize, u32),
    Cap(usize, Option<usize>),
    Reg(usize, usize),
    Flags(Flags),
    AllCaps(Vec<Option<usize>>),
}

struct Matcher<'a> {
    re: &'a Regex,
    text: &'a [u16],
    caps: Vec<Option<usize>>,
    regs: Vec<usize>,
    budget: &'a mut u64,
    flags: Flags,
}

fn is_line_terminator(c: u32) -> bool {
    matches!(c, 0x0A | 0x0D | 0x2028 | 0x2029)
}

impl Matcher<'_> {
    fn canon(&self, c: u32) -> u32 {
        if !self.flags.ignore_case {
            return c;
        }
        if self.flags.unicode {
            simple_fold(c)
        } else {
            upper_canon(c)
        }
    }

    /// The character after `pos` (forward) or before it (backward), and its width.
    fn read(&self, pos: usize, back: bool) -> Option<(u32, usize)> {
        if back {
            if pos == 0 {
                return None;
            }
            let lo = self.text[pos - 1] as u32;
            if self.flags.unicode && (0xDC00..0xE000).contains(&lo) && pos >= 2 {
                let hi = self.text[pos - 2] as u32;
                if (0xD800..0xDC00).contains(&hi) {
                    return Some((0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00), 2));
                }
            }
            Some((lo, 1))
        } else {
            let hi = *self.text.get(pos)? as u32;
            if self.flags.unicode && (0xD800..0xDC00).contains(&hi) {
                if let Some(&lo) = self.text.get(pos + 1) {
                    let lo = lo as u32;
                    if (0xDC00..0xE000).contains(&lo) {
                        return Some((0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00), 2));
                    }
                }
            }
            Some((hi, 1))
        }
    }

    fn set_has(&self, i: usize, c: u32) -> bool {
        let r = &self.re.sets[i].ranges;
        if contains(r, c) {
            return true;
        }
        if !self.flags.ignore_case {
            return false;
        }
        let cc = self.canon(c);
        if cc != c && contains(r, cc) {
            return true;
        }
        // Any member with the same canonical form.
        if self.flags.unicode {
            unicode::tables().fold_class.get(&c).is_some_and(|o| o.iter().any(|&x| contains(r, x)))
        } else {
            let t = unicode::tables();
            let mut cands: Vec<u32> = t.fold_class.get(&c).cloned().unwrap_or_default();
            if let Some(ch) = char::from_u32(c) {
                cands.extend(ch.to_lowercase().map(|x| x as u32));
                cands.extend(ch.to_uppercase().map(|x| x as u32));
            }
            cands.into_iter().any(|x| x <= 0xFFFF && upper_canon(x) == cc && contains(r, x))
        }
    }

    fn is_word(&self, pos: usize, back: bool) -> bool {
        match self.read(pos, back) {
            Some((c, _)) => {
                if contains(WORD, c) {
                    return true;
                }
                self.flags.unicode && self.flags.ignore_case && (c == 0x17F || c == 0x212A)
            }
            None => false,
        }
    }

    /// The width of the single-character matcher at `pc` matching forward at `pos`.
    fn item_at(&self, pc: usize, pos: usize) -> Option<usize> {
        let (x, w) = self.read(pos, false)?;
        let ok = match &self.re.prog[pc] {
            Inst::Char(c) => x == *c || (self.flags.ignore_case && self.canon(x) == self.canon(*c)),
            Inst::Any => self.flags.dot_all || !is_line_terminator(x),
            Inst::Set(i, neg) => self.set_has(*i, x) != *neg,
            _ => false,
        };
        ok.then_some(w)
    }

    /// Run from `pc` at `pos`; `Some(end)` on reaching Match/LookEnd.
    fn run(&mut self, pc0: usize, pos0: usize, back0: bool) -> Result<Option<usize>, Exhausted> {
        let mut stack: Vec<Undo> = Vec::new();
        let (mut pc, mut pos) = (pc0, pos0);
        let back = back0;
        let mut dir_back = back;
        loop {
            if *self.budget == 0 {
                return Err(Exhausted);
            }
            *self.budget -= 1;
            let ok = match &self.re.prog[pc] {
                Inst::Match | Inst::LookEnd => return Ok(Some(pos)),
                Inst::Char(c) => match self.read(pos, dir_back) {
                    Some((x, w))
                        if x == *c || (self.flags.ignore_case && self.canon(x) == self.canon(*c)) =>
                    {
                        pos = if dir_back { pos - w } else { pos + w };
                        pc += 1;
                        true
                    }
                    _ => false,
                },
                Inst::Any => match self.read(pos, dir_back) {
                    Some((x, w)) if self.flags.dot_all || !is_line_terminator(x) => {
                        pos = if dir_back { pos - w } else { pos + w };
                        pc += 1;
                        true
                    }
                    _ => false,
                },
                Inst::Set(i, neg) => match self.read(pos, dir_back) {
                    Some((x, w)) if self.set_has(*i, x) != *neg => {
                        pos = if dir_back { pos - w } else { pos + w };
                        pc += 1;
                        true
                    }
                    _ => false,
                },
                Inst::Start => {
                    let ok =
                        pos == 0 || (self.flags.multiline && is_line_terminator(self.text[pos - 1] as u32));
                    pc += 1;
                    ok
                }
                Inst::End => {
                    let ok = pos == self.text.len()
                        || (self.flags.multiline && is_line_terminator(self.text[pos] as u32));
                    pc += 1;
                    ok
                }
                Inst::WordB(want) => {
                    let a = self.is_word(pos, true);
                    let b = self.is_word(pos, false);
                    pc += 1;
                    (a != b) == *want
                }
                Inst::Split(a, b) => {
                    stack.push(Undo::Choice(*b, pos));
                    pc = *a;
                    true
                }
                Inst::Jmp(a) => {
                    pc = *a;
                    true
                }
                Inst::Save(slot) => {
                    stack.push(Undo::Cap(*slot, self.caps[*slot]));
                    self.caps[*slot] = Some(pos);
                    pc += 1;
                    true
                }
                Inst::ClearCaps(first, n) => {
                    for g in *first..*first + *n {
                        for s in [2 * g, 2 * g + 1] {
                            if s < self.caps.len() && self.caps[s].is_some() {
                                stack.push(Undo::Cap(s, self.caps[s]));
                                self.caps[s] = None;
                            }
                        }
                    }
                    pc += 1;
                    true
                }
                Inst::BackRef(groups) => {
                    // The group that participated (duplicate names: at most one did).
                    let span = groups.iter().find_map(|&g| {
                        match (
                            self.caps.get(2 * g).copied().flatten(),
                            self.caps.get(2 * g + 1).copied().flatten(),
                        ) {
                            (Some(a), Some(b)) => Some((a.min(b), a.max(b))),
                            _ => None,
                        }
                    });
                    match span {
                        None => {
                            pc += 1;
                            true
                        }
                        Some((a, b)) => {
                            let len = b - a;
                            let ok = if dir_back {
                                pos >= len && self.eq_units(a, pos - len, len)
                            } else {
                                pos + len <= self.text.len() && self.eq_units(a, pos, len)
                            };
                            if ok {
                                pos = if dir_back { pos - len } else { pos + len };
                                pc += 1;
                            }
                            ok
                        }
                    }
                }
                Inst::Look { ahead, negate, next } => {
                    let (ahead, negate, next) = (*ahead, *negate, *next);
                    let saved = self.caps.clone();
                    let saved_flags = self.flags;
                    let r = self.run(pc + 1, pos, !ahead)?;
                    self.flags = saved_flags;
                    match (r.is_some(), negate) {
                        (true, false) => {
                            stack.push(Undo::AllCaps(saved));
                            pc = next;
                            true
                        }
                        (false, true) => {
                            self.caps = saved;
                            pc = next;
                            true
                        }
                        _ => {
                            self.caps = saved;
                            false
                        }
                    }
                }
                Inst::CountInit(r) => {
                    stack.push(Undo::Reg(*r, self.regs[*r]));
                    self.regs[*r] = 0;
                    pc += 1;
                    true
                }
                Inst::Loop { r, min, max, greedy, body, exit } => {
                    let n = self.regs[*r];
                    if (n as u64) < *min as u64 {
                        pc = *body;
                    } else if max.is_some_and(|m| n as u64 >= m as u64) {
                        pc = *exit;
                    } else if *greedy {
                        stack.push(Undo::Choice(*exit, pos));
                        pc = *body;
                    } else {
                        stack.push(Undo::Choice(*body, pos));
                        pc = *exit;
                    }
                    true
                }
                Inst::PosSave(r) => {
                    stack.push(Undo::Reg(*r, self.regs[*r]));
                    self.regs[*r] = pos;
                    pc += 1;
                    true
                }
                Inst::LoopEnd { r, min, head } => {
                    let n = self.regs[*r];
                    // An iteration past the minimum that consumed nothing fails (§22.2.2.3.1).
                    if (n as u64) >= *min as u64 && self.regs[*r + 1] == pos {
                        false
                    } else {
                        stack.push(Undo::Reg(*r, n));
                        self.regs[*r] = n + 1;
                        pc = *head;
                        true
                    }
                }
                Inst::SetFlags(f) => {
                    stack.push(Undo::Flags(self.flags));
                    self.flags = *f;
                    dir_back = back;
                    pc += 1;
                    true
                }
                Inst::Star { min, max, greedy } => {
                    let (min, max, greedy) = (*min, *max, *greedy);
                    let start = pos;
                    let limit = if greedy { max.unwrap_or(u32::MAX) } else { min };
                    let mut n = 0u32;
                    while n < limit {
                        let Some(w) = self.item_at(pc + 1, pos) else { break };
                        if *self.budget == 0 {
                            return Err(Exhausted);
                        }
                        *self.budget -= 1;
                        pos += w;
                        n += 1;
                    }
                    if n < min {
                        false
                    } else {
                        stack.push(Undo::Star(pc, start, pos, n));
                        pc += 2;
                        true
                    }
                }
            };
            if ok {
                continue;
            }
            // Backtrack.
            loop {
                match stack.pop() {
                    None => return Ok(None),
                    Some(Undo::Choice(p, q)) => {
                        pc = p;
                        pos = q;
                        break;
                    }
                    Some(Undo::Star(spc, start, at, n)) => {
                        let Inst::Star { min, max, greedy } = self.re.prog[spc] else { continue };
                        if greedy {
                            // Give back one character.
                            if n > min {
                                let w = match self.read(at, true) {
                                    Some((_, w)) if at - w >= start => w,
                                    _ => 1,
                                };
                                stack.push(Undo::Star(spc, start, at - w, n - 1));
                                pc = spc + 2;
                                pos = at - w;
                                break;
                            }
                        } else if max.is_none_or(|m| n < m) {
                            // Take one more.
                            if let Some(w) = self.item_at(spc + 1, at) {
                                stack.push(Undo::Star(spc, start, at + w, n + 1));
                                pc = spc + 2;
                                pos = at + w;
                                break;
                            }
                        }
                    }
                    Some(Undo::Cap(s, v)) => self.caps[s] = v,
                    Some(Undo::Reg(r, v)) => self.regs[r] = v,
                    Some(Undo::Flags(f)) => self.flags = f,
                    Some(Undo::AllCaps(c)) => self.caps = c,
                }
            }
        }
    }

    fn eq_units(&self, a: usize, b: usize, len: usize) -> bool {
        if !self.flags.ignore_case {
            return self.text[a..a + len] == self.text[b..b + len];
        }
        let mut i = 0;
        while i < len {
            let Some((x, w)) = self.read(a + i, false) else { return false };
            let Some((y, _)) = self.read(b + i, false) else { return false };
            if self.canon(x) != self.canon(y) {
                return false;
            }
            i += w;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, f: &str, s: &str) -> Option<Vec<Option<String>>> {
        let flags = Flags {
            ignore_case: f.contains('i'),
            multiline: f.contains('m'),
            dot_all: f.contains('s'),
            unicode: f.contains('u') || f.contains('v'),
            sets: f.contains('v'),
        };
        let re = Regex::new(&p.encode_utf16().collect::<Vec<_>>(), flags);
        let t: Vec<u16> = s.encode_utf16().collect();
        let mut budget = 1_000_000;
        for start in 0..=t.len() {
            if let Some(c) = re.match_at(&t, start, &mut budget).unwrap() {
                return Some(
                    c.into_iter().map(|g| g.map(|(a, b)| String::from_utf16_lossy(&t[a..b]))).collect(),
                );
            }
        }
        None
    }

    fn s(v: &[Option<&str>]) -> Option<Vec<Option<String>>> {
        Some(v.iter().map(|x| x.map(String::from)).collect())
    }

    #[test]
    fn basics() {
        assert_eq!(m("a(b+)c", "", "xabbbc"), s(&[Some("abbbc"), Some("bbb")]));
        assert_eq!(m("a|ab", "", "abc"), s(&[Some("a")]));
        assert_eq!(m("(a*?)b", "", "aaab"), s(&[Some("aaab"), Some("aaa")]));
        assert_eq!(m("^\\d{2,3}$", "", "1234"), None);
        assert_eq!(m("(?<y>\\d{4})-\\k<y>", "", "2024-2024"), s(&[Some("2024-2024"), Some("2024")]));
        assert_eq!(m("(?<=\\$)\\d+", "", "cost $42"), s(&[Some("42")]));
        assert_eq!(m("(?<!\\$)\\b\\d+", "", "$4 5"), s(&[Some("5")]));
        assert_eq!(m("(a)|b", "", "b"), s(&[Some("b"), None]));
        assert_eq!(
            m("(z)((a+)?(b+)?(c))*", "", "zaacbbbcac"),
            s(&[Some("zaacbbbcac"), Some("z"), Some("ac"), Some("a"), None, Some("c")])
        );
        assert_eq!(m("(a*)*", "", "b"), s(&[Some(""), None]));
        assert_eq!(m("ABC", "i", "xabc"), s(&[Some("abc")]));
        assert_eq!(m("^b", "m", "a\nb"), s(&[Some("b")]));
        assert_eq!(m("a.c", "s", "a\nc"), s(&[Some("a\nc")]));
        assert_eq!(m(".", "u", "😀"), s(&[Some("😀")]));
        assert_eq!(m("\\p{Script=Greek}+", "u", "abγδε"), s(&[Some("γδε")]));
        assert_eq!(m("[\\p{L}--[a-z]]+", "v", "abcDEF"), s(&[Some("DEF")]));
        assert_eq!(m("\\1(a)", "", "aa"), s(&[Some("a"), Some("a")]));
        assert_eq!(m("a{2}", "", "aaa"), s(&[Some("aa")]));
        assert_eq!(m("x{1,", "", "x{1,"), s(&[Some("x{1,")]));
        assert_eq!(m("[\\b]", "", "\u{8}"), s(&[Some("\u{8}")]));
        assert_eq!(m("\\u{1F600}", "u", "😀"), s(&[Some("😀")]));
    }

    #[test]
    fn backtracking_is_bounded() {
        let re = Regex::new(&"(a*)*b".encode_utf16().collect::<Vec<_>>(), Flags::default());
        let t: Vec<u16> = "a".repeat(40).encode_utf16().collect();
        let mut budget = 100_000;
        assert!(re.match_at(&t, 0, &mut budget).is_err());
    }
}

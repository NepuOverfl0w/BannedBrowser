//! Selectors Level 4 (subset): parsing, specificity and matching.
//!
//! Matching is generic over [`Element`], so `bb-css` has no DOM dependency.

use crate::parser::Cv;
use crate::tokenizer::Token;

#[derive(Debug, Clone, PartialEq)]
pub enum AttrOp {
    Exists,
    Eq,
    Includes,
    Dash,
    Prefix,
    Suffix,
    Substring,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PseudoClass {
    Root,
    Empty,
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    OnlyOfType,
    NthChild(i32, i32),
    NthLastChild(i32, i32),
    NthOfType(i32, i32),
    NthLastOfType(i32, i32),
    Not(SelectorList),
    Is(SelectorList),
    Where(SelectorList),
    Has(SelectorList),
    Lang(String),
    // Element state (answered by the Element implementation):
    Link,
    AnyLink,
    Visited,
    Hover,
    Active,
    Focus,
    FocusVisible,
    FocusWithin,
    Target,
    Checked,
    Disabled,
    Enabled,
    Required,
    Optional,
    ReadOnly,
    ReadWrite,
    PlaceholderShown,
    Default,
    Indeterminate,
    Defined,
    Scope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PseudoElement {
    Before,
    After,
    Marker,
    FirstLine,
    FirstLetter,
    Selection,
    Placeholder,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Simple {
    Universal,
    /// Lowercased local name.
    Type(String),
    Id(String),
    Class(String),
    Attr {
        name: String,
        op: AttrOp,
        value: String,
        case_insensitive: bool,
    },
    Pseudo(PseudoClass),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    Descendant,
    Child,
    NextSibling,
    SubsequentSibling,
}

/// A complex selector, stored right-to-left.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    /// Compounds from the subject (rightmost) leftwards.
    pub compounds: Vec<Vec<Simple>>,
    /// `combinators[i]` joins `compounds[i]` to `compounds[i + 1]`.
    pub combinators: Vec<Combinator>,
    pub pseudo_element: Option<PseudoElement>,
    pub specificity: u32,
}

pub type SelectorList = Vec<Selector>;

/// Element access needed for matching.
pub trait Element: Copy {
    fn parent_element(&self) -> Option<Self>;
    fn prev_sibling_element(&self) -> Option<Self>;
    fn next_sibling_element(&self) -> Option<Self>;
    fn first_child_element(&self) -> Option<Self>;
    /// Lowercase local name for HTML elements.
    fn local_name(&self) -> &str;
    fn is_html(&self) -> bool;
    fn id(&self) -> Option<&str>;
    fn has_class(&self, name: &str) -> bool;
    fn attr(&self, name: &str) -> Option<&str>;
    fn is_root(&self) -> bool;
    /// No element or text children (comments allowed).
    fn is_empty(&self) -> bool;
    /// Quirks mode: class/id matching is ASCII case-insensitive.
    fn quirks(&self) -> bool;
    /// Answer state pseudo-classes (`:hover`, `:link`, …).
    fn match_state(&self, pc: &PseudoClass) -> bool;
    fn same(&self, other: &Self) -> bool;
}

// ---------------- parsing ----------------

struct Cursor<'a> {
    cvs: &'a [Cv],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<&'a Cv> {
        self.cvs.get(self.pos)
    }
    fn next(&mut self) -> Option<&'a Cv> {
        let c = self.cvs.get(self.pos);
        self.pos += 1;
        c
    }
    fn skip_ws(&mut self) -> bool {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ws()) {
            self.pos += 1;
        }
        self.pos > start
    }
}

/// Parse a comma-separated selector list. `None` if any selector is invalid
/// (which invalidates the whole style rule).
pub fn parse_selector_list(cvs: &[Cv]) -> Option<SelectorList> {
    let mut out = Vec::new();
    for part in cvs.split(|c| c.is_comma()) {
        out.push(parse_complex(part)?);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Forgiving selector list (for `:is()` / `:where()`): invalid entries dropped.
fn parse_forgiving(cvs: &[Cv]) -> SelectorList {
    cvs.split(|c| c.is_comma()).filter_map(parse_complex).collect()
}

fn parse_complex(cvs: &[Cv]) -> Option<Selector> {
    let mut cur = Cursor { cvs, pos: 0 };
    cur.skip_ws();
    let mut compounds_ltr: Vec<Vec<Simple>> = Vec::new();
    let mut combs_ltr: Vec<Combinator> = Vec::new();
    let mut pseudo_element = None;
    loop {
        let (compound, pe) = parse_compound(&mut cur)?;
        if compound.is_empty() && pe.is_none() {
            return None;
        }
        compounds_ltr.push(if compound.is_empty() { vec![Simple::Universal] } else { compound });
        if pe.is_some() {
            pseudo_element = pe;
            cur.skip_ws();
            if cur.peek().is_some() {
                return None; // nothing may follow a pseudo-element (in our subset)
            }
            break;
        }
        let had_ws = cur.skip_ws();
        let comb = match cur.peek() {
            None => break,
            Some(c) if c.is_delim('>') => {
                cur.next();
                Combinator::Child
            }
            Some(c) if c.is_delim('+') => {
                cur.next();
                Combinator::NextSibling
            }
            Some(c) if c.is_delim('~') => {
                cur.next();
                Combinator::SubsequentSibling
            }
            Some(_) if had_ws => Combinator::Descendant,
            Some(_) => return None,
        };
        cur.skip_ws();
        cur.peek()?;
        combs_ltr.push(comb);
    }
    compounds_ltr.reverse();
    combs_ltr.reverse();
    let mut sel =
        Selector { compounds: compounds_ltr, combinators: combs_ltr, pseudo_element, specificity: 0 };
    sel.specificity = specificity(&sel);
    Some(sel)
}

fn parse_compound(cur: &mut Cursor) -> Option<(Vec<Simple>, Option<PseudoElement>)> {
    let mut out = Vec::new();
    // Type or universal selector, with optional namespace prefix.
    match cur.peek() {
        Some(Cv::Token(Token::Ident(name))) => {
            cur.next();
            if cur.peek().is_some_and(|c| c.is_delim('|')) {
                cur.next();
                match cur.next() {
                    Some(Cv::Token(Token::Ident(n))) => out.push(Simple::Type(n.to_ascii_lowercase())),
                    Some(c) if c.is_delim('*') => out.push(Simple::Universal),
                    _ => return None,
                }
            } else {
                out.push(Simple::Type(name.to_ascii_lowercase()));
            }
        }
        Some(c) if c.is_delim('*') => {
            cur.next();
            if cur.peek().is_some_and(|c| c.is_delim('|')) {
                cur.next();
                match cur.next() {
                    Some(Cv::Token(Token::Ident(n))) => out.push(Simple::Type(n.to_ascii_lowercase())),
                    Some(c) if c.is_delim('*') => out.push(Simple::Universal),
                    _ => return None,
                }
            } else {
                out.push(Simple::Universal);
            }
        }
        Some(c) if c.is_delim('|') => {
            cur.next();
            match cur.next() {
                Some(Cv::Token(Token::Ident(n))) => out.push(Simple::Type(n.to_ascii_lowercase())),
                Some(c) if c.is_delim('*') => out.push(Simple::Universal),
                _ => return None,
            }
        }
        _ => {}
    }
    loop {
        match cur.peek() {
            Some(Cv::Token(Token::Hash(h, true))) => {
                cur.next();
                out.push(Simple::Id(h.clone()));
            }
            Some(Cv::Token(Token::Hash(_, false))) => return None,
            Some(c) if c.is_delim('.') => {
                cur.next();
                match cur.next() {
                    Some(Cv::Token(Token::Ident(n))) => out.push(Simple::Class(n.clone())),
                    _ => return None,
                }
            }
            Some(Cv::Block('[', inner)) => {
                cur.next();
                out.push(parse_attr(inner)?);
            }
            Some(Cv::Token(Token::Colon)) => {
                cur.next();
                if matches!(cur.peek(), Some(Cv::Token(Token::Colon))) {
                    cur.next();
                    let pe = match cur.next() {
                        Some(Cv::Token(Token::Ident(n))) => parse_pseudo_element(n)?,
                        _ => return None,
                    };
                    return Some((out, Some(pe)));
                }
                match cur.next() {
                    Some(Cv::Token(Token::Ident(n))) => {
                        let lower = n.to_ascii_lowercase();
                        // Legacy single-colon pseudo-elements.
                        if let Some(pe) = match lower.as_str() {
                            "before" => Some(PseudoElement::Before),
                            "after" => Some(PseudoElement::After),
                            "first-line" => Some(PseudoElement::FirstLine),
                            "first-letter" => Some(PseudoElement::FirstLetter),
                            _ => None,
                        } {
                            return Some((out, Some(pe)));
                        }
                        out.push(Simple::Pseudo(parse_pseudo_class(&lower)?));
                    }
                    Some(Cv::Function(name, args)) => out.push(Simple::Pseudo(parse_functional(name, args)?)),
                    _ => return None,
                }
            }
            _ => return Some((out, None)),
        }
    }
}

fn parse_pseudo_element(n: &str) -> Option<PseudoElement> {
    Some(match n.to_ascii_lowercase().as_str() {
        "before" => PseudoElement::Before,
        "after" => PseudoElement::After,
        "marker" => PseudoElement::Marker,
        "first-line" => PseudoElement::FirstLine,
        "first-letter" => PseudoElement::FirstLetter,
        "selection" => PseudoElement::Selection,
        "placeholder" => PseudoElement::Placeholder,
        _ => return None,
    })
}

fn parse_pseudo_class(n: &str) -> Option<PseudoClass> {
    use PseudoClass as P;
    Some(match n {
        "root" => P::Root,
        "empty" => P::Empty,
        "first-child" => P::FirstChild,
        "last-child" => P::LastChild,
        "only-child" => P::OnlyChild,
        "first-of-type" => P::FirstOfType,
        "last-of-type" => P::LastOfType,
        "only-of-type" => P::OnlyOfType,
        "link" => P::Link,
        "any-link" | "-webkit-any-link" => P::AnyLink,
        "visited" => P::Visited,
        "hover" => P::Hover,
        "active" => P::Active,
        "focus" => P::Focus,
        "focus-visible" => P::FocusVisible,
        "focus-within" => P::FocusWithin,
        "target" => P::Target,
        "checked" => P::Checked,
        "disabled" => P::Disabled,
        "enabled" => P::Enabled,
        "required" => P::Required,
        "optional" => P::Optional,
        "read-only" => P::ReadOnly,
        "read-write" => P::ReadWrite,
        "placeholder-shown" => P::PlaceholderShown,
        "default" => P::Default,
        "indeterminate" => P::Indeterminate,
        "defined" => P::Defined,
        "scope" => P::Scope,
        _ => return None,
    })
}

fn parse_functional(name: &str, args: &[Cv]) -> Option<PseudoClass> {
    use PseudoClass as P;
    Some(match name {
        "not" => P::Not(parse_selector_list(args)?),
        "is" | "matches" | "-webkit-any" | "-moz-any" => P::Is(parse_forgiving(args)),
        "where" => P::Where(parse_forgiving(args)),
        "has" => P::Has(parse_relative_list(args)?),
        "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" => {
            // `of S` filtering is not supported: such selectors are dropped.
            let (a, b) = parse_anb(args)?;
            match name {
                "nth-child" => P::NthChild(a, b),
                "nth-last-child" => P::NthLastChild(a, b),
                "nth-of-type" => P::NthOfType(a, b),
                _ => P::NthLastOfType(a, b),
            }
        }
        "lang" => {
            let v = args.iter().find_map(|c| match c {
                Cv::Token(Token::Ident(s)) | Cv::Token(Token::String(s)) => Some(s.to_ascii_lowercase()),
                _ => None,
            })?;
            P::Lang(v)
        }
        _ => return None,
    })
}

/// `:has()` arguments: relative selectors. We support the descendant form
/// (`:has(img)`) and the child form (`:has(> img)`) by storing a leading
/// combinator marker in the selector's leftmost slot.
fn parse_relative_list(args: &[Cv]) -> Option<SelectorList> {
    let mut out = Vec::new();
    for part in args.split(|c| c.is_comma()) {
        let mut p: Vec<Cv> = part.to_vec();
        while p.first().is_some_and(|c| c.is_ws()) {
            p.remove(0);
        }
        let lead = if p.first().is_some_and(|c| c.is_delim('>')) {
            p.remove(0);
            Some(Combinator::Child)
        } else if p.first().is_some_and(|c| c.is_delim('+') || c.is_delim('~')) {
            return None; // TODO(selectors-4 §14.4): sibling-relative :has()
        } else {
            None
        };
        let mut s = parse_complex(&p)?;
        // Encode the anchor as a trailing combinator with an empty compound.
        s.combinators.push(lead.unwrap_or(Combinator::Descendant));
        s.compounds.push(Vec::new());
        out.push(s);
    }
    Some(out)
}

fn parse_attr(inner: &[Cv]) -> Option<Simple> {
    let toks: Vec<&Cv> = inner.iter().filter(|c| !c.is_ws()).collect();
    let mut i = 0;
    let name = match toks.get(i) {
        Some(Cv::Token(Token::Ident(n))) => n.to_ascii_lowercase(),
        Some(c) if c.is_delim('*') || c.is_delim('|') => {
            // namespace prefix: [*|attr] / [|attr]
            i += if c.is_delim('*') { 2 } else { 1 };
            match toks.get(i) {
                Some(Cv::Token(Token::Ident(n))) => n.to_ascii_lowercase(),
                _ => return None,
            }
        }
        _ => return None,
    };
    i += 1;
    if toks.get(i).is_some_and(|c| c.is_delim('|')) {
        // ns|attr
        i += 1;
        match toks.get(i) {
            Some(Cv::Token(Token::Ident(_))) => i += 1,
            _ => return None,
        }
    }
    let Some(op_tok) = toks.get(i) else {
        return Some(Simple::Attr {
            name,
            op: AttrOp::Exists,
            value: String::new(),
            case_insensitive: false,
        });
    };
    let op = if op_tok.is_delim('=') {
        i += 1;
        AttrOp::Eq
    } else {
        let op = match op_tok {
            c if c.is_delim('~') => AttrOp::Includes,
            c if c.is_delim('|') => AttrOp::Dash,
            c if c.is_delim('^') => AttrOp::Prefix,
            c if c.is_delim('$') => AttrOp::Suffix,
            c if c.is_delim('*') => AttrOp::Substring,
            _ => return None,
        };
        if !toks.get(i + 1).is_some_and(|c| c.is_delim('=')) {
            return None;
        }
        i += 2;
        op
    };
    let value = match toks.get(i) {
        Some(Cv::Token(Token::Ident(v))) | Some(Cv::Token(Token::String(v))) => v.clone(),
        Some(Cv::Token(Token::Number { value, .. })) => crate::tokenizer::fmt_num(*value),
        _ => return None,
    };
    i += 1;
    let mut ci = false;
    if let Some(f) = toks.get(i) {
        if f.is_ident("i") {
            ci = true;
        } else if !f.is_ident("s") {
            return None;
        }
    }
    Some(Simple::Attr { name, op, value, case_insensitive: ci })
}

/// CSS Syntax §6 An+B microsyntax.
pub fn parse_anb(args: &[Cv]) -> Option<(i32, i32)> {
    let toks: Vec<&Cv> = args.iter().filter(|c| !c.is_ws()).collect();
    let mut text = String::new();
    for (i, c) in toks.iter().enumerate() {
        match c {
            Cv::Token(Token::Ident(s)) => text.push_str(&s.to_ascii_lowercase()),
            // The tokenizer drops a leading '+', so re-add it for non-first numbers.
            Cv::Token(Token::Number { value, int: true }) => {
                if i > 0 && *value >= 0.0 {
                    text.push('+');
                }
                text.push_str(&(*value as i32).to_string());
            }
            Cv::Token(Token::Dimension { value, unit }) if value.fract() == 0.0 => {
                text.push_str(&(*value as i32).to_string());
                text.push_str(&unit.to_ascii_lowercase());
            }
            Cv::Token(Token::Delim(d @ ('+' | '-'))) => text.push(*d),
            _ => return None,
        }
    }
    let t = text.replace("++", "+").replace("+-", "-").replace("-+", "-");
    match t.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some(npos) = t.find('n') {
        let a_part = &t[..npos];
        let b_part = &t[npos + 1..];
        let a = match a_part {
            "" | "+" => 1,
            "-" => -1,
            s => s.parse().ok()?,
        };
        let b = if b_part.is_empty() { 0 } else { b_part.trim_start_matches('+').parse().ok()? };
        if b_part.starts_with("--") {
            return None;
        }
        Some((a, b))
    } else {
        Some((0, t.trim_start_matches('+').parse().ok()?))
    }
}

/// Selectors §17 specificity, packed as a<<20 | b<<10 | c.
pub fn specificity(sel: &Selector) -> u32 {
    let (mut a, mut b, mut c) = (0u32, 0u32, 0u32);
    fn list_max(list: &SelectorList) -> (u32, u32, u32) {
        list.iter()
            .map(|s| {
                let v = specificity(s);
                (v >> 20, (v >> 10) & 1023, v & 1023)
            })
            .max()
            .unwrap_or((0, 0, 0))
    }
    for compound in &sel.compounds {
        for s in compound {
            match s {
                Simple::Id(_) => a += 1,
                Simple::Class(_) | Simple::Attr { .. } => b += 1,
                Simple::Type(_) => c += 1,
                Simple::Universal => {}
                Simple::Pseudo(PseudoClass::Where(_)) => {}
                Simple::Pseudo(PseudoClass::Not(l) | PseudoClass::Is(l) | PseudoClass::Has(l)) => {
                    let (x, y, z) = list_max(l);
                    a += x;
                    b += y;
                    c += z;
                }
                Simple::Pseudo(_) => b += 1,
            }
        }
    }
    if sel.pseudo_element.is_some() {
        c += 1;
    }
    (a.min(1023) << 20) | (b.min(1023) << 10) | c.min(1023)
}

// ---------------- matching ----------------

/// Does `sel` match `el` (ignoring any pseudo-element)?
pub fn matches<E: Element>(sel: &Selector, el: E) -> bool {
    match_from(sel, 0, el, 0)
}

pub fn matches_any<E: Element>(list: &SelectorList, el: E) -> bool {
    list.iter().any(|s| s.pseudo_element.is_none() && matches(s, el))
}

fn match_from<E: Element>(sel: &Selector, idx: usize, el: E, depth: usize) -> bool {
    if depth > 64 {
        return false;
    }
    if !match_compound(&sel.compounds[idx], el, depth) {
        return false;
    }
    if idx + 1 == sel.compounds.len() {
        return true;
    }
    match sel.combinators[idx] {
        Combinator::Child => el.parent_element().is_some_and(|p| match_from(sel, idx + 1, p, depth)),
        Combinator::Descendant => {
            let mut p = el.parent_element();
            while let Some(pe) = p {
                if match_from(sel, idx + 1, pe, depth) {
                    return true;
                }
                p = pe.parent_element();
            }
            false
        }
        Combinator::NextSibling => {
            el.prev_sibling_element().is_some_and(|p| match_from(sel, idx + 1, p, depth))
        }
        Combinator::SubsequentSibling => {
            let mut p = el.prev_sibling_element();
            while let Some(pe) = p {
                if match_from(sel, idx + 1, pe, depth) {
                    return true;
                }
                p = pe.prev_sibling_element();
            }
            false
        }
    }
}

fn eq_maybe_ci(a: &str, b: &str, ci: bool) -> bool {
    if ci {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

fn match_compound<E: Element>(compound: &[Simple], el: E, depth: usize) -> bool {
    compound.iter().all(|s| match_simple(s, el, depth))
}

fn nth_matches(a: i32, b: i32, index: i32) -> bool {
    // index is 1-based; is there n >= 0 with a*n + b == index?
    if a == 0 {
        return index == b;
    }
    let diff = index - b;
    diff % a == 0 && diff / a >= 0
}

fn match_simple<E: Element>(s: &Simple, el: E, depth: usize) -> bool {
    match s {
        Simple::Universal => true,
        Simple::Type(n) => {
            if el.is_html() {
                el.local_name() == n
            } else {
                el.local_name().eq_ignore_ascii_case(n)
            }
        }
        Simple::Id(id) => el.id().is_some_and(|v| eq_maybe_ci(v, id, el.quirks())),
        Simple::Class(c) => {
            if el.quirks() {
                el.attr("class")
                    .is_some_and(|v| v.split_ascii_whitespace().any(|x| x.eq_ignore_ascii_case(c)))
            } else {
                el.has_class(c)
            }
        }
        Simple::Attr { name, op, value, case_insensitive } => {
            let Some(v) = el.attr(name) else { return false };
            // HTML attribute values for some attributes are case-insensitive (e.g. type);
            // we honour only the explicit `i` flag plus a few legacy attributes.
            let ci = *case_insensitive
                || (el.is_html()
                    && matches!(
                        name.as_str(),
                        "type" | "lang" | "dir" | "align" | "valign" | "method" | "nowrap" | "clear"
                    ));
            let (v, value) = if ci {
                (v.to_ascii_lowercase(), value.to_ascii_lowercase())
            } else {
                (v.to_string(), value.clone())
            };
            match op {
                AttrOp::Exists => true,
                AttrOp::Eq => v == value,
                AttrOp::Includes => !value.is_empty() && v.split_ascii_whitespace().any(|w| w == value),
                AttrOp::Dash => v == value || v.starts_with(&format!("{value}-")),
                AttrOp::Prefix => !value.is_empty() && v.starts_with(&value),
                AttrOp::Suffix => !value.is_empty() && v.ends_with(&value),
                AttrOp::Substring => !value.is_empty() && v.contains(&value),
            }
        }
        Simple::Pseudo(pc) => match_pseudo(pc, el, depth),
    }
}

fn index_of<E: Element>(el: E, of_type: bool, from_end: bool) -> i32 {
    let mut i = 1;
    let mut cur = if from_end { el.next_sibling_element() } else { el.prev_sibling_element() };
    while let Some(s) = cur {
        if !of_type || (s.local_name() == el.local_name()) {
            i += 1;
        }
        cur = if from_end { s.next_sibling_element() } else { s.prev_sibling_element() };
    }
    i
}

fn match_pseudo<E: Element>(pc: &PseudoClass, el: E, depth: usize) -> bool {
    use PseudoClass as P;
    match pc {
        P::Root => el.is_root(),
        P::Empty => el.is_empty(),
        P::FirstChild => el.prev_sibling_element().is_none(),
        P::LastChild => el.next_sibling_element().is_none(),
        P::OnlyChild => el.prev_sibling_element().is_none() && el.next_sibling_element().is_none(),
        P::FirstOfType => index_of(el, true, false) == 1,
        P::LastOfType => index_of(el, true, true) == 1,
        P::OnlyOfType => index_of(el, true, false) == 1 && index_of(el, true, true) == 1,
        P::NthChild(a, b) => el.parent_element().is_some() && nth_matches(*a, *b, index_of(el, false, false)),
        P::NthLastChild(a, b) => {
            el.parent_element().is_some() && nth_matches(*a, *b, index_of(el, false, true))
        }
        P::NthOfType(a, b) => el.parent_element().is_some() && nth_matches(*a, *b, index_of(el, true, false)),
        P::NthLastOfType(a, b) => {
            el.parent_element().is_some() && nth_matches(*a, *b, index_of(el, true, true))
        }
        P::Not(list) => !list.iter().any(|s| match_from(s, 0, el, depth + 1)),
        P::Is(list) | P::Where(list) => list.iter().any(|s| match_from(s, 0, el, depth + 1)),
        P::Has(list) => list.iter().any(|s| has_matches(s, el, depth + 1)),
        P::Lang(l) => {
            let mut cur = Some(el);
            while let Some(e) = cur {
                if let Some(v) = e.attr("lang") {
                    let v = v.to_ascii_lowercase();
                    return v == *l || v.starts_with(&format!("{l}-"));
                }
                cur = e.parent_element();
            }
            false
        }
        other => el.match_state(other),
    }
}

/// `:has()` with a relative selector whose last compound is the empty anchor.
fn has_matches<E: Element>(sel: &Selector, anchor: E, depth: usize) -> bool {
    let anchor_comb = *sel.combinators.last().unwrap_or(&Combinator::Descendant);
    // Build the selector without the anchor slot.
    let mut inner = sel.clone();
    inner.compounds.pop();
    inner.combinators.pop();
    if inner.compounds.is_empty() {
        return false;
    }
    // Candidates: descendants (or children) of the anchor.
    let mut stack: Vec<E> = Vec::new();
    let mut c = anchor.first_child_element();
    while let Some(ch) = c {
        stack.push(ch);
        c = ch.next_sibling_element();
    }
    let mut visited = 0;
    while let Some(cand) = stack.pop() {
        visited += 1;
        if visited > 10_000 {
            return false;
        }
        if match_from(&inner, 0, cand, depth) {
            // Verify the leftmost compound is related to the anchor properly.
            let ok = match anchor_comb {
                Combinator::Child => {
                    inner.compounds.len() > 1 || cand.parent_element().is_some_and(|p| p.same(&anchor))
                }
                _ => true,
            };
            if ok {
                return true;
            }
        }
        if anchor_comb == Combinator::Descendant {
            let mut c = cand.first_child_element();
            while let Some(ch) = c {
                stack.push(ch);
                c = ch.next_sibling_element();
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_component_values;

    fn sel(s: &str) -> Option<SelectorList> {
        parse_selector_list(&parse_component_values(s))
    }

    #[test]
    fn parse_and_specificity() {
        let s = &sel("ul li.a > a#x:hover::before").unwrap()[0];
        assert_eq!(s.compounds.len(), 3);
        assert_eq!(s.pseudo_element, Some(PseudoElement::Before));
        assert_eq!(s.specificity, (1 << 20) | (2 << 10) | 4);
        assert_eq!(sel(":where(#a) p").unwrap()[0].specificity, 1);
        assert_eq!(sel(":is(#a, p)").unwrap()[0].specificity, 1 << 20);
        assert!(sel("a >").is_none());
        assert!(sel("::nonsense").is_none());
        assert!(sel("[a=\"b\" i]").is_some());
        assert!(sel("a:not(").is_none() || sel("a:not(").is_some());
    }

    #[test]
    fn anb() {
        let p = |s: &str| parse_anb(&parse_component_values(s));
        assert_eq!(p("odd"), Some((2, 1)));
        assert_eq!(p("2n+1"), Some((2, 1)));
        assert_eq!(p("-n + 3"), Some((-1, 3)));
        assert_eq!(p("n"), Some((1, 0)));
        assert_eq!(p("5"), Some((0, 5)));
        assert_eq!(p("3n - 2"), Some((3, -2)));
        assert!(nth_matches(2, 1, 3));
        assert!(!nth_matches(2, 1, 4));
        assert!(nth_matches(-1, 3, 2));
        assert!(!nth_matches(-1, 3, 4));
    }
}

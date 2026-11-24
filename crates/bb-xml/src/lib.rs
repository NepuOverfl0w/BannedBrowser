//! `bb-xml` — an XML parser for BannedBrowser: XML 1.0 (Fifth Edition) with Namespaces in
//! XML 1.0 (Third Edition), building a [`bb_dom::Document`]; and the XML serializer of the
//! DOM Parsing spec ([`serialize`]).
//!
//! The parser is non-validating and reads no external entities (like browsers): the internal
//! DTD subset's general entities, attribute defaults and attribute types are honoured;
//! external parsed entities expand to nothing; a reference to an undeclared entity is an
//! error unless the document has an external subset or parameter entity references (then
//! "Entity Declared" is a validity constraint, XML §4.1, and the reference is skipped).
//! Any well-formedness or namespace error stops the parse with an [`Error`] (XML §5.1:
//! fatal errors).

pub mod serialize;

use std::collections::HashMap;

use bb_dom::{Attribute, Document, Namespace, NodeData, NodeId};

pub const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";
pub const XMLNS_NS: &str = "http://www.w3.org/2000/xmlns/";

/// Nested elements past this depth are an error rather than a stack overflow.
const MAX_DEPTH: usize = 2048;
/// Entity references nested past this depth are an error.
const MAX_ENTITY_DEPTH: usize = 64;
/// Characters entity expansion may produce in all (against "billion laughs" documents).
const MAX_EXPANSION: usize = 16 << 20;

/// A fatal error, with where it was found (1-based line and column, in characters).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}, column {}: {}", self.line, self.column, self.message)
    }
}

impl std::error::Error for Error {}

type R<T> = Result<T, Error>;

/// XML §2.2 Char.
pub fn is_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}

/// XML §2.3 NameStartChar.
pub fn is_name_start(c: char) -> bool {
    matches!(c,
        ':' | 'A'..='Z' | '_' | 'a'..='z' | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}'
        | '\u{F8}'..='\u{2FF}' | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}'
        | '\u{200C}'..='\u{200D}' | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}'
        | '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}'
        | '\u{10000}'..='\u{EFFFF}')
}

/// XML §2.3 NameChar.
pub fn is_name_char(c: char) -> bool {
    is_name_start(c)
        || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

/// XML §2.3 Name.
pub fn is_name(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(is_name_start) && cs.all(is_name_char)
}

/// Namespaces in XML §3 NCName (a Name without colons).
pub fn is_ncname(s: &str) -> bool {
    is_name(s) && !s.contains(':')
}

/// Namespaces in XML §3 QName: (prefix, local name), if `s` is one.
pub fn split_qname(s: &str) -> Option<(Option<&str>, &str)> {
    match s.split_once(':') {
        None => is_ncname(s).then_some((None, s)),
        Some((p, l)) => (is_ncname(p) && is_ncname(l)).then_some((Some(p), l)),
    }
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

#[derive(Clone, Debug)]
enum Entity {
    /// Replacement text (XML §4.5).
    Internal(String),
    /// An external entity: never read; `unparsed` for NDATA ones.
    External { unparsed: bool },
}

#[derive(Clone, Debug)]
struct AttDef {
    name: String,
    /// CDATA (else tokenized or enumerated: values are normalized further, XML §3.3.3).
    cdata: bool,
    /// The default value (normalized), for #FIXED and plain defaults.
    default: Option<String>,
}

/// The XML declaration's fields (XML §2.8).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct XmlDecl {
    pub version: String,
    pub encoding: Option<String>,
    pub standalone: Option<bool>,
}

struct Parser {
    s: Vec<char>,
    i: usize,
    /// Outer buffers while reading an entity's replacement text: (buffer, position, name).
    stack: Vec<(Vec<char>, usize, String)>,
    doc: Document,
    general: HashMap<String, Entity>,
    parameter: HashMap<String, Entity>,
    attlists: HashMap<String, Vec<AttDef>>,
    standalone: bool,
    /// An external subset or a parameter entity reference was seen (XML §4.1 WFC: Entity
    /// Declared then doesn't apply).
    external_refs: bool,
    /// Declarations after an unread external parameter entity aren't processed (XML §5.1).
    skip_decls: bool,
    expanded: usize,
    /// In-scope namespace bindings (prefix, URI), innermost last.
    ns: Vec<(Option<String>, String)>,
    depth: usize,
    /// Character data waiting to become a Text node of `text_parent`.
    text: String,
    text_parent: NodeId,
    decl: XmlDecl,
}

/// Parse a whole XML document.
pub fn parse(input: &str) -> Result<Document, Error> {
    parse_with_decl(input).map(|(d, _)| d)
}

/// Parse a whole XML document; also returns its XML declaration (version "1.0" if none).
pub fn parse_with_decl(input: &str) -> Result<(Document, XmlDecl), Error> {
    let mut p = Parser::new(input);
    p.document()?;
    let decl = std::mem::take(&mut p.decl);
    Ok((p.doc, decl))
}

/// The XML fragment parsing algorithm (HTML §13.4): `markup` parsed as the content of
/// an element with the namespace bindings `in_scope` (prefix → URI, outermost first; a
/// None prefix is the default namespace). The returned document's root has the parsed
/// nodes as its children (in a DocumentFragment-like position).
pub fn parse_fragment(markup: &str, in_scope: &[(Option<String>, String)]) -> Result<Document, Error> {
    let mut p = Parser::new(markup);
    if let Some(c) = p.s.iter().position(|&c| !is_char(c)) {
        p.i = c;
        return p.err(format!("U+{:04X} is not allowed in XML", p.s[c] as u32));
    }
    p.ns.extend(in_scope.iter().cloned());
    p.content(Document::ROOT)?;
    p.flush_text();
    if !p.eof() {
        return p.err("an end tag without a start tag");
    }
    Ok(p.doc)
}

impl Parser {
    fn new(input: &str) -> Parser {
        // XML §2.11 end-of-line handling.
        let mut s: Vec<char> = Vec::with_capacity(input.len());
        let mut it = input.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\r' {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                s.push('\n');
            } else {
                s.push(c);
            }
        }
        if s.first() == Some(&'\u{FEFF}') {
            s.remove(0);
        }
        Parser {
            s,
            i: 0,
            stack: Vec::new(),
            doc: Document::new(),
            general: HashMap::new(),
            parameter: HashMap::new(),
            attlists: HashMap::new(),
            standalone: false,
            external_refs: false,
            skip_decls: false,
            expanded: 0,
            ns: vec![(Some("xml".to_string()), XML_NS.to_string())],
            depth: 0,
            text: String::new(),
            text_parent: Document::ROOT,
            decl: XmlDecl { version: "1.0".to_string(), ..Default::default() },
        }
    }

    // ---- input ----

    fn err<T>(&self, msg: impl Into<String>) -> R<T> {
        let (buf, pos) = match self.stack.first() {
            Some((b, p, _)) => (b.as_slice(), *p),
            None => (self.s.as_slice(), self.i),
        };
        let pos = pos.min(buf.len());
        let line = buf[..pos].iter().filter(|&&c| c == '\n').count() + 1;
        let column = pos - buf[..pos].iter().rposition(|&c| c == '\n').map_or(0, |x| x + 1) + 1;
        Err(Error { line, column, message: msg.into() })
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn at(&self, lit: &str) -> bool {
        (self.i..).zip(lit.chars()).all(|(j, c)| self.s.get(j) == Some(&c))
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self.at(lit) {
            self.i += lit.chars().count();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, lit: &str) -> R<()> {
        if self.eat(lit) {
            Ok(())
        } else {
            self.err(format!("expected '{lit}'"))
        }
    }

    fn eof(&self) -> bool {
        self.i >= self.s.len()
    }

    /// Skip S; returns whether there was any.
    fn space(&mut self) -> bool {
        let start = self.i;
        while self.peek().is_some_and(is_space) {
            self.i += 1;
        }
        self.i > start
    }

    fn need_space(&mut self) -> R<()> {
        if self.space() {
            Ok(())
        } else {
            self.err("expected white space")
        }
    }

    fn name(&mut self) -> R<String> {
        match self.peek() {
            Some(c) if is_name_start(c) => {}
            _ => return self.err("expected a name"),
        }
        let start = self.i;
        while self.peek().is_some_and(is_name_char) {
            self.i += 1;
        }
        Ok(self.s[start..self.i].iter().collect())
    }

    fn nmtoken(&mut self) -> R<String> {
        let start = self.i;
        while self.peek().is_some_and(is_name_char) {
            self.i += 1;
        }
        if self.i == start {
            return self.err("expected a name token");
        }
        Ok(self.s[start..self.i].iter().collect())
    }

    /// A quoted literal (SystemLiteral, PubidLiteral, version, …): its text.
    fn quoted(&mut self) -> R<String> {
        let q = match self.peek() {
            Some(q @ ('"' | '\'')) => q,
            _ => return self.err("expected a quoted value"),
        };
        self.i += 1;
        let start = self.i;
        while self.peek().is_some_and(|c| c != q) {
            self.i += 1;
        }
        if self.eof() {
            return self.err("unterminated quoted value");
        }
        let v: String = self.s[start..self.i].iter().collect();
        self.i += 1;
        Ok(v)
    }

    /// Enter an entity's replacement text.
    fn push_entity(&mut self, name: &str, text: &str) -> R<()> {
        if self.stack.iter().any(|(_, _, n)| n == name) {
            return self.err(format!("the entity '{name}' refers to itself"));
        }
        if self.stack.len() >= MAX_ENTITY_DEPTH {
            return self.err("entity references are nested too deeply");
        }
        self.expanded += text.len();
        if self.expanded > MAX_EXPANSION {
            return self.err("entities expand to too much text");
        }
        let buf: Vec<char> = text.chars().collect();
        let outer = std::mem::replace(&mut self.s, buf);
        self.stack.push((outer, self.i, name.to_string()));
        self.i = 0;
        Ok(())
    }

    fn pop_entity(&mut self) {
        if let Some((b, i, _)) = self.stack.pop() {
            self.s = b;
            self.i = i;
        }
    }

    // ---- document (XML §2.1) ----

    fn document(&mut self) -> R<()> {
        if let Some(c) = self.s.iter().position(|&c| !is_char(c)) {
            self.i = c;
            return self.err(format!("U+{:04X} is not allowed in XML", self.s[c] as u32));
        }
        if self.at("<?xml") && self.s.get(self.i + 5).is_some_and(|&c| is_space(c)) {
            self.xml_decl()?;
        }
        self.misc()?;
        if self.at("<!DOCTYPE") {
            self.doctype()?;
            self.misc()?;
        }
        if !self.at("<") || self.at("</") {
            return self.err("expected the document element");
        }
        self.element(Document::ROOT)?;
        self.misc()?;
        if !self.eof() {
            return self.err("content after the document element");
        }
        Ok(())
    }

    /// Misc* (XML §2.8): comments, PIs and white space outside the document element.
    fn misc(&mut self) -> R<()> {
        loop {
            self.space();
            if self.at("<!--") {
                let c = self.comment()?;
                let id = self.doc.create_comment(&c);
                self.doc.append_child(Document::ROOT, id);
            } else if self.at("<?") {
                let (t, d) = self.pi()?;
                let id = self.doc.create_processing_instruction(&t, &d);
                self.doc.append_child(Document::ROOT, id);
            } else {
                return Ok(());
            }
        }
    }

    /// XML §2.8 XMLDecl.
    fn xml_decl(&mut self) -> R<()> {
        self.expect("<?xml")?;
        self.need_space()?;
        self.expect("version")?;
        self.eq()?;
        let v = self.quoted()?;
        let ok = v.strip_prefix("1.").is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()));
        if !ok {
            return self.err(format!("unsupported XML version '{v}'"));
        }
        self.decl.version = v;
        let sp = self.space();
        if sp && self.eat("encoding") {
            self.eq()?;
            let e = self.quoted()?;
            let mut cs = e.chars();
            let ok = cs.next().is_some_and(|c| c.is_ascii_alphabetic())
                && cs.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
            if !ok {
                return self.err(format!("'{e}' is not an encoding name"));
            }
            self.decl.encoding = Some(e);
            self.space();
        }
        let sp = sp || self.s.get(self.i.wrapping_sub(1)).is_some_and(|&c| is_space(c));
        if sp && self.eat("standalone") {
            self.eq()?;
            let v = self.quoted()?;
            self.standalone = match v.as_str() {
                "yes" => true,
                "no" => false,
                _ => return self.err("standalone must be 'yes' or 'no'"),
            };
            self.decl.standalone = Some(self.standalone);
            self.space();
        }
        self.expect("?>")
    }

    /// XML §2.3 Eq.
    fn eq(&mut self) -> R<()> {
        self.space();
        self.expect("=")?;
        self.space();
        Ok(())
    }

    /// XML §2.5 Comment; returns its text.
    fn comment(&mut self) -> R<String> {
        self.expect("<!--")?;
        let start = self.i;
        loop {
            if self.eof() {
                return self.err("unterminated comment");
            }
            if self.at("--") {
                if self.at("-->") {
                    break;
                }
                return self.err("'--' is not allowed in a comment");
            }
            self.i += 1;
        }
        let t: String = self.s[start..self.i].iter().collect();
        self.i += 3;
        Ok(t)
    }

    /// XML §2.6 PI; returns (target, data).
    fn pi(&mut self) -> R<(String, String)> {
        self.expect("<?")?;
        let target = self.name()?;
        if target.eq_ignore_ascii_case("xml") {
            return self.err("the XML declaration must come first");
        }
        if target.contains(':') {
            return self.err("a processing instruction target can't contain ':'");
        }
        if self.eat("?>") {
            return Ok((target, String::new()));
        }
        self.need_space()?;
        let start = self.i;
        while !self.at("?>") {
            if self.eof() {
                return self.err("unterminated processing instruction");
            }
            self.i += 1;
        }
        let data: String = self.s[start..self.i].iter().collect();
        self.i += 2;
        Ok((target, data))
    }

    // ---- DTD (XML §2.8, §3.2–3.3, §4.2) ----

    fn external_id(&mut self, public_allowed_alone: bool) -> R<Option<(String, String)>> {
        if self.eat("SYSTEM") {
            self.need_space()?;
            let sys = self.quoted()?;
            return Ok(Some((String::new(), sys)));
        }
        if self.eat("PUBLIC") {
            self.need_space()?;
            let public = self.quoted()?;
            if let Some(c) =
                public.chars().find(|&c| !(c.is_ascii_alphanumeric() || " \n-'()+,./:=?;!*#@$_%".contains(c)))
            {
                return self.err(format!("'{c}' is not allowed in a public identifier"));
            }
            let save = self.i;
            let sp = self.space();
            if sp && matches!(self.peek(), Some('"' | '\'')) {
                let sys = self.quoted()?;
                return Ok(Some((public, sys)));
            }
            if public_allowed_alone {
                self.i = save;
                return Ok(Some((public, String::new())));
            }
            return self.err("expected a system identifier");
        }
        Ok(None)
    }

    /// XML §2.8 doctypedecl.
    fn doctype(&mut self) -> R<()> {
        self.expect("<!DOCTYPE")?;
        self.need_space()?;
        let name = self.name()?;
        if split_qname(&name).is_none() {
            return self.err(format!("'{name}' is not a qualified name"));
        }
        self.space();
        let (public, system) = self.external_id(false)?.unwrap_or_default();
        if !public.is_empty() || !system.is_empty() {
            // The external subset isn't read; what it declares can't be known.
            self.external_refs = true;
        }
        self.space();
        if self.eat("[") {
            self.int_subset()?;
            self.expect("]")?;
            self.space();
        }
        self.expect(">")?;
        let id = self.doc.create_doctype(&name, &public, &system);
        self.doc.append_child(Document::ROOT, id);
        Ok(())
    }

    /// XML §2.8 intSubset (and the replacement text of parameter entities in it).
    fn int_subset(&mut self) -> R<()> {
        loop {
            self.space();
            if self.eof() {
                if self.stack.is_empty() {
                    return self.err("unterminated document type declaration");
                }
                return Ok(());
            }
            if self.stack.is_empty() && self.at("]") {
                return Ok(());
            }
            if self.eat("%") {
                let name = self.name()?;
                self.expect(";")?;
                self.external_refs = true;
                match self.parameter.get(&name).cloned() {
                    Some(Entity::Internal(text)) => {
                        self.push_entity(&format!("%{name}"), &text)?;
                        let r = self.int_subset();
                        self.pop_entity();
                        r?;
                    }
                    Some(Entity::External { .. }) => {
                        // Not read: later declarations may depend on it (XML §5.1).
                        if !self.standalone {
                            self.skip_decls = true;
                        }
                    }
                    None => {
                        if self.standalone {
                            return self.err(format!("the parameter entity '{name}' isn't declared"));
                        }
                        self.skip_decls = true;
                    }
                }
            } else if self.at("<!ENTITY") {
                self.entity_decl()?;
            } else if self.at("<!ATTLIST") {
                self.attlist_decl()?;
            } else if self.at("<!ELEMENT") {
                self.element_decl()?;
            } else if self.at("<!NOTATION") {
                self.notation_decl()?;
            } else if self.at("<!--") {
                self.comment()?;
            } else if self.at("<?") {
                self.pi()?;
            } else {
                return self.err("expected a markup declaration");
            }
        }
    }

    /// XML §4.2 EntityDecl.
    fn entity_decl(&mut self) -> R<()> {
        self.expect("<!ENTITY")?;
        self.need_space()?;
        let pe = if self.eat("%") {
            self.need_space()?;
            true
        } else {
            false
        };
        let name = self.name()?;
        if name.contains(':') {
            return self.err("an entity name can't contain ':'");
        }
        self.need_space()?;
        let entity = if matches!(self.peek(), Some('"' | '\'')) {
            Entity::Internal(self.entity_value()?)
        } else {
            let Some(_) = self.external_id(false)? else { return self.err("expected an entity value") };
            let mut unparsed = false;
            let sp = self.space();
            if !pe && sp && self.eat("NDATA") {
                self.need_space()?;
                self.name()?;
                unparsed = true;
            }
            Entity::External { unparsed }
        };
        self.space();
        self.expect(">")?;
        if self.skip_decls {
            return Ok(());
        }
        let table = if pe { &mut self.parameter } else { &mut self.general };
        // The first declaration binds (XML §4.2); the predefined ones stay predefined.
        if pe || !matches!(name.as_str(), "lt" | "gt" | "amp" | "apos" | "quot") {
            table.entry(name).or_insert(entity);
        }
        Ok(())
    }

    /// XML §2.3 EntityValue: character references are expanded now, general entity
    /// references are kept (bypassed, XML §4.4.7).
    fn entity_value(&mut self) -> R<String> {
        let q = self.peek().unwrap_or('"');
        self.i += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return self.err("unterminated entity value"),
                Some(c) if c == q => {
                    self.i += 1;
                    return Ok(out);
                }
                Some('%') => {
                    return self.err("parameter entity references can't be used inside declarations here")
                }
                Some('&') => {
                    if self.at("&#") {
                        out.push(self.char_ref()?);
                    } else {
                        self.i += 1;
                        let n = self.name()?;
                        self.expect(";")?;
                        out.push('&');
                        out.push_str(&n);
                        out.push(';');
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    /// XML §3.3 AttlistDecl.
    fn attlist_decl(&mut self) -> R<()> {
        self.expect("<!ATTLIST")?;
        self.need_space()?;
        let el = self.name()?;
        let mut defs = Vec::new();
        loop {
            let sp = self.space();
            if self.eat(">") {
                break;
            }
            if !sp {
                return self.err("expected white space");
            }
            let name = self.name()?;
            self.need_space()?;
            let cdata = if self.eat("CDATA") {
                true
            } else if self.eat("NOTATION") {
                self.need_space()?;
                self.enumeration(true)?;
                false
            } else if self.at("(") {
                self.enumeration(false)?;
                false
            } else {
                let t = self.name()?;
                if !matches!(
                    t.as_str(),
                    "ID" | "IDREF" | "IDREFS" | "ENTITY" | "ENTITIES" | "NMTOKEN" | "NMTOKENS"
                ) {
                    return self.err(format!("'{t}' is not an attribute type"));
                }
                false
            };
            self.need_space()?;
            let default = if self.eat("#REQUIRED") || self.eat("#IMPLIED") {
                None
            } else {
                if self.eat("#FIXED") {
                    self.need_space()?;
                }
                let raw = self.att_value()?;
                Some(if cdata { raw } else { collapse(&raw) })
            };
            defs.push(AttDef { name, cdata, default });
        }
        if !self.skip_decls {
            let list = self.attlists.entry(el).or_default();
            for d in defs {
                // The first definition of an attribute binds (XML §3.3).
                if !list.iter().any(|x| x.name == d.name) {
                    list.push(d);
                }
            }
        }
        Ok(())
    }

    /// XML §3.3.1 Enumeration / NotationType: '(' S? token (S? '|' S? token)* S? ')'.
    fn enumeration(&mut self, names: bool) -> R<()> {
        self.expect("(")?;
        loop {
            self.space();
            if names {
                self.name()?;
            } else {
                self.nmtoken()?;
            }
            self.space();
            if self.eat(")") {
                return Ok(());
            }
            self.expect("|")?;
        }
    }

    /// XML §3.2 elementdecl (the content model is checked for balance only).
    fn element_decl(&mut self) -> R<()> {
        self.expect("<!ELEMENT")?;
        self.need_space()?;
        self.name()?;
        self.need_space()?;
        if self.eat("EMPTY") || self.eat("ANY") {
        } else if self.at("(") {
            let mut depth = 0usize;
            loop {
                match self.peek() {
                    None => return self.err("unterminated element declaration"),
                    Some('(') => depth += 1,
                    Some(')') => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            self.i += 1;
                            if matches!(self.peek(), Some('?' | '*' | '+')) {
                                self.i += 1;
                            }
                            break;
                        }
                    }
                    Some(c)
                        if is_name_char(c)
                            || is_space(c)
                            || matches!(c, '|' | ',' | '?' | '*' | '+' | '#') => {}
                    Some(c) => return self.err(format!("'{c}' is not allowed in a content model")),
                }
                self.i += 1;
            }
        } else {
            return self.err("expected a content model");
        }
        self.space();
        self.expect(">")
    }

    /// XML §4.7 NotationDecl.
    fn notation_decl(&mut self) -> R<()> {
        self.expect("<!NOTATION")?;
        self.need_space()?;
        let n = self.name()?;
        if n.contains(':') {
            return self.err("a notation name can't contain ':'");
        }
        self.need_space()?;
        if self.external_id(true)?.is_none() {
            return self.err("expected an external or public identifier");
        }
        self.space();
        self.expect(">")
    }

    // ---- references (XML §4.1) ----

    /// CharRef: `&#…;` (the position is at '&').
    fn char_ref(&mut self) -> R<char> {
        self.expect("&#")?;
        let hex = self.eat("x");
        let start = self.i;
        while self.peek().is_some_and(|c| if hex { c.is_ascii_hexdigit() } else { c.is_ascii_digit() }) {
            self.i += 1;
        }
        let digits: String = self.s[start..self.i].iter().collect();
        if digits.is_empty() {
            return self.err("expected digits in a character reference");
        }
        self.expect(";")?;
        let v = u32::from_str_radix(&digits, if hex { 16 } else { 10 }).ok();
        match v.and_then(char::from_u32) {
            Some(c) if is_char(c) => Ok(c),
            _ => self.err(format!("&#{}{digits}; is not a character XML allows", if hex { "x" } else { "" })),
        }
    }

    /// A predefined entity's character (XML §4.6).
    fn predefined(name: &str) -> Option<char> {
        Some(match name {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "apos" => '\'',
            "quot" => '"',
            _ => return None,
        })
    }

    /// What a general entity reference stands for: Some(text) to expand, or None to skip.
    fn general_entity(&self, name: &str, in_attr: bool) -> R<Option<String>> {
        match self.general.get(name) {
            Some(Entity::Internal(t)) => Ok(Some(t.clone())),
            Some(Entity::External { unparsed: true }) => {
                self.err(format!("the unparsed entity '{name}' can't be referenced"))
            }
            Some(Entity::External { .. }) if in_attr => {
                self.err(format!("the external entity '{name}' can't be used in an attribute value"))
            }
            Some(Entity::External { .. }) => Ok(None),
            None if self.external_refs && !self.standalone => Ok(None),
            None => self.err(format!("the entity '{name}' isn't declared")),
        }
    }

    // ---- attribute values (XML §3.3.3) ----

    fn att_value(&mut self) -> R<String> {
        let q = match self.peek() {
            Some(q @ ('"' | '\'')) => q,
            _ => return self.err("expected a quoted attribute value"),
        };
        self.i += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return self.err("unterminated attribute value"),
                Some(c) if c == q => {
                    self.i += 1;
                    return Ok(out);
                }
                Some('<') => return self.err("'<' is not allowed in an attribute value"),
                Some('&') => self.att_ref(&mut out)?,
                Some(c) => {
                    out.push(if is_space(c) { ' ' } else { c });
                    self.i += 1;
                }
            }
        }
    }

    /// A reference in an attribute value: appended to `out`, normalized.
    fn att_ref(&mut self, out: &mut String) -> R<()> {
        if self.at("&#") {
            out.push(self.char_ref()?);
            return Ok(());
        }
        self.i += 1;
        let name = self.name()?;
        self.expect(";")?;
        if let Some(c) = Self::predefined(&name) {
            out.push(c);
            return Ok(());
        }
        let Some(text) = self.general_entity(&name, true)? else { return Ok(()) };
        self.push_entity(&name, &text)?;
        let r = (|| {
            while let Some(c) = self.peek() {
                match c {
                    '<' => return self.err("'<' is not allowed in an attribute value"),
                    '&' => self.att_ref(out)?,
                    c => {
                        out.push(if is_space(c) { ' ' } else { c });
                        self.i += 1;
                    }
                }
            }
            Ok(())
        })();
        self.pop_entity();
        r
    }

    // ---- elements (XML §3.1) ----

    fn flush_text(&mut self) {
        if self.text.is_empty() {
            return;
        }
        let t = std::mem::take(&mut self.text);
        let p = self.text_parent;
        match self.doc.last_child(p) {
            Some(l) if matches!(self.doc.node(l).data, NodeData::Text(_)) && !self.doc.is_cdata(l) => {
                let mut cur = self.doc.node(l).as_text().unwrap_or("").to_string();
                cur.push_str(&t);
                self.doc.set_text(l, &cur);
            }
            _ => {
                let id = self.doc.create_text(&t);
                self.doc.append_child(p, id);
            }
        }
    }

    fn lookup(&self, prefix: Option<&str>) -> Option<&str> {
        self.ns.iter().rev().find(|(p, _)| p.as_deref() == prefix).map(|(_, u)| u.as_str())
    }

    /// element (XML §3.1, with Namespaces in XML §5–6).
    fn element(&mut self, parent: NodeId) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err("elements are nested too deeply");
        }
        self.flush_text();
        self.expect("<")?;
        let qname = self.name()?;
        let mut raw: Vec<(String, String)> = Vec::new();
        let empty;
        loop {
            let sp = self.space();
            if self.eat("/>") {
                empty = true;
                break;
            }
            if self.eat(">") {
                empty = false;
                break;
            }
            if !sp {
                return self.err("expected white space before an attribute");
            }
            let an = self.name()?;
            self.eq()?;
            let v = self.att_value()?;
            if raw.iter().any(|(n, _)| *n == an) {
                return self.err(format!("the attribute '{an}' is given twice"));
            }
            raw.push((an, v));
        }
        // Defaults and types from the internal subset.
        if let Some(defs) = self.attlists.get(&qname) {
            for d in defs {
                match raw.iter_mut().find(|(n, _)| *n == d.name) {
                    Some((_, v)) if !d.cdata => *v = collapse(v),
                    Some(_) => {}
                    None => {
                        if let Some(dv) = &d.default {
                            raw.push((d.name.clone(), dv.clone()));
                        }
                    }
                }
            }
        }
        // Namespace declarations.
        let scope = self.ns.len();
        for (n, v) in &raw {
            let prefix = if n == "xmlns" {
                None
            } else if let Some(p) = n.strip_prefix("xmlns:") {
                Some(p.to_string())
            } else {
                continue;
            };
            match prefix.as_deref() {
                Some("xmlns") => return self.err("the prefix 'xmlns' can't be declared"),
                Some("xml") if v != XML_NS => {
                    return self.err("the prefix 'xml' is bound to the XML namespace")
                }
                Some(p) if !is_ncname(p) => return self.err(format!("'{p}' is not a valid prefix")),
                Some(_) if v.is_empty() => return self.err("a prefix can't be bound to the empty namespace"),
                _ => {}
            }
            if (v == XML_NS && prefix.as_deref() != Some("xml")) || v == XMLNS_NS {
                return self.err(format!("the namespace '{v}' can't be bound here"));
            }
            self.ns.push((prefix, v.clone()));
        }
        let Some((prefix, local)) = split_qname(&qname) else {
            return self.err(format!("'{qname}' is not a qualified name"));
        };
        let uri = match self.lookup(prefix) {
            Some(u) => u.to_string(),
            None if prefix.is_none() => String::new(),
            None => return self.err(format!("the prefix '{}' isn't declared", prefix.unwrap_or(""))),
        };
        let mut attrs: Vec<Attribute> = Vec::with_capacity(raw.len());
        for (n, v) in &raw {
            let (ns, pfx, local) = if n == "xmlns" {
                (XMLNS_NS.to_string(), None, "xmlns".to_string())
            } else {
                let Some((p, l)) = split_qname(n) else {
                    return self.err(format!("'{n}' is not a qualified name"));
                };
                match p {
                    None => (String::new(), None, l.to_string()),
                    Some("xmlns") => (XMLNS_NS.to_string(), Some("xmlns".to_string()), l.to_string()),
                    Some(p) => match self.lookup(Some(p)) {
                        Some(u) => (u.to_string(), Some(p.to_string()), l.to_string()),
                        None => return self.err(format!("the prefix '{p}' isn't declared")),
                    },
                }
            };
            let ns = Namespace::from_url(&ns);
            if attrs.iter().any(|a| a.ns == ns && a.name == local) {
                return self.err(format!("the attribute '{n}' is given twice (by namespace)"));
            }
            attrs.push(Attribute { ns, prefix: pfx, name: local, value: v.clone() });
        }
        let id = self.doc.create_element(Namespace::from_url(&uri), local, attrs);
        if let (Some(p), Some(e)) = (prefix, self.doc.element_mut(id)) {
            e.prefix = Some(p.to_string());
        }
        self.doc.append_child(parent, id);
        // Content goes into a template's contents, as in HTML documents.
        let target = self.doc.template_of(id).unwrap_or(id);
        if !empty {
            self.content(target)?;
            self.flush_text();
            if !self.at("</") {
                return self.err(format!("the element '{qname}' isn't closed"));
            }
            self.i += 2;
            let end = self.name()?;
            if end != qname {
                return self.err(format!("the end tag '{end}' doesn't match '{qname}'"));
            }
            self.space();
            self.expect(">")?;
        }
        self.ns.truncate(scope);
        self.text_parent = parent;
        self.depth -= 1;
        Ok(())
    }

    /// content (XML §3.1): up to an end tag (or the end of the entity being read).
    fn content(&mut self, parent: NodeId) -> R<()> {
        self.text_parent = parent;
        loop {
            match self.peek() {
                None => return Ok(()),
                Some('<') => {
                    if self.at("</") {
                        return Ok(());
                    }
                    self.flush_text();
                    if self.at("<!--") {
                        let c = self.comment()?;
                        let id = self.doc.create_comment(&c);
                        self.doc.append_child(parent, id);
                    } else if self.at("<![CDATA[") {
                        self.i += 9;
                        let start = self.i;
                        while !self.at("]]>") {
                            if self.eof() {
                                return self.err("unterminated CDATA section");
                            }
                            self.i += 1;
                        }
                        let t: String = self.s[start..self.i].iter().collect();
                        self.i += 3;
                        let id = self.doc.create_cdata(&t);
                        self.doc.append_child(parent, id);
                    } else if self.at("<?") {
                        let (t, d) = self.pi()?;
                        let id = self.doc.create_processing_instruction(&t, &d);
                        self.doc.append_child(parent, id);
                    } else if self.at("<!") {
                        return self.err("unexpected markup declaration in content");
                    } else {
                        self.element(parent)?;
                    }
                    self.text_parent = parent;
                }
                Some('&') => {
                    if self.at("&#") {
                        let c = self.char_ref()?;
                        self.text.push(c);
                        continue;
                    }
                    self.i += 1;
                    let name = self.name()?;
                    self.expect(";")?;
                    if let Some(c) = Self::predefined(&name) {
                        self.text.push(c);
                        continue;
                    }
                    let Some(text) = self.general_entity(&name, false)? else { continue };
                    self.push_entity(&name, &text)?;
                    let r = self.content(parent);
                    let balanced = self.eof();
                    self.pop_entity();
                    r?;
                    if !balanced {
                        return self.err(format!("the entity '{name}' ends an element it didn't start"));
                    }
                    self.text_parent = parent;
                }
                Some(c) => {
                    if c == ']' && self.at("]]>") {
                        return self.err("']]>' is not allowed in text");
                    }
                    self.text.push(c);
                    self.i += 1;
                }
            }
        }
    }
}

/// Normalization for attributes not of type CDATA (XML §3.3.3): trim, collapse spaces.
fn collapse(s: &str) -> String {
    s.split(' ').filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" ")
}

/// The XML encoding declared by bytes `b` (XML §4.3.3 and Appendix F): from a BOM or
/// the XML declaration's `encoding`; None when neither says.
pub fn sniff_encoding(b: &[u8]) -> Option<String> {
    if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some("UTF-8".into());
    }
    if b.starts_with(&[0xFE, 0xFF]) {
        return Some("UTF-16BE".into());
    }
    if b.starts_with(&[0xFF, 0xFE]) {
        return Some("UTF-16LE".into());
    }
    if b.starts_with(&[0x3C, 0x00, 0x3F, 0x00]) {
        return Some("UTF-16LE".into());
    }
    if b.starts_with(&[0x00, 0x3C, 0x00, 0x3F]) {
        return Some("UTF-16BE".into());
    }
    if !b.starts_with(b"<?xml") {
        return None;
    }
    let end = b.iter().position(|&c| c == b'>')?;
    let decl = std::str::from_utf8(&b[..end]).ok()?;
    let rest = &decl[decl.find("encoding")? + 8..];
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let q = rest.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let v = &rest[1..];
    Some(v[..v.find(q)?].to_string())
}

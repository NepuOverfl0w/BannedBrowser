//! HTML tokenizer (HTML §13.2.5), implemented as the spec's state machine.
//!
//! Character tokens are batched into runs ([`Token::Characters`]). The tree
//! builder may change [`Tokenizer::state`] between tokens (e.g. to RCDATA
//! after `<title>`), so the tokenizer never runs ahead past a tag token.
//!
//! The input is a sequence of code points that may include lone surrogates (HTML §13.2.3.5:
//! they are a parse error but are passed through), so the tokenizer works on WTF-8
//! ([`bb_str::Wtf8Buf`]) and [`Tokenizer::next_raw_token`] returns tokens that keep them.
//! [`Tokenizer::next_token`] returns the same tokens as UTF-8 `String`s with lone surrogates
//! replaced by U+FFFD, which is what the tree builder (and today's UTF-8 DOM) consumes.

use crate::entities::ENTITIES;
use bb_str::Wtf8Buf;
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Doctype<S = String> {
    pub name: Option<S>,
    pub public_id: Option<S>,
    pub system_id: Option<S>,
    pub force_quirks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tag<S = String> {
    pub name: S,
    pub attrs: Vec<(S, S)>,
    pub self_closing: bool,
}

impl Tag {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token<S = String> {
    Doctype(Doctype<S>),
    StartTag(Tag<S>),
    EndTag(Tag<S>),
    Comment(S),
    /// HTML §13.2.5 processing instruction token: a target and data.
    ProcessingInstruction {
        target: S,
        data: S,
    },
    Characters(S),
    Eof,
}

/// A token whose strings keep lone surrogates.
pub type RawToken = Token<Wtf8Buf>;

impl RawToken {
    /// Convert to UTF-8 strings, replacing lone surrogates with U+FFFD.
    pub fn into_lossy(self) -> Token {
        let s = Wtf8Buf::into_string_lossy;
        let tag = |t: Tag<Wtf8Buf>| Tag {
            name: s(t.name),
            attrs: t.attrs.into_iter().map(|(k, v)| (s(k), s(v))).collect(),
            self_closing: t.self_closing,
        };
        match self {
            Token::Doctype(d) => Token::Doctype(Doctype {
                name: d.name.map(s),
                public_id: d.public_id.map(s),
                system_id: d.system_id.map(s),
                force_quirks: d.force_quirks,
            }),
            Token::StartTag(t) => Token::StartTag(tag(t)),
            Token::EndTag(t) => Token::EndTag(tag(t)),
            Token::Comment(c) => Token::Comment(s(c)),
            Token::ProcessingInstruction { target, data } => {
                Token::ProcessingInstruction { target: s(target), data: s(data) }
            }
            Token::Characters(c) => Token::Characters(s(c)),
            Token::Eof => Token::Eof,
        }
    }
}

/// Stands in for a lone surrogate in `Tokenizer::input`; the real code point is in
/// `Tokenizer::surrogates`. Any non-ASCII character works: surrogates take the "anything else"
/// branch in every tokenizer state, like U+FFFD.
const SURROGATE_PLACEHOLDER: char = '\u{FFFD}';

/// Tokenizer states (HTML §13.2.5.1–80).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Data,
    Rcdata,
    Rawtext,
    ScriptData,
    Plaintext,
    TagOpen,
    EndTagOpen,
    TagName,
    RcdataLessThanSign,
    RcdataEndTagOpen,
    RcdataEndTagName,
    RawtextLessThanSign,
    RawtextEndTagOpen,
    RawtextEndTagName,
    ScriptDataLessThanSign,
    ScriptDataEndTagOpen,
    ScriptDataEndTagName,
    ScriptDataEscapeStart,
    ScriptDataEscapeStartDash,
    ScriptDataEscaped,
    ScriptDataEscapedDash,
    ScriptDataEscapedDashDash,
    ScriptDataEscapedLessThanSign,
    ScriptDataEscapedEndTagOpen,
    ScriptDataEscapedEndTagName,
    ScriptDataDoubleEscapeStart,
    ScriptDataDoubleEscaped,
    ScriptDataDoubleEscapedDash,
    ScriptDataDoubleEscapedDashDash,
    ScriptDataDoubleEscapedLessThanSign,
    ScriptDataDoubleEscapeEnd,
    BeforeAttributeName,
    AttributeName,
    AfterAttributeName,
    BeforeAttributeValue,
    AttributeValueDoubleQuoted,
    AttributeValueSingleQuoted,
    AttributeValueUnquoted,
    AfterAttributeValueQuoted,
    SelfClosingStartTag,
    BogusComment,
    MarkupDeclarationOpen,
    CommentStart,
    CommentStartDash,
    Comment,
    CommentLessThanSign,
    CommentLessThanSignBang,
    CommentLessThanSignBangDash,
    CommentLessThanSignBangDashDash,
    CommentEndDash,
    CommentEnd,
    CommentEndBang,
    Doctype,
    BeforeDoctypeName,
    DoctypeName,
    AfterDoctypeName,
    AfterDoctypePublicKeyword,
    BeforeDoctypePublicIdentifier,
    DoctypePublicIdentifierDoubleQuoted,
    DoctypePublicIdentifierSingleQuoted,
    AfterDoctypePublicIdentifier,
    BetweenDoctypePublicAndSystemIdentifiers,
    AfterDoctypeSystemKeyword,
    BeforeDoctypeSystemIdentifier,
    DoctypeSystemIdentifierDoubleQuoted,
    DoctypeSystemIdentifierSingleQuoted,
    AfterDoctypeSystemIdentifier,
    BogusDoctype,
    CdataSection,
    CdataSectionBracket,
    CdataSectionEnd,
    ProcessingInstructionOpen,
    ProcessingInstructionTarget,
    AfterProcessingInstructionTarget,
    ProcessingInstructionData,
    ProcessingInstructionQuestionable,
    CharacterReference,
    NamedCharacterReference,
    AmbiguousAmpersand,
    NumericCharacterReference,
    HexadecimalCharacterReferenceStart,
    DecimalCharacterReferenceStart,
    HexadecimalCharacterReference,
    DecimalCharacterReference,
    NumericCharacterReferenceEnd,
}

enum TagKind {
    Start,
    End,
}

pub struct Tokenizer {
    input: Vec<char>,
    /// Lone surrogates in the input: position in `input` → code point. Those positions hold
    /// `SURROGATE_PLACEHOLDER`.
    surrogates: HashMap<usize, u32>,
    pos: usize,
    pub state: State,
    return_state: State,
    queue: VecDeque<RawToken>,
    pending_chars: Wtf8Buf,
    // Current tag being built.
    tag_kind: TagKind,
    tag: Tag<Wtf8Buf>,
    attr_name: Wtf8Buf,
    attr_value: Wtf8Buf,
    attr_active: bool,
    comment: Wtf8Buf,
    /// The current processing instruction token (target, data).
    pi: Option<(Wtf8Buf, Wtf8Buf)>,
    doctype: Doctype<Wtf8Buf>,
    temp: Wtf8Buf,
    char_ref_code: u32,
    last_start_tag: Option<Wtf8Buf>,
    /// Set by the tree builder: adjusted current node is not in the HTML namespace.
    pub allow_cdata: bool,
    eof_emitted: bool,
    /// Parse errors seen (count only; messages are not needed yet).
    pub errors: usize,
}

fn is_ascii_ws(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0C' | ' ')
}

impl Tokenizer {
    pub fn new(input: &str) -> Self {
        Self::from_code_points(input.chars().map(u32::from), input.len())
    }

    /// Tokenize WTF-8 input, which may contain lone surrogates.
    pub fn from_wtf8(input: &Wtf8Buf) -> Self {
        Self::from_code_points(input.code_points(), input.len())
    }

    fn from_code_points(input: impl Iterator<Item = u32>, size_hint: usize) -> Self {
        // HTML §13.2.3.5 preprocessing: normalize newlines.
        let mut chars = Vec::with_capacity(size_hint);
        let mut surrogates = HashMap::new();
        let mut it = input.peekable();
        while let Some(cp) = it.next() {
            if cp == u32::from('\r') {
                if it.peek() == Some(&u32::from('\n')) {
                    it.next();
                }
                chars.push('\n');
            } else if let Some(c) = char::from_u32(cp) {
                chars.push(c);
            } else {
                // A lone surrogate ("surrogate-in-input-stream", passed through).
                surrogates.insert(chars.len(), cp);
                chars.push(SURROGATE_PLACEHOLDER);
            }
        }
        Tokenizer {
            input: chars,
            surrogates,
            pos: 0,
            state: State::Data,
            return_state: State::Data,
            queue: VecDeque::new(),
            pending_chars: Wtf8Buf::new(),
            tag_kind: TagKind::Start,
            tag: Tag::default(),
            attr_name: Wtf8Buf::new(),
            attr_value: Wtf8Buf::new(),
            attr_active: false,
            comment: Wtf8Buf::new(),
            pi: None,
            doctype: Doctype::default(),
            temp: Wtf8Buf::new(),
            char_ref_code: 0,
            last_start_tag: None,
            allow_cdata: false,
            eof_emitted: false,
            errors: 0,
        }
    }

    /// The insertion point (HTML §13.2.5): where `document.write` text goes, as an index
    /// into the input.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Insert `text` at the insertion point (HTML §8.4.3 `document.write`), preprocessed
    /// like the rest of the input; returns the position just after it.
    pub fn insert_input(&mut self, text: &str) -> usize {
        let mut chars: Vec<char> = Vec::with_capacity(text.len());
        let mut it = text.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\r' {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                chars.push('\n');
            } else {
                chars.push(c);
            }
        }
        let n = chars.len();
        if n > 0 {
            // Lone surrogates after the insertion point move along.
            if !self.surrogates.is_empty() {
                let moved: HashMap<usize, u32> = self
                    .surrogates
                    .drain()
                    .map(|(k, v)| (if k >= self.pos { k + n } else { k }, v))
                    .collect();
                self.surrogates = moved;
            }
            self.input.splice(self.pos..self.pos, chars);
            // Input written after the end was reached is tokenized too.
            self.eof_emitted = false;
        }
        self.pos + n
    }

    /// Set the "last start tag" (used for appropriate end tags in tests and fragments).
    pub fn set_last_start_tag(&mut self, name: Option<&str>) {
        self.last_start_tag = name.map(Wtf8Buf::from);
    }

    /// Produce the next token, with lone surrogates replaced by U+FFFD.
    /// Returns `Token::Eof` forever once input is exhausted.
    pub fn next_token(&mut self) -> Token {
        self.next_raw_token().into_lossy()
    }

    /// Produce the next token, keeping lone surrogates.
    pub fn next_raw_token(&mut self) -> RawToken {
        loop {
            if let Some(t) = self.queue.pop_front() {
                return t;
            }
            if self.eof_emitted {
                return Token::Eof;
            }
            self.step();
        }
    }

    // ----- input helpers -----

    fn consume(&mut self) -> Option<char> {
        let c = self.input.get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    /// The code point at `pos`, given the char `c` stored there (a lone surrogate if `c` is
    /// the placeholder for one).
    fn code_point_at(&self, pos: usize, c: char) -> u32 {
        if c == SURROGATE_PLACEHOLDER {
            if let Some(&cp) = self.surrogates.get(&pos) {
                return cp;
            }
        }
        u32::from(c)
    }

    /// The code point of `c`, the character just consumed.
    fn cur(&self, c: char) -> u32 {
        self.code_point_at(self.pos.wrapping_sub(1), c)
    }

    fn reconsume(&mut self, c: Option<char>) {
        if c.is_some() {
            self.pos -= 1;
        }
    }

    fn peek_matches(&self, s: &str, case_insensitive: bool) -> bool {
        for (i, sc) in (self.pos..).zip(s.chars()) {
            match self.input.get(i) {
                Some(&c) if c == sc || (case_insensitive && c.eq_ignore_ascii_case(&sc)) => {}
                _ => return false,
            }
        }
        true
    }

    fn error(&mut self) {
        self.errors += 1;
    }

    // ----- emission -----

    fn emit_char(&mut self, c: char) {
        self.pending_chars.push(c);
    }

    fn emit_str(&mut self, s: &str) {
        self.pending_chars.push_str(s);
    }

    fn emit_wtf8(&mut self, s: &Wtf8Buf) {
        self.pending_chars.push_wtf8(s);
    }

    /// Emit `c`, the input character just consumed (it may stand for a lone surrogate).
    fn emit_input(&mut self, c: char) {
        let cp = self.cur(c);
        self.pending_chars.push_code_point(cp);
    }

    fn flush_chars(&mut self) {
        if !self.pending_chars.is_empty() {
            let s = std::mem::take(&mut self.pending_chars);
            self.queue.push_back(Token::Characters(s));
        }
    }

    fn emit(&mut self, t: RawToken) {
        self.flush_chars();
        self.queue.push_back(t);
    }

    fn emit_eof(&mut self) {
        self.flush_chars();
        self.queue.push_back(Token::Eof);
        self.eof_emitted = true;
    }

    fn new_tag(&mut self, kind: TagKind) {
        self.tag_kind = kind;
        self.tag = Tag::default();
        self.attr_active = false;
    }

    fn start_attr(&mut self) {
        self.finish_attr();
        self.attr_name.clear();
        self.attr_value.clear();
        self.attr_active = true;
    }

    fn finish_attr(&mut self) {
        if !self.attr_active {
            return;
        }
        self.attr_active = false;
        let name = std::mem::take(&mut self.attr_name);
        let value = std::mem::take(&mut self.attr_value);
        if self.tag.attrs.iter().any(|(k, _)| *k == name) {
            self.error(); // duplicate-attribute: drop
        } else {
            self.tag.attrs.push((name, value));
        }
    }

    fn emit_tag(&mut self) {
        self.finish_attr();
        let tag = std::mem::take(&mut self.tag);
        match self.tag_kind {
            TagKind::Start => {
                self.last_start_tag = Some(tag.name.clone());
                self.emit(Token::StartTag(tag));
            }
            TagKind::End => {
                if !tag.attrs.is_empty() || tag.self_closing {
                    self.error();
                }
                self.emit(Token::EndTag(Tag { name: tag.name, attrs: Vec::new(), self_closing: false }));
            }
        }
    }

    /// "Convert the temporary buffer to a comment": a comment token whose data is "?" followed
    /// by the temporary buffer (an invalid processing instruction becomes a bogus comment).
    fn temp_to_comment(&mut self) {
        self.comment = Wtf8Buf::from("?");
        let t = std::mem::take(&mut self.temp);
        self.comment.push_wtf8(&t);
    }

    fn emit_pi(&mut self) {
        if let Some((target, data)) = self.pi.take() {
            self.emit(Token::ProcessingInstruction { target, data });
        }
    }

    fn emit_comment(&mut self) {
        let c = std::mem::take(&mut self.comment);
        self.emit(Token::Comment(c));
    }

    fn emit_doctype(&mut self) {
        let d = std::mem::take(&mut self.doctype);
        self.emit(Token::Doctype(d));
    }

    fn is_appropriate_end_tag(&self) -> bool {
        matches!(self.tag_kind, TagKind::End) && self.last_start_tag.as_ref() == Some(&self.tag.name)
    }

    fn char_ref_in_attr(&self) -> bool {
        matches!(
            self.return_state,
            State::AttributeValueDoubleQuoted
                | State::AttributeValueSingleQuoted
                | State::AttributeValueUnquoted
        )
    }

    fn flush_temp_as_char_ref(&mut self) {
        let t = std::mem::take(&mut self.temp);
        if self.char_ref_in_attr() {
            self.attr_value.push_wtf8(&t);
        } else {
            self.emit_wtf8(&t);
        }
    }

    /// Shared "anything else" for the RCDATA/RAWTEXT/script end-tag-name states.
    fn end_tag_name_anything_else(&mut self, c: Option<char>, back: State) {
        self.emit_str("</");
        let t = std::mem::take(&mut self.temp);
        self.emit_wtf8(&t);
        self.reconsume(c);
        self.state = back;
    }

    fn end_tag_name_state(&mut self, back: State) {
        let c = self.consume();
        match c {
            Some(ch) if is_ascii_ws(ch) && self.is_appropriate_end_tag() => {
                self.state = State::BeforeAttributeName
            }
            Some('/') if self.is_appropriate_end_tag() => self.state = State::SelfClosingStartTag,
            Some('>') if self.is_appropriate_end_tag() => {
                self.state = State::Data;
                self.emit_tag();
            }
            Some(ch) if ch.is_ascii_alphabetic() => {
                {
                    let cp = self.cur(ch.to_ascii_lowercase());
                    self.tag.name.push_code_point(cp)
                };
                {
                    let cp = self.cur(ch);
                    self.temp.push_code_point(cp)
                };
            }
            _ => self.end_tag_name_anything_else(c, back),
        }
    }

    /// One step of the state machine.
    fn step(&mut self) {
        use State as S;
        match self.state {
            S::Data => match self.consume() {
                Some('&') => {
                    self.return_state = S::Data;
                    self.state = S::CharacterReference;
                }
                Some('<') => self.state = S::TagOpen,
                Some('\0') => {
                    self.error();
                    self.emit_char('\0');
                }
                Some(c) => {
                    self.emit_input(c);
                    // Fast path: copy a run of plain text.
                    while let Some(&n) = self.input.get(self.pos) {
                        if n == '&' || n == '<' || n == '\0' {
                            break;
                        }
                        let cp = self.code_point_at(self.pos, n);
                        self.pending_chars.push_code_point(cp);
                        self.pos += 1;
                    }
                }
                None => self.emit_eof(),
            },
            S::Rcdata => match self.consume() {
                Some('&') => {
                    self.return_state = S::Rcdata;
                    self.state = S::CharacterReference;
                }
                Some('<') => self.state = S::RcdataLessThanSign,
                Some('\0') => {
                    self.error();
                    self.emit_char('\u{FFFD}');
                }
                Some(c) => self.emit_input(c),
                None => self.emit_eof(),
            },
            S::Rawtext => match self.consume() {
                Some('<') => self.state = S::RawtextLessThanSign,
                Some('\0') => {
                    self.error();
                    self.emit_char('\u{FFFD}');
                }
                Some(c) => self.emit_input(c),
                None => self.emit_eof(),
            },
            S::ScriptData => match self.consume() {
                Some('<') => self.state = S::ScriptDataLessThanSign,
                Some('\0') => {
                    self.error();
                    self.emit_char('\u{FFFD}');
                }
                Some(c) => self.emit_input(c),
                None => self.emit_eof(),
            },
            S::Plaintext => match self.consume() {
                Some('\0') => {
                    self.error();
                    self.emit_char('\u{FFFD}');
                }
                Some(c) => self.emit_input(c),
                None => self.emit_eof(),
            },
            S::TagOpen => {
                let c = self.consume();
                match c {
                    Some('!') => self.state = S::MarkupDeclarationOpen,
                    Some('/') => self.state = S::EndTagOpen,
                    Some(ch) if ch.is_ascii_alphabetic() => {
                        self.new_tag(TagKind::Start);
                        self.reconsume(c);
                        self.state = S::TagName;
                    }
                    Some('?') => {
                        self.temp.clear();
                        self.state = S::ProcessingInstructionOpen;
                    }
                    None => {
                        self.error();
                        self.emit_char('<');
                        self.emit_eof();
                    }
                    _ => {
                        self.error();
                        self.emit_char('<');
                        self.reconsume(c);
                        self.state = S::Data;
                    }
                }
            }
            S::EndTagOpen => {
                let c = self.consume();
                match c {
                    Some(ch) if ch.is_ascii_alphabetic() => {
                        self.new_tag(TagKind::End);
                        self.reconsume(c);
                        self.state = S::TagName;
                    }
                    Some('>') => {
                        self.error();
                        self.state = S::Data;
                    }
                    None => {
                        self.error();
                        self.emit_str("</");
                        self.emit_eof();
                    }
                    _ => {
                        self.error();
                        self.comment.clear();
                        self.reconsume(c);
                        self.state = S::BogusComment;
                    }
                }
            }
            S::TagName => match self.consume() {
                Some(c) if is_ascii_ws(c) => self.state = S::BeforeAttributeName,
                Some('/') => self.state = S::SelfClosingStartTag,
                Some('>') => {
                    self.state = S::Data;
                    self.emit_tag();
                }
                Some('\0') => {
                    self.error();
                    self.tag.name.push('\u{FFFD}');
                }
                Some(c) => {
                    let cp = self.cur(c.to_ascii_lowercase());
                    self.tag.name.push_code_point(cp)
                }
                None => {
                    self.error();
                    self.emit_eof();
                }
            },
            S::RcdataLessThanSign => {
                let c = self.consume();
                if c == Some('/') {
                    self.temp.clear();
                    self.state = S::RcdataEndTagOpen;
                } else {
                    self.emit_char('<');
                    self.reconsume(c);
                    self.state = S::Rcdata;
                }
            }
            S::RcdataEndTagOpen => {
                let c = self.consume();
                if c.is_some_and(|ch| ch.is_ascii_alphabetic()) {
                    self.new_tag(TagKind::End);
                    self.reconsume(c);
                    self.state = S::RcdataEndTagName;
                } else {
                    self.emit_str("</");
                    self.reconsume(c);
                    self.state = S::Rcdata;
                }
            }
            S::RcdataEndTagName => self.end_tag_name_state(S::Rcdata),
            S::RawtextLessThanSign => {
                let c = self.consume();
                if c == Some('/') {
                    self.temp.clear();
                    self.state = S::RawtextEndTagOpen;
                } else {
                    self.emit_char('<');
                    self.reconsume(c);
                    self.state = S::Rawtext;
                }
            }
            S::RawtextEndTagOpen => {
                let c = self.consume();
                if c.is_some_and(|ch| ch.is_ascii_alphabetic()) {
                    self.new_tag(TagKind::End);
                    self.reconsume(c);
                    self.state = S::RawtextEndTagName;
                } else {
                    self.emit_str("</");
                    self.reconsume(c);
                    self.state = S::Rawtext;
                }
            }
            S::RawtextEndTagName => self.end_tag_name_state(S::Rawtext),
            S::ScriptDataLessThanSign => {
                let c = self.consume();
                match c {
                    Some('/') => {
                        self.temp.clear();
                        self.state = S::ScriptDataEndTagOpen;
                    }
                    Some('!') => {
                        self.state = S::ScriptDataEscapeStart;
                        self.emit_str("<!");
                    }
                    _ => {
                        self.emit_char('<');
                        self.reconsume(c);
                        self.state = S::ScriptData;
                    }
                }
            }
            S::ScriptDataEndTagOpen => {
                let c = self.consume();
                if c.is_some_and(|ch| ch.is_ascii_alphabetic()) {
                    self.new_tag(TagKind::End);
                    self.reconsume(c);
                    self.state = S::ScriptDataEndTagName;
                } else {
                    self.emit_str("</");
                    self.reconsume(c);
                    self.state = S::ScriptData;
                }
            }
            S::ScriptDataEndTagName => self.end_tag_name_state(S::ScriptData),
            S::ScriptDataEscapeStart => {
                let c = self.consume();
                if c == Some('-') {
                    self.state = S::ScriptDataEscapeStartDash;
                    self.emit_char('-');
                } else {
                    self.reconsume(c);
                    self.state = S::ScriptData;
                }
            }
            S::ScriptDataEscapeStartDash => {
                let c = self.consume();
                if c == Some('-') {
                    self.state = S::ScriptDataEscapedDashDash;
                    self.emit_char('-');
                } else {
                    self.reconsume(c);
                    self.state = S::ScriptData;
                }
            }
            S::ScriptDataEscaped => match self.consume() {
                Some('-') => {
                    self.state = S::ScriptDataEscapedDash;
                    self.emit_char('-');
                }
                Some('<') => self.state = S::ScriptDataEscapedLessThanSign,
                Some('\0') => {
                    self.error();
                    self.emit_char('\u{FFFD}');
                }
                Some(c) => self.emit_input(c),
                None => {
                    self.error();
                    self.emit_eof();
                }
            },
            S::ScriptDataEscapedDash => match self.consume() {
                Some('-') => {
                    self.state = S::ScriptDataEscapedDashDash;
                    self.emit_char('-');
                }
                Some('<') => self.state = S::ScriptDataEscapedLessThanSign,
                Some('\0') => {
                    self.error();
                    self.state = S::ScriptDataEscaped;
                    self.emit_char('\u{FFFD}');
                }
                Some(c) => {
                    self.state = S::ScriptDataEscaped;
                    self.emit_input(c);
                }
                None => {
                    self.error();
                    self.emit_eof();
                }
            },
            S::ScriptDataEscapedDashDash => match self.consume() {
                Some('-') => self.emit_char('-'),
                Some('<') => self.state = S::ScriptDataEscapedLessThanSign,
                Some('>') => {
                    self.state = S::ScriptData;
                    self.emit_char('>');
                }
                Some('\0') => {
                    self.error();
                    self.state = S::ScriptDataEscaped;
                    self.emit_char('\u{FFFD}');
                }
                Some(c) => {
                    self.state = S::ScriptDataEscaped;
                    self.emit_input(c);
                }
                None => {
                    self.error();
                    self.emit_eof();
                }
            },
            S::ScriptDataEscapedLessThanSign => {
                let c = self.consume();
                match c {
                    Some('/') => {
                        self.temp.clear();
                        self.state = S::ScriptDataEscapedEndTagOpen;
                    }
                    Some(ch) if ch.is_ascii_alphabetic() => {
                        self.temp.clear();
                        self.emit_char('<');
                        self.reconsume(c);
                        self.state = S::ScriptDataDoubleEscapeStart;
                    }
                    _ => {
                        self.emit_char('<');
                        self.reconsume(c);
                        self.state = S::ScriptDataEscaped;
                    }
                }
            }
            S::ScriptDataEscapedEndTagOpen => {
                let c = self.consume();
                if c.is_some_and(|ch| ch.is_ascii_alphabetic()) {
                    self.new_tag(TagKind::End);
                    self.reconsume(c);
                    self.state = S::ScriptDataEscapedEndTagName;
                } else {
                    self.emit_str("</");
                    self.reconsume(c);
                    self.state = S::ScriptDataEscaped;
                }
            }
            S::ScriptDataEscapedEndTagName => self.end_tag_name_state(S::ScriptDataEscaped),
            S::ScriptDataDoubleEscapeStart => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) || ch == '/' || ch == '>' => {
                        self.state = if self.temp == "script" {
                            S::ScriptDataDoubleEscaped
                        } else {
                            S::ScriptDataEscaped
                        };
                        self.emit_input(ch);
                    }
                    Some(ch) if ch.is_ascii_alphabetic() => {
                        {
                            let cp = self.cur(ch.to_ascii_lowercase());
                            self.temp.push_code_point(cp)
                        };
                        self.emit_input(ch);
                    }
                    _ => {
                        self.reconsume(c);
                        self.state = S::ScriptDataEscaped;
                    }
                }
            }
            S::ScriptDataDoubleEscaped => match self.consume() {
                Some('-') => {
                    self.state = S::ScriptDataDoubleEscapedDash;
                    self.emit_char('-');
                }
                Some('<') => {
                    self.state = S::ScriptDataDoubleEscapedLessThanSign;
                    self.emit_char('<');
                }
                Some('\0') => {
                    self.error();
                    self.emit_char('\u{FFFD}');
                }
                Some(c) => self.emit_input(c),
                None => {
                    self.error();
                    self.emit_eof();
                }
            },
            S::ScriptDataDoubleEscapedDash => match self.consume() {
                Some('-') => {
                    self.state = S::ScriptDataDoubleEscapedDashDash;
                    self.emit_char('-');
                }
                Some('<') => {
                    self.state = S::ScriptDataDoubleEscapedLessThanSign;
                    self.emit_char('<');
                }
                Some('\0') => {
                    self.error();
                    self.state = S::ScriptDataDoubleEscaped;
                    self.emit_char('\u{FFFD}');
                }
                Some(c) => {
                    self.state = S::ScriptDataDoubleEscaped;
                    self.emit_input(c);
                }
                None => {
                    self.error();
                    self.emit_eof();
                }
            },
            S::ScriptDataDoubleEscapedDashDash => match self.consume() {
                Some('-') => self.emit_char('-'),
                Some('<') => {
                    self.state = S::ScriptDataDoubleEscapedLessThanSign;
                    self.emit_char('<');
                }
                Some('>') => {
                    self.state = S::ScriptData;
                    self.emit_char('>');
                }
                Some('\0') => {
                    self.error();
                    self.state = S::ScriptDataDoubleEscaped;
                    self.emit_char('\u{FFFD}');
                }
                Some(c) => {
                    self.state = S::ScriptDataDoubleEscaped;
                    self.emit_input(c);
                }
                None => {
                    self.error();
                    self.emit_eof();
                }
            },
            S::ScriptDataDoubleEscapedLessThanSign => {
                let c = self.consume();
                if c == Some('/') {
                    self.temp.clear();
                    self.state = S::ScriptDataDoubleEscapeEnd;
                    self.emit_char('/');
                } else {
                    self.reconsume(c);
                    self.state = S::ScriptDataDoubleEscaped;
                }
            }
            S::ScriptDataDoubleEscapeEnd => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) || ch == '/' || ch == '>' => {
                        self.state = if self.temp == "script" {
                            S::ScriptDataEscaped
                        } else {
                            S::ScriptDataDoubleEscaped
                        };
                        self.emit_input(ch);
                    }
                    Some(ch) if ch.is_ascii_alphabetic() => {
                        {
                            let cp = self.cur(ch.to_ascii_lowercase());
                            self.temp.push_code_point(cp)
                        };
                        self.emit_input(ch);
                    }
                    _ => {
                        self.reconsume(c);
                        self.state = S::ScriptDataDoubleEscaped;
                    }
                }
            }
            S::BeforeAttributeName => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) => {}
                    Some('/') | Some('>') | None => {
                        self.reconsume(c);
                        self.state = S::AfterAttributeName;
                    }
                    Some('=') => {
                        self.error();
                        self.start_attr();
                        self.attr_name.push('=');
                        self.state = S::AttributeName;
                    }
                    _ => {
                        self.start_attr();
                        self.reconsume(c);
                        self.state = S::AttributeName;
                    }
                }
            }
            S::AttributeName => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) || ch == '/' || ch == '>' => {
                        self.reconsume(c);
                        self.state = S::AfterAttributeName;
                    }
                    None => {
                        self.reconsume(c);
                        self.state = S::AfterAttributeName;
                    }
                    Some('=') => self.state = S::BeforeAttributeValue,
                    Some('\0') => {
                        self.error();
                        self.attr_name.push('\u{FFFD}');
                    }
                    Some(ch) => {
                        if matches!(ch, '"' | '\'' | '<') {
                            self.error();
                        }
                        {
                            let cp = self.cur(ch.to_ascii_lowercase());
                            self.attr_name.push_code_point(cp)
                        };
                    }
                }
            }
            S::AfterAttributeName => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) => {}
                    Some('/') => self.state = S::SelfClosingStartTag,
                    Some('=') => self.state = S::BeforeAttributeValue,
                    Some('>') => {
                        self.state = S::Data;
                        self.emit_tag();
                    }
                    None => {
                        self.error();
                        self.emit_eof();
                    }
                    _ => {
                        self.start_attr();
                        self.reconsume(c);
                        self.state = S::AttributeName;
                    }
                }
            }
            S::BeforeAttributeValue => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) => {}
                    Some('"') => self.state = S::AttributeValueDoubleQuoted,
                    Some('\'') => self.state = S::AttributeValueSingleQuoted,
                    Some('>') => {
                        self.error();
                        self.state = S::Data;
                        self.emit_tag();
                    }
                    _ => {
                        self.reconsume(c);
                        self.state = S::AttributeValueUnquoted;
                    }
                }
            }
            S::AttributeValueDoubleQuoted | S::AttributeValueSingleQuoted => {
                let quote = if self.state == S::AttributeValueDoubleQuoted { '"' } else { '\'' };
                match self.consume() {
                    Some(c) if c == quote => self.state = S::AfterAttributeValueQuoted,
                    Some('&') => {
                        self.return_state = self.state;
                        self.state = S::CharacterReference;
                    }
                    Some('\0') => {
                        self.error();
                        self.attr_value.push('\u{FFFD}');
                    }
                    Some(c) => {
                        let cp = self.cur(c);
                        self.attr_value.push_code_point(cp)
                    }
                    None => {
                        self.error();
                        self.emit_eof();
                    }
                }
            }
            S::AttributeValueUnquoted => match self.consume() {
                Some(c) if is_ascii_ws(c) => self.state = S::BeforeAttributeName,
                Some('&') => {
                    self.return_state = S::AttributeValueUnquoted;
                    self.state = S::CharacterReference;
                }
                Some('>') => {
                    self.state = S::Data;
                    self.emit_tag();
                }
                Some('\0') => {
                    self.error();
                    self.attr_value.push('\u{FFFD}');
                }
                Some(c) => {
                    if matches!(c, '"' | '\'' | '<' | '=' | '`') {
                        self.error();
                    }
                    {
                        let cp = self.cur(c);
                        self.attr_value.push_code_point(cp)
                    };
                }
                None => {
                    self.error();
                    self.emit_eof();
                }
            },
            S::AfterAttributeValueQuoted => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) => self.state = S::BeforeAttributeName,
                    Some('/') => self.state = S::SelfClosingStartTag,
                    Some('>') => {
                        self.state = S::Data;
                        self.emit_tag();
                    }
                    None => {
                        self.error();
                        self.emit_eof();
                    }
                    _ => {
                        self.error();
                        self.reconsume(c);
                        self.state = S::BeforeAttributeName;
                    }
                }
            }
            S::SelfClosingStartTag => {
                let c = self.consume();
                match c {
                    Some('>') => {
                        self.tag.self_closing = true;
                        self.state = S::Data;
                        self.emit_tag();
                    }
                    None => {
                        self.error();
                        self.emit_eof();
                    }
                    _ => {
                        self.error();
                        self.reconsume(c);
                        self.state = S::BeforeAttributeName;
                    }
                }
            }
            // HTML §13.2.5.72 processing instruction open state.
            S::ProcessingInstructionOpen => {
                let c = self.consume();
                match c {
                    Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => {
                        self.reconsume(c);
                        self.state = S::ProcessingInstructionTarget;
                    }
                    None => {
                        self.error(); // eof-in-processing-instruction
                        self.emit_eof();
                    }
                    Some(_) => {
                        self.error(); // invalid-first-character-of-processing-instruction-target
                        self.temp_to_comment();
                        self.reconsume(c);
                        self.state = S::BogusComment;
                    }
                }
            }
            // HTML §13.2.5.73 processing instruction target state.
            S::ProcessingInstructionTarget => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) || ch == '?' || ch == '>' => {
                        let lower = self.temp.as_str().map(str::to_ascii_lowercase);
                        if matches!(lower.as_deref(), Some("xml" | "xml-stylesheet")) {
                            self.error(); // disallowed-processing-instruction-target
                            self.temp_to_comment();
                            self.reconsume(c);
                            self.state = S::BogusComment;
                        } else {
                            let target = std::mem::take(&mut self.temp);
                            self.pi = Some((target, Wtf8Buf::new()));
                            self.reconsume(c);
                            self.state = S::AfterProcessingInstructionTarget;
                        }
                    }
                    Some(ch) if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' => self.temp.push(ch),
                    None => {
                        self.error(); // eof-in-processing-instruction
                        self.emit_eof();
                    }
                    Some(_) => {
                        self.error(); // invalid-processing-instruction-target
                        self.temp_to_comment();
                        self.reconsume(c);
                        self.state = S::BogusComment;
                    }
                }
            }
            // HTML §13.2.5.74 after processing instruction target state.
            S::AfterProcessingInstructionTarget => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) => {}
                    _ => {
                        self.reconsume(c);
                        self.state = S::ProcessingInstructionData;
                    }
                }
            }
            // HTML §13.2.5.75 processing instruction data state.
            S::ProcessingInstructionData => match self.consume() {
                Some('?') => self.state = S::ProcessingInstructionQuestionable,
                Some('>') => {
                    self.state = S::Data;
                    self.emit_pi();
                }
                None => {
                    self.error(); // eof-in-processing-instruction
                    self.pi = None;
                    self.emit_eof();
                }
                Some(c) => {
                    let cp = self.cur(c);
                    if let Some((_, data)) = &mut self.pi {
                        data.push_code_point(cp);
                    }
                }
            },
            // HTML §13.2.5.76 processing instruction questionable state.
            S::ProcessingInstructionQuestionable => {
                let c = self.consume();
                match c {
                    Some('>') => {
                        self.state = S::Data;
                        self.emit_pi();
                    }
                    None => {
                        self.error(); // eof-in-processing-instruction
                        self.pi = None;
                        self.emit_eof();
                    }
                    Some(_) => {
                        if let Some((_, data)) = &mut self.pi {
                            data.push('?');
                        }
                        self.reconsume(c);
                        self.state = S::ProcessingInstructionData;
                    }
                }
            }
            S::BogusComment => match self.consume() {
                Some('>') => {
                    self.state = S::Data;
                    self.emit_comment();
                }
                None => {
                    self.emit_comment();
                    self.emit_eof();
                }
                Some('\0') => {
                    self.error();
                    self.comment.push('\u{FFFD}');
                }
                Some(c) => {
                    let cp = self.cur(c);
                    self.comment.push_code_point(cp)
                }
            },
            S::MarkupDeclarationOpen => {
                if self.peek_matches("--", false) {
                    self.pos += 2;
                    self.comment.clear();
                    self.state = S::CommentStart;
                } else if self.peek_matches("doctype", true) {
                    self.pos += 7;
                    self.state = S::Doctype;
                } else if self.peek_matches("[CDATA[", false) {
                    self.pos += 7;
                    if self.allow_cdata {
                        self.state = S::CdataSection;
                    } else {
                        self.error();
                        self.comment = Wtf8Buf::from("[CDATA[");
                        self.state = S::BogusComment;
                    }
                } else {
                    self.error();
                    self.comment.clear();
                    self.state = S::BogusComment;
                }
            }
            S::CommentStart => {
                let c = self.consume();
                match c {
                    Some('-') => self.state = S::CommentStartDash,
                    Some('>') => {
                        self.error();
                        self.state = S::Data;
                        self.emit_comment();
                    }
                    _ => {
                        self.reconsume(c);
                        self.state = S::Comment;
                    }
                }
            }
            S::CommentStartDash => {
                let c = self.consume();
                match c {
                    Some('-') => self.state = S::CommentEnd,
                    Some('>') => {
                        self.error();
                        self.state = S::Data;
                        self.emit_comment();
                    }
                    None => {
                        self.error();
                        self.emit_comment();
                        self.emit_eof();
                    }
                    _ => {
                        self.comment.push('-');
                        self.reconsume(c);
                        self.state = S::Comment;
                    }
                }
            }
            S::Comment => match self.consume() {
                Some('<') => {
                    self.comment.push('<');
                    self.state = S::CommentLessThanSign;
                }
                Some('-') => self.state = S::CommentEndDash,
                Some('\0') => {
                    self.error();
                    self.comment.push('\u{FFFD}');
                }
                Some(c) => {
                    let cp = self.cur(c);
                    self.comment.push_code_point(cp)
                }
                None => {
                    self.error();
                    self.emit_comment();
                    self.emit_eof();
                }
            },
            S::CommentLessThanSign => {
                let c = self.consume();
                match c {
                    Some('!') => {
                        self.comment.push('!');
                        self.state = S::CommentLessThanSignBang;
                    }
                    Some('<') => self.comment.push('<'),
                    _ => {
                        self.reconsume(c);
                        self.state = S::Comment;
                    }
                }
            }
            S::CommentLessThanSignBang => {
                let c = self.consume();
                if c == Some('-') {
                    self.state = S::CommentLessThanSignBangDash;
                } else {
                    self.reconsume(c);
                    self.state = S::Comment;
                }
            }
            S::CommentLessThanSignBangDash => {
                let c = self.consume();
                if c == Some('-') {
                    self.state = S::CommentLessThanSignBangDashDash;
                } else {
                    self.reconsume(c);
                    self.state = S::CommentEndDash;
                }
            }
            S::CommentLessThanSignBangDashDash => {
                let c = self.consume();
                if c != Some('>') && c.is_some() {
                    self.error();
                }
                self.reconsume(c);
                self.state = S::CommentEnd;
            }
            S::CommentEndDash => {
                let c = self.consume();
                match c {
                    Some('-') => self.state = S::CommentEnd,
                    None => {
                        self.error();
                        self.emit_comment();
                        self.emit_eof();
                    }
                    _ => {
                        self.comment.push('-');
                        self.reconsume(c);
                        self.state = S::Comment;
                    }
                }
            }
            S::CommentEnd => {
                let c = self.consume();
                match c {
                    Some('>') => {
                        self.state = S::Data;
                        self.emit_comment();
                    }
                    Some('!') => self.state = S::CommentEndBang,
                    Some('-') => self.comment.push('-'),
                    None => {
                        self.error();
                        self.emit_comment();
                        self.emit_eof();
                    }
                    _ => {
                        self.comment.push_str("--");
                        self.reconsume(c);
                        self.state = S::Comment;
                    }
                }
            }
            S::CommentEndBang => {
                let c = self.consume();
                match c {
                    Some('-') => {
                        self.comment.push_str("--!");
                        self.state = S::CommentEndDash;
                    }
                    Some('>') => {
                        self.error();
                        self.state = S::Data;
                        self.emit_comment();
                    }
                    None => {
                        self.error();
                        self.emit_comment();
                        self.emit_eof();
                    }
                    _ => {
                        self.comment.push_str("--!");
                        self.reconsume(c);
                        self.state = S::Comment;
                    }
                }
            }
            S::Doctype => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) => self.state = S::BeforeDoctypeName,
                    Some('>') => {
                        self.reconsume(c);
                        self.state = S::BeforeDoctypeName;
                    }
                    None => {
                        self.error();
                        self.doctype = Doctype { force_quirks: true, ..Default::default() };
                        self.emit_doctype();
                        self.emit_eof();
                    }
                    _ => {
                        self.error();
                        self.reconsume(c);
                        self.state = S::BeforeDoctypeName;
                    }
                }
            }
            S::BeforeDoctypeName => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) => {}
                    Some('\0') => {
                        self.error();
                        self.doctype = Doctype { name: Some("\u{FFFD}".into()), ..Default::default() };
                        self.state = S::DoctypeName;
                    }
                    Some('>') => {
                        self.error();
                        self.doctype = Doctype { force_quirks: true, ..Default::default() };
                        self.state = S::Data;
                        self.emit_doctype();
                    }
                    None => {
                        self.error();
                        self.doctype = Doctype { force_quirks: true, ..Default::default() };
                        self.emit_doctype();
                        self.emit_eof();
                    }
                    Some(ch) => {
                        let cp = self.cur(ch.to_ascii_lowercase());
                        self.doctype =
                            Doctype { name: Some(Wtf8Buf::from_code_points([cp])), ..Default::default() };
                        self.state = S::DoctypeName;
                    }
                }
            }
            S::DoctypeName => match self.consume() {
                Some(c) if is_ascii_ws(c) => self.state = S::AfterDoctypeName,
                Some('>') => {
                    self.state = S::Data;
                    self.emit_doctype();
                }
                Some('\0') => {
                    self.error();
                    self.doctype.name.get_or_insert_with(Wtf8Buf::new).push('\u{FFFD}');
                }
                Some(c) => {
                    let cp = self.cur(c.to_ascii_lowercase());
                    self.doctype.name.get_or_insert_with(Wtf8Buf::new).push_code_point(cp)
                }
                None => {
                    self.error();
                    self.doctype.force_quirks = true;
                    self.emit_doctype();
                    self.emit_eof();
                }
            },
            S::AfterDoctypeName => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) => {}
                    Some('>') => {
                        self.state = S::Data;
                        self.emit_doctype();
                    }
                    None => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.emit_doctype();
                        self.emit_eof();
                    }
                    _ => {
                        self.reconsume(c);
                        if self.peek_matches("public", true) {
                            self.pos += 6;
                            self.state = S::AfterDoctypePublicKeyword;
                        } else if self.peek_matches("system", true) {
                            self.pos += 6;
                            self.state = S::AfterDoctypeSystemKeyword;
                        } else {
                            self.error();
                            self.doctype.force_quirks = true;
                            self.pos += 1;
                            self.state = S::BogusDoctype;
                        }
                    }
                }
            }
            S::AfterDoctypePublicKeyword | S::AfterDoctypeSystemKeyword => {
                let public = self.state == S::AfterDoctypePublicKeyword;
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) => {
                        self.state = if public {
                            S::BeforeDoctypePublicIdentifier
                        } else {
                            S::BeforeDoctypeSystemIdentifier
                        }
                    }
                    Some(q @ ('"' | '\'')) => {
                        self.error();
                        self.set_doctype_id(public, q);
                    }
                    Some('>') => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.state = S::Data;
                        self.emit_doctype();
                    }
                    None => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.emit_doctype();
                        self.emit_eof();
                    }
                    _ => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.reconsume(c);
                        self.state = S::BogusDoctype;
                    }
                }
            }
            S::BeforeDoctypePublicIdentifier | S::BeforeDoctypeSystemIdentifier => {
                let public = self.state == S::BeforeDoctypePublicIdentifier;
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) => {}
                    Some(q @ ('"' | '\'')) => self.set_doctype_id(public, q),
                    Some('>') => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.state = S::Data;
                        self.emit_doctype();
                    }
                    None => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.emit_doctype();
                        self.emit_eof();
                    }
                    _ => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.reconsume(c);
                        self.state = S::BogusDoctype;
                    }
                }
            }
            S::DoctypePublicIdentifierDoubleQuoted
            | S::DoctypePublicIdentifierSingleQuoted
            | S::DoctypeSystemIdentifierDoubleQuoted
            | S::DoctypeSystemIdentifierSingleQuoted => {
                let (public, quote) = match self.state {
                    S::DoctypePublicIdentifierDoubleQuoted => (true, '"'),
                    S::DoctypePublicIdentifierSingleQuoted => (true, '\''),
                    S::DoctypeSystemIdentifierDoubleQuoted => (false, '"'),
                    _ => (false, '\''),
                };
                let c = self.consume();
                fn target(d: &mut Doctype<Wtf8Buf>, public: bool) -> &mut Wtf8Buf {
                    if public {
                        d.public_id.get_or_insert_with(Wtf8Buf::new)
                    } else {
                        d.system_id.get_or_insert_with(Wtf8Buf::new)
                    }
                }
                match c {
                    Some(ch) if ch == quote => {
                        self.state = if public {
                            S::AfterDoctypePublicIdentifier
                        } else {
                            S::AfterDoctypeSystemIdentifier
                        }
                    }
                    Some('\0') => {
                        self.error();
                        target(&mut self.doctype, public).push('\u{FFFD}');
                    }
                    Some('>') => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.state = S::Data;
                        self.emit_doctype();
                    }
                    Some(ch) => {
                        let cp = self.cur(ch);
                        target(&mut self.doctype, public).push_code_point(cp)
                    }
                    None => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.emit_doctype();
                        self.emit_eof();
                    }
                }
            }
            S::AfterDoctypePublicIdentifier => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) => self.state = S::BetweenDoctypePublicAndSystemIdentifiers,
                    Some('>') => {
                        self.state = S::Data;
                        self.emit_doctype();
                    }
                    Some(q @ ('"' | '\'')) => {
                        self.error();
                        self.set_doctype_id(false, q);
                    }
                    None => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.emit_doctype();
                        self.emit_eof();
                    }
                    _ => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.reconsume(c);
                        self.state = S::BogusDoctype;
                    }
                }
            }
            S::BetweenDoctypePublicAndSystemIdentifiers => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) => {}
                    Some('>') => {
                        self.state = S::Data;
                        self.emit_doctype();
                    }
                    Some(q @ ('"' | '\'')) => self.set_doctype_id(false, q),
                    None => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.emit_doctype();
                        self.emit_eof();
                    }
                    _ => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.reconsume(c);
                        self.state = S::BogusDoctype;
                    }
                }
            }
            S::AfterDoctypeSystemIdentifier => {
                let c = self.consume();
                match c {
                    Some(ch) if is_ascii_ws(ch) => {}
                    Some('>') => {
                        self.state = S::Data;
                        self.emit_doctype();
                    }
                    None => {
                        self.error();
                        self.doctype.force_quirks = true;
                        self.emit_doctype();
                        self.emit_eof();
                    }
                    _ => {
                        self.error();
                        self.reconsume(c);
                        self.state = S::BogusDoctype;
                    }
                }
            }
            S::BogusDoctype => match self.consume() {
                Some('>') => {
                    self.state = S::Data;
                    self.emit_doctype();
                }
                Some('\0') => self.error(),
                Some(_) => {}
                None => {
                    self.emit_doctype();
                    self.emit_eof();
                }
            },
            S::CdataSection => match self.consume() {
                Some(']') => self.state = S::CdataSectionBracket,
                Some(c) => self.emit_input(c),
                None => {
                    self.error();
                    self.emit_eof();
                }
            },
            S::CdataSectionBracket => {
                let c = self.consume();
                if c == Some(']') {
                    self.state = S::CdataSectionEnd;
                } else {
                    self.emit_char(']');
                    self.reconsume(c);
                    self.state = S::CdataSection;
                }
            }
            S::CdataSectionEnd => {
                let c = self.consume();
                match c {
                    Some(']') => self.emit_char(']'),
                    Some('>') => self.state = S::Data,
                    _ => {
                        self.emit_str("]]");
                        self.reconsume(c);
                        self.state = S::CdataSection;
                    }
                }
            }
            S::CharacterReference => {
                self.temp.clear();
                self.temp.push('&');
                let c = self.consume();
                match c {
                    Some(ch) if ch.is_ascii_alphanumeric() => {
                        self.reconsume(c);
                        self.state = S::NamedCharacterReference;
                    }
                    Some('#') => {
                        self.temp.push('#');
                        self.state = S::NumericCharacterReference;
                    }
                    _ => {
                        self.flush_temp_as_char_ref();
                        self.reconsume(c);
                        self.state = self.return_state;
                    }
                }
            }
            S::NamedCharacterReference => self.named_char_ref(),
            S::AmbiguousAmpersand => {
                let c = self.consume();
                match c {
                    Some(ch) if ch.is_ascii_alphanumeric() => {
                        if self.char_ref_in_attr() {
                            {
                                let cp = self.cur(ch);
                                self.attr_value.push_code_point(cp)
                            };
                        } else {
                            self.emit_input(ch);
                        }
                    }
                    Some(';') => {
                        self.error();
                        self.reconsume(c);
                        self.state = self.return_state;
                    }
                    _ => {
                        self.reconsume(c);
                        self.state = self.return_state;
                    }
                }
            }
            S::NumericCharacterReference => {
                self.char_ref_code = 0;
                let c = self.consume();
                match c {
                    Some(x @ ('x' | 'X')) => {
                        self.temp.push(x);
                        self.state = S::HexadecimalCharacterReferenceStart;
                    }
                    _ => {
                        self.reconsume(c);
                        self.state = S::DecimalCharacterReferenceStart;
                    }
                }
            }
            S::HexadecimalCharacterReferenceStart => {
                let c = self.consume();
                if c.is_some_and(|ch| ch.is_ascii_hexdigit()) {
                    self.reconsume(c);
                    self.state = S::HexadecimalCharacterReference;
                } else {
                    self.error();
                    self.flush_temp_as_char_ref();
                    self.reconsume(c);
                    self.state = self.return_state;
                }
            }
            S::DecimalCharacterReferenceStart => {
                let c = self.consume();
                if c.is_some_and(|ch| ch.is_ascii_digit()) {
                    self.reconsume(c);
                    self.state = S::DecimalCharacterReference;
                } else {
                    self.error();
                    self.flush_temp_as_char_ref();
                    self.reconsume(c);
                    self.state = self.return_state;
                }
            }
            S::HexadecimalCharacterReference | S::DecimalCharacterReference => {
                let radix = if self.state == S::HexadecimalCharacterReference { 16 } else { 10 };
                let c = self.consume();
                match c.and_then(|ch| ch.to_digit(radix)) {
                    Some(d) => {
                        self.char_ref_code =
                            self.char_ref_code.saturating_mul(radix).saturating_add(d).min(0x11_0000);
                    }
                    None if c == Some(';') => self.state = S::NumericCharacterReferenceEnd,
                    None => {
                        self.error();
                        self.reconsume(c);
                        self.state = S::NumericCharacterReferenceEnd;
                    }
                }
            }
            S::NumericCharacterReferenceEnd => {
                let ch = numeric_char_ref_value(self.char_ref_code, &mut self.errors);
                self.temp.clear();
                self.temp.push(ch);
                self.flush_temp_as_char_ref();
                self.state = self.return_state;
            }
        }
    }

    fn set_doctype_id(&mut self, public: bool, quote: char) {
        use State as S;
        if public {
            self.doctype.public_id = Some(Wtf8Buf::new());
            self.state = if quote == '"' {
                S::DoctypePublicIdentifierDoubleQuoted
            } else {
                S::DoctypePublicIdentifierSingleQuoted
            };
        } else {
            self.doctype.system_id = Some(Wtf8Buf::new());
            self.state = if quote == '"' {
                S::DoctypeSystemIdentifierDoubleQuoted
            } else {
                S::DoctypeSystemIdentifierSingleQuoted
            };
        }
    }

    /// HTML §13.2.5.73 named character reference state.
    fn named_char_ref(&mut self) {
        // Find the longest entity name matching the upcoming input.
        let mut best: Option<(usize, &'static str)> = None;
        let mut prefix = String::new();
        let mut lo = 0usize;
        let mut hi = ENTITIES.len();
        for k in 0..32 {
            let Some(&c) = self.input.get(self.pos + k) else { break };
            prefix.push(c);
            // Narrow [lo, hi) to entries starting with `prefix`.
            let slice = &ENTITIES[lo..hi];
            let start = slice.partition_point(|(n, _)| *n < prefix.as_str());
            let end = slice[start..].partition_point(|(n, _)| n.starts_with(prefix.as_str()));
            if end == 0 {
                break;
            }
            lo += start;
            hi = lo + end;
            if ENTITIES[lo].0 == prefix {
                best = Some((k + 1, ENTITIES[lo].1));
            }
        }
        match best {
            Some((len, chars)) => {
                let name: String = self.input[self.pos..self.pos + len].iter().collect();
                self.pos += len;
                self.temp.push_str(&name);
                let last_semicolon = name.ends_with(';');
                let next = self.input.get(self.pos).copied();
                if self.char_ref_in_attr()
                    && !last_semicolon
                    && next.is_some_and(|n| n == '=' || n.is_ascii_alphanumeric())
                {
                    self.flush_temp_as_char_ref();
                    self.state = self.return_state;
                    return;
                }
                if !last_semicolon {
                    self.error();
                }
                self.temp.clear();
                self.temp.push_str(chars);
                self.flush_temp_as_char_ref();
                self.state = self.return_state;
            }
            None => {
                self.flush_temp_as_char_ref();
                self.state = State::AmbiguousAmpersand;
            }
        }
    }
}

/// HTML §13.2.5.80 numeric character reference end state: map the code.
fn numeric_char_ref_value(code: u32, errors: &mut usize) -> char {
    if code == 0 || code > 0x10FFFF || (0xD800..=0xDFFF).contains(&code) {
        *errors += 1;
        return '\u{FFFD}';
    }
    if (0x80..=0x9F).contains(&code) {
        const TABLE: [u32; 32] = [
            0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
            0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC,
            0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
        ];
        *errors += 1;
        return char::from_u32(TABLE[(code - 0x80) as usize]).unwrap_or('\u{FFFD}');
    }
    char::from_u32(code).unwrap_or('\u{FFFD}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Token> {
        let mut t = Tokenizer::new(s);
        let mut v = Vec::new();
        loop {
            let tok = t.next_token();
            if tok == Token::Eof {
                break;
            }
            v.push(tok);
        }
        v
    }

    #[test]
    fn simple() {
        let v = toks("<p class=a id='b' x>Hi &amp; &notit; &#x41;</p><!--c-->");
        assert_eq!(
            v[0],
            Token::StartTag(Tag {
                name: "p".into(),
                attrs: vec![("class".into(), "a".into()), ("id".into(), "b".into()), ("x".into(), "".into())],
                self_closing: false
            })
        );
        assert_eq!(v[1], Token::Characters("Hi & ¬it; A".into()));
        assert_eq!(v[3], Token::Comment("c".into()));
    }

    /// Tokenize UTF-16 input (so lone surrogates can be written) and return raw tokens.
    fn raw(units: &[u16]) -> Vec<RawToken> {
        let mut t = Tokenizer::from_wtf8(&Wtf8Buf::from_utf16(units));
        let mut v = Vec::new();
        loop {
            let tok = t.next_raw_token();
            if tok == Token::Eof {
                break;
            }
            v.push(tok);
        }
        v
    }

    fn u16s(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// `prefix` + lone surrogate U+D800 + `suffix`, as UTF-16.
    fn with_surrogate(prefix: &str, suffix: &str) -> Vec<u16> {
        let mut v = u16s(prefix);
        v.push(0xD800);
        v.extend(u16s(suffix));
        v
    }

    fn w(prefix: &str, suffix: &str) -> Wtf8Buf {
        Wtf8Buf::from_utf16(&with_surrogate(prefix, suffix))
    }

    // HTML §13.2.3.5: lone surrogates are a parse error but are passed through unchanged.
    #[test]
    fn lone_surrogates_pass_through_everywhere() {
        // Text, including the plain-text fast path after the first character.
        assert_eq!(raw(&with_surrogate("ab", "cd")), vec![Token::Characters(w("ab", "cd"))]);
        // Attribute values (quoted and unquoted), attribute names and tag names.
        match &raw(&with_surrogate("<a title='x", "y' b=z>"))[0] {
            Token::StartTag(t) => assert_eq!(t.attrs[0].1, w("x", "y")),
            other => panic!("{other:?}"),
        }
        match &raw(&with_surrogate("<a b=", ">"))[0] {
            Token::StartTag(t) => assert_eq!(t.attrs[0].1, w("", "")),
            other => panic!("{other:?}"),
        }
        match &raw(&with_surrogate("<A B", "C=1>"))[0] {
            Token::StartTag(t) => assert_eq!(t.attrs[0].0, w("b", "c")),
            other => panic!("{other:?}"),
        }
        match &raw(&with_surrogate("<X", "Y>"))[0] {
            Token::StartTag(t) => assert_eq!(t.name, w("x", "y")),
            other => panic!("{other:?}"),
        }
        // Comments, bogus comments and doctype names.
        assert_eq!(raw(&with_surrogate("<!--a", "b-->")), vec![Token::Comment(w("a", "b"))]);
        match &raw(&with_surrogate("<!DOCTYPE ", "x>"))[0] {
            Token::Doctype(d) => assert_eq!(d.name, Some(w("", "x"))),
            other => panic!("{other:?}"),
        }
        // After a character reference, and in RCDATA after an end-tag-name mismatch.
        assert_eq!(raw(&with_surrogate("&amp;", "")), vec![Token::Characters(w("&", ""))]);
        let mut t = Tokenizer::from_wtf8(&w("</ti", "</title>"));
        t.state = State::Rcdata;
        t.set_last_start_tag(Some("title"));
        assert_eq!(t.next_raw_token(), Token::Characters(w("</ti", "")));
    }

    #[test]
    fn real_replacement_characters_stay_replacement_characters() {
        // U+FFFD in the input is the surrogate placeholder internally; it must not turn into a
        // surrogate, even next to one.
        let mut v = u16s("a\u{FFFD}");
        v.push(0xDC00);
        v.extend(u16s("\u{FFFD}"));
        assert_eq!(raw(&v), vec![Token::Characters(Wtf8Buf::from_utf16(&v))]);
        assert_eq!(toks("<p x='\u{FFFD}'>")[0], toks("<p x='\u{FFFD}'>")[0]);
    }

    #[test]
    fn lossy_tokens_replace_lone_surrogates() {
        let mut t = Tokenizer::from_wtf8(&w("a", "b"));
        assert_eq!(t.next_token(), Token::Characters("a\u{FFFD}b".into()));
    }

    #[test]
    fn entity_in_attribute_historical() {
        let v = toks("<a href='?a=1&copy=2&copy;'>");
        match &v[0] {
            Token::StartTag(t) => assert_eq!(t.attr("href"), Some("?a=1&copy=2©")),
            _ => panic!(),
        }
    }

    #[test]
    fn rcdata_switch() {
        let mut t = Tokenizer::new("<title>a<b>&lt;</title>x");
        assert!(matches!(t.next_token(), Token::StartTag(_)));
        t.state = State::Rcdata;
        assert_eq!(t.next_token(), Token::Characters("a<b><".into()));
        assert!(matches!(t.next_token(), Token::EndTag(_)));
        assert_eq!(t.next_token(), Token::Characters("x".into()));
    }
}

//! `bb-url` — an implementation of the [WHATWG URL Standard](https://url.spec.whatwg.org/).
//!
//! The parser follows the "basic URL parser" state machine (URL §4.4) code
//! point by code point. Hosts are parsed per URL §3.5, including IPv4/IPv6
//! literals and a UTS #46 "domain to ASCII" implementation (see [`idna`]).
//!
//! ```
//! let base = bb_url::Url::parse("https://example.com/a/b?c#d").unwrap();
//! let u = base.join("../x?y").unwrap();
//! assert_eq!(u.href(), "https://example.com/x?y");
//! ```

mod host;
pub mod idna;
pub mod percent;

pub use host::Host;
use percent::{utf8_percent_encode_char, EncodeSet};
use std::fmt;

/// Why a URL failed to parse. The URL Standard only has "failure"; the
/// variants exist to make debugging easier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Relative input with no (or an unusable) base URL.
    MissingSchemeNonRelativeUrl,
    /// The host component was empty where one is required.
    HostMissing,
    /// The host failed host parsing (invalid IPv4/IPv6/domain).
    InvalidHost,
    /// The port is not a number or out of range.
    InvalidPort,
    /// Credentials but no host (`http://user@/`).
    InvalidCredentials,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            ParseError::MissingSchemeNonRelativeUrl => "relative URL without a base",
            ParseError::HostMissing => "missing host",
            ParseError::InvalidHost => "invalid host",
            ParseError::InvalidPort => "invalid port",
            ParseError::InvalidCredentials => "credentials without host",
        };
        f.write_str(s)
    }
}

impl std::error::Error for ParseError {}

/// A URL's path: either an opaque string (`mailto:x`, `data:...`) or a list of
/// segments (URL §4.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Path {
    Opaque(String),
    List(Vec<String>),
}

/// A parsed URL record (URL §4.1).
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Url {
    scheme: String,
    username: String,
    password: String,
    host: Option<Host>,
    port: Option<u16>,
    path: Path,
    query: Option<String>,
    fragment: Option<String>,
}

impl fmt::Debug for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Url({})", self.href())
    }
}

impl fmt::Display for Url {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.href())
    }
}

/// An origin (HTML §7.1.1): either opaque or a (scheme, host, port) tuple.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Origin {
    Opaque,
    Tuple(String, Host, Option<u16>),
}

impl Origin {
    /// ASCII serialization of an origin (HTML §7.1.1).
    pub fn serialize(&self) -> String {
        match self {
            Origin::Opaque => "null".to_string(),
            Origin::Tuple(scheme, host, port) => {
                let mut s = format!("{scheme}://{}", host.serialize());
                if let Some(p) = port {
                    s.push(':');
                    s.push_str(&p.to_string());
                }
                s
            }
        }
    }

    /// Same-origin check. Opaque origins are never same-origin with anything
    /// (we don't track opaque origin identity yet).
    pub fn same_origin(&self, other: &Origin) -> bool {
        match (self, other) {
            (Origin::Tuple(..), Origin::Tuple(..)) => self == other,
            _ => false,
        }
    }
}

/// Schemes that are "special" (URL §4.1).
pub fn is_special_scheme(s: &str) -> bool {
    matches!(s, "ftp" | "file" | "http" | "https" | "ws" | "wss")
}

/// The default port of a special scheme.
pub fn default_port(s: &str) -> Option<u16> {
    match s {
        "ftp" => Some(21),
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    SchemeStart,
    Scheme,
    NoScheme,
    SpecialRelativeOrAuthority,
    PathOrAuthority,
    Relative,
    RelativeSlash,
    SpecialAuthoritySlashes,
    SpecialAuthorityIgnoreSlashes,
    Authority,
    Host,
    Port,
    File,
    FileSlash,
    FileHost,
    PathStart,
    Path,
    OpaquePath,
    Query,
    Fragment,
}

fn is_windows_drive_letter(s: &[char]) -> bool {
    s.len() == 2 && s[0].is_ascii_alphabetic() && (s[1] == ':' || s[1] == '|')
}

fn is_normalized_windows_drive_letter(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// URL §4.2 "starts with a Windows drive letter".
fn starts_with_windows_drive_letter(s: &[char]) -> bool {
    s.len() >= 2
        && is_windows_drive_letter(&s[..2])
        && (s.len() == 2 || matches!(s[2], '/' | '\\' | '?' | '#'))
}

fn is_single_dot(s: &str) -> bool {
    s == "." || s.eq_ignore_ascii_case("%2e")
}

fn is_double_dot(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), ".." | ".%2e" | "%2e." | "%2e%2e")
}

impl Url {
    /// Parse `input` with no base URL.
    pub fn parse(input: &str) -> Result<Url, ParseError> {
        Self::parse_with_base(input, None)
    }

    /// Parse `input` against this URL as base.
    pub fn join(&self, input: &str) -> Result<Url, ParseError> {
        Self::parse_with_base(input, Some(self))
    }

    /// The URL Standard's "URL parser" (§4.4) without encoding override.
    pub fn parse_with_base(input: &str, base: Option<&Url>) -> Result<Url, ParseError> {
        // Strip leading/trailing C0 control or space, remove ASCII tab/newline.
        let trimmed = input.trim_matches(|c: char| c <= ' ');
        let chars: Vec<char> = trimmed.chars().filter(|&c| c != '\t' && c != '\n' && c != '\r').collect();
        Parser::new(&chars, base).run()
    }

    // ----- getters (URL §6.1 API-ish) -----

    /// Full serialization (`href`).
    pub fn href(&self) -> String {
        self.serialize(false)
    }

    /// Serialization without the fragment.
    pub fn href_without_fragment(&self) -> String {
        self.serialize(true)
    }

    /// The scheme without the trailing `:` (e.g. `"https"`).
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// `protocol` getter: scheme followed by `:`.
    pub fn protocol(&self) -> String {
        format!("{}:", self.scheme)
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn password(&self) -> &str {
        &self.password
    }

    pub fn host(&self) -> Option<&Host> {
        self.host.as_ref()
    }

    /// `host` getter: hostname plus `:port` if a non-default port is set.
    pub fn host_with_port(&self) -> String {
        match &self.host {
            None => String::new(),
            Some(h) => match self.port {
                Some(p) => format!("{}:{p}", h.serialize()),
                None => h.serialize(),
            },
        }
    }

    /// `hostname` getter.
    pub fn hostname(&self) -> String {
        self.host.as_ref().map(|h| h.serialize()).unwrap_or_default()
    }

    /// The explicit port, if it differs from the scheme's default.
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// The port to connect to (explicit or scheme default).
    pub fn port_or_default(&self) -> Option<u16> {
        self.port.or_else(|| default_port(&self.scheme))
    }

    /// `pathname` getter.
    pub fn pathname(&self) -> String {
        match &self.path {
            Path::Opaque(s) => s.clone(),
            Path::List(segs) => {
                let mut out = String::new();
                for s in segs {
                    out.push('/');
                    out.push_str(s);
                }
                out
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn has_opaque_path(&self) -> bool {
        matches!(self.path, Path::Opaque(_))
    }

    /// The query without `?`, if any.
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// `search` getter: `?query` or empty.
    pub fn search(&self) -> String {
        match &self.query {
            Some(q) if !q.is_empty() => format!("?{q}"),
            _ => String::new(),
        }
    }

    /// The fragment without `#`, if any.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    /// `hash` getter: `#fragment` or empty.
    pub fn hash(&self) -> String {
        match &self.fragment {
            Some(f) if !f.is_empty() => format!("#{f}"),
            _ => String::new(),
        }
    }

    /// Path plus query, as sent in an HTTP request line.
    pub fn request_target(&self) -> String {
        let mut s = self.pathname();
        if s.is_empty() {
            s.push('/');
        }
        if let Some(q) = &self.query {
            s.push('?');
            s.push_str(q);
        }
        s
    }

    pub fn is_special(&self) -> bool {
        is_special_scheme(&self.scheme)
    }

    /// Remove the fragment.
    pub fn set_fragment(&mut self, fragment: Option<&str>) {
        self.fragment = fragment.map(|f| {
            let mut out = String::new();
            for c in f.chars() {
                utf8_percent_encode_char(c, EncodeSet::Fragment, &mut out);
            }
            out
        });
    }

    /// Replace the query with an already-encoded string (e.g. from a form).
    pub fn set_query_encoded(&mut self, query: Option<&str>) {
        self.query = query.map(str::to_string);
    }

    /// The URL's origin (URL §4.6 / HTML §7.1.1).
    pub fn origin(&self) -> Origin {
        match self.scheme.as_str() {
            "blob" => {
                if let Path::Opaque(p) = &self.path {
                    if let Ok(inner) = Url::parse(p) {
                        if matches!(inner.scheme(), "http" | "https") {
                            return inner.origin();
                        }
                    }
                }
                Origin::Opaque
            }
            "ftp" | "http" | "https" | "ws" | "wss" => {
                Origin::Tuple(self.scheme.clone(), self.host.clone().unwrap_or(Host::Empty), self.port)
            }
            _ => Origin::Opaque,
        }
    }

    /// For `file:` URLs: the path as an OS path string (percent-decoded).
    pub fn to_file_path(&self) -> Option<String> {
        if self.scheme != "file" {
            return None;
        }
        let p = self.pathname();
        let bytes = percent::percent_decode(p.as_bytes());
        let mut s = String::from_utf8_lossy(&bytes).into_owned();
        // `/C:/x` -> `C:/x` on Windows-style paths.
        let b = s.as_bytes();
        if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
            s.remove(0);
        }
        Some(s)
    }

    /// Build a `file:` URL from an absolute OS path.
    pub fn from_file_path(path: &str) -> Option<Url> {
        let p = path.replace('\\', "/");
        let p = if p.as_bytes().get(1) == Some(&b':') { format!("/{p}") } else { p };
        if !p.starts_with('/') {
            return None;
        }
        let mut s = String::from("file://");
        for c in p.chars() {
            utf8_percent_encode_char(c, EncodeSet::Path, &mut s);
        }
        Url::parse(&s).ok()
    }

    /// URL serializer (URL §4.5).
    fn serialize(&self, exclude_fragment: bool) -> String {
        let mut out = String::with_capacity(64);
        out.push_str(&self.scheme);
        out.push(':');
        if let Some(host) = &self.host {
            out.push_str("//");
            if !self.username.is_empty() || !self.password.is_empty() {
                out.push_str(&self.username);
                if !self.password.is_empty() {
                    out.push(':');
                    out.push_str(&self.password);
                }
                out.push('@');
            }
            out.push_str(&host.serialize());
            if let Some(p) = self.port {
                out.push(':');
                out.push_str(&p.to_string());
            }
        }
        if let Path::List(segs) = &self.path {
            if self.host.is_none() && segs.len() > 1 && segs[0].is_empty() {
                out.push_str("/.");
            }
        }
        out.push_str(&self.pathname());
        if let Some(q) = &self.query {
            out.push('?');
            out.push_str(q);
        }
        if !exclude_fragment {
            if let Some(f) = &self.fragment {
                out.push('#');
                out.push_str(f);
            }
        }
        out
    }

    fn shorten_path(&mut self) {
        if let Path::List(segs) = &mut self.path {
            if self.scheme == "file" && segs.len() == 1 && is_normalized_windows_drive_letter(&segs[0]) {
                return;
            }
            segs.pop();
        }
    }

    fn path_list(&mut self) -> &mut Vec<String> {
        if let Path::Opaque(_) = self.path {
            self.path = Path::List(Vec::new());
        }
        match &mut self.path {
            Path::List(v) => v,
            Path::Opaque(_) => unreachable!("path converted to list above"),
        }
    }
}

impl std::str::FromStr for Url {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Url::parse(s)
    }
}

struct Parser<'a> {
    input: &'a [char],
    base: Option<&'a Url>,
    url: Url,
    buffer: String,
    at_sign_seen: bool,
    inside_brackets: bool,
    password_token_seen: bool,
}

impl<'a> Parser<'a> {
    fn new(input: &'a [char], base: Option<&'a Url>) -> Self {
        Parser {
            input,
            base,
            url: Url {
                scheme: String::new(),
                username: String::new(),
                password: String::new(),
                host: None,
                port: None,
                path: Path::List(Vec::new()),
                query: None,
                fragment: None,
            },
            buffer: String::new(),
            at_sign_seen: false,
            inside_brackets: false,
            password_token_seen: false,
        }
    }

    fn remaining(&self, p: isize) -> &[char] {
        let start = (p + 1).max(0) as usize;
        self.input.get(start..).unwrap_or(&[])
    }

    // URL §4.4 basic URL parser (no state override, UTF-8 encoding).
    fn run(mut self) -> Result<Url, ParseError> {
        let mut state = State::SchemeStart;
        let len = self.input.len() as isize;
        let mut p: isize = 0;
        loop {
            let c: Option<char> = if p < len { Some(self.input[p as usize]) } else { None };
            let special = is_special_scheme(&self.url.scheme);
            match state {
                State::SchemeStart => match c {
                    Some(ch) if ch.is_ascii_alphabetic() => {
                        self.buffer.push(ch.to_ascii_lowercase());
                        state = State::Scheme;
                    }
                    _ => {
                        state = State::NoScheme;
                        p -= 1;
                    }
                },
                State::Scheme => match c {
                    Some(ch) if ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.') => {
                        self.buffer.push(ch.to_ascii_lowercase());
                    }
                    Some(':') => {
                        self.url.scheme = std::mem::take(&mut self.buffer);
                        let special = is_special_scheme(&self.url.scheme);
                        if self.url.scheme == "file" {
                            state = State::File;
                        } else if special && self.base.is_some_and(|b| b.scheme == self.url.scheme) {
                            state = State::SpecialRelativeOrAuthority;
                        } else if special {
                            state = State::SpecialAuthoritySlashes;
                        } else if self.remaining(p).first() == Some(&'/') {
                            state = State::PathOrAuthority;
                            p += 1;
                        } else {
                            self.url.path = Path::Opaque(String::new());
                            state = State::OpaquePath;
                        }
                    }
                    _ => {
                        self.buffer.clear();
                        state = State::NoScheme;
                        p = -1;
                    }
                },
                State::NoScheme => {
                    let base = match self.base {
                        None => return Err(ParseError::MissingSchemeNonRelativeUrl),
                        Some(b) => b,
                    };
                    if base.has_opaque_path() {
                        if c != Some('#') {
                            return Err(ParseError::MissingSchemeNonRelativeUrl);
                        }
                        self.url.scheme = base.scheme.clone();
                        self.url.path = base.path.clone();
                        self.url.query = base.query.clone();
                        self.url.fragment = Some(String::new());
                        state = State::Fragment;
                    } else if base.scheme != "file" {
                        state = State::Relative;
                        p -= 1;
                    } else {
                        state = State::File;
                        p -= 1;
                    }
                }
                State::SpecialRelativeOrAuthority => {
                    if c == Some('/') && self.remaining(p).first() == Some(&'/') {
                        state = State::SpecialAuthorityIgnoreSlashes;
                        p += 1;
                    } else {
                        state = State::Relative;
                        p -= 1;
                    }
                }
                State::PathOrAuthority => {
                    if c == Some('/') {
                        state = State::Authority;
                    } else {
                        state = State::Path;
                        p -= 1;
                    }
                }
                State::Relative => {
                    let base = self.base.expect("relative state requires a base");
                    self.url.scheme = base.scheme.clone();
                    let special = is_special_scheme(&self.url.scheme);
                    if c == Some('/') || (special && c == Some('\\')) {
                        state = State::RelativeSlash;
                    } else {
                        self.url.username = base.username.clone();
                        self.url.password = base.password.clone();
                        self.url.host = base.host.clone();
                        self.url.port = base.port;
                        self.url.path = base.path.clone();
                        self.url.query = base.query.clone();
                        match c {
                            Some('?') => {
                                self.url.query = Some(String::new());
                                state = State::Query;
                            }
                            Some('#') => {
                                self.url.fragment = Some(String::new());
                                state = State::Fragment;
                            }
                            Some(_) => {
                                self.url.query = None;
                                self.url.shorten_path();
                                state = State::Path;
                                p -= 1;
                            }
                            None => {}
                        }
                    }
                }
                State::RelativeSlash => {
                    if special && (c == Some('/') || c == Some('\\')) {
                        state = State::SpecialAuthorityIgnoreSlashes;
                    } else if c == Some('/') {
                        state = State::Authority;
                    } else {
                        let base = self.base.expect("relative slash state requires a base");
                        self.url.username = base.username.clone();
                        self.url.password = base.password.clone();
                        self.url.host = base.host.clone();
                        self.url.port = base.port;
                        state = State::Path;
                        p -= 1;
                    }
                }
                State::SpecialAuthoritySlashes => {
                    if c == Some('/') && self.remaining(p).first() == Some(&'/') {
                        state = State::SpecialAuthorityIgnoreSlashes;
                        p += 1;
                    } else {
                        state = State::SpecialAuthorityIgnoreSlashes;
                        p -= 1;
                    }
                }
                State::SpecialAuthorityIgnoreSlashes => {
                    if c != Some('/') && c != Some('\\') {
                        state = State::Authority;
                        p -= 1;
                    }
                }
                State::Authority => {
                    if c == Some('@') {
                        if self.at_sign_seen {
                            self.buffer.insert_str(0, "%40");
                        }
                        self.at_sign_seen = true;
                        let buf = std::mem::take(&mut self.buffer);
                        for ch in buf.chars() {
                            if ch == ':' && !self.password_token_seen {
                                self.password_token_seen = true;
                                continue;
                            }
                            let target = if self.password_token_seen {
                                &mut self.url.password
                            } else {
                                &mut self.url.username
                            };
                            utf8_percent_encode_char(ch, EncodeSet::Userinfo, target);
                        }
                    } else if matches!(c, None | Some('/') | Some('?') | Some('#'))
                        || (special && c == Some('\\'))
                    {
                        if self.at_sign_seen && self.buffer.is_empty() {
                            return Err(ParseError::InvalidCredentials);
                        }
                        p -= self.buffer.chars().count() as isize + 1;
                        self.buffer.clear();
                        state = State::Host;
                    } else if let Some(ch) = c {
                        self.buffer.push(ch);
                    }
                }
                State::Host => {
                    if c == Some(':') && !self.inside_brackets {
                        if self.buffer.is_empty() {
                            return Err(ParseError::HostMissing);
                        }
                        let host = host::parse_host(&self.buffer, !special)?;
                        self.url.host = Some(host);
                        self.buffer.clear();
                        state = State::Port;
                    } else if matches!(c, None | Some('/') | Some('?') | Some('#'))
                        || (special && c == Some('\\'))
                    {
                        p -= 1;
                        if special && self.buffer.is_empty() {
                            return Err(ParseError::HostMissing);
                        }
                        let host = host::parse_host(&self.buffer, !special)?;
                        self.url.host = Some(host);
                        self.buffer.clear();
                        state = State::PathStart;
                    } else if let Some(ch) = c {
                        if ch == '[' {
                            self.inside_brackets = true;
                        } else if ch == ']' {
                            self.inside_brackets = false;
                        }
                        self.buffer.push(ch);
                    }
                }
                State::Port => match c {
                    Some(ch) if ch.is_ascii_digit() => self.buffer.push(ch),
                    _ if matches!(c, None | Some('/') | Some('?') | Some('#'))
                        || (special && c == Some('\\')) =>
                    {
                        if !self.buffer.is_empty() {
                            let mut port: u32 = 0;
                            for d in self.buffer.bytes() {
                                port = port * 10 + (d - b'0') as u32;
                                if port > 65535 {
                                    return Err(ParseError::InvalidPort);
                                }
                            }
                            let port = port as u16;
                            self.url.port =
                                if default_port(&self.url.scheme) == Some(port) { None } else { Some(port) };
                            self.buffer.clear();
                        }
                        state = State::PathStart;
                        p -= 1;
                    }
                    _ => return Err(ParseError::InvalidPort),
                },
                State::File => {
                    self.url.scheme = "file".to_string();
                    self.url.host = Some(Host::Empty);
                    if c == Some('/') || c == Some('\\') {
                        state = State::FileSlash;
                    } else if let Some(base) = self.base.filter(|b| b.scheme == "file") {
                        self.url.host = base.host.clone();
                        self.url.path = base.path.clone();
                        self.url.query = base.query.clone();
                        match c {
                            Some('?') => {
                                self.url.query = Some(String::new());
                                state = State::Query;
                            }
                            Some('#') => {
                                self.url.fragment = Some(String::new());
                                state = State::Fragment;
                            }
                            Some(_) => {
                                self.url.query = None;
                                let rest = &self.input[p as usize..];
                                if !starts_with_windows_drive_letter(rest) {
                                    self.url.shorten_path();
                                } else {
                                    self.url.path = Path::List(Vec::new());
                                }
                                state = State::Path;
                                p -= 1;
                            }
                            None => {}
                        }
                    } else {
                        state = State::Path;
                        p -= 1;
                    }
                }
                State::FileSlash => {
                    if c == Some('/') || c == Some('\\') {
                        state = State::FileHost;
                    } else {
                        if let Some(base) = self.base.filter(|b| b.scheme == "file") {
                            self.url.host = base.host.clone();
                            let rest = &self.input[(p.max(0) as usize).min(self.input.len())..];
                            if !starts_with_windows_drive_letter(rest) {
                                if let Path::List(bp) = &base.path {
                                    if let Some(first) = bp.first() {
                                        if is_normalized_windows_drive_letter(first) {
                                            let f = first.clone();
                                            self.url.path_list().push(f);
                                        }
                                    }
                                }
                            }
                        }
                        state = State::Path;
                        p -= 1;
                    }
                }
                State::FileHost => {
                    if matches!(c, None | Some('/') | Some('\\') | Some('?') | Some('#')) {
                        p -= 1;
                        let buf: Vec<char> = self.buffer.chars().collect();
                        if is_windows_drive_letter(&buf) {
                            state = State::Path;
                        } else if self.buffer.is_empty() {
                            self.url.host = Some(Host::Empty);
                            state = State::PathStart;
                        } else {
                            let mut host = host::parse_host(&self.buffer, false)?;
                            if let Host::Domain(d) = &host {
                                if d == "localhost" {
                                    host = Host::Empty;
                                }
                            }
                            self.url.host = Some(host);
                            self.buffer.clear();
                            state = State::PathStart;
                        }
                    } else if let Some(ch) = c {
                        self.buffer.push(ch);
                    }
                }
                State::PathStart => {
                    if special {
                        state = State::Path;
                        if c != Some('/') && c != Some('\\') {
                            p -= 1;
                        }
                    } else if c == Some('?') {
                        self.url.query = Some(String::new());
                        state = State::Query;
                    } else if c == Some('#') {
                        self.url.fragment = Some(String::new());
                        state = State::Fragment;
                    } else if c.is_some() {
                        state = State::Path;
                        if c != Some('/') {
                            p -= 1;
                        }
                    }
                }
                State::Path => {
                    let slashlike = c == Some('/') || (special && c == Some('\\'));
                    if c.is_none() || slashlike || c == Some('?') || c == Some('#') {
                        let buf = std::mem::take(&mut self.buffer);
                        if is_double_dot(&buf) {
                            self.url.shorten_path();
                            if !slashlike {
                                self.url.path_list().push(String::new());
                            }
                        } else if is_single_dot(&buf) && !slashlike {
                            self.url.path_list().push(String::new());
                        } else if !is_single_dot(&buf) {
                            let mut seg = buf;
                            let is_file = self.url.scheme == "file";
                            let list = self.url.path_list();
                            if is_file && list.is_empty() {
                                let cs: Vec<char> = seg.chars().collect();
                                if is_windows_drive_letter(&cs) {
                                    seg = format!("{}:", cs[0]);
                                }
                            }
                            list.push(seg);
                        }
                        if c == Some('?') {
                            self.url.query = Some(String::new());
                            state = State::Query;
                        } else if c == Some('#') {
                            self.url.fragment = Some(String::new());
                            state = State::Fragment;
                        }
                    } else if let Some(ch) = c {
                        utf8_percent_encode_char(ch, EncodeSet::Path, &mut self.buffer);
                    }
                }
                State::OpaquePath => match c {
                    Some('?') => {
                        self.url.query = Some(String::new());
                        state = State::Query;
                    }
                    Some('#') => {
                        self.url.fragment = Some(String::new());
                        state = State::Fragment;
                    }
                    Some(' ') => {
                        let next = self.remaining(p).first().copied();
                        let s = if matches!(next, Some('?') | Some('#')) { "%20" } else { " " };
                        if let Path::Opaque(path) = &mut self.url.path {
                            path.push_str(s);
                        }
                    }
                    Some(ch) => {
                        if let Path::Opaque(path) = &mut self.url.path {
                            utf8_percent_encode_char(ch, EncodeSet::C0Control, path);
                        }
                    }
                    None => {}
                },
                State::Query => {
                    if c == Some('#') || c.is_none() {
                        let set = if special { EncodeSet::SpecialQuery } else { EncodeSet::Query };
                        let buf = std::mem::take(&mut self.buffer);
                        let q = self.url.query.get_or_insert_with(String::new);
                        for ch in buf.chars() {
                            utf8_percent_encode_char(ch, set, q);
                        }
                        if c == Some('#') {
                            self.url.fragment = Some(String::new());
                            state = State::Fragment;
                        }
                    } else if let Some(ch) = c {
                        self.buffer.push(ch);
                    }
                }
                State::Fragment => {
                    if let Some(ch) = c {
                        let f = self.url.fragment.get_or_insert_with(String::new);
                        utf8_percent_encode_char(ch, EncodeSet::Fragment, f);
                    }
                }
            }
            if c.is_none() && p >= len {
                break;
            }
            p += 1;
            if p > len {
                break;
            }
        }
        Ok(self.url)
    }
}

/// `application/x-www-form-urlencoded` serializer (URL §5.2).
pub fn form_urlencode(pairs: &[(String, String)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        percent::form_encode_into(k, &mut out);
        out.push('=');
        percent::form_encode_into(v, &mut out);
    }
    out
}

/// `application/x-www-form-urlencoded` parser (URL §5.1).
pub fn form_urldecode(input: &str) -> Vec<(String, String)> {
    input
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|seq| {
            let (name, value) = match seq.find('=') {
                Some(i) => (&seq[..i], &seq[i + 1..]),
                None => (seq, ""),
            };
            let dec = |s: &str| {
                let s = s.replace('+', " ");
                String::from_utf8_lossy(&percent::percent_decode(s.as_bytes())).into_owned()
            };
            (dec(name), dec(value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        let u = Url::parse("HTTPS://User:Pw@Example.COM:443/a/./b/../c?q=1#frag").unwrap();
        assert_eq!(u.href(), "https://User:Pw@example.com/a/c?q=1#frag");
        assert_eq!(u.port(), None);
        assert_eq!(u.port_or_default(), Some(443));
        assert_eq!(u.request_target(), "/a/c?q=1");
    }

    #[test]
    fn relative() {
        let b = Url::parse("http://a/b/c/d;p?q").unwrap();
        for (i, o) in [
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g/"),
            ("?y", "http://a/b/c/d;p?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("../../../g", "http://a/g"),
            ("", "http://a/b/c/d;p?q"),
        ] {
            assert_eq!(b.join(i).unwrap().href(), o, "input {i}");
        }
    }

    #[test]
    fn failures() {
        assert!(Url::parse("foo").is_err());
        assert!(Url::parse("http://").is_err());
        assert!(Url::parse("http://a:99999/").is_err());
        assert!(Url::parse("http://[::1/").is_err());
    }

    #[test]
    fn file_paths() {
        let u = Url::from_file_path("/tmp/a b.html").unwrap();
        assert_eq!(u.href(), "file:///tmp/a%20b.html");
        assert_eq!(u.to_file_path().unwrap(), "/tmp/a b.html");
        let w = Url::from_file_path("C:\\x\\y.html").unwrap();
        assert_eq!(w.href(), "file:///C:/x/y.html");
        assert_eq!(w.to_file_path().unwrap(), "C:/x/y.html");
    }

    #[test]
    fn forms() {
        let pairs = vec![("a b".to_string(), "c&d=é".to_string())];
        let s = form_urlencode(&pairs);
        assert_eq!(s, "a+b=c%26d%3D%C3%A9");
        assert_eq!(form_urldecode(&s), pairs);
    }

    #[test]
    fn origins() {
        let u = Url::parse("https://example.com:8443/x").unwrap();
        assert_eq!(u.origin().serialize(), "https://example.com:8443");
        assert_eq!(Url::parse("data:x").unwrap().origin(), Origin::Opaque);
    }
}

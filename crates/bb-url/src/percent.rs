//! Percent-encoding and decoding (URL §1.3).

/// The percent-encode sets defined by the URL Standard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeSet {
    /// C0 controls and everything above U+007E.
    C0Control,
    /// C0 + space, `"`, `<`, `>`, `` ` ``.
    Fragment,
    /// C0 + space, `"`, `#`, `<`, `>`.
    Query,
    /// Query + `'`.
    SpecialQuery,
    /// Query + `?`, `^`, `` ` ``, `{`, `}`.
    Path,
    /// Path + `/`, `:`, `;`, `=`, `@`, `[`, `\`, `]`, `|`.
    Userinfo,
    /// Userinfo + `$`, `%`, `&`, `+`, `,`.
    Component,
    /// Component + `!`, `'`, `(`, `)`, `~`.
    FormUrlencoded,
}

impl EncodeSet {
    /// Whether byte `b` must be percent-encoded in this set.
    pub fn contains(self, b: u8) -> bool {
        if b < 0x20 || b > 0x7E {
            return true;
        }
        let c = b as char;
        let fragment = |c| matches!(c, ' ' | '"' | '<' | '>' | '`');
        let query = |c| matches!(c, ' ' | '"' | '#' | '<' | '>');
        let path = |c| query(c) || matches!(c, '?' | '^' | '`' | '{' | '}');
        let userinfo = |c| path(c) || matches!(c, '/' | ':' | ';' | '=' | '@' | '[' | '\\' | ']' | '|');
        let component = |c| userinfo(c) || matches!(c, '$' | '%' | '&' | '+' | ',');
        match self {
            EncodeSet::C0Control => false,
            EncodeSet::Fragment => fragment(c),
            EncodeSet::Query => query(c),
            EncodeSet::SpecialQuery => query(c) || c == '\'',
            EncodeSet::Path => path(c),
            EncodeSet::Userinfo => userinfo(c),
            EncodeSet::Component => component(c),
            EncodeSet::FormUrlencoded => component(c) || matches!(c, '!' | '\'' | '(' | ')' | '~'),
        }
    }
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Percent-encode a single byte as `%XX` into `out`.
pub fn push_percent_byte(b: u8, out: &mut String) {
    out.push('%');
    out.push(HEX[(b >> 4) as usize] as char);
    out.push(HEX[(b & 15) as usize] as char);
}

/// UTF-8 percent-encode code point `c` using `set` (URL §1.3).
pub fn utf8_percent_encode_char(c: char, set: EncodeSet, out: &mut String) {
    let mut buf = [0u8; 4];
    let s = c.encode_utf8(&mut buf);
    if s.len() == 1 && !set.contains(s.as_bytes()[0]) {
        out.push(c);
        return;
    }
    for &b in s.as_bytes() {
        if set.contains(b) {
            push_percent_byte(b, out);
        } else {
            out.push(b as char);
        }
    }
}

/// UTF-8 percent-encode a whole string.
pub fn utf8_percent_encode(s: &str, set: EncodeSet) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        utf8_percent_encode_char(c, set, &mut out);
    }
    out
}

/// Form-urlencode `s` (spaces become `+`).
pub fn form_encode_into(s: &str, out: &mut String) {
    for &b in s.as_bytes() {
        if b == b' ' {
            out.push('+');
        } else if EncodeSet::FormUrlencoded.contains(b) {
            push_percent_byte(b, out);
        } else {
            out.push(b as char);
        }
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decode a byte sequence (URL §1.3).
pub fn percent_decode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let b = input[i];
        if b == b'%' && i + 2 < input.len() {
            if let (Some(h), Some(l)) = (hex_val(input[i + 1]), hex_val(input[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode() {
        assert_eq!(percent_decode(b"a%20b%zz%4"), b"a b%zz%4");
        assert_eq!(percent_decode(b"%41%42"), b"AB");
        assert_eq!(percent_decode(b"%"), b"%");
    }

    #[test]
    fn encode() {
        assert_eq!(utf8_percent_encode("a b/é", EncodeSet::Path), "a%20b/%C3%A9");
        assert_eq!(utf8_percent_encode("a:b", EncodeSet::Userinfo), "a%3Ab");
    }
}

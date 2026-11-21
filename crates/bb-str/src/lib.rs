//! `bb-str`: strings for the web platform.
//!
//! Web strings (`DOMString`, JavaScript strings, HTML parser input) are sequences of UTF-16
//! code units and may contain **lone surrogates** (U+D800–U+DFFF not part of a pair), which a
//! Rust [`String`] can't hold. [`Wtf8Buf`] stores them as
//! [WTF-8](https://simonsapin.github.io/wtf-8/) ("Wobbly Transformation Format − 8-bit"):
//! UTF-8 generalized so that surrogate code points are encoded like any other three-byte
//! sequence. Text without lone surrogates is byte-for-byte UTF-8, so it converts to `&str` for
//! free in the common case.
//!
//! Invariant (WTF-8 §4.2 "well-formed"): a high surrogate is never directly followed by a low
//! surrogate; such pairs are always stored as the supplementary code point they encode.
//! [`Wtf8Buf::push_code_point`] and [`Wtf8Buf::push_wtf8`] join them.
//!
//! ```
//! use bb_str::Wtf8Buf;
//! let mut s = Wtf8Buf::from("a");
//! s.push_code_point(0xD800); // a lone high surrogate
//! assert_eq!(s.as_str(), None);
//! assert_eq!(s.to_string_lossy(), "a\u{FFFD}");
//! s.push_code_point(0xDC00); // completes the pair: U+10000
//! assert_eq!(s.as_str(), Some("a\u{10000}"));
//! ```

use std::borrow::Cow;
use std::fmt;

/// An owned WTF-8 string. See the [crate docs](crate).
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Wtf8Buf {
    bytes: Vec<u8>,
}

/// Is `cp` a surrogate code point (U+D800–U+DFFF)?
pub fn is_surrogate(cp: u32) -> bool {
    (0xD800..=0xDFFF).contains(&cp)
}

fn is_low_surrogate(cp: u32) -> bool {
    (0xDC00..=0xDFFF).contains(&cp)
}

/// Decode one generalized-UTF-8 code point at the start of `b`: `(code point, length)`.
/// The bytes come from a `Wtf8Buf`, so they are well-formed; anything else decodes as
/// U+FFFD for one byte rather than panicking.
fn decode_at(b: &[u8]) -> (u32, usize) {
    let cont = |i: usize| b.get(i).map(|&x| u32::from(x & 0x3F));
    match b.first() {
        None => (0xFFFD, 1),
        Some(&x) if x < 0x80 => (u32::from(x), 1),
        Some(&x) if x & 0xE0 == 0xC0 => match cont(1) {
            Some(c1) => ((u32::from(x & 0x1F) << 6) | c1, 2),
            None => (0xFFFD, 1),
        },
        Some(&x) if x & 0xF0 == 0xE0 => match (cont(1), cont(2)) {
            (Some(c1), Some(c2)) => ((u32::from(x & 0x0F) << 12) | (c1 << 6) | c2, 3),
            _ => (0xFFFD, 1),
        },
        Some(&x) if x & 0xF8 == 0xF0 => match (cont(1), cont(2), cont(3)) {
            (Some(c1), Some(c2), Some(c3)) => ((u32::from(x & 0x07) << 18) | (c1 << 12) | (c2 << 6) | c3, 4),
            _ => (0xFFFD, 1),
        },
        Some(_) => (0xFFFD, 1),
    }
}

impl Wtf8Buf {
    pub fn new() -> Self {
        Wtf8Buf { bytes: Vec::new() }
    }

    pub fn with_capacity(bytes: usize) -> Self {
        Wtf8Buf { bytes: Vec::with_capacity(bytes) }
    }

    /// Build from code points. Values above U+10FFFF become U+FFFD; surrogate pairs are joined.
    pub fn from_code_points(cps: impl IntoIterator<Item = u32>) -> Self {
        let mut s = Wtf8Buf::new();
        for cp in cps {
            s.push_code_point(cp);
        }
        s
    }

    /// Decode UTF-16, keeping lone surrogates (WTF-8 §4.3 "encoding from potentially ill-formed UTF-16").
    pub fn from_utf16(units: &[u16]) -> Self {
        Wtf8Buf::from_code_points(units.iter().map(|&u| u32::from(u)))
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn clear(&mut self) {
        self.bytes.clear();
    }

    pub fn push(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }

    pub fn push_str(&mut self, s: &str) {
        self.bytes.extend_from_slice(s.as_bytes());
    }

    /// The last code point, if it is a high surrogate.
    fn trailing_high_surrogate(&self) -> Option<u32> {
        let n = self.bytes.len();
        if n >= 3 && self.bytes[n - 3] == 0xED && (0xA0..=0xAF).contains(&self.bytes[n - 2]) {
            Some(decode_at(&self.bytes[n - 3..]).0)
        } else {
            None
        }
    }

    /// Append any code point, including a lone surrogate. A low surrogate that follows a
    /// high surrogate completes the pair (WTF-8 §4.2). Values above U+10FFFF become U+FFFD.
    pub fn push_code_point(&mut self, cp: u32) {
        if let Some(c) = char::from_u32(cp) {
            self.push(c);
            return;
        }
        if !is_surrogate(cp) {
            self.push('\u{FFFD}');
            return;
        }
        if is_low_surrogate(cp) {
            if let Some(high) = self.trailing_high_surrogate() {
                self.bytes.truncate(self.bytes.len() - 3);
                let joined = 0x10000 + ((high - 0xD800) << 10) + (cp - 0xDC00);
                self.push(char::from_u32(joined).unwrap_or('\u{FFFD}'));
                return;
            }
        }
        // Three-byte generalized UTF-8 for U+D800–U+DFFF.
        self.bytes.push(0xE0 | (cp >> 12) as u8);
        self.bytes.push(0x80 | ((cp >> 6) & 0x3F) as u8);
        self.bytes.push(0x80 | (cp & 0x3F) as u8);
    }

    /// Append another WTF-8 string, joining a surrogate pair split across the boundary.
    pub fn push_wtf8(&mut self, other: &Wtf8Buf) {
        let mut rest: &[u8] = &other.bytes;
        if self.trailing_high_surrogate().is_some() {
            let (cp, len) = decode_at(rest);
            if !rest.is_empty() && is_low_surrogate(cp) {
                self.push_code_point(cp);
                rest = &rest[len..];
            }
        }
        self.bytes.extend_from_slice(rest);
    }

    /// The code points, lone surrogates included.
    pub fn code_points(&self) -> CodePoints<'_> {
        CodePoints { bytes: &self.bytes }
    }

    /// Does the string contain a lone surrogate?
    pub fn has_surrogates(&self) -> bool {
        self.as_str().is_none()
    }

    /// The string as UTF-8, or `None` if it contains a lone surrogate.
    pub fn as_str(&self) -> Option<&str> {
        // Surrogates are the only generalized-UTF-8 sequences that UTF-8 validation rejects.
        std::str::from_utf8(&self.bytes).ok()
    }

    /// Lone surrogates replaced by U+FFFD (borrowed when there are none).
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        match self.as_str() {
            Some(s) => Cow::Borrowed(s),
            None => Cow::Owned(self.lossy_slow()),
        }
    }

    /// Lone surrogates replaced by U+FFFD.
    pub fn into_string_lossy(self) -> String {
        match String::from_utf8(self.bytes) {
            Ok(s) => s,
            Err(e) => Wtf8Buf { bytes: e.into_bytes() }.lossy_slow(),
        }
    }

    /// The string as UTF-8, or the original back if it contains a lone surrogate.
    pub fn into_string(self) -> Result<String, Wtf8Buf> {
        String::from_utf8(self.bytes).map_err(|e| Wtf8Buf { bytes: e.into_bytes() })
    }

    fn lossy_slow(&self) -> String {
        self.code_points().map(|cp| char::from_u32(cp).unwrap_or('\u{FFFD}')).collect()
    }

    /// Encode as UTF-16 (lone surrogates become single code units).
    pub fn to_utf16(&self) -> Vec<u16> {
        let mut out = Vec::with_capacity(self.bytes.len());
        for cp in self.code_points() {
            if cp >= 0x10000 {
                let v = cp - 0x10000;
                out.push(0xD800 | (v >> 10) as u16);
                out.push(0xDC00 | (v & 0x3FF) as u16);
            } else {
                out.push(cp as u16);
            }
        }
        out
    }

    /// ASCII-lowercase in place (surrogates and other non-ASCII are left alone).
    pub fn make_ascii_lowercase(&mut self) {
        self.bytes.make_ascii_lowercase();
    }

    pub fn starts_with_str(&self, s: &str) -> bool {
        self.bytes.starts_with(s.as_bytes())
    }

    pub fn ends_with_str(&self, s: &str) -> bool {
        self.bytes.ends_with(s.as_bytes())
    }
}

/// Iterator over the code points of a [`Wtf8Buf`].
pub struct CodePoints<'a> {
    bytes: &'a [u8],
}

impl Iterator for CodePoints<'_> {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        if self.bytes.is_empty() {
            return None;
        }
        let (cp, len) = decode_at(self.bytes);
        self.bytes = &self.bytes[len.min(self.bytes.len())..];
        Some(cp)
    }
}

impl From<&str> for Wtf8Buf {
    fn from(s: &str) -> Self {
        Wtf8Buf { bytes: s.as_bytes().to_vec() }
    }
}

impl From<String> for Wtf8Buf {
    fn from(s: String) -> Self {
        Wtf8Buf { bytes: s.into_bytes() }
    }
}

impl From<char> for Wtf8Buf {
    fn from(c: char) -> Self {
        let mut s = Wtf8Buf::new();
        s.push(c);
        s
    }
}

impl PartialEq<str> for Wtf8Buf {
    fn eq(&self, other: &str) -> bool {
        self.bytes == other.as_bytes()
    }
}

impl PartialEq<&str> for Wtf8Buf {
    fn eq(&self, other: &&str) -> bool {
        self.bytes == other.as_bytes()
    }
}

impl fmt::Display for Wtf8Buf {
    /// Lossy: lone surrogates are shown as U+FFFD.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_lossy())
    }
}

impl fmt::Debug for Wtf8Buf {
    /// Like `str`'s Debug, with lone surrogates written as `\u{D800}`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\"")?;
        for cp in self.code_points() {
            match char::from_u32(cp) {
                Some(c) => write!(f, "{}", c.escape_debug())?,
                None => write!(f, "\\u{{{cp:X}}}")?,
            }
        }
        f.write_str("\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lone_surrogates_round_trip() {
        let s = Wtf8Buf::from_code_points([0x61, 0xD800, 0x62, 0xDFFF]);
        assert_eq!(s.code_points().collect::<Vec<_>>(), vec![0x61, 0xD800, 0x62, 0xDFFF]);
        assert!(s.has_surrogates());
        assert_eq!(s.to_string_lossy(), "a\u{FFFD}b\u{FFFD}");
        assert_eq!(format!("{s:?}"), "\"a\\u{D800}b\\u{DFFF}\"");
        assert_eq!(s.to_utf16(), vec![0x61, 0xD800, 0x62, 0xDFFF]);
    }

    #[test]
    fn pairs_are_joined() {
        let s = Wtf8Buf::from_utf16(&[0xD83D, 0xDE00]);
        assert_eq!(s.as_str(), Some("\u{1F600}"));
        let mut a = Wtf8Buf::from_code_points([0xD83D]);
        a.push_wtf8(&Wtf8Buf::from_code_points([0xDE00, 0x21]));
        assert_eq!(a.as_str(), Some("\u{1F600}!"));
        // A low surrogate first, then a high one: not a pair.
        let s = Wtf8Buf::from_code_points([0xDE00, 0xD83D]);
        assert_eq!(s.code_points().count(), 2);
    }

    #[test]
    fn out_of_range_is_replaced() {
        assert_eq!(Wtf8Buf::from_code_points([0x11_0000]).as_str(), Some("\u{FFFD}"));
    }

    #[test]
    fn utf8_is_unchanged() {
        let s = Wtf8Buf::from("héllo 😀");
        assert_eq!(s.as_str(), Some("héllo 😀"));
        assert!(s == "héllo 😀");
        assert_eq!(s.clone().into_string(), Ok("héllo 😀".to_string()));
    }
}

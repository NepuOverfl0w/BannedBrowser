//! Host parsing and serialization (URL §3).

use crate::percent::{percent_decode, utf8_percent_encode_char, EncodeSet};
use crate::{idna, ParseError};

/// A host (URL §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Host {
    /// An ASCII domain (already lowercased / punycoded).
    Domain(String),
    Ipv4(u32),
    Ipv6([u16; 8]),
    /// An opaque host (non-special schemes), percent-encoded.
    Opaque(String),
    /// The empty host (`file:///x`).
    Empty,
}

impl Host {
    /// Host serializer (URL §3.6).
    pub fn serialize(&self) -> String {
        match self {
            Host::Domain(d) | Host::Opaque(d) => d.clone(),
            Host::Empty => String::new(),
            Host::Ipv4(a) => format!("{}.{}.{}.{}", a >> 24, (a >> 16) & 255, (a >> 8) & 255, a & 255),
            Host::Ipv6(pieces) => format!("[{}]", serialize_ipv6(pieces)),
        }
    }

    /// The name to use for DNS / socket connection (IPv6 without brackets).
    pub fn connect_name(&self) -> String {
        match self {
            Host::Ipv6(p) => serialize_ipv6(p),
            other => other.serialize(),
        }
    }

    pub fn is_loopback(&self) -> bool {
        match self {
            Host::Domain(d) => d == "localhost" || d.ends_with(".localhost"),
            Host::Ipv4(a) => a >> 24 == 127,
            Host::Ipv6(p) => *p == [0, 0, 0, 0, 0, 0, 0, 1],
            _ => false,
        }
    }
}

fn is_forbidden_host_code_point(c: char) -> bool {
    matches!(
        c,
        '\0' | '\t'
            | '\n'
            | '\r'
            | ' '
            | '#'
            | '/'
            | ':'
            | '<'
            | '>'
            | '?'
            | '@'
            | '['
            | '\\'
            | ']'
            | '^'
            | '|'
    )
}

pub(crate) fn is_forbidden_domain_code_point(c: char) -> bool {
    is_forbidden_host_code_point(c) || c <= '\u{1F}' || c == '%' || c == '\u{7F}'
}

/// URL §3.5 host parser.
pub(crate) fn parse_host(input: &str, is_opaque: bool) -> Result<Host, ParseError> {
    if let Some(rest) = input.strip_prefix('[') {
        let inner = rest.strip_suffix(']').ok_or(ParseError::InvalidHost)?;
        return parse_ipv6(inner).map(Host::Ipv6).ok_or(ParseError::InvalidHost);
    }
    if is_opaque {
        return parse_opaque_host(input);
    }
    debug_assert!(!input.is_empty());
    let decoded = percent_decode(input.as_bytes());
    let domain = String::from_utf8_lossy(&decoded);
    let ascii = idna::domain_to_ascii(&domain).ok_or(ParseError::InvalidHost)?;
    if ascii.chars().any(is_forbidden_domain_code_point) {
        return Err(ParseError::InvalidHost);
    }
    if ends_in_a_number(&ascii) {
        return parse_ipv4(&ascii).map(Host::Ipv4).ok_or(ParseError::InvalidHost);
    }
    Ok(Host::Domain(ascii))
}

fn parse_opaque_host(input: &str) -> Result<Host, ParseError> {
    if input.chars().any(|c| c != '%' && is_forbidden_host_code_point(c)) {
        return Err(ParseError::InvalidHost);
    }
    let mut out = String::new();
    for c in input.chars() {
        utf8_percent_encode_char(c, EncodeSet::C0Control, &mut out);
    }
    Ok(Host::Opaque(out))
}

/// URL §3.5 "ends in a number checker".
fn ends_in_a_number(input: &str) -> bool {
    let mut parts: Vec<&str> = input.split('.').collect();
    if parts.last() == Some(&"") {
        if parts.len() == 1 {
            return false;
        }
        parts.pop();
    }
    let last = parts.last().copied().unwrap_or("");
    if !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    parse_ipv4_number(last).is_some()
}

/// URL §3.5 IPv4 number parser. Returns `None` on failure.
fn parse_ipv4_number(mut input: &str) -> Option<u64> {
    if input.is_empty() {
        return None;
    }
    let mut radix = 10;
    if input.len() >= 2 && (input.starts_with("0x") || input.starts_with("0X")) {
        radix = 16;
        input = &input[2..];
    } else if input.len() >= 2 && input.starts_with('0') {
        radix = 8;
        input = &input[1..];
    }
    if input.is_empty() {
        return Some(0);
    }
    let mut v: u64 = 0;
    for c in input.chars() {
        let d = c.to_digit(radix)? as u64;
        // Saturate: anything this large fails the range check anyway.
        v = v.saturating_mul(radix as u64).saturating_add(d);
    }
    Some(v)
}

/// URL §3.5 IPv4 parser.
fn parse_ipv4(input: &str) -> Option<u32> {
    let mut parts: Vec<&str> = input.split('.').collect();
    if parts.last() == Some(&"") && parts.len() > 1 {
        parts.pop();
    }
    if parts.len() > 4 {
        return None;
    }
    let mut numbers = Vec::with_capacity(4);
    for p in &parts {
        numbers.push(parse_ipv4_number(p)?);
    }
    let n = numbers.len();
    for (i, &v) in numbers.iter().enumerate() {
        if v > 255 && i != n - 1 {
            return None;
        }
    }
    let last = numbers[n - 1];
    if last >= 256u64.pow((5 - n) as u32) {
        return None;
    }
    let mut ipv4 = last;
    for (i, &v) in numbers.iter().take(n - 1).enumerate() {
        ipv4 += v * 256u64.pow((3 - i) as u32);
    }
    Some(ipv4 as u32)
}

/// URL §3.5 IPv6 parser.
pub(crate) fn parse_ipv6(input: &str) -> Option<[u16; 8]> {
    let s: Vec<char> = input.chars().collect();
    let mut address = [0u16; 8];
    let mut piece_index = 0usize;
    let mut compress: Option<usize> = None;
    let mut p = 0usize;
    let at = |i: usize| s.get(i).copied();
    if at(p) == Some(':') {
        if at(p + 1) != Some(':') {
            return None;
        }
        p += 2;
        piece_index += 1;
        compress = Some(piece_index);
    }
    while at(p).is_some() {
        if piece_index == 8 {
            return None;
        }
        if at(p) == Some(':') {
            if compress.is_some() {
                return None;
            }
            p += 1;
            piece_index += 1;
            compress = Some(piece_index);
            continue;
        }
        let mut value: u32 = 0;
        let mut length = 0;
        while length < 4 {
            match at(p).and_then(|c| c.to_digit(16)) {
                Some(d) => {
                    value = value * 16 + d;
                    p += 1;
                    length += 1;
                }
                None => break,
            }
        }
        if at(p) == Some('.') {
            if length == 0 {
                return None;
            }
            p -= length;
            if piece_index > 6 {
                return None;
            }
            let mut numbers_seen = 0;
            while at(p).is_some() {
                let mut ipv4_piece: Option<u32> = None;
                if numbers_seen > 0 {
                    if at(p) == Some('.') && numbers_seen < 4 {
                        p += 1;
                    } else {
                        return None;
                    }
                }
                if !at(p).is_some_and(|c| c.is_ascii_digit()) {
                    return None;
                }
                while let Some(c) = at(p).filter(|c| c.is_ascii_digit()) {
                    let number = c.to_digit(10).unwrap_or(0);
                    match ipv4_piece {
                        None => ipv4_piece = Some(number),
                        Some(0) => return None,
                        Some(v) => ipv4_piece = Some(v * 10 + number),
                    }
                    if ipv4_piece.unwrap_or(0) > 255 {
                        return None;
                    }
                    p += 1;
                }
                let v = ipv4_piece.unwrap_or(0);
                address[piece_index] = (address[piece_index] as u32 * 0x100 + v) as u16;
                numbers_seen += 1;
                if numbers_seen == 2 || numbers_seen == 4 {
                    piece_index += 1;
                }
            }
            if numbers_seen != 4 {
                return None;
            }
            break;
        } else if at(p) == Some(':') {
            p += 1;
            at(p)?;
        } else if at(p).is_some() {
            return None;
        }
        address[piece_index] = value as u16;
        piece_index += 1;
    }
    if let Some(c) = compress {
        let mut swaps = piece_index - c;
        piece_index = 7;
        while piece_index != 0 && swaps > 0 {
            address.swap(piece_index, c + swaps - 1);
            piece_index -= 1;
            swaps -= 1;
        }
    } else if piece_index != 8 {
        return None;
    }
    Some(address)
}

/// URL §3.6 IPv6 serializer (without brackets).
fn serialize_ipv6(a: &[u16; 8]) -> String {
    // Find the first longest run of 2+ zero pieces.
    let mut best: Option<(usize, usize)> = None;
    let mut i = 0;
    while i < 8 {
        if a[i] == 0 {
            let start = i;
            while i < 8 && a[i] == 0 {
                i += 1;
            }
            let len = i - start;
            if len > 1 && best.is_none_or(|(_, l)| len > l) {
                best = Some((start, len));
            }
        } else {
            i += 1;
        }
    }
    let mut out = String::new();
    let mut ignore0 = false;
    for (idx, piece) in a.iter().enumerate() {
        if ignore0 && *piece == 0 {
            continue;
        }
        ignore0 = false;
        if let Some((start, _)) = best {
            if start == idx {
                out.push_str(if idx == 0 { "::" } else { ":" });
                ignore0 = true;
                continue;
            }
        }
        out.push_str(&format!("{piece:x}"));
        if idx != 7 {
            out.push(':');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4() {
        assert_eq!(parse_ipv4("127.0.0.1"), Some(0x7f000001));
        assert_eq!(parse_ipv4("0x7f.1"), Some(0x7f000001));
        assert_eq!(parse_ipv4("2130706433"), Some(0x7f000001));
        assert_eq!(parse_ipv4("256.0.0.1"), None);
    }

    #[test]
    fn ipv6() {
        let a = parse_ipv6("::1").unwrap();
        assert_eq!(serialize_ipv6(&a), "::1");
        let b = parse_ipv6("2001:db8:0:0:1:0:0:1").unwrap();
        assert_eq!(serialize_ipv6(&b), "2001:db8::1:0:0:1");
        let c = parse_ipv6("::ffff:192.168.0.1").unwrap();
        assert_eq!(serialize_ipv6(&c), "::ffff:c0a8:1");
        assert!(parse_ipv6("1:2:3:4:5:6:7:8:9").is_none());
    }
}

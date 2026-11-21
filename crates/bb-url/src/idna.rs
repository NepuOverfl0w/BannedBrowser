//! Domain to ASCII (URL §3.3) via a UTS #46 approximation, plus Punycode
//! (RFC 3492).
//!
//! UTS #46 needs the full IDNA mapping table. We approximate the mapping
//! step with `NFKC(case_fold(NFKC(x)))`, which is how the table is derived for
//! the vast majority of code points, plus the explicit "ignored" set and the
//! dot-like separators. TODO(UTS46 §4): bundle the real IdnaMappingTable,
//! CheckBidi and CheckJoiners.

use unicode_normalization::{is_nfc, UnicodeNormalization};

/// Code points UTS #46 maps to nothing.
fn is_ignored(c: char) -> bool {
    matches!(c,
        '\u{AD}' | '\u{34F}' | '\u{180B}'..='\u{180F}' | '\u{200B}' | '\u{2060}' | '\u{2064}'
        | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{1BCA0}'..='\u{1BCA3}'
        | '\u{E0100}'..='\u{E01EF}')
}

/// Code points we treat as "disallowed" in UTS #46 (a conservative subset).
fn is_disallowed(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x80..=0x9F            // C1 controls
        | 0xD800..=0xDFFF
        | 0xE000..=0xF8FF      // private use
        | 0xFDD0..=0xFDEF      // noncharacters
        | 0xFFF9..=0xFFFF      // specials incl. U+FFFD
        | 0xF0000..=0x10FFFF   // supplementary private use
        | 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069 // bidi controls
        | 0x2028 | 0x2029 | 0x061C | 0x115F | 0x1160 | 0x3164 | 0xFFA0
        | 0x1806 | 0x2061..=0x2063
    ) || (u & 0xFFFE) == 0xFFFE
}

fn map_char(c: char, out: &mut String) -> bool {
    if is_ignored(c) {
        return true;
    }
    match c {
        '\u{3002}' | '\u{FF0E}' | '\u{FF61}' => {
            out.push('.');
            return true;
        }
        _ => {}
    }
    if is_disallowed(c) {
        return false;
    }
    if c.is_ascii() {
        out.push(c.to_ascii_lowercase());
        return true;
    }
    // Map ẞ / ß etc. through full lowercasing; NFKC handles compatibility forms.
    let s: String = std::iter::once(c).nfkc().collect();
    for ch in s.chars() {
        if ch == 'ß' {
            out.push('ß'); // non-transitional processing keeps ß
            continue;
        }
        for l in ch.to_lowercase() {
            if is_ignored(l) {
                continue;
            }
            if is_disallowed(l) {
                return false;
            }
            out.push(l);
        }
    }
    true
}

/// Domain to ASCII with `beStrict = false` (URL §3.3). Returns `None` on failure.
pub fn domain_to_ascii(domain: &str) -> Option<String> {
    // Fast path: plain ASCII without punycode labels.
    if domain.is_ascii() && !domain.split('.').any(|l| l.len() >= 4 && l[..4].eq_ignore_ascii_case("xn--")) {
        let out = domain.to_ascii_lowercase();
        if out.is_empty() {
            return None;
        }
        return Some(out);
    }
    // UTS #46 §4 Processing: map, normalize, break, convert/validate.
    let mut mapped = String::with_capacity(domain.len());
    for c in domain.chars() {
        if !map_char(c, &mut mapped) {
            return None;
        }
    }
    let normalized: String = mapped.nfc().collect();
    let mut out_labels = Vec::new();
    for label in normalized.split('.') {
        if let Some(rest) = label.strip_prefix("xn--") {
            if !label.is_ascii() {
                return None;
            }
            let decoded = punycode_decode(rest)?;
            if (!decoded.is_empty() && decoded.is_ascii()) || !is_nfc(&decoded) {
                return None;
            }
            if !validate_label(&decoded) {
                return None;
            }
            out_labels.push(label.to_string());
        } else {
            if !validate_label(label) {
                return None;
            }
            if label.is_ascii() {
                out_labels.push(label.to_string());
            } else {
                out_labels.push(format!("xn--{}", punycode_encode(label)?));
            }
        }
    }
    let out = out_labels.join(".");
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// UTS #46 §4.1 validity criteria (subset).
fn validate_label(label: &str) -> bool {
    if label.is_empty() {
        return true;
    }
    let first = label.chars().next().unwrap_or(' ');
    // Must not begin with a combining mark (common ranges).
    let fu = first as u32;
    if matches!(fu, 0x300..=0x36F | 0x483..=0x489 | 0x591..=0x5BD | 0x610..=0x61A
        | 0x64B..=0x65F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
    {
        return false;
    }
    label.chars().all(|c| !is_disallowed(c))
}

// ---------------- Punycode (RFC 3492) ----------------

const BASE: u32 = 36;
const TMIN: u32 = 1;
const TMAX: u32 = 26;
const SKEW: u32 = 38;
const DAMP: u32 = 700;
const INITIAL_BIAS: u32 = 72;
const INITIAL_N: u32 = 128;

fn adapt(mut delta: u32, num_points: u32, first_time: bool) -> u32 {
    delta /= if first_time { DAMP } else { 2 };
    delta += delta / num_points;
    let mut k = 0;
    while delta > ((BASE - TMIN) * TMAX) / 2 {
        delta /= BASE - TMIN;
        k += BASE;
    }
    k + (((BASE - TMIN + 1) * delta) / (delta + SKEW))
}

fn digit_value(c: u8) -> Option<u32> {
    match c {
        b'a'..=b'z' => Some((c - b'a') as u32),
        b'A'..=b'Z' => Some((c - b'A') as u32),
        b'0'..=b'9' => Some((c - b'0') as u32 + 26),
        _ => None,
    }
}

fn encode_digit(d: u32) -> char {
    if d < 26 {
        (b'a' + d as u8) as char
    } else {
        (b'0' + (d - 26) as u8) as char
    }
}

/// Decode a Punycode label (without the `xn--` prefix).
pub fn punycode_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let (basic, rest) = match input.rfind('-') {
        Some(i) => (&bytes[..i], &bytes[i + 1..]),
        None => (&bytes[..0], bytes),
    };
    if !basic.is_ascii() {
        return None;
    }
    let mut output: Vec<char> = basic.iter().map(|&b| b as char).collect();
    let mut n = INITIAL_N;
    let mut i: u32 = 0;
    let mut bias = INITIAL_BIAS;
    let mut pos = 0;
    while pos < rest.len() {
        let old_i = i;
        let mut w: u32 = 1;
        let mut k = BASE;
        loop {
            let digit = digit_value(*rest.get(pos)?)?;
            pos += 1;
            i = i.checked_add(digit.checked_mul(w)?)?;
            let t = if k <= bias {
                TMIN
            } else if k >= bias + TMAX {
                TMAX
            } else {
                k - bias
            };
            if digit < t {
                break;
            }
            w = w.checked_mul(BASE - t)?;
            k += BASE;
        }
        let len = output.len() as u32 + 1;
        bias = adapt(i - old_i, len, old_i == 0);
        n = n.checked_add(i / len)?;
        i %= len;
        let c = char::from_u32(n)?;
        output.insert(i as usize, c);
        i += 1;
    }
    Some(output.into_iter().collect())
}

/// Encode a Unicode label as Punycode (without the `xn--` prefix).
pub fn punycode_encode(input: &str) -> Option<String> {
    let chars: Vec<u32> = input.chars().map(|c| c as u32).collect();
    let mut output: String = input.chars().filter(|c| c.is_ascii()).collect();
    let b = output.len() as u32;
    let mut h = b;
    if b > 0 {
        output.push('-');
    }
    let mut n = INITIAL_N;
    let mut delta: u32 = 0;
    let mut bias = INITIAL_BIAS;
    while (h as usize) < chars.len() {
        let m = *chars.iter().filter(|&&c| c >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(h + 1)?)?;
        n = m;
        for &c in &chars {
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        TMIN
                    } else if k >= bias + TMAX {
                        TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    output.push(encode_digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                output.push(encode_digit(q));
                bias = adapt(delta, h + 1, h == b);
                delta = 0;
                h += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punycode_roundtrip() {
        assert_eq!(punycode_encode("bücher").unwrap(), "bcher-kva");
        assert_eq!(punycode_decode("bcher-kva").unwrap(), "bücher");
        assert_eq!(punycode_encode("你好").unwrap(), "6qq79v");
    }

    #[test]
    fn to_ascii() {
        assert_eq!(domain_to_ascii("Bücher.example").unwrap(), "xn--bcher-kva.example");
        assert_eq!(domain_to_ascii("ＥＸＡＭＰＬＥ.com").unwrap(), "example.com");
        assert_eq!(domain_to_ascii("xn--").as_deref(), Some("xn--"));
    }
}

//! Domain to ASCII (URL §3.3): UTS #46 processing (§4) and ToASCII (§4.2) with the URL
//! Standard's options (CheckHyphens, UseSTD3ASCIIRules, VerifyDnsLength and Transitional
//! processing off; CheckBidi and CheckJoiners on), plus Punycode (RFC 3492).
//!
//! The IDNA mapping table, joining types and combining marks come from Unicode's data files
//! (`idna_data.txt`, made by `scripts/gen-idna.py`); NFC from `unicode-normalization`,
//! canonical combining classes (viramas) from it too, and bidi classes from `unicode-bidi`.

use std::sync::OnceLock;

use unicode_bidi::BidiClass;
use unicode_normalization::{char::canonical_combining_class, is_nfc, UnicodeNormalization};

/// A code point's UTS #46 status (§5 IDNA Mapping Table).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Valid,
    Ignored,
    Mapped,
    Deviation,
    Disallowed,
}

struct Data {
    /// (start, end, status, index into `mappings`, mapping length), sorted.
    table: Vec<(u32, u32, Status, u32, u8)>,
    mappings: Vec<char>,
    /// Joining types L, D, R and T by range (others are U).
    joining: Vec<(u32, u32, u8)>,
    /// General_Category=Mark ranges.
    marks: Vec<(u32, u32)>,
}

fn data() -> &'static Data {
    static DATA: OnceLock<Data> = OnceLock::new();
    DATA.get_or_init(|| {
        let mut d = Data { table: Vec::new(), mappings: Vec::new(), joining: Vec::new(), marks: Vec::new() };
        let hex = |s: &str| u32::from_str_radix(s, 16).unwrap_or(0);
        for line in include_str!("idna_data.txt").lines() {
            let mut f = line.split(' ');
            match f.next() {
                Some("T") => {
                    let (a, b) = (hex(f.next().unwrap_or("0")), hex(f.next().unwrap_or("0")));
                    let st = match f.next() {
                        Some("v") => Status::Valid,
                        Some("i") => Status::Ignored,
                        Some("m") => Status::Mapped,
                        Some("d") => Status::Deviation,
                        _ => Status::Disallowed,
                    };
                    let start = d.mappings.len() as u32;
                    d.mappings.extend(f.filter_map(|c| char::from_u32(hex(c))));
                    let len = (d.mappings.len() as u32 - start) as u8;
                    d.table.push((a, b, st, start, len));
                }
                Some("J") => {
                    let (a, b) = (hex(f.next().unwrap_or("0")), hex(f.next().unwrap_or("0")));
                    let t = f.next().and_then(|t| t.bytes().next()).unwrap_or(b'U');
                    d.joining.push((a, b, t));
                }
                Some("M") => {
                    let (a, b) = (hex(f.next().unwrap_or("0")), hex(f.next().unwrap_or("0")));
                    d.marks.push((a, b));
                }
                _ => {}
            }
        }
        d.joining.sort_unstable();
        d
    })
}

/// The status of `c`, and its mapping when it is mapped.
fn lookup(c: char) -> (Status, &'static [char]) {
    let d = data();
    let u = c as u32;
    match d.table.binary_search_by(|&(a, b, ..)| {
        if b < u {
            std::cmp::Ordering::Less
        } else if a > u {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }) {
        Ok(i) => {
            let (_, _, st, start, len) = d.table[i];
            (st, &d.mappings[start as usize..start as usize + len as usize])
        }
        Err(_) => (Status::Disallowed, &[]),
    }
}

fn in_ranges(r: &[(u32, u32)], u: u32) -> bool {
    r.binary_search_by(|&(a, b)| {
        if b < u {
            std::cmp::Ordering::Less
        } else if a > u {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    })
    .is_ok()
}

fn is_mark(c: char) -> bool {
    in_ranges(&data().marks, c as u32)
}

/// The Joining_Type (L, D, R, T, or U).
fn joining_type(c: char) -> u8 {
    let u = c as u32;
    let j = &data().joining;
    match j.binary_search_by(|&(a, b, _)| {
        if b < u {
            std::cmp::Ordering::Less
        } else if a > u {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    }) {
        Ok(i) => j[i].2,
        Err(_) => b'U',
    }
}

/// Whether a label starts with "xn--" (ASCII case-insensitively), compared as bytes.
fn xn_prefix(label: &str) -> bool {
    label.as_bytes().get(..4).is_some_and(|p| p.eq_ignore_ascii_case(b"xn--"))
}

/// Domain to ASCII with `beStrict = false` (URL §3.3). Returns `None` on failure.
pub fn domain_to_ascii(domain: &str) -> Option<String> {
    // An ASCII domain is lowercased without UTS #46 processing (its Punycode labels aren't
    // validated), as the URL Standard's tests expect (WPT url/resources/IdnaTestV2.json,
    // toascii.json).
    if domain.is_ascii() {
        let out = domain.to_ascii_lowercase();
        return (!out.is_empty()).then_some(out);
    }
    let out = to_ascii(domain)?;
    (!out.is_empty()).then_some(out)
}

/// UTS #46 §4.2 ToASCII.
fn to_ascii(domain: &str) -> Option<String> {
    let labels = process(domain)?;
    let mut out = Vec::with_capacity(labels.len());
    for (label, from_punycode) in labels {
        if from_punycode.is_some() || label.is_ascii() {
            out.push(from_punycode.unwrap_or(label));
        } else {
            out.push(format!("xn--{}", punycode_encode(&label)?));
        }
    }
    Some(out.join("."))
}

/// UTS #46 §4 Processing: map, normalize, break into labels, convert and validate. Returns
/// each label (decoded) with its original ASCII form when it came as Punycode, or None if
/// any error was recorded.
fn process(domain: &str) -> Option<Vec<(String, Option<String>)>> {
    // Step 1: map.
    let mut mapped = String::with_capacity(domain.len());
    for c in domain.chars() {
        match lookup(c) {
            (Status::Ignored, _) => {}
            (Status::Mapped, m) => mapped.extend(m.iter()),
            // Disallowed code points stay; the validity criteria reject them.
            _ => mapped.push(c),
        }
    }
    // Step 2: normalize. Step 3: break.
    let normalized: String = mapped.nfc().collect();
    let mut labels = Vec::new();
    for label in normalized.split('.') {
        // Step 4: convert/validate.
        if xn_prefix(label) {
            if !label.is_ascii() {
                return None;
            }
            let decoded = punycode_decode(&label[4..])?;
            if decoded.is_empty() || decoded.is_ascii() {
                return None;
            }
            if !valid_label(&decoded) {
                return None;
            }
            labels.push((decoded, Some(label.to_string())));
        } else {
            if !valid_label(label) {
                return None;
            }
            labels.push((label.to_string(), None));
        }
    }
    // CheckBidi applies when the domain name is a Bidi domain name (RFC 5893 §1.4): one of
    // its labels has a right-to-left character.
    let rtl = |c: char| matches!(unicode_bidi::bidi_class(c), BidiClass::R | BidiClass::AL | BidiClass::AN);
    if labels.iter().any(|(l, _)| l.chars().any(rtl)) && !labels.iter().all(|(l, _)| bidi_rule(l)) {
        return None;
    }
    Some(labels)
}

/// UTS #46 §4.1 validity criteria (nontransitional, CheckHyphens off, CheckJoiners on).
fn valid_label(label: &str) -> bool {
    if label.is_empty() {
        return true;
    }
    // 1: NFC. 2: (CheckHyphens off) no "xn--" prefix. 3: no FULL STOP.
    if !is_nfc(label) || (xn_prefix(label)) || label.contains('.') {
        return false;
    }
    // 4: no leading combining mark.
    if label.chars().next().is_some_and(is_mark) {
        return false;
    }
    // 5: every code point valid (deviations are valid in nontransitional processing).
    if !label.chars().all(|c| matches!(lookup(c).0, Status::Valid | Status::Deviation)) {
        return false;
    }
    // 6: CheckJoiners (RFC 5892 Appendix A.1, A.2).
    let cs: Vec<char> = label.chars().collect();
    for (i, &c) in cs.iter().enumerate() {
        if c != '\u{200C}' && c != '\u{200D}' {
            continue;
        }
        let virama = i > 0 && canonical_combining_class(cs[i - 1]) == 9;
        if virama {
            continue;
        }
        if c == '\u{200D}' {
            return false;
        }
        // ZWNJ: (L|D) T* ZWNJ T* (R|D).
        let before = cs[..i].iter().rev().find(|&&x| joining_type(x) != b'T');
        let after = cs[i + 1..].iter().find(|&&x| joining_type(x) != b'T');
        let ok = before.is_some_and(|&x| matches!(joining_type(x), b'L' | b'D'))
            && after.is_some_and(|&x| matches!(joining_type(x), b'R' | b'D'));
        if !ok {
            return false;
        }
    }
    true
}

/// RFC 5893 §2 the Bidi Rule.
fn bidi_rule(label: &str) -> bool {
    use BidiClass::*;
    let classes: Vec<BidiClass> = label.chars().map(unicode_bidi::bidi_class).collect();
    let Some(&first) = classes.first() else { return true };
    // The last character that isn't a non-spacing mark.
    let last = classes.iter().rev().find(|&&c| c != NSM).copied();
    match first {
        R | AL => {
            // Rules 2–4.
            classes.iter().all(|c| matches!(c, R | AL | AN | EN | ES | CS | ET | ON | BN | NSM))
                && matches!(last, Some(R | AL | EN | AN))
                && !(classes.contains(&EN) && classes.contains(&AN))
        }
        L => {
            // Rules 5–6.
            classes.iter().all(|c| matches!(c, L | EN | ES | CS | ET | ON | BN | NSM))
                && matches!(last, Some(L | EN))
        }
        _ => false,
    }
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
        // ASCII domains aren't processed; a non-ASCII one with an empty Punycode label fails.
        assert_eq!(domain_to_ascii("xn--").as_deref(), Some("xn--"));
        assert_eq!(domain_to_ascii("xn--.ü"), None);
        assert_eq!(domain_to_ascii("faß.de").unwrap(), "xn--fa-hia.de");
        assert_eq!(domain_to_ascii("a\u{200D}b"), None, "a ZWJ needs a virama");
    }
}

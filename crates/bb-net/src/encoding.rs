//! Character encoding detection and decoding for HTML documents.
//!
//! HTML §13.2.3.1 "Parsing with a known character encoding" /
//! §13.2.3.2 "Determining the character encoding": BOM sniffing, then the
//! transport-layer charset, then the `<meta>` prescan of the first 1024
//! bytes, then a fallback (windows-1252, as for most Western locales).

pub use encoding_rs::Encoding;
use encoding_rs::{UTF_16BE, UTF_16LE, UTF_8, WINDOWS_1252, X_USER_DEFINED};

/// How the encoding was determined (HTML "confidence").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Certain,
    Tentative,
}

/// Look up an encoding by WHATWG label.
pub fn encoding_for_label(label: &str) -> Option<&'static Encoding> {
    Encoding::for_label(label.trim().as_bytes())
}

/// BOM sniffing (Encoding §6 "BOM sniff").
pub fn bom_sniff(bytes: &[u8]) -> Option<&'static Encoding> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some(UTF_8)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Some(UTF_16BE)
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        Some(UTF_16LE)
    } else {
        None
    }
}

/// Determine the encoding of an HTML byte stream (HTML §13.2.3.2).
pub fn sniff_html_encoding(bytes: &[u8], transport_charset: Option<&str>) -> (&'static Encoding, Confidence) {
    if let Some(e) = bom_sniff(bytes) {
        return (e, Confidence::Certain);
    }
    if let Some(e) = transport_charset.and_then(encoding_for_label) {
        return (e, Confidence::Certain);
    }
    if let Some(e) = prescan(&bytes[..bytes.len().min(1024)]) {
        return (e, Confidence::Tentative);
    }
    // Heuristic: if the document is valid UTF-8 with non-ASCII content, use it
    // (what Firefox's detector would conclude for such input).
    if bytes.iter().any(|&b| b >= 0x80) && std::str::from_utf8(bytes).is_ok() {
        return (UTF_8, Confidence::Tentative);
    }
    (WINDOWS_1252, Confidence::Tentative)
}

/// Meta-declared UTF-16 means UTF-8; x-user-defined means windows-1252.
fn fixup_meta(e: &'static Encoding) -> &'static Encoding {
    if e == UTF_16BE || e == UTF_16LE {
        UTF_8
    } else if e == X_USER_DEFINED {
        WINDOWS_1252
    } else {
        e
    }
}

/// Decode bytes to a string with `enc`, handling a BOM if present.
pub fn decode(bytes: &[u8], enc: &'static Encoding) -> String {
    let (s, _, _) = enc.decode(bytes);
    s.into_owned()
}

/// Sniff and decode an HTML document in one step.
pub fn decode_html(bytes: &[u8], transport_charset: Option<&str>) -> (String, &'static Encoding) {
    let (enc, _) = sniff_html_encoding(bytes, transport_charset);
    (decode(bytes, enc), enc)
}

fn is_ws(b: u8) -> bool {
    matches!(b, 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

/// HTML §13.2.3.2 "prescan a byte stream to determine its encoding".
pub fn prescan(input: &[u8]) -> Option<&'static Encoding> {
    let n = input.len();
    let mut pos = 0;
    let lower_at = |i: usize| input.get(i).map(|b| b.to_ascii_lowercase());
    while pos < n {
        if input[pos..].starts_with(b"<!--") {
            pos += 2; // allow "<!-->"
            {
                let i = find(&input[pos..], b"-->")?;
                pos += i + 3
            }
            continue;
        }
        if input[pos..].len() >= 6
            && input[pos..pos + 5].eq_ignore_ascii_case(b"<meta")
            && (is_ws(input[pos + 5]) || input[pos + 5] == b'/')
        {
            pos += 6;
            let mut seen: Vec<Vec<u8>> = Vec::new();
            let mut got_pragma = false;
            let mut need_pragma: Option<bool> = None;
            let mut charset: Option<&'static Encoding> = None;
            while let Some((name, value)) = get_attribute(input, &mut pos) {
                if seen.contains(&name) {
                    continue;
                }
                seen.push(name.clone());
                match name.as_slice() {
                    b"http-equiv" => {
                        if value.eq_ignore_ascii_case(b"content-type") {
                            got_pragma = true;
                        }
                    }
                    b"content" => {
                        if charset.is_none() {
                            if let Some(label) = extract_charset_from_meta(&value) {
                                if let Some(e) = Encoding::for_label(&label) {
                                    charset = Some(e);
                                    need_pragma = Some(true);
                                }
                            }
                        }
                    }
                    b"charset" => {
                        charset = Encoding::for_label(&value);
                        need_pragma = Some(false);
                    }
                    _ => {}
                }
            }
            match need_pragma {
                None => continue,
                Some(true) if !got_pragma => continue,
                _ => {}
            }
            if let Some(e) = charset {
                return Some(fixup_meta(e));
            }
            continue;
        }
        if input[pos] == b'<'
            && (lower_at(pos + 1).is_some_and(|b| b.is_ascii_lowercase())
                || (input.get(pos + 1) == Some(&b'/')
                    && lower_at(pos + 2).is_some_and(|b| b.is_ascii_lowercase())))
        {
            // Skip the tag name, then its attributes.
            while pos < n && !is_ws(input[pos]) && input[pos] != b'>' {
                pos += 1;
            }
            while get_attribute(input, &mut pos).is_some() {}
            continue;
        }
        if input[pos..].starts_with(b"<!")
            || input[pos..].starts_with(b"</")
            || input[pos..].starts_with(b"<?")
        {
            {
                let i = input[pos..].iter().position(|&b| b == b'>')?;
                pos += i + 1
            }
            continue;
        }
        pos += 1;
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// HTML §13.2.3.2 "get an attribute". Advances `pos`; returns (name, value)
/// lowercased name, raw-lowercased value bytes.
fn get_attribute(input: &[u8], pos: &mut usize) -> Option<(Vec<u8>, Vec<u8>)> {
    let n = input.len();
    while *pos < n && (is_ws(input[*pos]) || input[*pos] == b'/') {
        *pos += 1;
    }
    if *pos >= n || input[*pos] == b'>' {
        return None;
    }
    let mut name = Vec::new();
    let mut value = Vec::new();
    // Attribute name.
    loop {
        if *pos >= n {
            return None;
        }
        let b = input[*pos];
        if b == b'=' && !name.is_empty() {
            *pos += 1;
            break;
        }
        if is_ws(b) {
            while *pos < n && is_ws(input[*pos]) {
                *pos += 1;
            }
            if *pos < n && input[*pos] == b'=' {
                *pos += 1;
                break;
            }
            return Some((name, value));
        }
        if b == b'/' || b == b'>' {
            return Some((name, value));
        }
        name.push(b.to_ascii_lowercase());
        *pos += 1;
    }
    while *pos < n && is_ws(input[*pos]) {
        *pos += 1;
    }
    if *pos >= n {
        return None;
    }
    let b = input[*pos];
    if b == b'"' || b == b'\'' {
        *pos += 1;
        while *pos < n {
            if input[*pos] == b {
                *pos += 1;
                return Some((name, value));
            }
            value.push(input[*pos].to_ascii_lowercase());
            *pos += 1;
        }
        return None;
    }
    if b == b'>' {
        return Some((name, value));
    }
    while *pos < n {
        let b = input[*pos];
        if is_ws(b) || b == b'>' {
            return Some((name, value));
        }
        value.push(b.to_ascii_lowercase());
        *pos += 1;
    }
    None
}

/// HTML §2.? "algorithm for extracting a character encoding from a meta element".
pub fn extract_charset_from_meta(content: &[u8]) -> Option<Vec<u8>> {
    let lower: Vec<u8> = content.to_ascii_lowercase();
    let mut pos = 0;
    loop {
        let i = find(&lower[pos..], b"charset")?;
        pos += i + 7;
        while pos < lower.len() && is_ws(lower[pos]) {
            pos += 1;
        }
        if pos < lower.len() && lower[pos] == b'=' {
            pos += 1;
            break;
        }
    }
    while pos < lower.len() && is_ws(lower[pos]) {
        pos += 1;
    }
    let b = *lower.get(pos)?;
    if b == b'"' || b == b'\'' {
        let end = lower[pos + 1..].iter().position(|&c| c == b)?;
        return Some(lower[pos + 1..pos + 1 + end].to_vec());
    }
    let end = lower[pos..].iter().position(|&c| is_ws(c) || c == b';').unwrap_or(lower.len() - pos);
    if end == 0 {
        return None;
    }
    Some(lower[pos..pos + end].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prescan_meta_charset() {
        let html = b"<!-- x --><html><head><meta charset=\"Shift_JIS\"></head>";
        assert_eq!(prescan(html), Some(encoding_rs::SHIFT_JIS));
    }

    #[test]
    fn prescan_http_equiv() {
        let html = b"<meta http-equiv=Content-Type content='text/html; charset=iso-8859-1'>";
        assert_eq!(prescan(html), Some(WINDOWS_1252));
        // content without http-equiv is ignored
        let html = b"<meta content='text/html; charset=koi8-r'>";
        assert_eq!(prescan(html), None);
    }

    #[test]
    fn utf16_meta_becomes_utf8() {
        assert_eq!(prescan(b"<meta charset=utf-16le>"), Some(UTF_8));
    }

    #[test]
    fn order() {
        let (e, c) = sniff_html_encoding(b"\xEF\xBB\xBF<meta charset=koi8-r>", Some("latin1"));
        assert_eq!((e, c), (UTF_8, Confidence::Certain));
        let (e, _) = sniff_html_encoding(b"<meta charset=koi8-r>", Some("latin1"));
        assert_eq!(e, WINDOWS_1252);
        let (e, _) = sniff_html_encoding(b"plain \x93quotes\x94", None);
        assert_eq!(e, WINDOWS_1252);
        let (s, _) = decode_html(b"caf\xe9", None);
        assert_eq!(s, "café");
    }

    #[test]
    fn prescan_never_panics() {
        let mut seed: u32 = 12345;
        for _ in 0..3000 {
            let len = (seed % 200) as usize;
            let mut v = Vec::with_capacity(len);
            for _ in 0..len {
                seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                let pick = b"<meta charset=\"'/>!-? x=utf-8;content";
                v.push(pick[(seed >> 16) as usize % pick.len()]);
            }
            let _ = prescan(&v);
            let _ = extract_charset_from_meta(&v);
        }
    }
}

//! `data:` URL processor (Fetch §4.? "data: URL processor") and forgiving base64.

use crate::{NetError, NetErrorKind, NetResult, Response};
use bb_url::percent::percent_decode;
use bb_url::Url;

/// Forgiving-base64 decode (Infra §4.? ). Returns `None` on failure.
pub fn forgiving_base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut data: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if data.len() % 4 == 0 {
        if data.ends_with(b"==") {
            data.truncate(data.len() - 2);
        } else if data.ends_with(b"=") {
            data.truncate(data.len() - 1);
        }
    }
    if data.len() % 4 == 1 {
        return None;
    }
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0;
    for &c in &data {
        buf = (buf << 6) | val(c)?;
        bits += 6;
        if bits == 24 {
            out.extend_from_slice(&[(buf >> 16) as u8, (buf >> 8) as u8, buf as u8]);
            buf = 0;
            bits = 0;
        }
    }
    match bits {
        12 => out.push((buf >> 4) as u8),
        18 => {
            buf >>= 2;
            out.extend_from_slice(&[(buf >> 8) as u8, buf as u8]);
        }
        _ => {}
    }
    Some(out)
}

/// Process a `data:` URL into `(mime type, body)`.
pub fn process(url: &Url) -> Option<(String, Vec<u8>)> {
    let href = url.href_without_fragment();
    let input = href.strip_prefix("data:")?;
    let comma = input.find(',')?;
    let mut mime = input[..comma].trim_matches(|c: char| c.is_ascii_whitespace()).to_string();
    let body = percent_decode(&input.as_bytes()[comma + 1..]);
    let mut is_base64 = false;
    if let Some(idx) = mime.rfind(';') {
        let tail = mime[idx + 1..].trim_matches(' ');
        if tail.eq_ignore_ascii_case("base64") {
            is_base64 = true;
            mime.truncate(idx);
        }
    }
    let body = if is_base64 { forgiving_base64_decode(&String::from_utf8_lossy(&body))? } else { body };
    if mime.starts_with(';') {
        mime = format!("text/plain{mime}");
    }
    if mime.trim().is_empty() || !mime.contains('/') {
        mime = "text/plain;charset=US-ASCII".to_string();
    }
    Some((mime, body))
}

pub(crate) fn fetch(url: &Url) -> NetResult<Response> {
    let (mime, body) =
        process(url).ok_or_else(|| NetError::new(NetErrorKind::InvalidUrl, "malformed data: URL"))?;
    Ok(Response::synthetic(url.clone(), 200, &mime, body))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64() {
        assert_eq!(forgiving_base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(forgiving_base64_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(forgiving_base64_decode(" aG Vs bG8 = ").unwrap(), b"hello");
        assert!(forgiving_base64_decode("a").is_none());
        assert!(forgiving_base64_decode("a$==").is_none());
    }
    #[test]
    fn data_urls() {
        let u = Url::parse("data:text/html;charset=utf-8,%3Cb%3Ehi%3C/b%3E").unwrap();
        let (m, b) = process(&u).unwrap();
        assert_eq!(m, "text/html;charset=utf-8");
        assert_eq!(b, b"<b>hi</b>");
        let u = Url::parse("data:;base64,SGk=").unwrap();
        assert_eq!(process(&u).unwrap(), ("text/plain;charset=US-ASCII".into(), b"Hi".to_vec()));
        assert!(process(&Url::parse("data:nocomma").unwrap()).is_none());
    }
}

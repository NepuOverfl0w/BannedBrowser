//! HPACK (RFC 7541): header compression for HTTP/2.
//!
//! The decoder implements the whole format: indexed fields, literals with and without
//! indexing and never-indexed, the dynamic table with size updates and eviction (§4), integer
//! (§5.1) and string (§5.2) primitives and Huffman decoding (Appendix B), with limits against
//! oversized or malicious blocks. The encoder never adds to the dynamic table (it uses static
//! table matches and literals without indexing, never-indexed for credentials), so it has no
//! state to keep in sync with the peer.

use super::tables::{HUFFMAN, STATIC};
use std::collections::{HashMap, VecDeque};
use std::sync::OnceLock;

/// A decoding error (a connection error of type COMPRESSION_ERROR, RFC 9113 §4.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HpackError(pub &'static str);

/// §5.1 integer representation with an `n`-bit prefix.
pub fn decode_int(buf: &[u8], pos: &mut usize, n: u8) -> Result<u64, HpackError> {
    let first = *buf.get(*pos).ok_or(HpackError("truncated integer"))?;
    *pos += 1;
    let mask = ((1u16 << n) - 1) as u8;
    let mut v = u64::from(first & mask);
    if v < u64::from(mask) {
        return Ok(v);
    }
    let mut shift = 0u32;
    loop {
        let b = *buf.get(*pos).ok_or(HpackError("truncated integer"))?;
        *pos += 1;
        if shift > 56 {
            return Err(HpackError("integer overflow"));
        }
        v = v.checked_add(u64::from(b & 0x7f) << shift).ok_or(HpackError("integer overflow"))?;
        shift += 7;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
}

/// §5.1: encode `v` with an `n`-bit prefix; `flags` are the bits above the prefix.
pub fn encode_int(out: &mut Vec<u8>, v: u64, n: u8, flags: u8) {
    let max = (1u64 << n) - 1;
    if v < max {
        out.push(flags | v as u8);
        return;
    }
    out.push(flags | max as u8);
    let mut v = v - max;
    while v >= 128 {
        out.push((v % 128) as u8 | 0x80);
        v /= 128;
    }
    out.push(v as u8);
}

/// Huffman decoding table: (bit length, code) → symbol.
fn huffman_map() -> &'static HashMap<(u8, u32), u16> {
    static MAP: OnceLock<HashMap<(u8, u32), u16>> = OnceLock::new();
    MAP.get_or_init(|| {
        HUFFMAN.iter().enumerate().map(|(sym, &(code, len))| ((len, code), sym as u16)).collect()
    })
}

/// Appendix B / §5.2 Huffman decoding. Padding must be fewer than 8 bits of the EOS prefix
/// (all ones), and EOS itself must not appear.
pub fn huffman_decode(data: &[u8]) -> Result<Vec<u8>, HpackError> {
    let map = huffman_map();
    let mut out = Vec::with_capacity(data.len() * 8 / 5);
    let (mut code, mut len) = (0u32, 0u8);
    for &byte in data {
        for i in (0..8).rev() {
            code = (code << 1) | u32::from((byte >> i) & 1);
            len += 1;
            if len >= 5 {
                if let Some(&sym) = map.get(&(len, code)) {
                    if sym == 256 {
                        return Err(HpackError("EOS in a Huffman string"));
                    }
                    out.push(sym as u8);
                    code = 0;
                    len = 0;
                }
            }
            if len > 30 {
                return Err(HpackError("invalid Huffman code"));
            }
        }
    }
    if len > 7 || code != (1u32 << len) - 1 {
        return Err(HpackError("invalid Huffman padding"));
    }
    Ok(out)
}

pub fn huffman_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u64, 0u32);
    for &b in data {
        let (code, len) = HUFFMAN[b as usize];
        acc = (acc << len) | u64::from(code);
        bits += u32::from(len);
        while bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
        acc &= (1u64 << bits) - 1;
    }
    if bits > 0 {
        // Pad with the most significant bits of EOS (ones).
        out.push(((acc << (8 - bits)) | ((1u64 << (8 - bits)) - 1)) as u8);
    }
    out
}

fn huffman_len(data: &[u8]) -> usize {
    data.iter().map(|&b| usize::from(HUFFMAN[b as usize].1)).sum::<usize>().div_ceil(8)
}

/// §5.2 string literal.
fn decode_string(buf: &[u8], pos: &mut usize, max: usize) -> Result<Vec<u8>, HpackError> {
    let huffman = buf.get(*pos).ok_or(HpackError("truncated string"))? & 0x80 != 0;
    let len = decode_int(buf, pos, 7)? as usize;
    if len > max {
        return Err(HpackError("string too long"));
    }
    let end = pos.checked_add(len).ok_or(HpackError("truncated string"))?;
    let raw = buf.get(*pos..end).ok_or(HpackError("truncated string"))?;
    *pos = end;
    if huffman {
        let s = huffman_decode(raw)?;
        if s.len() > max {
            return Err(HpackError("string too long"));
        }
        Ok(s)
    } else {
        Ok(raw.to_vec())
    }
}

fn encode_string(out: &mut Vec<u8>, s: &[u8]) {
    let h = huffman_len(s);
    if h < s.len() {
        encode_int(out, h as u64, 7, 0x80);
        out.extend_from_slice(&huffman_encode(s));
    } else {
        encode_int(out, s.len() as u64, 7, 0);
        out.extend_from_slice(s);
    }
}

/// §4.1 entry size.
fn entry_size(name: &[u8], value: &[u8]) -> usize {
    name.len() + value.len() + 32
}

/// The decoding context of one connection.
#[derive(Debug)]
pub struct Decoder {
    dynamic: VecDeque<(Vec<u8>, Vec<u8>)>,
    size: usize,
    /// The table size the decoder currently allows (set by the encoder's size updates).
    max_size: usize,
    /// Our SETTINGS_HEADER_TABLE_SIZE: the encoder may not go above it.
    limit: usize,
    /// Most bytes of decoded fields per block (SETTINGS_MAX_HEADER_LIST_SIZE).
    pub max_list: usize,
}

impl Decoder {
    pub fn new(table_size: usize, max_list: usize) -> Self {
        Decoder { dynamic: VecDeque::new(), size: 0, max_size: table_size, limit: table_size, max_list }
    }

    fn evict(&mut self) {
        while self.size > self.max_size {
            match self.dynamic.pop_back() {
                Some((n, v)) => self.size -= entry_size(&n, &v),
                None => break,
            }
        }
    }

    fn insert(&mut self, name: Vec<u8>, value: Vec<u8>) {
        let s = entry_size(&name, &value);
        if s > self.max_size {
            // §4.4: a larger entry empties the table and isn't added.
            self.dynamic.clear();
            self.size = 0;
            return;
        }
        self.size += s;
        self.dynamic.push_front((name, value));
        self.evict();
    }

    /// §2.3.3 index space: static entries first, then the dynamic table.
    fn get(&self, index: u64) -> Result<(Vec<u8>, Vec<u8>), HpackError> {
        let i = usize::try_from(index).map_err(|_| HpackError("index out of range"))?;
        if i == 0 {
            return Err(HpackError("index 0"));
        }
        if let Some(&(n, v)) = STATIC.get(i - 1) {
            return Ok((n.as_bytes().to_vec(), v.as_bytes().to_vec()));
        }
        self.dynamic.get(i - 1 - STATIC.len()).cloned().ok_or(HpackError("index out of range"))
    }

    /// Decode one complete header block into (name, value) pairs.
    pub fn decode(&mut self, block: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, HpackError> {
        let mut out = Vec::new();
        let mut list_size = 0usize;
        let mut pos = 0;
        let mut seen_field = false;
        while pos < block.len() {
            let b = block[pos];
            let field = if b & 0x80 != 0 {
                // §6.1 indexed field.
                let i = decode_int(block, &mut pos, 7)?;
                self.get(i)?
            } else if b & 0x40 != 0 {
                // §6.2.1 literal with incremental indexing.
                let (n, v) = self.literal(block, &mut pos, 6)?;
                self.insert(n.clone(), v.clone());
                (n, v)
            } else if b & 0x20 != 0 {
                // §6.3 dynamic table size update: only before the first field.
                if seen_field {
                    return Err(HpackError("table size update after a field"));
                }
                let size = decode_int(block, &mut pos, 5)?;
                let size = usize::try_from(size).map_err(|_| HpackError("table size too large"))?;
                if size > self.limit {
                    return Err(HpackError("table size above the limit"));
                }
                self.max_size = size;
                self.evict();
                continue;
            } else {
                // §6.2.2 without indexing / §6.2.3 never indexed (4-bit prefix).
                self.literal(block, &mut pos, 4)?
            };
            seen_field = true;
            list_size += entry_size(&field.0, &field.1);
            if list_size > self.max_list {
                return Err(HpackError("header list too large"));
            }
            out.push(field);
        }
        Ok(out)
    }

    fn literal(&self, block: &[u8], pos: &mut usize, n: u8) -> Result<(Vec<u8>, Vec<u8>), HpackError> {
        let i = decode_int(block, pos, n)?;
        let name = if i == 0 { decode_string(block, pos, self.max_list)? } else { self.get(i)?.0 };
        let value = decode_string(block, pos, self.max_list)?;
        Ok((name, value))
    }

    /// The dynamic table's current size (for tests).
    pub fn table_size(&self) -> usize {
        self.size
    }
}

/// Header names whose values are never put in any table (§7.1.3).
const SENSITIVE: &[&str] = &["authorization", "proxy-authorization", "cookie", "set-cookie"];

/// Encode a header list (names already lowercase).
pub fn encode(fields: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(name, value) in fields {
        if let Some(i) = STATIC.iter().position(|&(n, v)| n == name && v == value) {
            encode_int(&mut out, (i + 1) as u64, 7, 0x80);
            continue;
        }
        let flags = if SENSITIVE.contains(&name) { 0x10 } else { 0x00 };
        match STATIC.iter().position(|&(n, _)| n == name) {
            Some(i) => encode_int(&mut out, (i + 1) as u64, 4, flags),
            None => {
                encode_int(&mut out, 0, 4, flags);
                encode_string(&mut out, name.as_bytes());
            }
        }
        encode_string(&mut out, value.as_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn pairs(v: Vec<(Vec<u8>, Vec<u8>)>) -> Vec<(String, String)> {
        v.into_iter().map(|(n, v)| (String::from_utf8(n).unwrap(), String::from_utf8(v).unwrap())).collect()
    }

    fn p(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn integers_c1() {
        // C.1.1: 10 with a 5-bit prefix; C.1.2: 1337; C.1.3: 42 at an octet boundary.
        let mut out = Vec::new();
        encode_int(&mut out, 10, 5, 0);
        assert_eq!(out, [0x0a]);
        out.clear();
        encode_int(&mut out, 1337, 5, 0);
        assert_eq!(out, [0x1f, 0x9a, 0x0a]);
        let mut pos = 0;
        assert_eq!(decode_int(&out, &mut pos, 5), Ok(1337));
        out.clear();
        encode_int(&mut out, 42, 8, 0);
        assert_eq!(out, [0x2a]);
        // Overlong / overflowing encodings are rejected, never panic.
        let mut pos = 0;
        assert!(decode_int(
            &[0x1f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
            &mut pos,
            5
        )
        .is_err());
        let mut pos = 0;
        assert!(decode_int(&[0x1f, 0x80], &mut pos, 5).is_err());
    }

    #[test]
    fn requests_without_huffman_c3() {
        let mut d = Decoder::new(4096, 65536);
        let r1 = d.decode(&hex("8286 8441 0f77 7777 2e65 7861 6d70 6c65 2e63 6f6d")).unwrap();
        assert_eq!(
            pairs(r1),
            p(&[(":method", "GET"), (":scheme", "http"), (":path", "/"), (":authority", "www.example.com")])
        );
        assert_eq!(d.table_size(), 57);
        let r2 = d.decode(&hex("8286 84be 5808 6e6f 2d63 6163 6865")).unwrap();
        assert_eq!(
            pairs(r2),
            p(&[
                (":method", "GET"),
                (":scheme", "http"),
                (":path", "/"),
                (":authority", "www.example.com"),
                ("cache-control", "no-cache")
            ])
        );
        assert_eq!(d.table_size(), 110);
        let r3 = d
            .decode(&hex("8287 85bf 400a 6375 7374 6f6d 2d6b 6579 0c63 7573 746f 6d2d 7661 6c75 65"))
            .unwrap();
        assert_eq!(
            pairs(r3),
            p(&[
                (":method", "GET"),
                (":scheme", "https"),
                (":path", "/index.html"),
                (":authority", "www.example.com"),
                ("custom-key", "custom-value")
            ])
        );
        assert_eq!(d.table_size(), 164);
    }

    #[test]
    fn requests_with_huffman_c4() {
        let mut d = Decoder::new(4096, 65536);
        let r1 = d.decode(&hex("8286 8441 8cf1 e3c2 e5f2 3a6b a0ab 90f4 ff")).unwrap();
        assert_eq!(pairs(r1)[3], ("".to_string() + ":authority", "www.example.com".to_string()));
        let r2 = d.decode(&hex("8286 84be 5886 a8eb 1064 9cbf")).unwrap();
        assert_eq!(pairs(r2)[4], ("cache-control".to_string(), "no-cache".to_string()));
        let r3 = d.decode(&hex("8287 85bf 4088 25a8 49e9 5ba9 7d7f 8925 a849 e95b b8e8 b4bf")).unwrap();
        assert_eq!(pairs(r3)[4], ("custom-key".to_string(), "custom-value".to_string()));
        assert_eq!(d.table_size(), 164);
        // And the encoder's Huffman output matches the RFC's.
        assert_eq!(huffman_encode(b"www.example.com"), hex("f1e3 c2e5 f23a 6ba0 ab90 f4ff"));
        assert_eq!(huffman_encode(b"custom-value"), hex("25a8 49e9 5bb8 e8b4 bf"));
    }

    #[test]
    fn responses_with_eviction_c6() {
        // C.6: a 256-byte table, Huffman-coded responses; entries get evicted.
        let mut d = Decoder::new(256, 65536);
        let r1 = d
            .decode(&hex(
                "4882 6402 5885 aec3 771a 4b61 96d0 7abe 9410 54d4 44a8 2005 9504 0b81 66e0 82a6 2d1b ff6e 919d 29ad 1718 63c7 8f0b 97c8 e9ae 82ae 43d3",
            ))
            .unwrap();
        assert_eq!(
            pairs(r1),
            p(&[
                (":status", "302"),
                ("cache-control", "private"),
                ("date", "Mon, 21 Oct 2013 20:13:21 GMT"),
                ("location", "https://www.example.com")
            ])
        );
        assert_eq!(d.table_size(), 222);
        let r2 = d.decode(&hex("4883 640e ffc1 c0bf")).unwrap();
        assert_eq!(pairs(r2)[0], (":status".to_string(), "307".to_string()));
        assert_eq!(d.table_size(), 222);
        let r3 = d
            .decode(&hex(
                "88c1 6196 d07a be94 1054 d444 a820 0595 040b 8166 e084 a62d 1bff c05a 839b d9ab 77ad 94e7 821d d7f2 e6c7 b335 dfdf cd5b 3960 d5af 2708 7f36 72c1 ab27 0fb5 291f 9587 3160 65c0 03ed 4ee5 b106 3d50 07",
            ))
            .unwrap();
        assert_eq!(
            pairs(r3),
            p(&[
                (":status", "200"),
                ("cache-control", "private"),
                ("date", "Mon, 21 Oct 2013 20:13:22 GMT"),
                ("location", "https://www.example.com"),
                ("content-encoding", "gzip"),
                ("set-cookie", "foo=ASDJKHQKBZXOQWEOPIUAXQWEOIU; max-age=3600; version=1")
            ])
        );
        assert_eq!(d.table_size(), 215);
    }

    #[test]
    fn encoder_round_trips_through_the_decoder() {
        let fields = [
            (":method", "GET"),
            (":scheme", "https"),
            (":path", "/a?b=c"),
            (":authority", "example.com"),
            ("user-agent", "Mozilla/5.0 BannedBrowser"),
            ("cookie", "a=1; b=2"),
            ("x-custom", "ÿ\u{1}"),
        ];
        let block = encode(&fields);
        let mut d = Decoder::new(4096, 65536);
        let got = d.decode(&block).unwrap();
        let want: Vec<(Vec<u8>, Vec<u8>)> =
            fields.iter().map(|(a, b)| (a.as_bytes().to_vec(), b.as_bytes().to_vec())).collect();
        assert_eq!(got, want);
        assert_eq!(d.table_size(), 0, "the encoder never indexes");
        // The cookie is never indexed (0x1X prefix).
        assert!(block.iter().any(|&b| b & 0xf0 == 0x10));
    }

    #[test]
    fn bad_blocks_are_errors() {
        let mut d = Decoder::new(4096, 100);
        assert!(d.decode(&[0x80]).is_err(), "index 0");
        assert!(d.decode(&[0xff, 0x00]).is_err(), "index out of range");
        assert!(d.decode(&[0x3f, 0xe1, 0x1f]).is_ok(), "4096 is allowed");
        assert!(d.decode(&[0x3f, 0xe2, 0x1f]).is_err(), "4097 is above our limit");
        assert!(d.decode(&[0x82, 0x20]).is_err(), "size update after a field");
        // Huffman: EOS inside, bad padding.
        assert!(huffman_decode(&[0xff, 0xff, 0xff, 0xff]).is_err());
        assert!(huffman_decode(&[0x00]).is_err());
        // A header list over the limit.
        let big = encode(&[("x-a", &"v".repeat(200))]);
        assert!(d.decode(&big).is_err());
        // Arbitrary bytes never panic.
        for seed in 0..2000u32 {
            let bytes: Vec<u8> = (0..(seed % 40))
                .map(|i| (seed.wrapping_mul(2654435761).wrapping_add(i * 97) >> 7) as u8)
                .collect();
            let _ = Decoder::new(4096, 65536).decode(&bytes);
            let _ = huffman_decode(&bytes);
        }
    }

    #[test]
    fn huffman_round_trip_all_bytes() {
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(huffman_decode(&huffman_encode(&all)).unwrap(), all);
    }
}

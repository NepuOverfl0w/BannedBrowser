//! Uint8Array to and from base64 and hex (ES2026 §23.3.1–2: Uint8Array.fromBase64,
//! Uint8Array.fromHex, Uint8Array.prototype.{toBase64, toHex, setFromBase64, setFromHex}).

use super::super::heap::*;
use super::super::realm::method;
use super::super::typed::TaKind;
use super::super::value::{JsString, PropKey, Value};
use super::super::{JsResult, Vm};

/// The result of FromBase64 / FromHex: code units read, bytes, and a SyntaxError message.
struct Decoded {
    read: usize,
    bytes: Vec<u8>,
    error: Option<&'static str>,
}

#[derive(Clone, Copy, PartialEq)]
enum LastChunk {
    Loose,
    Strict,
    StopBeforePartial,
}

fn is_ascii_ws(c: u16) -> bool {
    matches!(c, 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

fn skip_ws(s: &[u16], mut i: usize) -> usize {
    while i < s.len() && is_ascii_ws(s[i]) {
        i += 1;
    }
    i
}

fn b64_value(c: u16) -> Option<u8> {
    Some(match c {
        0x41..=0x5A => (c - 0x41) as u8,
        0x61..=0x7A => (c - 0x61 + 26) as u8,
        0x30..=0x39 => (c - 0x30 + 52) as u8,
        0x2B => 62,
        0x2F => 63,
        _ => return None,
    })
}

/// DecodeBase64Chunk: `Err` when `throw_on_extra_bits` and the padding bits aren't zero.
fn decode_chunk(chunk: &[u8], throw_on_extra_bits: bool) -> Result<Vec<u8>, ()> {
    let mut v = [0u8; 4];
    v[..chunk.len()].copy_from_slice(chunk);
    let n = (v[0] as u32) << 18 | (v[1] as u32) << 12 | (v[2] as u32) << 6 | v[3] as u32;
    let b = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
    match chunk.len() {
        2 => {
            if throw_on_extra_bits && b[1] != 0 {
                return Err(());
            }
            Ok(vec![b[0]])
        }
        3 => {
            if throw_on_extra_bits && b[2] != 0 {
                return Err(());
            }
            Ok(vec![b[0], b[1]])
        }
        _ => Ok(b.to_vec()),
    }
}

/// FromBase64.
fn from_base64(s: &[u16], url: bool, last: LastChunk, max: usize) -> Decoded {
    let mut d = Decoded { read: 0, bytes: Vec::new(), error: None };
    if max == 0 {
        return d;
    }
    let fail = |mut d: Decoded, m: &'static str| {
        d.error = Some(m);
        d
    };
    let mut chunk: Vec<u8> = Vec::with_capacity(4);
    let mut index = 0;
    let length = s.len();
    loop {
        index = skip_ws(s, index);
        if index == length {
            if !chunk.is_empty() {
                match last {
                    LastChunk::StopBeforePartial => return d,
                    LastChunk::Loose => {
                        if chunk.len() == 1 {
                            return fail(d, "incomplete base64 chunk");
                        }
                        if let Ok(b) = decode_chunk(&chunk, false) {
                            d.bytes.extend(b);
                        }
                    }
                    LastChunk::Strict => return fail(d, "missing base64 padding"),
                }
            }
            d.read = length;
            return d;
        }
        let mut c = s[index];
        index += 1;
        if c == u16::from(b'=') {
            if chunk.len() < 2 {
                return fail(d, "unexpected base64 padding");
            }
            index = skip_ws(s, index);
            if chunk.len() == 2 {
                if index == length {
                    if last == LastChunk::StopBeforePartial {
                        return d;
                    }
                    return fail(d, "incomplete base64 padding");
                }
                if s[index] == u16::from(b'=') {
                    index = skip_ws(s, index + 1);
                }
            }
            if index < length {
                return fail(d, "characters after base64 padding");
            }
            match decode_chunk(&chunk, last == LastChunk::Strict) {
                Ok(b) => d.bytes.extend(b),
                Err(()) => return fail(d, "nonzero padding bits in base64"),
            }
            d.read = length;
            return d;
        }
        if url {
            if c == u16::from(b'+') || c == u16::from(b'/') {
                return fail(d, "invalid base64url character");
            } else if c == u16::from(b'-') {
                c = u16::from(b'+');
            } else if c == u16::from(b'_') {
                c = u16::from(b'/');
            }
        }
        let Some(v) = b64_value(c) else { return fail(d, "invalid base64 character") };
        let remaining = max - d.bytes.len();
        if (remaining == 1 && chunk.len() == 2) || (remaining == 2 && chunk.len() == 3) {
            return d;
        }
        chunk.push(v);
        if chunk.len() == 4 {
            if let Ok(b) = decode_chunk(&chunk, false) {
                d.bytes.extend(b);
            }
            chunk.clear();
            d.read = index;
            if d.bytes.len() == max {
                return d;
            }
        }
    }
}

/// FromHex.
fn from_hex(s: &[u16], max: usize) -> Decoded {
    let mut d = Decoded { read: 0, bytes: Vec::new(), error: None };
    if s.len() % 2 != 0 {
        d.error = Some("hex string has an odd length");
        return d;
    }
    let hex = |c: u16| char::from_u32(c as u32).and_then(|c| c.to_digit(16));
    while d.read < s.len() && d.bytes.len() < max {
        let (Some(a), Some(b)) = (hex(s[d.read]), hex(s[d.read + 1])) else {
            d.error = Some("invalid hex character");
            return d;
        };
        d.read += 2;
        d.bytes.push((a * 16 + b) as u8);
    }
    d
}

fn string_arg(vm: &mut Vm, v: &Value) -> JsResult<JsString> {
    match v {
        Value::String(s) => Ok(s.clone()),
        _ => vm.type_error("the argument must be a string"),
    }
}

/// GetOptionsObject: `None` stands for an empty object.
fn options(vm: &mut Vm, v: &Value) -> JsResult<Option<ObjRef>> {
    match v {
        Value::Undefined => Ok(None),
        Value::Object(o) => Ok(Some(*o)),
        _ => vm.type_error("options must be an object"),
    }
}

fn opt_get(vm: &mut Vm, o: Option<ObjRef>, k: &'static str) -> JsResult<Value> {
    match o {
        Some(o) => vm.get(o, &PropKey::lit(k)),
        None => Ok(Value::Undefined),
    }
}

fn str_is(v: &Value, s: &str) -> bool {
    matches!(v, Value::String(x) if x.iter().copied().eq(s.encode_utf16()))
}

/// The `alphabet` option: true for base64url.
fn alphabet(vm: &mut Vm, o: Option<ObjRef>) -> JsResult<bool> {
    let a = opt_get(vm, o, "alphabet")?;
    if a.is_undefined() || str_is(&a, "base64") {
        Ok(false)
    } else if str_is(&a, "base64url") {
        Ok(true)
    } else {
        vm.type_error("alphabet must be \"base64\" or \"base64url\"")
    }
}

fn last_chunk(vm: &mut Vm, o: Option<ObjRef>) -> JsResult<LastChunk> {
    let l = opt_get(vm, o, "lastChunkHandling")?;
    if l.is_undefined() || str_is(&l, "loose") {
        Ok(LastChunk::Loose)
    } else if str_is(&l, "strict") {
        Ok(LastChunk::Strict)
    } else if str_is(&l, "stop-before-partial") {
        Ok(LastChunk::StopBeforePartial)
    } else {
        vm.type_error("lastChunkHandling must be \"loose\", \"strict\" or \"stop-before-partial\"")
    }
}

/// ValidateUint8Array.
fn validate(vm: &mut Vm, v: &Value) -> JsResult<ObjRef> {
    if let Value::Object(o) = v {
        if vm.ta(*o).is_some_and(|t| t.0 == TaKind::Uint8) {
            return Ok(*o);
        }
    }
    vm.type_error("the receiver is not a Uint8Array")
}

/// GetUint8ArrayBytes: TypeError when detached or out of bounds.
fn ta_bytes(vm: &mut Vm, o: ObjRef) -> JsResult<Vec<u8>> {
    let Some(len) = vm.ta_length(o) else {
        return vm.type_error("the Uint8Array is detached or out of bounds");
    };
    let (_, buf, off, _) = vm.ta(o).expect("validated");
    Ok(vm.buffer_read(buf, off, len).unwrap_or_default())
}

fn new_uint8(vm: &mut Vm, bytes: &[u8]) -> JsResult<Value> {
    let p = vm.realm.ta_protos[TaKind::Uint8.index()];
    let a = super::typed::alloc_ta(vm, TaKind::Uint8, p, bytes.len() as f64)?;
    let (_, buf, off, _) = vm.ta(a).expect("typed array");
    vm.buffer_write(buf, off, bytes);
    Ok(Value::Object(a))
}

/// The tail of setFromBase64 / setFromHex: write the bytes, then throw or report.
fn set_result(vm: &mut Vm, into: ObjRef, d: Decoded) -> JsResult<Value> {
    let (_, buf, off, _) = vm.ta(into).expect("validated");
    vm.buffer_write(buf, off, &d.bytes);
    if let Some(m) = d.error {
        return vm.syntax_error(m);
    }
    let r = vm.new_object();
    let props = &mut vm.heap.obj_mut(r).props;
    props.insert(PropKey::lit("read"), Prop::data(Value::Number(d.read as f64), true, true, true));
    props.insert(PropKey::lit("written"), Prop::data(Value::Number(d.bytes.len() as f64), true, true, true));
    Ok(Value::Object(r))
}

/// The writable length of `into` (out of bounds or detached: TypeError).
fn target_len(vm: &mut Vm, into: ObjRef) -> JsResult<usize> {
    match vm.ta_length(into) {
        Some(l) => Ok(l),
        None => vm.type_error("the Uint8Array is detached or out of bounds"),
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encode_base64(bytes: &[u8], url: bool, omit_padding: bool) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let ch = |v: u32| {
        let c = ALPHABET[(v & 63) as usize];
        match (url, c) {
            (true, b'+') => '-',
            (true, b'/') => '_',
            _ => c as char,
        }
    };
    for g in bytes.chunks(3) {
        let n = (g[0] as u32) << 16 | (*g.get(1).unwrap_or(&0) as u32) << 8 | *g.get(2).unwrap_or(&0) as u32;
        out.push(ch(n >> 18));
        out.push(ch(n >> 12));
        if g.len() > 1 {
            out.push(ch(n >> 6));
        } else if !omit_padding {
            out.push('=');
        }
        if g.len() > 2 {
            out.push(ch(n));
        } else if !omit_padding {
            out.push('=');
        }
    }
    out
}

pub(super) fn install(vm: &mut Vm) {
    let ctor = vm.realm.ta_ctors[TaKind::Uint8.index()];
    let proto = vm.realm.ta_protos[TaKind::Uint8.index()];
    method(vm, ctor, "fromBase64", 1, |vm, c| {
        let s = string_arg(vm, &c.arg(0))?;
        let o = options(vm, &c.arg(1))?;
        let url = alphabet(vm, o)?;
        let last = last_chunk(vm, o)?;
        let d = from_base64(&s, url, last, usize::MAX);
        if let Some(m) = d.error {
            return vm.syntax_error(m);
        }
        new_uint8(vm, &d.bytes)
    });
    method(vm, ctor, "fromHex", 1, |vm, c| {
        let s = string_arg(vm, &c.arg(0))?;
        let d = from_hex(&s, usize::MAX);
        if let Some(m) = d.error {
            return vm.syntax_error(m);
        }
        new_uint8(vm, &d.bytes)
    });
    method(vm, proto, "toBase64", 0, |vm, c| {
        let o = validate(vm, &c.this)?;
        let opts = options(vm, &c.arg(0))?;
        let url = alphabet(vm, opts)?;
        let omit = opt_get(vm, opts, "omitPadding")?.truthy();
        let bytes = ta_bytes(vm, o)?;
        Ok(Value::str(&encode_base64(&bytes, url, omit)))
    });
    method(vm, proto, "toHex", 0, |vm, c| {
        let o = validate(vm, &c.this)?;
        let bytes = ta_bytes(vm, o)?;
        let s: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Ok(Value::str(&s))
    });
    method(vm, proto, "setFromBase64", 1, |vm, c| {
        let into = validate(vm, &c.this)?;
        if vm.views_immutable(into) {
            return vm.type_error("the Uint8Array's buffer is immutable");
        }
        let s = string_arg(vm, &c.arg(0))?;
        let o = options(vm, &c.arg(1))?;
        let url = alphabet(vm, o)?;
        let last = last_chunk(vm, o)?;
        let max = target_len(vm, into)?;
        let d = from_base64(&s, url, last, max);
        set_result(vm, into, d)
    });
    method(vm, proto, "setFromHex", 1, |vm, c| {
        let into = validate(vm, &c.this)?;
        if vm.views_immutable(into) {
            return vm.type_error("the Uint8Array's buffer is immutable");
        }
        let s = string_arg(vm, &c.arg(0))?;
        let max = target_len(vm, into)?;
        let d = from_hex(&s, max);
        set_result(vm, into, d)
    });
}

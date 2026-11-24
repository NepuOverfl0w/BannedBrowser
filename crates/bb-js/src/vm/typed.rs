//! ArrayBuffer and SharedArrayBuffer (§25.1–2), DataView (§25.3), typed arrays (§23.2) and
//! the TypedArray exotic object (§10.4.5).

use super::heap::*;
use super::object::PropDesc;
use super::value::{number_to_string, string_to_number, PropKey, Value};
use super::{JsResult, Vm};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// The largest buffer we allocate (bytes); bigger requests are a RangeError.
pub const MAX_BUFFER: usize = 1 << 31;

pub struct Buffer {
    /// An ArrayBuffer's bytes; `None` once detached (and for a SharedArrayBuffer).
    pub bytes: Option<Vec<u8>>,
    /// A SharedArrayBuffer's Shared Data Block, which other agents can hold too.
    pub block: Option<Arc<SharedBlock>>,
    /// Resizable (ArrayBuffer) or growable (SharedArrayBuffer) up to this.
    pub max: Option<usize>,
    pub shared: bool,
    /// An immutable ArrayBuffer (immutable-arraybuffer proposal): its bytes never change.
    pub immutable: bool,
}

/// A Shared Data Block (§6.2.9): bytes several agents read and write at once. Plain
/// accesses are byte-wise relaxed atomics (racing ones may tear, as the memory model
/// allows); Atomics operations hold `waiters`' lock, which also guards the WaiterList.
pub struct SharedBlock {
    data: Box<[AtomicU8]>,
    len: AtomicUsize,
    pub waiters: Mutex<Vec<Waiter>>,
}

/// A waiter in a WaiterList (§25.4.3.x): the byte index it waits on and its signal.
pub struct Waiter {
    pub index: usize,
    pub signal: Arc<Signal>,
}

/// Set once by Atomics.notify; a blocking waiter sleeps on `cv`.
#[derive(Default)]
pub struct Signal {
    pub notified: Mutex<bool>,
    pub cv: Condvar,
}

impl SharedBlock {
    /// A block of `len` zero bytes that can grow to `cap`.
    pub fn new(len: usize, cap: usize) -> Arc<SharedBlock> {
        let data: Box<[AtomicU8]> = (0..cap.max(len)).map(|_| AtomicU8::new(0)).collect();
        Arc::new(SharedBlock { data, len: AtomicUsize::new(len), waiters: Mutex::new(Vec::new()) })
    }

    pub fn len(&self) -> usize {
        self.len.load(Ordering::SeqCst)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn read(&self, at: usize, out: &mut [u8]) -> bool {
        let Some(src) = self.data.get(at..at + out.len()) else { return false };
        for (o, b) in out.iter_mut().zip(src) {
            *o = b.load(Ordering::Relaxed);
        }
        true
    }

    pub fn write(&self, at: usize, data: &[u8]) -> bool {
        let Some(dst) = self.data.get(at..at + data.len()) else { return false };
        for (b, &v) in dst.iter().zip(data) {
            b.store(v, Ordering::Relaxed);
        }
        true
    }

    /// Grow to `n` bytes (within the capacity allocated up front); false if it can't.
    pub fn grow(&self, n: usize) -> bool {
        if n > self.data.len() {
            return false;
        }
        self.len.fetch_max(n, Ordering::SeqCst);
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaKind {
    Int8,
    Uint8,
    Uint8Clamped,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float16,
    Float32,
    Float64,
    BigInt64,
    BigUint64,
}

pub const TA_KINDS: [(TaKind, &str); 12] = [
    (TaKind::Int8, "Int8Array"),
    (TaKind::Uint8, "Uint8Array"),
    (TaKind::Uint8Clamped, "Uint8ClampedArray"),
    (TaKind::Int16, "Int16Array"),
    (TaKind::Uint16, "Uint16Array"),
    (TaKind::Int32, "Int32Array"),
    (TaKind::Uint32, "Uint32Array"),
    (TaKind::Float16, "Float16Array"),
    (TaKind::Float32, "Float32Array"),
    (TaKind::Float64, "Float64Array"),
    (TaKind::BigInt64, "BigInt64Array"),
    (TaKind::BigUint64, "BigUint64Array"),
];

impl TaKind {
    pub fn size(self) -> usize {
        match self {
            TaKind::Int8 | TaKind::Uint8 | TaKind::Uint8Clamped => 1,
            TaKind::Int16 | TaKind::Uint16 | TaKind::Float16 => 2,
            TaKind::Int32 | TaKind::Uint32 | TaKind::Float32 => 4,
            TaKind::Float64 | TaKind::BigInt64 | TaKind::BigUint64 => 8,
        }
    }
    /// [[ContentType]] BigInt (§23.2.6).
    pub fn is_bigint(self) -> bool {
        matches!(self, TaKind::BigInt64 | TaKind::BigUint64)
    }
    pub fn index(self) -> usize {
        TA_KINDS.iter().position(|(k, _)| *k == self).unwrap_or(0)
    }
    pub fn name(self) -> &'static str {
        TA_KINDS[self.index()].1
    }
}

pub struct TypedArray {
    pub kind: TaKind,
    pub buffer: ObjRef,
    pub offset: usize,
    /// `None`: tracks the length of a resizable buffer.
    pub length: Option<usize>,
}

pub struct DataView {
    pub buffer: ObjRef,
    pub offset: usize,
    pub length: Option<usize>,
}

/// IEEE half precision from a double, rounding to nearest, ties to even (§25.1.3.17).
pub fn f64_to_f16_bits(x: f64) -> u16 {
    let sign = if x.is_sign_negative() { 0x8000u16 } else { 0 };
    if x.is_nan() {
        return 0x7E00;
    }
    let a = x.abs();
    if a.is_infinite() {
        return sign | 0x7C00;
    }
    let round_even = |m: f64| -> u32 {
        let f = m.floor();
        let frac = m - f;
        if frac > 0.5 || (frac == 0.5 && (f as u64) % 2 == 1) {
            f as u32 + 1
        } else {
            f as u32
        }
    };
    if a < 2f64.powi(-14) {
        // Subnormal: units of 2^-24 (a carry into 1024 is exactly the smallest normal).
        return sign | round_even(a * 2f64.powi(24)) as u16;
    }
    let mut e = ((a.to_bits() >> 52) & 0x7FF) as i32 - 1023;
    let mut r = round_even(a / 2f64.powi(e - 10));
    if r == 2048 {
        r = 1024;
        e += 1;
    }
    if e + 15 >= 31 {
        return sign | 0x7C00;
    }
    sign | (((e + 15) as u16) << 10) | ((r - 1024) as u16)
}

pub fn f16_bits_to_f64(h: u16) -> f64 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1F) as i32;
    let man = (h & 0x3FF) as f64;
    match exp {
        0 => sign * man * 2f64.powi(-24),
        31 => {
            if man == 0.0 {
                sign * f64::INFINITY
            } else {
                f64::NAN
            }
        }
        _ => sign * (1.0 + man / 1024.0) * 2f64.powi(exp - 15),
    }
}

/// RawBytesToNumeric (§25.1.3.15), little-endian unless `big`.
pub fn read_elem(bytes: &[u8], kind: TaKind, at: usize, big: bool) -> f64 {
    let n = kind.size();
    let mut b = [0u8; 8];
    b[..n].copy_from_slice(&bytes[at..at + n]);
    if big {
        b[..n].reverse();
    }
    match kind {
        TaKind::Int8 => b[0] as i8 as f64,
        TaKind::Uint8 | TaKind::Uint8Clamped => b[0] as f64,
        TaKind::Int16 => i16::from_le_bytes([b[0], b[1]]) as f64,
        TaKind::Uint16 => u16::from_le_bytes([b[0], b[1]]) as f64,
        TaKind::Float16 => f16_bits_to_f64(u16::from_le_bytes([b[0], b[1]])),
        TaKind::Int32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        TaKind::Uint32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        TaKind::Float32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        TaKind::Float64 | TaKind::BigInt64 | TaKind::BigUint64 => f64::from_le_bytes(b),
    }
}

/// A BigInt element (BigInt64 / BigUint64), little-endian unless `big`.
pub fn read_big(bytes: &[u8], kind: TaKind, at: usize, big: bool) -> super::bigint::BigInt {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[at..at + 8]);
    if big {
        b.reverse();
    }
    let u = u64::from_le_bytes(b);
    if kind == TaKind::BigInt64 {
        super::bigint::BigInt::from_i64(u as i64)
    } else {
        super::bigint::BigInt::from_u64(u)
    }
}

pub fn write_big(bytes: &mut [u8], at: usize, v: &super::bigint::BigInt, big: bool) {
    let mut b = v.to_u64_wrapping().to_le_bytes();
    if big {
        b.reverse();
    }
    bytes[at..at + 8].copy_from_slice(&b);
}

/// NumericToRawBytes (§25.1.3.17) after ToNumber.
pub fn write_elem(bytes: &mut [u8], kind: TaKind, at: usize, v: f64, big: bool) {
    let i = super::ops::f64_to_int32(v);
    let mut out: Vec<u8> = match kind {
        TaKind::Int8 | TaKind::Uint8 => vec![i as u8],
        TaKind::Uint8Clamped => {
            let c = if v.is_nan() {
                0.0
            } else {
                let c = v.clamp(0.0, 255.0);
                let f = c.floor();
                if c - f > 0.5 || (c - f == 0.5 && f as u32 % 2 == 1) {
                    f + 1.0
                } else {
                    f
                }
            };
            vec![c as u8]
        }
        TaKind::Int16 | TaKind::Uint16 => (i as u16).to_le_bytes().to_vec(),
        TaKind::Float16 => f64_to_f16_bits(v).to_le_bytes().to_vec(),
        TaKind::Int32 | TaKind::Uint32 => (i as u32).to_le_bytes().to_vec(),
        TaKind::Float32 => (v as f32).to_le_bytes().to_vec(),
        TaKind::Float64 | TaKind::BigInt64 | TaKind::BigUint64 => v.to_le_bytes().to_vec(),
    };
    if big {
        out.reverse();
    }
    bytes[at..at + out.len()].copy_from_slice(&out);
}

/// CanonicalNumericIndexString (§7.1.21) for a property key.
pub fn canonical_numeric(k: &PropKey) -> Option<f64> {
    match k {
        PropKey::Index(i) => Some(*i as f64),
        PropKey::Str(s) => {
            if s.iter().copied().eq("-0".encode_utf16()) {
                return Some(-0.0);
            }
            let n = string_to_number(s);
            if number_to_string(n).encode_utf16().eq(s.iter().copied()) {
                Some(n)
            } else {
                None
            }
        }
        PropKey::Sym(_) => None,
    }
}

impl Vm {
    /// The byte length of an ArrayBuffer or SharedArrayBuffer (`None` when detached).
    pub fn buffer_len(&self, b: ObjRef) -> Option<usize> {
        match &self.heap.obj(b).kind {
            Kind::ArrayBuffer(d) => match (&d.bytes, &d.block) {
                (Some(v), _) => Some(v.len()),
                (None, Some(s)) => Some(s.len()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Copy `n` bytes from `at` (`None` when detached or out of range).
    pub fn buffer_read(&self, b: ObjRef, at: usize, n: usize) -> Option<Vec<u8>> {
        match &self.heap.obj(b).kind {
            Kind::ArrayBuffer(d) => match (&d.bytes, &d.block) {
                (Some(v), _) => v.get(at..at + n).map(<[u8]>::to_vec),
                (None, Some(s)) if at + n <= s.len() => {
                    let mut out = vec![0; n];
                    s.read(at, &mut out).then_some(out)
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Write bytes at `at`; false when detached or out of range.
    pub fn buffer_write(&mut self, b: ObjRef, at: usize, data: &[u8]) -> bool {
        match &mut self.heap.obj_mut(b).kind {
            Kind::ArrayBuffer(d) => match (&mut d.bytes, &d.block) {
                (Some(v), _) => match v.get_mut(at..at + data.len()) {
                    Some(dst) => {
                        dst.copy_from_slice(data);
                        true
                    }
                    None => false,
                },
                (None, Some(s)) => at + data.len() <= s.len() && s.write(at, data),
                _ => false,
            },
            _ => false,
        }
    }

    /// The Shared Data Block of a SharedArrayBuffer.
    pub fn shared_block(&self, v: &Value) -> Option<Arc<SharedBlock>> {
        match v {
            Value::Object(o) => match &self.heap.obj(*o).kind {
                Kind::ArrayBuffer(d) => d.block.clone(),
                _ => None,
            },
            _ => None,
        }
    }

    /// A SharedArrayBuffer of this realm over an existing block (another agent's).
    pub fn new_shared_array_buffer(&mut self, block: Arc<SharedBlock>, max: Option<usize>) -> Value {
        let proto = self.realm.shared_array_buffer_proto;
        let buf = Buffer { bytes: None, block: Some(block), max, shared: true, immutable: false };
        Value::Object(self.heap.alloc(Object::new(Some(proto), Kind::ArrayBuffer(Box::new(buf)))))
    }

    /// A raw element's bytes, read in one go (for decoding).
    fn elem_bytes(&self, b: ObjRef, at: usize, n: usize) -> Option<[u8; 8]> {
        let mut out = [0u8; 8];
        let v = self.buffer_read(b, at, n)?;
        out[..n].copy_from_slice(&v);
        Some(out)
    }

    /// GetValueFromBuffer (§25.1.3.16): a Number or BigInt element.
    pub fn buffer_get(&self, b: ObjRef, kind: TaKind, at: usize, big_endian: bool) -> Option<Value> {
        let raw = self.elem_bytes(b, at, kind.size())?;
        Some(if kind.is_bigint() {
            Value::BigInt(std::rc::Rc::new(read_big(&raw, kind, 0, big_endian)))
        } else {
            Value::Number(read_elem(&raw, kind, 0, big_endian))
        })
    }

    /// SetValueInBuffer (§25.1.3.18) with an already converted value.
    pub fn buffer_set(&mut self, b: ObjRef, kind: TaKind, at: usize, v: &Value, big_endian: bool) {
        let mut raw = [0u8; 8];
        match v {
            Value::BigInt(x) => write_big(&mut raw, 0, x, big_endian),
            Value::Number(n) => write_elem(&mut raw, kind, 0, *n, big_endian),
            _ => return,
        }
        self.buffer_write(b, at, &raw[..kind.size()]);
    }

    /// A typed array's element kind and the bytes it views: (kind, buffer, byte offset,
    /// byte length); None for anything else or an out-of-bounds view.
    pub fn ta_view(&self, o: ObjRef) -> Option<(TaKind, ObjRef, usize, usize)> {
        let Kind::TypedArray(t) = &self.heap.obj(o).kind else { return None };
        let n = self.ta_length(o)? * t.kind.size();
        Some((t.kind, t.buffer, t.offset, n))
    }

    /// A typed array's buffer and byte offset.
    pub fn ta_buffer(&self, o: ObjRef) -> Option<(ObjRef, usize)> {
        match &self.heap.obj(o).kind {
            Kind::TypedArray(t) => Some((t.buffer, t.offset)),
            _ => None,
        }
    }

    /// A new ArrayBuffer of the current realm holding `bytes`.
    pub fn new_array_buffer(&mut self, bytes: Vec<u8>) -> ObjRef {
        let proto = self.realm.array_buffer_proto;
        let buf = Buffer { bytes: Some(bytes), block: None, max: None, shared: false, immutable: false };
        self.heap.alloc(Object::new(Some(proto), Kind::ArrayBuffer(Box::new(buf))))
    }

    /// The bytes a BufferSource (an ArrayBuffer, a typed array or a DataView) views (WebIDL
    /// "get a copy of the bytes held by the buffer source"); None for anything else.
    pub fn buffer_source_bytes(&self, v: &Value) -> Option<Vec<u8>> {
        let Value::Object(o) = v else { return None };
        let (buf, off, len) = match &self.heap.obj(*o).kind {
            Kind::ArrayBuffer(_) => {
                let n = self.buffer_byte_len(*o)?;
                (*o, 0, n)
            }
            Kind::TypedArray(t) => {
                let n = self.ta_length(*o)? * t.kind.size();
                (t.buffer, t.offset, n)
            }
            Kind::DataView(d) => {
                let total = self.buffer_byte_len(d.buffer)?;
                let n = d.length.unwrap_or(total.saturating_sub(d.offset));
                (d.buffer, d.offset, n)
            }
            _ => return None,
        };
        self.buffer_read(buf, off, len)
    }

    fn buffer_byte_len(&self, b: ObjRef) -> Option<usize> {
        match &self.heap.obj(b).kind {
            Kind::ArrayBuffer(d) => match (&d.bytes, &d.block) {
                (Some(v), _) => Some(v.len()),
                (None, Some(blk)) => Some(blk.len()),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn buffer_bytes(&self, b: ObjRef) -> Option<&Vec<u8>> {
        match &self.heap.obj(b).kind {
            Kind::ArrayBuffer(d) => d.bytes.as_ref(),
            _ => None,
        }
    }

    pub fn buffer_bytes_mut(&mut self, b: ObjRef) -> Option<&mut Vec<u8>> {
        match &mut self.heap.obj_mut(b).kind {
            Kind::ArrayBuffer(d) => d.bytes.as_mut(),
            _ => None,
        }
    }

    /// IsImmutableBuffer.
    pub fn is_immutable(&self, b: ObjRef) -> bool {
        matches!(&self.heap.obj(b).kind, Kind::ArrayBuffer(d) if d.immutable)
    }

    /// Whether a typed array or DataView views an immutable buffer.
    pub fn views_immutable(&self, o: ObjRef) -> bool {
        match &self.heap.obj(o).kind {
            Kind::TypedArray(t) => self.is_immutable(t.buffer),
            Kind::DataView(d) => self.is_immutable(d.buffer),
            _ => false,
        }
    }

    pub fn is_detached(&self, b: ObjRef) -> bool {
        matches!(&self.heap.obj(b).kind, Kind::ArrayBuffer(d) if d.bytes.is_none() && d.block.is_none())
    }

    /// DetachArrayBuffer (§25.1.3.5), for hosts and `transfer`.
    pub fn detach_array_buffer(&mut self, b: ObjRef) -> JsResult<()> {
        match &mut self.heap.obj_mut(b).kind {
            Kind::ArrayBuffer(d) if !d.shared && !d.immutable => {
                d.bytes = None;
                Ok(())
            }
            _ => self.type_error("not a detachable ArrayBuffer"),
        }
    }

    pub(crate) fn ta(&self, o: ObjRef) -> Option<(TaKind, ObjRef, usize, Option<usize>)> {
        match &self.heap.obj(o).kind {
            Kind::TypedArray(t) => Some((t.kind, t.buffer, t.offset, t.length)),
            _ => None,
        }
    }

    /// TypedArrayLength, or `None` when out of bounds or detached (§10.4.5.12–13).
    pub fn ta_length(&self, o: ObjRef) -> Option<usize> {
        let (kind, buf, off, len) = self.ta(o)?;
        let blen = self.buffer_len(buf)?;
        if off > blen {
            return None;
        }
        match len {
            Some(l) => (off + l * kind.size() <= blen).then_some(l),
            None => Some((blen - off) / kind.size()),
        }
    }

    /// ValidateTypedArray (§23.2.4.4): the length, TypeError when out of bounds.
    pub fn ta_validate(&mut self, v: &Value) -> JsResult<(ObjRef, usize)> {
        let Value::Object(o) = v else { return self.type_error("not a typed array") };
        if self.ta(*o).is_none() {
            return self.type_error("not a typed array");
        }
        match self.ta_length(*o) {
            Some(l) => Ok((*o, l)),
            None => self.type_error("typed array is detached or out of bounds"),
        }
    }

    fn valid_index(&self, o: ObjRef, n: f64) -> Option<usize> {
        if n.fract() != 0.0 || (n == 0.0 && n.is_sign_negative()) || n < 0.0 {
            return None;
        }
        let len = self.ta_length(o)?;
        (n < len as f64).then_some(n as usize)
    }

    /// TypedArrayGetElement (§10.4.5.15).
    pub fn ta_get(&self, o: ObjRef, n: f64) -> Value {
        let Some(i) = self.valid_index(o, n) else { return Value::Undefined };
        let (kind, buf, off, _) = self.ta(o).expect("typed array");
        self.buffer_get(buf, kind, off + i * kind.size(), false).unwrap_or_default()
    }

    /// TypedArraySetElement (§10.4.5.16): convert first, then write if still valid.
    pub fn ta_set(&mut self, o: ObjRef, n: f64, v: &Value) -> JsResult<()> {
        let x = self.ta_coerce(o, v)?;
        if let Some(i) = self.valid_index(o, n) {
            let (kind, buf, off, _) = self.ta(o).expect("typed array");
            self.buffer_set(buf, kind, off + i * kind.size(), &x, false);
        }
        Ok(())
    }

    /// ToBigInt or ToNumber by the array's content type.
    pub fn ta_coerce(&mut self, o: ObjRef, v: &Value) -> JsResult<Value> {
        let kind = self.ta(o).map(|t| t.0).unwrap_or(TaKind::Float64);
        if kind.is_bigint() {
            Ok(Value::BigInt(self.to_bigint(v)?))
        } else {
            Ok(Value::Number(self.to_number(v)?))
        }
    }

    // ---- the exotic internal methods, called from object.rs for numeric keys ----

    pub(super) fn ta_get_own(&self, o: ObjRef, n: f64) -> Option<PropDesc> {
        // An immutable buffer's elements are non-writable and non-configurable.
        let mutable = !self.views_immutable(o);
        match self.ta_get(o, n) {
            Value::Undefined => None,
            v => Some(PropDesc::data(v, mutable, true, mutable)),
        }
    }

    pub(super) fn ta_has(&self, o: ObjRef, n: f64) -> bool {
        self.valid_index(o, n).is_some()
    }

    pub(super) fn ta_define(&mut self, o: ObjRef, n: f64, d: PropDesc) -> JsResult<bool> {
        if self.valid_index(o, n).is_none() {
            return Ok(false);
        }
        if self.views_immutable(o) {
            // ValidateAndApplyPropertyDescriptor against the frozen element.
            let Some(cur) = self.ta_get_own(o, n) else { return Ok(false) };
            if d.configurable == Some(true)
                || d.enumerable == Some(false)
                || d.is_accessor()
                || d.writable == Some(true)
            {
                return Ok(false);
            }
            return Ok(match (&d.value, &cur.value) {
                (Some(v), Some(c)) => super::value::same_value(v, c),
                _ => true,
            });
        }
        if d.configurable == Some(false)
            || d.enumerable == Some(false)
            || d.is_accessor()
            || d.writable == Some(false)
        {
            return Ok(false);
        }
        if let Some(v) = &d.value {
            self.ta_set(o, n, v)?;
        }
        Ok(true)
    }

    pub(super) fn ta_keys(&self, o: ObjRef) -> Vec<PropKey> {
        let n = self.ta_length(o).unwrap_or(0);
        (0..n).map(|i| PropKey::from_f64(i as f64)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_precision_rounding() {
        for (x, h) in [
            (1.0, 0x3C00),
            (65504.0, 0x7BFF),
            (65520.0, 0x7C00),
            (2f64.powi(-24), 1),
            (2f64.powi(-25), 0),
            (-0.0, 0x8000),
            (0.1, 0x2E66),
            (1.0 + 2f64.powi(-11), 0x3C00),
            (1.0 + 3.0 * 2f64.powi(-11), 0x3C02),
        ] {
            assert_eq!(f64_to_f16_bits(x), h, "{x}");
        }
        assert_eq!(f16_bits_to_f64(0x3C00), 1.0);
        assert_eq!(f16_bits_to_f64(1), 2f64.powi(-24));
    }

    #[test]
    fn canonical_numeric_strings() {
        let k = |s: &str| canonical_numeric(&PropKey::named(s));
        assert_eq!(k("1.5"), Some(1.5));
        assert!(k("-0").is_some_and(|n| n == 0.0 && n.is_sign_negative()));
        assert_eq!(k("01"), None);
        assert_eq!(k("Infinity"), Some(f64::INFINITY));
        assert_eq!(k("foo"), None);
    }
}

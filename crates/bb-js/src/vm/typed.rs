//! ArrayBuffer and SharedArrayBuffer (§25.1–2), DataView (§25.3), typed arrays (§23.2) and
//! the TypedArray exotic object (§10.4.5).

use super::heap::*;
use super::object::PropDesc;
use super::value::{number_to_string, string_to_number, PropKey, Value};
use super::{JsResult, Vm};

/// The largest buffer we allocate (bytes); bigger requests are a RangeError.
pub const MAX_BUFFER: usize = 1 << 31;

pub struct Buffer {
    /// `None` once detached.
    pub bytes: Option<Vec<u8>>,
    /// Resizable (ArrayBuffer) or growable (SharedArrayBuffer) up to this.
    pub max: Option<usize>,
    pub shared: bool,
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
}

pub const TA_KINDS: [(TaKind, &str); 10] = [
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
];

impl TaKind {
    pub fn size(self) -> usize {
        match self {
            TaKind::Int8 | TaKind::Uint8 | TaKind::Uint8Clamped => 1,
            TaKind::Int16 | TaKind::Uint16 | TaKind::Float16 => 2,
            TaKind::Int32 | TaKind::Uint32 | TaKind::Float32 => 4,
            TaKind::Float64 => 8,
        }
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
        TaKind::Float64 => f64::from_le_bytes(b),
    }
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
        TaKind::Float64 => v.to_le_bytes().to_vec(),
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

    pub fn is_detached(&self, b: ObjRef) -> bool {
        matches!(&self.heap.obj(b).kind, Kind::ArrayBuffer(d) if d.bytes.is_none())
    }

    /// DetachArrayBuffer (§25.1.3.5), for hosts and `transfer`.
    pub fn detach_array_buffer(&mut self, b: ObjRef) -> JsResult<()> {
        match &mut self.heap.obj_mut(b).kind {
            Kind::ArrayBuffer(d) if !d.shared => {
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
        let blen = self.buffer_bytes(buf)?.len();
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
        match self.buffer_bytes(buf) {
            Some(b) => Value::Number(read_elem(b, kind, off + i * kind.size(), false)),
            None => Value::Undefined,
        }
    }

    /// TypedArraySetElement (§10.4.5.16): convert first, then write if still valid.
    pub fn ta_set(&mut self, o: ObjRef, n: f64, v: &Value) -> JsResult<()> {
        let x = self.to_number(v)?;
        if let Some(i) = self.valid_index(o, n) {
            let (kind, buf, off, _) = self.ta(o).expect("typed array");
            if let Some(b) = self.buffer_bytes_mut(buf) {
                write_elem(b, kind, off + i * kind.size(), x, false);
            }
        }
        Ok(())
    }

    // ---- the exotic internal methods, called from object.rs for numeric keys ----

    pub(super) fn ta_get_own(&self, o: ObjRef, n: f64) -> Option<PropDesc> {
        match self.ta_get(o, n) {
            Value::Undefined => None,
            v => Some(PropDesc::data(v, true, true, true)),
        }
    }

    pub(super) fn ta_has(&self, o: ObjRef, n: f64) -> bool {
        self.valid_index(o, n).is_some()
    }

    pub(super) fn ta_define(&mut self, o: ObjRef, n: f64, d: PropDesc) -> JsResult<bool> {
        if self.valid_index(o, n).is_none() {
            return Ok(false);
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

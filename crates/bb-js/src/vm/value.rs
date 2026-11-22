//! ECMAScript language values (ECMA-262 §6.1) and the number ↔ string conversions
//! (§6.1.6.1.20 Number::toString, §7.1.4.1.1 StringToNumber).

use super::heap::ObjRef;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

/// A JavaScript string: UTF-16 code units, possibly with lone surrogates (§6.1.4).
pub type JsString = Rc<[u16]>;

/// The longest string we build (code units); longer ones are a RangeError.
pub const MAX_STRING_LEN: usize = 1 << 28;

pub fn js_str(s: &str) -> JsString {
    s.encode_utf16().collect::<Vec<_>>().into()
}

pub fn to_rust(s: &[u16]) -> String {
    String::from_utf16_lossy(s)
}

/// A Symbol (§6.1.5): compared by identity.
#[derive(Clone, Debug)]
pub struct Sym(pub Rc<SymbolData>);

#[derive(Debug)]
pub struct SymbolData {
    pub description: Option<JsString>,
    /// Registered with `Symbol.for` (§20.4.2.2).
    pub registered: bool,
    /// A Private Name (§6.2.12), never visible to scripts as a value.
    pub private: bool,
}

impl Sym {
    pub fn new(description: Option<JsString>) -> Sym {
        Sym(Rc::new(SymbolData { description, registered: false, private: false }))
    }
    pub fn private(description: &str) -> Sym {
        Sym(Rc::new(SymbolData { description: Some(js_str(description)), registered: false, private: true }))
    }
}

impl PartialEq for Sym {
    fn eq(&self, o: &Self) -> bool {
        Rc::ptr_eq(&self.0, &o.0)
    }
}
impl Eq for Sym {}
impl Hash for Sym {
    fn hash<H: Hasher>(&self, h: &mut H) {
        (Rc::as_ptr(&self.0) as usize).hash(h)
    }
}

#[derive(Clone, Debug, Default)]
pub enum Value {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
    Symbol(Sym),
    Object(ObjRef),
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::String(js_str(s))
    }
    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }
    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }
    pub fn as_object(&self) -> Option<ObjRef> {
        match self {
            Value::Object(o) => Some(*o),
            _ => None,
        }
    }
    /// ToBoolean (§7.1.2).
    pub fn truthy(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Number(n) => !(*n == 0.0 || n.is_nan()),
            Value::String(s) => !s.is_empty(),
            Value::Symbol(_) | Value::Object(_) => true,
        }
    }
}

/// SameValue (§7.2.10).
pub fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            (x.is_nan() && y.is_nan()) || (x == y && x.is_sign_negative() == y.is_sign_negative())
        }
        _ => strict_equals(a, b),
    }
}

/// SameValueZero (§7.2.11).
pub fn same_value_zero(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => (x.is_nan() && y.is_nan()) || x == y,
        _ => strict_equals(a, b),
    }
}

/// IsStrictlyEqual (§7.2.15).
pub fn strict_equals(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Symbol(x), Value::Symbol(y)) => x == y,
        (Value::Object(x), Value::Object(y)) => x == y,
        _ => false,
    }
}

/// A property key (§6.1.7): array indices (§6.1.7, "array index": 0 ≤ i < 2³² − 1) are kept as
/// numbers so they sort numerically in OrdinaryOwnPropertyKeys (§10.1.11.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PropKey {
    Index(u32),
    Str(JsString),
    Sym(Sym),
}

impl PropKey {
    pub fn from_js(s: JsString) -> PropKey {
        match array_index(&s) {
            Some(i) => PropKey::Index(i),
            None => PropKey::Str(s),
        }
    }
    pub fn named(s: &str) -> PropKey {
        PropKey::from_js(js_str(s))
    }
    pub fn from_u32(i: u32) -> PropKey {
        if i == u32::MAX {
            PropKey::Str(js_str("4294967295"))
        } else {
            PropKey::Index(i)
        }
    }
    pub fn from_f64(n: f64) -> PropKey {
        if n >= 0.0 && n < 4294967295.0 && n.fract() == 0.0 && !(n == 0.0 && n.is_sign_negative()) {
            PropKey::Index(n as u32)
        } else {
            PropKey::Str(js_str(&number_to_string(n)))
        }
    }
    pub fn to_value(&self) -> Value {
        match self {
            PropKey::Index(i) => Value::String(js_str(&i.to_string())),
            PropKey::Str(s) => Value::String(s.clone()),
            PropKey::Sym(s) => Value::Symbol(s.clone()),
        }
    }
    /// The key as a string (symbols: their descriptive string, for messages).
    pub fn display(&self) -> String {
        match self {
            PropKey::Index(i) => i.to_string(),
            PropKey::Str(s) => to_rust(s),
            PropKey::Sym(s) => {
                format!("Symbol({})", s.0.description.as_deref().map(to_rust).unwrap_or_default())
            }
        }
    }
    pub fn is_str(&self, s: &str) -> bool {
        matches!(self, PropKey::Str(k) if k.iter().copied().eq(s.encode_utf16()))
    }
}

/// CanonicalNumericIndexString restricted to array indices.
pub fn array_index(s: &[u16]) -> Option<u32> {
    if s.is_empty() || s.len() > 10 || (s.len() > 1 && s[0] == b'0' as u16) {
        return None;
    }
    let mut n: u64 = 0;
    for &c in s {
        if !(b'0' as u16..=b'9' as u16).contains(&c) {
            return None;
        }
        n = n * 10 + (c - b'0' as u16) as u64;
    }
    (n < 4294967295).then_some(n as u32)
}

/// Number::toString(x, 10) (§6.1.6.1.20).
pub fn number_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x < 0.0 {
        return format!("-{}", number_to_string(-x));
    }
    if x.is_infinite() {
        return "Infinity".into();
    }
    // Rust's `{:e}` prints the shortest digits that round-trip: exactly k and n of step 5.
    let sci = format!("{x:e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let k = digits.len() as i32;
    let n = exp.parse::<i32>().unwrap_or(0) + 1;
    if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let sign = if e >= 0 { '+' } else { '-' };
        if k == 1 {
            format!("{digits}e{sign}{}", e.abs())
        } else {
            format!("{}.{}e{sign}{}", &digits[..1], &digits[1..], e.abs())
        }
    }
}

/// WhiteSpace or LineTerminator (§12.2, §12.3), what StringToNumber and `trim` strip.
pub fn is_js_space(c: u16) -> bool {
    matches!(
        c,
        0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000
            ..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF
    )
}

pub fn trim_js(s: &[u16]) -> &[u16] {
    let start = s.iter().position(|&c| !is_js_space(c)).unwrap_or(s.len());
    let end = s.iter().rposition(|&c| !is_js_space(c)).map_or(start, |e| e + 1);
    &s[start..end]
}

/// Digits in `radix` to a correctly rounded double (for 0x/0o/0b and parseInt).
pub fn digits_to_f64(digits: &[u8], radix: u32) -> f64 {
    if radix.is_power_of_two() {
        return pow2_digits_to_f64(digits, radix.trailing_zeros());
    }
    // Exact while it fits in a u128; beyond that the result is "implementation-approximated"
    // (§19.2.5 parseInt step 16) for radices other than powers of two.
    let mut acc: u128 = 0;
    let mut i = 0;
    while i < digits.len() {
        match acc.checked_mul(radix as u128).and_then(|a| a.checked_add(digits[i] as u128)) {
            Some(a) => acc = a,
            None => break,
        }
        i += 1;
    }
    let mut f = u128_to_f64(acc);
    for &d in &digits[i..] {
        f = f * radix as f64 + d as f64;
    }
    f
}

fn u128_to_f64(v: u128) -> f64 {
    // `as` rounds to nearest, ties to even.
    v as f64
}

/// Correct rounding for power-of-two radices of any length.
fn pow2_digits_to_f64(digits: &[u8], bits: u32) -> f64 {
    let mut bitvec: Vec<bool> = Vec::with_capacity(digits.len() * bits as usize);
    for &d in digits {
        for b in (0..bits).rev() {
            bitvec.push((d >> b) & 1 == 1);
        }
    }
    let first = match bitvec.iter().position(|&b| b) {
        Some(p) => p,
        None => return 0.0,
    };
    let bits = &bitvec[first..];
    let len = bits.len();
    if len > 1024 {
        return f64::INFINITY;
    }
    let mut mant: u64 = 0;
    for &b in bits.iter().take(53) {
        mant = (mant << 1) | b as u64;
    }
    if len <= 53 {
        return mant as f64;
    }
    let round = bits[53];
    let sticky = bits[54..].iter().any(|&b| b);
    if round && (sticky || mant & 1 == 1) {
        mant += 1;
    }
    let exp = (len - 53) as i32;
    let r = mant as f64 * 2f64.powi(exp);
    if r.is_finite() {
        r
    } else {
        f64::INFINITY
    }
}

/// StringToNumber (§7.1.4.1.1).
pub fn string_to_number(s: &[u16]) -> f64 {
    let t = trim_js(s);
    if t.is_empty() {
        return 0.0;
    }
    let Ok(ascii) = String::from_utf16(t) else { return f64::NAN };
    if !ascii.is_ascii() {
        return f64::NAN;
    }
    let b = ascii.as_bytes();
    if b.len() > 2 && b[0] == b'0' {
        let radix = match b[1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            b'b' | b'B' => 2,
            _ => 0,
        };
        if radix != 0 {
            let mut ds = Vec::with_capacity(b.len() - 2);
            for &c in &b[2..] {
                match (c as char).to_digit(radix) {
                    Some(d) => ds.push(d as u8),
                    None => return f64::NAN,
                }
            }
            return digits_to_f64(&ds, radix);
        }
    }
    let (sign, body) = match b[0] {
        b'+' => (1.0, &ascii[1..]),
        b'-' => (-1.0, &ascii[1..]),
        _ => (1.0, &ascii[..]),
    };
    if body == "Infinity" {
        return sign * f64::INFINITY;
    }
    if !is_decimal_literal(body.as_bytes()) {
        return f64::NAN;
    }
    sign * body.parse::<f64>().unwrap_or(f64::NAN)
}

/// StrUnsignedDecimalLiteral without `Infinity` (§7.1.4.1).
fn is_decimal_literal(b: &[u8]) -> bool {
    let mut i = 0;
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut digits = i > int_start;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let f = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        digits |= i > f;
    }
    if !digits {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let e = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == e {
            return false;
        }
    }
    i == b.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_to_string_follows_the_spec() {
        for (x, s) in [
            (1.0, "1"),
            (0.1, "0.1"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123.456, "123.456"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (1.5e-10, "1.5e-10"),
            (-0.0, "0"),
            (f64::NAN, "NaN"),
            (-f64::INFINITY, "-Infinity"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (0.1 + 0.2, "0.30000000000000004"),
        ] {
            assert_eq!(number_to_string(x), s, "{x}");
        }
    }

    #[test]
    fn string_to_number_follows_the_grammar() {
        let n = |s: &str| string_to_number(&js_str(s));
        assert_eq!(n("  12  "), 12.0);
        assert_eq!(n(""), 0.0);
        assert_eq!(n("0x1F"), 31.0);
        assert_eq!(n("0b101"), 5.0);
        assert_eq!(n("-Infinity"), f64::NEG_INFINITY);
        assert!(n("infinity").is_nan());
        assert!(n("1_000").is_nan());
        assert!(n("-0x10").is_nan());
        assert!(n(".").is_nan());
        assert_eq!(n(".5"), 0.5);
        assert_eq!(n("5."), 5.0);
        assert_eq!(n("1e3"), 1000.0);
        assert!(n("1e").is_nan());
        assert_eq!(n("0x20000000000001"), 9007199254740992.0);
        assert_eq!(n("0x20000000000003"), 9007199254740996.0);
    }

    #[test]
    fn array_indices() {
        assert_eq!(array_index(&js_str("0")), Some(0));
        assert_eq!(array_index(&js_str("01")), None);
        assert_eq!(array_index(&js_str("4294967294")), Some(4294967294));
        assert_eq!(array_index(&js_str("4294967295")), None);
    }
}

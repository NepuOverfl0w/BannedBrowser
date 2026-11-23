//! Number (§21.1), Math (§21.3), `parseInt`/`parseFloat` (§19.2.4–5).

use super::super::bigint::BigInt;
use super::super::heap::*;
use super::super::realm::{self, constant, method};
use super::super::value::{digits_to_f64, is_js_space, number_to_string, PropKey, Value};
use super::super::{ErrorKind, JsResult, Vm};
use super::arg_string;

fn this_number(vm: &mut Vm, v: &Value) -> JsResult<f64> {
    match v {
        Value::Number(n) => Ok(*n),
        Value::Object(o) => match vm.heap.obj(*o).kind {
            Kind::Number(n) => Ok(n),
            _ => vm.type_error("not a Number"),
        },
        _ => vm.type_error("not a Number"),
    }
}

/// The exact decimal expansion of a finite positive double: digits and the exponent of the
/// first digit (x = d₀.d₁d₂… × 10^e).
fn exact_digits(x: f64) -> (Vec<u8>, i32) {
    let s = format!("{x:.1100e}");
    let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
    let mut d: Vec<u8> = m.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect();
    while d.len() > 1 && d.last() == Some(&0) {
        d.pop();
    }
    (d, e.parse().unwrap_or(0))
}

/// Round digits to `n` significant digits, half up (ties away from zero on the exact value).
fn round_sig(d: &[u8], e: i32, n: usize) -> (Vec<u8>, i32) {
    let mut out: Vec<u8> = d.iter().copied().take(n).collect();
    out.resize(n, 0);
    let mut e = e;
    if d.len() > n && d[n] >= 5 {
        let mut i = n;
        loop {
            if i == 0 {
                out.insert(0, 1);
                out.pop();
                e += 1;
                break;
            }
            i -= 1;
            if out[i] == 9 {
                out[i] = 0;
            } else {
                out[i] += 1;
                break;
            }
        }
    }
    (out, e)
}

fn digits_str(d: &[u8]) -> String {
    d.iter().map(|x| (b'0' + x) as char).collect()
}

/// Number.prototype.toFixed (§21.1.3.3) for 0 ≤ x < 1e21.
fn to_fixed(x: f64, f: usize) -> String {
    if x == 0.0 {
        return if f == 0 { "0".into() } else { format!("0.{}", "0".repeat(f)) };
    }
    let (d, e) = exact_digits(x);
    // Keep the digits up to 10^-f.
    let keep = e + 1 + f as i32;
    let n: Vec<u8> = if keep < 0 {
        vec![0]
    } else if keep == 0 {
        vec![if d[0] >= 5 { 1 } else { 0 }]
    } else {
        let (r, e2) = round_sig(&d, e, keep as usize);
        if e2 > e {
            let mut v = r;
            v.push(0);
            v
        } else {
            r
        }
    };
    let mut s = digits_str(&n);
    if f > 0 {
        if s.len() <= f {
            s = format!("{}{s}", "0".repeat(f + 1 - s.len()));
        }
        let at = s.len() - f;
        s.insert(at, '.');
    }
    s
}

/// Number::toString(x, radix) for radix ≠ 10.
fn to_radix(x: f64, radix: u32) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let neg = x < 0.0;
    let v = x.abs();
    let chars: Vec<char> = "0123456789abcdefghijklmnopqrstuvwxyz".chars().collect();
    let mut int = v.floor();
    let mut frac = v - int;
    // Fraction digits until the remaining precision is used up (shortest that round-trips).
    let next = f64::from_bits(v.to_bits() + 1);
    let mut delta = (0.5 * (next - v)).max(f64::from_bits(1));
    let mut fd: Vec<u32> = Vec::new();
    if frac >= delta {
        loop {
            frac *= radix as f64;
            delta *= radix as f64;
            let digit = frac as u32;
            fd.push(digit);
            frac -= digit as f64;
            if frac > 0.5 || (frac == 0.5 && digit & 1 == 1) {
                if frac + delta > 1.0 {
                    // Round up, propagating carries.
                    loop {
                        match fd.pop() {
                            None => {
                                int += 1.0;
                                break;
                            }
                            Some(dg) if dg + 1 < radix => {
                                fd.push(dg + 1);
                                break;
                            }
                            Some(_) => {}
                        }
                    }
                    break;
                }
            }
            if frac < delta {
                break;
            }
        }
    }
    // Integer part: exact for any double (it's an integer times a power of two).
    let mut idigits: Vec<char> = Vec::new();
    if int < 9007199254740992.0 {
        let mut n = int as u64;
        if n == 0 {
            idigits.push('0');
        }
        while n > 0 {
            idigits.push(chars[(n % radix as u64) as usize]);
            n /= radix as u64;
        }
    } else {
        let bits = int.to_bits();
        let exp = ((bits >> 52) & 0x7FF) as i32 - 1075;
        let mant = (bits & ((1 << 52) - 1)) | (1 << 52);
        // Big integer as base-2^32 limbs.
        let mut limbs: Vec<u64> = vec![mant & 0xFFFF_FFFF, mant >> 32];
        for _ in 0..exp {
            let mut carry = 0;
            for l in limbs.iter_mut() {
                let v = (*l << 1) | carry;
                carry = v >> 32;
                *l = v & 0xFFFF_FFFF;
            }
            if carry > 0 {
                limbs.push(carry);
            }
        }
        while limbs.iter().any(|&l| l != 0) {
            let mut rem = 0u64;
            for l in limbs.iter_mut().rev() {
                let cur = (rem << 32) | *l;
                *l = cur / radix as u64;
                rem = cur % radix as u64;
            }
            idigits.push(chars[rem as usize]);
            while limbs.len() > 1 && limbs.last() == Some(&0) {
                limbs.pop();
            }
        }
    }
    idigits.reverse();
    let mut s: String = idigits.into_iter().collect();
    if !fd.is_empty() {
        s.push('.');
        s.extend(fd.iter().map(|&d| chars[d as usize]));
    }
    if neg {
        format!("-{s}")
    } else {
        s
    }
}

fn exponential(d: &[u8], e: i32) -> String {
    let mut s = digits_str(&d[..1]);
    if d.len() > 1 {
        s.push('.');
        s.push_str(&digits_str(&d[1..]));
    }
    format!("{s}e{}{}", if e >= 0 { '+' } else { '-' }, e.abs())
}

/// The shortest round-trip digits of a finite positive double.
fn shortest_digits(x: f64) -> (Vec<u8>, i32) {
    let s = format!("{x:e}");
    let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
    (m.bytes().filter(u8::is_ascii_digit).map(|b| b - b'0').collect(), e.parse().unwrap_or(0))
}

pub(super) fn parse_float(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = arg_string(vm, &c, 0)?;
    let start = s.iter().position(|&u| !is_js_space(u)).unwrap_or(s.len());
    let t: String = s[start..].iter().take_while(|&&u| u < 128).map(|&u| u as u8 as char).collect();
    let b = t.as_bytes();
    let mut i = 0;
    let mut sign = 1.0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        if b[i] == b'-' {
            sign = -1.0;
        }
        i += 1;
    }
    if t[i..].starts_with("Infinity") {
        return Ok(Value::Number(sign * f64::INFINITY));
    }
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut any = i > ds;
    if i < b.len() && b[i] == b'.' {
        let fs = i + 1;
        let mut j = fs;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > fs || any {
            any |= j > fs;
            i = j;
        }
    }
    if !any {
        return Ok(Value::Number(f64::NAN));
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let es = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > es {
            i = j;
        }
    }
    let body = &t[ds..i];
    let body = body.strip_suffix('.').unwrap_or(body);
    let v: f64 = body.parse().unwrap_or(f64::NAN);
    Ok(Value::Number(sign * v))
}

pub(super) fn parse_int(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = arg_string(vm, &c, 0)?;
    let mut i = s.iter().position(|&u| !is_js_space(u)).unwrap_or(s.len());
    let mut sign = 1.0;
    if i < s.len() && (s[i] == b'+' as u16 || s[i] == b'-' as u16) {
        if s[i] == b'-' as u16 {
            sign = -1.0;
        }
        i += 1;
    }
    let mut r = vm.to_int32(&c.arg(1))?;
    let mut strip = true;
    if r != 0 {
        if !(2..=36).contains(&r) {
            return Ok(Value::Number(f64::NAN));
        }
        if r != 16 {
            strip = false;
        }
    } else {
        r = 10;
    }
    if strip && i + 1 < s.len() && s[i] == b'0' as u16 && (s[i + 1] == b'x' as u16 || s[i + 1] == b'X' as u16)
    {
        i += 2;
        r = 16;
    }
    let mut digits = Vec::new();
    while i < s.len() {
        match char::from_u32(s[i] as u32).and_then(|ch| ch.to_digit(r as u32)) {
            Some(d) => digits.push(d as u8),
            None => break,
        }
        i += 1;
    }
    if digits.is_empty() {
        return Ok(Value::Number(f64::NAN));
    }
    let v = if r == 10 {
        // Correct rounding for decimal via the standard parser.
        let st: String = digits.iter().map(|d| (b'0' + d) as char).collect();
        st.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        digits_to_f64(&digits, r as u32)
    };
    Ok(Value::Number(sign * v))
}

pub(super) fn install(vm: &mut Vm) {
    let np = vm.realm.number_proto;
    let ctor = realm::constructor(
        vm,
        "Number",
        1,
        |vm, c| {
            // §21.1.1.1: ToNumeric, and a BigInt becomes the Number with its value.
            let n = if c.args.is_empty() {
                0.0
            } else {
                match vm.to_numeric(&c.args[0])? {
                    Value::BigInt(b) => b.to_f64(),
                    v => vm.to_number(&v)?,
                }
            };
            let Some(nt) = c.new_target else { return Ok(Value::Number(n)) };
            let proto = vm.prototype_from_ctor(nt, |r| r.number_proto)?;
            Ok(Value::Object(vm.heap.alloc(Object::new(Some(proto), Kind::Number(n)))))
        },
        np,
    );
    for (n, v) in [
        ("EPSILON", f64::EPSILON),
        ("MAX_SAFE_INTEGER", 9007199254740991.0),
        ("MIN_SAFE_INTEGER", -9007199254740991.0),
        ("MAX_VALUE", f64::MAX),
        ("MIN_VALUE", 5e-324),
        ("NaN", f64::NAN),
        ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ("POSITIVE_INFINITY", f64::INFINITY),
    ] {
        constant(vm, ctor, n, Value::Number(v));
    }
    method(vm, ctor, "isFinite", 1, |_, c| {
        Ok(Value::Bool(matches!(c.arg(0), Value::Number(n) if n.is_finite())))
    });
    method(vm, ctor, "isNaN", 1, |_, c| Ok(Value::Bool(matches!(c.arg(0), Value::Number(n) if n.is_nan()))));
    method(vm, ctor, "isInteger", 1, |_, c| {
        Ok(Value::Bool(matches!(c.arg(0), Value::Number(n) if n.is_finite() && n.trunc() == n)))
    });
    method(vm, ctor, "isSafeInteger", 1, |_, c| {
        Ok(Value::Bool(
            matches!(c.arg(0), Value::Number(n) if n.is_finite() && n.trunc() == n && n.abs() <= 9007199254740991.0),
        ))
    });

    method(vm, np, "valueOf", 0, |vm, c| Ok(Value::Number(this_number(vm, &c.this)?)));
    method(vm, np, "toString", 1, |vm, c| {
        let x = this_number(vm, &c.this)?;
        let r = if c.arg(0).is_undefined() { 10.0 } else { vm.to_integer_or_infinity(&c.arg(0))? };
        if !(2.0..=36.0).contains(&r) {
            return vm.range_error("toString() radix must be between 2 and 36");
        }
        Ok(Value::str(&if r == 10.0 { number_to_string(x) } else { to_radix(x, r as u32) }))
    });
    method(vm, np, "toLocaleString", 0, |vm, c| Ok(Value::str(&number_to_string(this_number(vm, &c.this)?))));
    method(vm, np, "toFixed", 1, |vm, c| {
        let x = this_number(vm, &c.this)?;
        let f = vm.to_integer_or_infinity(&c.arg(0))?;
        if !f.is_finite() || !(0.0..=100.0).contains(&f) {
            return vm.range_error("toFixed() digits argument must be between 0 and 100");
        }
        if !x.is_finite() || x.abs() >= 1e21 {
            return Ok(Value::str(&number_to_string(x)));
        }
        let s = to_fixed(x.abs(), f as usize);
        // §21.1.3.3 step 9: a negative x gets "-" (−0 is not negative).
        Ok(Value::str(&if x < 0.0 { format!("-{s}") } else { s }))
    });
    method(vm, np, "toExponential", 1, |vm, c| {
        let x = this_number(vm, &c.this)?;
        let f = vm.to_integer_or_infinity(&c.arg(0))?;
        if !x.is_finite() {
            return Ok(Value::str(&number_to_string(x)));
        }
        if !(0.0..=100.0).contains(&f) {
            return vm.range_error("toExponential() argument must be between 0 and 100");
        }
        let sign = if x < 0.0 { "-" } else { "" };
        let v = x.abs();
        let s = if v == 0.0 {
            let d = vec![0u8; if c.arg(0).is_undefined() { 1 } else { f as usize + 1 }];
            exponential(&d, 0)
        } else if c.arg(0).is_undefined() {
            let (d, e) = shortest_digits(v);
            exponential(&d, e)
        } else {
            let (d, e) = exact_digits(v);
            let (r, e) = round_sig(&d, e, f as usize + 1);
            exponential(&r, e)
        };
        Ok(Value::str(&format!("{sign}{s}")))
    });
    method(vm, np, "toPrecision", 1, |vm, c| {
        let x = this_number(vm, &c.this)?;
        if c.arg(0).is_undefined() {
            return Ok(Value::str(&number_to_string(x)));
        }
        let p = vm.to_integer_or_infinity(&c.arg(0))?;
        if !x.is_finite() {
            return Ok(Value::str(&number_to_string(x)));
        }
        if !(1.0..=100.0).contains(&p) {
            return vm.range_error("toPrecision() argument must be between 1 and 100");
        }
        let p = p as usize;
        let sign = if x < 0.0 { "-" } else { "" };
        let v = x.abs();
        let (d, e) = if v == 0.0 {
            (vec![0u8; p], 0)
        } else {
            let (d, e) = exact_digits(v);
            round_sig(&d, e, p)
        };
        let s = if e < -6 || e >= p as i32 {
            exponential(&d, e)
        } else if e >= 0 {
            let int = digits_str(&d[..e as usize + 1]);
            if p > e as usize + 1 {
                format!("{int}.{}", digits_str(&d[e as usize + 1..]))
            } else {
                int
            }
        } else {
            format!("0.{}{}", "0".repeat((-e - 1) as usize), digits_str(&d))
        };
        Ok(Value::str(&format!("{sign}{s}")))
    });

    install_math(vm);
    install_bigint(vm);
}

fn num(vm: &mut Vm, c: &NativeCall, i: usize) -> JsResult<f64> {
    vm.to_number(&c.arg(i))
}

thread_local! {
    static RNG: std::cell::Cell<u64> = std::cell::Cell::new({
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0x9E37_79B9);
        t | 1
    });
}

fn random() -> f64 {
    RNG.with(|r| {
        // xorshift64*
        let mut x = r.get();
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        r.set(x);
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    })
}

/// An exact sum of doubles: two's-complement 32-bit limbs (in i64 for lazy carries),
/// scaled by 2^1074 so every finite double is an integer.
struct ExactSum {
    limbs: Vec<i64>,
    /// The carry out of the top limb: the value is limbs + high · 2^(32·LIMBS).
    high: i64,
    pending: u32,
}

impl ExactSum {
    const LIMBS: usize = 72;

    fn new() -> Self {
        ExactSum { limbs: vec![0; Self::LIMBS], high: 0, pending: 0 }
    }

    fn normalize(&mut self) {
        let mut carry = 0i64;
        for l in self.limbs.iter_mut() {
            let v = *l + carry;
            *l = v & 0xFFFF_FFFF;
            carry = v >> 32;
        }
        self.high += carry;
        self.pending = 0;
    }

    fn add(&mut self, x: f64) {
        let bits = x.to_bits();
        let exp = ((bits >> 52) & 0x7FF) as i64;
        let frac = bits & ((1u64 << 52) - 1);
        let (mant, shift) = if exp == 0 { (frac, 0) } else { (frac | (1u64 << 52), exp - 1) };
        if mant == 0 {
            return;
        }
        let sign = if x < 0.0 { -1i64 } else { 1 };
        let (limb, off) = ((shift / 32) as usize, (shift % 32) as u32);
        let wide = (mant as u128) << off;
        for k in 0..3 {
            let part = ((wide >> (32 * k)) & 0xFFFF_FFFF) as i64;
            if part != 0 && limb + k < Self::LIMBS {
                self.limbs[limb + k] += sign * part;
            }
        }
        self.pending += 1;
        if self.pending >= 1 << 20 {
            self.normalize();
        }
    }

    /// The sum rounded to the nearest double, ties to even.
    fn into_f64(mut self) -> f64 {
        self.normalize();
        // |sum| < 2^(32·(LIMBS−1)), so `high` is 0 or −1 (two's complement).
        let negative = self.high < 0;
        let mut mag: Vec<u32> = Vec::with_capacity(Self::LIMBS);
        if negative {
            let mut carry = 1u64;
            for &l in &self.limbs {
                let s = (!(l as u32)) as u64 + carry;
                mag.push(s as u32);
                carry = s >> 32;
            }
        } else {
            mag.extend(self.limbs.iter().map(|&l| l as u32));
        }
        let Some(top) = mag.iter().rposition(|&l| l != 0) else { return 0.0 };
        let bitlen = top as i64 * 32 + (32 - mag[top].leading_zeros() as i64);
        let bit = |i: i64| -> u64 {
            if i < 0 {
                return 0;
            }
            ((mag[(i / 32) as usize] >> (i % 32)) & 1) as u64
        };
        let r = if bitlen <= 53 {
            let mut m = 0u64;
            for i in (0..bitlen).rev() {
                m = (m << 1) | bit(i);
            }
            scale(m as f64, -1074)
        } else {
            let low = bitlen - 53;
            let mut m = 0u64;
            for i in (low..bitlen).rev() {
                m = (m << 1) | bit(i);
            }
            let half = bit(low - 1) == 1;
            let sticky = (0..low - 1).any(|i| bit(i) == 1);
            if half && (sticky || m & 1 == 1) {
                m += 1;
            }
            scale(m as f64, low - 1074)
        };
        if negative {
            -r
        } else {
            r
        }
    }
}

/// x · 2^k, exact unless the result overflows (to ±∞) or is subnormal and inexact.
fn scale(mut x: f64, mut k: i64) -> f64 {
    while k > 0 {
        let s = k.min(1000);
        x *= f64::from_bits(((1023 + s) as u64) << 52);
        k -= s;
    }
    while k < 0 {
        let s = (-k).min(1000);
        x *= f64::from_bits(((1023 - s) as u64) << 52);
        k += s;
    }
    x
}

/// Math.sumPrecise (ES2026 §21.3.2.34).
fn sum_precise(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let items = c.arg(0);
    if items.is_nullish() {
        return vm.type_error("Math.sumPrecise requires an iterable");
    }
    let r = vm.get_iterator(&items)?;
    #[derive(PartialEq)]
    enum State {
        MinusZero,
        Finite,
        PlusInf,
        MinusInf,
        NaN,
    }
    let mut state = State::MinusZero;
    let mut sum = ExactSum::new();
    let mut count = 0f64;
    while let Some(v) = vm.iter_step(r)? {
        count += 1.0;
        let err = if count >= 9007199254740992.0 {
            Some(vm.error(ErrorKind::Range, "Math.sumPrecise: too many values"))
        } else if !matches!(v, Value::Number(_)) {
            Some(vm.error(ErrorKind::Type, "Math.sumPrecise: the values must be numbers"))
        } else {
            None
        };
        if let Some(e) = err {
            if let Kind::IterRecord(x) = &vm.heap.obj(r).kind {
                let it = x.iterator.clone();
                if let Ok(Some(f)) = vm.get_method(&it, &PropKey::lit("return")) {
                    let _ = vm.call(&f, it, Vec::new());
                }
            }
            return Err(e);
        }
        let Value::Number(n) = v else { continue };
        if state == State::NaN {
            continue;
        }
        if n.is_nan() {
            state = State::NaN;
        } else if n == f64::INFINITY {
            state = if state == State::MinusInf { State::NaN } else { State::PlusInf };
        } else if n == f64::NEG_INFINITY {
            state = if state == State::PlusInf { State::NaN } else { State::MinusInf };
        } else if !(n == 0.0 && n.is_sign_negative()) && matches!(state, State::MinusZero | State::Finite) {
            state = State::Finite;
            sum.add(n);
        }
    }
    Ok(Value::Number(match state {
        State::NaN => f64::NAN,
        State::PlusInf => f64::INFINITY,
        State::MinusInf => f64::NEG_INFINITY,
        State::MinusZero => -0.0,
        State::Finite => sum.into_f64(),
    }))
}

fn install_math(vm: &mut Vm) {
    let m = vm.new_object();
    let g = vm.realm.global;
    realm::value(vm, g, "Math", Value::Object(m));
    realm::to_string_tag(vm, m, "Math");
    for (n, v) in [
        ("E", std::f64::consts::E),
        ("LN10", std::f64::consts::LN_10),
        ("LN2", std::f64::consts::LN_2),
        ("LOG10E", std::f64::consts::LOG10_E),
        ("LOG2E", std::f64::consts::LOG2_E),
        ("PI", std::f64::consts::PI),
        ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
        ("SQRT2", std::f64::consts::SQRT_2),
    ] {
        constant(vm, m, n, Value::Number(v));
    }
    macro_rules! unary {
        ($($name:literal => $f:expr),* $(,)?) => {
            $(method(vm, m, $name, 1, |vm, c| { let x = num(vm, &c, 0)?; let f: fn(f64) -> f64 = $f; Ok(Value::Number(f(x))) });)*
        };
    }
    unary!(
        "abs" => f64::abs,
        "acos" => f64::acos,
        "acosh" => f64::acosh,
        "asin" => f64::asin,
        "asinh" => |x| if x == 0.0 { x } else { x.asinh() },
        "atan" => f64::atan,
        "atanh" => |x| if x == 0.0 { x } else { x.atanh() },
        "cbrt" => f64::cbrt,
        "ceil" => f64::ceil,
        "cos" => f64::cos,
        "cosh" => f64::cosh,
        "exp" => f64::exp,
        "expm1" => |x| if x == 0.0 { x } else { x.exp_m1() },
        "floor" => f64::floor,
        "fround" => |x| x as f32 as f64,
        // §21.3.2.17 Math.f16round: round to the nearest binary16 value.
        "f16round" => |x| crate::vm::typed::f16_bits_to_f64(crate::vm::typed::f64_to_f16_bits(x)),
        "log" => f64::ln,
        "log1p" => |x| if x == 0.0 { x } else { x.ln_1p() },
        "log10" => f64::log10,
        "log2" => f64::log2,
        "sign" => |x| if x.is_nan() || x == 0.0 { x } else { x.signum() },
        "sin" => f64::sin,
        "sinh" => |x| if x == 0.0 { x } else { x.sinh() },
        "sqrt" => f64::sqrt,
        "tan" => f64::tan,
        "tanh" => |x| if x == 0.0 { x } else { x.tanh() },
        "trunc" => f64::trunc,
        "round" => |x: f64| {
            if !x.is_finite() || x == 0.0 {
                return x;
            }
            if x > 0.0 && x < 0.5 {
                return 0.0;
            }
            if (-0.5..0.0).contains(&x) {
                return -0.0;
            }
            let r = x.floor();
            if x - r >= 0.5 { r + 1.0 } else { r }
        },
        "clz32" => |x| (super::super::ops::f64_to_int32(x) as u32).leading_zeros() as f64,
    );
    method(vm, m, "atan2", 2, |vm, c| {
        let y = num(vm, &c, 0)?;
        let x = num(vm, &c, 1)?;
        Ok(Value::Number(y.atan2(x)))
    });
    method(vm, m, "pow", 2, |vm, c| {
        let x = num(vm, &c, 0)?;
        let y = num(vm, &c, 1)?;
        Ok(Value::Number(super::super::ops::js_pow(x, y)))
    });
    method(vm, m, "imul", 2, |vm, c| {
        let a = vm.to_int32(&c.arg(0))?;
        let b = vm.to_int32(&c.arg(1))?;
        Ok(Value::Number(a.wrapping_mul(b) as f64))
    });
    method(vm, m, "random", 0, |_, _| Ok(Value::Number(random())));
    method(vm, m, "max", 2, |vm, c| {
        let mut r = f64::NEG_INFINITY;
        let mut nan = false;
        for a in &c.args {
            let x = vm.to_number(a)?;
            if x.is_nan() {
                nan = true;
            } else if x > r || (x == 0.0 && r == 0.0 && !x.is_sign_negative()) {
                r = x;
            }
        }
        Ok(Value::Number(if nan { f64::NAN } else { r }))
    });
    method(vm, m, "min", 2, |vm, c| {
        let mut r = f64::INFINITY;
        let mut nan = false;
        for a in &c.args {
            let x = vm.to_number(a)?;
            if x.is_nan() {
                nan = true;
            } else if x < r || (x == 0.0 && r == 0.0 && x.is_sign_negative()) {
                r = x;
            }
        }
        Ok(Value::Number(if nan { f64::NAN } else { r }))
    });
    method(vm, m, "sumPrecise", 1, sum_precise);
    method(vm, m, "hypot", 2, |vm, c| {
        let mut xs = Vec::new();
        for a in &c.args {
            xs.push(vm.to_number(a)?);
        }
        if xs.iter().any(|x| x.is_infinite()) {
            return Ok(Value::Number(f64::INFINITY));
        }
        if xs.iter().any(|x| x.is_nan()) {
            return Ok(Value::Number(f64::NAN));
        }
        let max = xs.iter().fold(0.0f64, |m, x| m.max(x.abs()));
        if max == 0.0 {
            return Ok(Value::Number(0.0));
        }
        let sum: f64 = xs.iter().map(|x| (x / max) * (x / max)).sum();
        Ok(Value::Number(sum.sqrt() * max))
    });
}

fn this_bigint(vm: &mut Vm, v: &Value) -> JsResult<std::rc::Rc<BigInt>> {
    match v {
        Value::BigInt(b) => Ok(b.clone()),
        Value::Object(o) => match &vm.heap.obj(*o).kind {
            Kind::BigInt(b) => Ok(b.clone()),
            _ => vm.type_error("not a BigInt"),
        },
        _ => vm.type_error("not a BigInt"),
    }
}

/// BigInt (§21.2).
fn install_bigint(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let bp = vm.new_object_with_proto(Some(op));
    vm.realm.bigint_proto = bp;
    let c = realm::constructor(
        vm,
        "BigInt",
        1,
        |vm, c| {
            if c.new_target.is_some() {
                return vm.type_error("BigInt is not a constructor");
            }
            let prim = vm.to_primitive(&c.arg(0), super::super::ops::Hint::Number)?;
            if let Value::Number(n) = prim {
                // NumberToBigInt (§21.2.1.1.1).
                return match BigInt::from_f64(n) {
                    Some(b) => Ok(Value::BigInt(std::rc::Rc::new(b))),
                    None => vm.range_error(&format!(
                        "The number {} cannot be converted to a BigInt because it is not an integer",
                        number_to_string(n)
                    )),
                };
            }
            Ok(Value::BigInt(vm.to_bigint(&prim)?))
        },
        bp,
    );
    method(vm, c, "asIntN", 2, |vm, c| {
        let bits = vm.to_index(&c.arg(0))?;
        let b = vm.to_bigint(&c.arg(1))?;
        Ok(Value::BigInt(std::rc::Rc::new(b.as_int_n(bits as u64))))
    });
    method(vm, c, "asUintN", 2, |vm, c| {
        let bits = vm.to_index(&c.arg(0))?;
        let b = vm.to_bigint(&c.arg(1))?;
        Ok(Value::BigInt(std::rc::Rc::new(b.as_uint_n(bits as u64))))
    });
    method(vm, bp, "toString", 0, |vm, c| {
        let b = this_bigint(vm, &c.this)?;
        let r = if c.arg(0).is_undefined() { 10.0 } else { vm.to_integer_or_infinity(&c.arg(0))? };
        if !(2.0..=36.0).contains(&r) {
            return vm.range_error("toString() radix must be between 2 and 36");
        }
        Ok(Value::str(&b.to_string_radix(r as u32)))
    });
    method(vm, bp, "toLocaleString", 0, |vm, c| {
        let b = this_bigint(vm, &c.this)?;
        Ok(Value::str(&b.to_string_radix(10)))
    });
    method(vm, bp, "valueOf", 0, |vm, c| Ok(Value::BigInt(this_bigint(vm, &c.this)?)));
    realm::to_string_tag(vm, bp, "BigInt");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_and_radix() {
        assert_eq!(to_fixed(0.5, 0), "1");
        assert_eq!(to_fixed(2.5, 0), "3");
        assert_eq!(to_fixed(1.005, 2), "1.00");
        assert_eq!(to_fixed(123.456, 1), "123.5");
        assert_eq!(to_fixed(0.000001, 2), "0.00");
        assert_eq!(to_fixed(999.99, 1), "1000.0");
        assert_eq!(to_radix(255.0, 16), "ff");
        assert_eq!(to_radix(-255.0, 2), "-11111111");
        assert_eq!(to_radix(0.5, 2), "0.1");
    }
}

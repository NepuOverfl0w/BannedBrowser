//! Arbitrary-precision integers for BigInt (§6.1.6.2): sign and magnitude in base-2³² limbs
//! (little-endian, no high zero limbs; zero is non-negative).

use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct BigInt {
    pub neg: bool,
    pub mag: Vec<u32>,
}

fn trim(v: &mut Vec<u32>) {
    while v.last() == Some(&0) {
        v.pop();
    }
}

fn cmp_mag(a: &[u32], b: &[u32]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

fn add_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry = 0u64;
    for i in 0..a.len().max(b.len()) {
        let s = *a.get(i).unwrap_or(&0) as u64 + *b.get(i).unwrap_or(&0) as u64 + carry;
        out.push(s as u32);
        carry = s >> 32;
    }
    if carry > 0 {
        out.push(carry as u32);
    }
    out
}

/// a − b for |a| ≥ |b|.
fn sub_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0i64;
    for i in 0..a.len() {
        let mut d = a[i] as i64 - *b.get(i).unwrap_or(&0) as i64 - borrow;
        borrow = 0;
        if d < 0 {
            d += 1 << 32;
            borrow = 1;
        }
        out.push(d as u32);
    }
    trim(&mut out);
    out
}

fn mul_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; a.len() + b.len()];
    for (i, &x) in a.iter().enumerate() {
        let mut carry = 0u64;
        for (j, &y) in b.iter().enumerate() {
            let t = out[i + j] as u64 + x as u64 * y as u64 + carry;
            out[i + j] = t as u32;
            carry = t >> 32;
        }
        let mut k = i + b.len();
        while carry > 0 {
            let t = out[k] as u64 + carry;
            out[k] = t as u32;
            carry = t >> 32;
            k += 1;
        }
    }
    trim(&mut out);
    out
}

fn divrem_small(a: &[u32], d: u32) -> (Vec<u32>, u32) {
    let mut q = vec![0u32; a.len()];
    let mut r = 0u64;
    for i in (0..a.len()).rev() {
        let cur = (r << 32) | a[i] as u64;
        q[i] = (cur / d as u64) as u32;
        r = cur % d as u64;
    }
    trim(&mut q);
    (q, r as u32)
}

/// Knuth's algorithm D (TAOCP 4.3.1): magnitudes, `b` non-empty.
fn divrem_mag(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
    if cmp_mag(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    if b.len() == 1 {
        let (q, r) = divrem_small(a, b[0]);
        let mut rv = vec![r];
        trim(&mut rv);
        return (q, rv);
    }
    let s = b[b.len() - 1].leading_zeros();
    let shl = |v: &[u32], extra: bool| -> Vec<u32> {
        let mut out = Vec::with_capacity(v.len() + 1);
        let mut carry = 0u32;
        for &x in v {
            out.push(if s == 0 { x } else { (x << s) | carry });
            carry = if s == 0 { 0 } else { x >> (32 - s) };
        }
        if extra {
            out.push(carry);
        }
        out
    };
    let v = shl(b, false);
    let mut u = shl(a, true);
    let n = v.len();
    let m = u.len() - n - 1;
    let mut q = vec![0u32; m + 1];
    let vtop = v[n - 1] as u64;
    let vnext = v[n - 2] as u64;
    for j in (0..=m).rev() {
        let num = ((u[j + n] as u64) << 32) | u[j + n - 1] as u64;
        let mut qhat = num / vtop;
        let mut rhat = num % vtop;
        while qhat >= 1 << 32 || qhat * vnext > ((rhat << 32) | u[j + n - 2] as u64) {
            qhat -= 1;
            rhat += vtop;
            if rhat >= 1 << 32 {
                break;
            }
        }
        // u[j..j+n+1] -= qhat * v
        let mut borrow = 0i64;
        let mut carry = 0u64;
        for i in 0..n {
            let p = qhat * v[i] as u64 + carry;
            carry = p >> 32;
            let t = u[i + j] as i64 - (p & 0xFFFF_FFFF) as i64 - borrow;
            borrow = if t < 0 { 1 } else { 0 };
            u[i + j] = t.rem_euclid(1 << 32) as u32;
        }
        let t = u[j + n] as i64 - carry as i64 - borrow;
        u[j + n] = t.rem_euclid(1 << 32) as u32;
        if t < 0 {
            // Added back: qhat was one too big.
            qhat -= 1;
            let mut c = 0u64;
            for i in 0..n {
                let s2 = u[i + j] as u64 + v[i] as u64 + c;
                u[i + j] = s2 as u32;
                c = s2 >> 32;
            }
            u[j + n] = u[j + n].wrapping_add(c as u32);
        }
        q[j] = qhat as u32;
    }
    trim(&mut q);
    // Remainder: u[..n] >> s.
    let mut r = vec![0u32; n];
    for i in 0..n {
        r[i] = if s == 0 { u[i] } else { (u[i] >> s) | (u.get(i + 1).copied().unwrap_or(0) << (32 - s)) };
    }
    trim(&mut r);
    (q, r)
}

impl Ord for BigInt {
    fn cmp(&self, o: &BigInt) -> Ordering {
        self.compare(o)
    }
}

impl PartialOrd for BigInt {
    fn partial_cmp(&self, o: &BigInt) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl BigInt {
    pub fn zero() -> BigInt {
        BigInt::default()
    }
    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }
    fn make(neg: bool, mut mag: Vec<u32>) -> BigInt {
        trim(&mut mag);
        let neg = neg && !mag.is_empty();
        BigInt { neg, mag }
    }
    pub fn from_i64(v: i64) -> BigInt {
        let m = v.unsigned_abs();
        BigInt::make(v < 0, vec![m as u32, (m >> 32) as u32])
    }
    pub fn from_u64(v: u64) -> BigInt {
        BigInt::make(false, vec![v as u32, (v >> 32) as u32])
    }
    /// An integral double, exactly.
    pub fn from_f64(x: f64) -> Option<BigInt> {
        if !x.is_finite() || x.fract() != 0.0 {
            return None;
        }
        let bits = x.to_bits();
        let exp = ((bits >> 52) & 0x7FF) as i32;
        if exp == 0 {
            return Some(BigInt::zero());
        }
        let mant = (bits & ((1 << 52) - 1)) | (1 << 52);
        let e = exp - 1075;
        let m = BigInt::from_u64(mant);
        let r = if e >= 0 { m.shl(e as u64) } else { m.shr_mag((-e) as u64) };
        Some(if x < 0.0 { r.neg() } else { r })
    }
    /// Number::toNumber of a BigInt (§21.1.1.1 step 2): round to nearest, ties to even.
    pub fn to_f64(&self) -> f64 {
        let bits = self.bit_len();
        if bits == 0 {
            return 0.0;
        }
        let r = if bits <= 64 {
            let mut v: u64 = 0;
            for (i, &l) in self.mag.iter().enumerate().take(2) {
                v |= (l as u64) << (32 * i);
            }
            v as f64
        } else {
            // Take the top 64 bits plus a sticky bit for the rest.
            let shift = bits - 64;
            let top = self.shr_mag(shift);
            let mut v: u64 = 0;
            for (i, &l) in top.mag.iter().enumerate().take(2) {
                v |= (l as u64) << (32 * i);
            }
            let sticky = self.low_bits_nonzero(shift);
            // Round 64 → 53 bits ourselves (u64 as f64 would ignore the sticky bit).
            let lower = v & 0x7FF;
            let mut m = v >> 11;
            let half = 0x400;
            if lower > half || (lower == half && (sticky || m & 1 == 1)) {
                m += 1;
            }
            let r = (m as f64) * 2f64.powi((shift + 11) as i32);
            if r.is_finite() {
                r
            } else {
                f64::INFINITY
            }
        };
        if self.neg {
            -r
        } else {
            r
        }
    }
    fn low_bits_nonzero(&self, n: u64) -> bool {
        let full = (n / 32) as usize;
        if self.mag.iter().take(full).any(|&l| l != 0) {
            return true;
        }
        let rem = n % 32;
        rem > 0 && self.mag.get(full).is_some_and(|&l| l & ((1u32 << rem) - 1) != 0)
    }
    pub fn bit_len(&self) -> u64 {
        match self.mag.last() {
            None => 0,
            Some(&top) => (self.mag.len() as u64 - 1) * 32 + (32 - top.leading_zeros()) as u64,
        }
    }
    pub fn neg(&self) -> BigInt {
        BigInt::make(!self.neg, self.mag.clone())
    }
    pub fn add(&self, o: &BigInt) -> BigInt {
        if self.neg == o.neg {
            return BigInt::make(self.neg, add_mag(&self.mag, &o.mag));
        }
        match cmp_mag(&self.mag, &o.mag) {
            Ordering::Less => BigInt::make(o.neg, sub_mag(&o.mag, &self.mag)),
            _ => BigInt::make(self.neg, sub_mag(&self.mag, &o.mag)),
        }
    }
    pub fn sub(&self, o: &BigInt) -> BigInt {
        self.add(&o.neg())
    }
    pub fn mul(&self, o: &BigInt) -> BigInt {
        BigInt::make(self.neg != o.neg, mul_mag(&self.mag, &o.mag))
    }
    /// Truncating division and remainder (BigInt::divide / remainder); `None` for ÷ 0.
    pub fn divrem(&self, o: &BigInt) -> Option<(BigInt, BigInt)> {
        if o.is_zero() {
            return None;
        }
        let (q, r) = divrem_mag(&self.mag, &o.mag);
        Some((BigInt::make(self.neg != o.neg, q), BigInt::make(self.neg, r)))
    }
    pub fn pow(&self, mut e: u64) -> BigInt {
        let mut base = self.clone();
        let mut r = BigInt::from_i64(1);
        while e > 0 {
            if e & 1 == 1 {
                r = r.mul(&base);
            }
            e >>= 1;
            if e > 0 {
                base = base.mul(&base);
            }
        }
        r
    }
    fn shr_mag(&self, n: u64) -> BigInt {
        let limbs = (n / 32) as usize;
        let bits = (n % 32) as u32;
        if limbs >= self.mag.len() {
            return BigInt::zero();
        }
        let src = &self.mag[limbs..];
        let mut out = Vec::with_capacity(src.len());
        for i in 0..src.len() {
            let lo = src[i] >> bits;
            let hi = if bits == 0 { 0 } else { src.get(i + 1).copied().unwrap_or(0) << (32 - bits) };
            out.push(lo | hi);
        }
        BigInt::make(self.neg, out)
    }
    pub fn shl(&self, n: u64) -> BigInt {
        let limbs = (n / 32) as usize;
        let bits = (n % 32) as u32;
        let mut out = vec![0u32; limbs];
        let mut carry = 0u32;
        for &x in &self.mag {
            out.push(if bits == 0 { x } else { (x << bits) | carry });
            carry = if bits == 0 { 0 } else { x >> (32 - bits) };
        }
        if carry > 0 {
            out.push(carry);
        }
        BigInt::make(self.neg, out)
    }
    /// Arithmetic shift right: floor(x / 2^n).
    pub fn shr(&self, n: u64) -> BigInt {
        if !self.neg {
            return self.shr_mag(n);
        }
        // floor for negatives: -((|x| - 1) >> n) - 1
        let one = BigInt::from_i64(1);
        let t = self.neg().sub(&one).shr_mag(n);
        t.neg().sub(&one)
    }
    fn compare(&self, o: &BigInt) -> Ordering {
        match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => cmp_mag(&self.mag, &o.mag),
            (true, true) => cmp_mag(&o.mag, &self.mag),
        }
    }
    /// Compare with a double exactly (NaN → None).
    pub fn cmp_f64(&self, x: f64) -> Option<Ordering> {
        if x.is_nan() {
            return None;
        }
        if x == f64::INFINITY {
            return Some(Ordering::Less);
        }
        if x == f64::NEG_INFINITY {
            return Some(Ordering::Greater);
        }
        let f = x.floor();
        let b = BigInt::from_f64(f).unwrap_or_default();
        Some(match self.cmp(&b) {
            Ordering::Equal if x > f => Ordering::Less,
            o => o,
        })
    }

    /// Two's complement limbs of `n` limbs (n large enough).
    fn twos(&self, n: usize) -> Vec<u32> {
        let mut v = self.mag.clone();
        v.resize(n, 0);
        if self.neg {
            let mut carry = 1u64;
            for l in v.iter_mut() {
                let t = (!*l) as u64 + carry;
                *l = t as u32;
                carry = t >> 32;
            }
        }
        v
    }
    fn from_twos(mut v: Vec<u32>) -> BigInt {
        let neg = v.last().is_some_and(|&t| t & 0x8000_0000 != 0);
        if neg {
            let mut carry = 1u64;
            for l in v.iter_mut() {
                let t = (!*l) as u64 + carry;
                *l = t as u32;
                carry = t >> 32;
            }
        }
        BigInt::make(neg, v)
    }
    pub fn bitop(&self, o: &BigInt, op: fn(u32, u32) -> u32) -> BigInt {
        let n = self.mag.len().max(o.mag.len()) + 1;
        let a = self.twos(n);
        let b = o.twos(n);
        BigInt::from_twos(a.iter().zip(&b).map(|(x, y)| op(*x, *y)).collect())
    }
    pub fn not(&self) -> BigInt {
        // ~x = -x - 1
        self.neg().sub(&BigInt::from_i64(1))
    }
    /// BigInt.asUintN (§21.2.2.2): x mod 2^bits.
    pub fn as_uint_n(&self, bits: u64) -> BigInt {
        if bits == 0 {
            return BigInt::zero();
        }
        let n = (bits as usize).div_ceil(32) + 1;
        let n = n.max(self.mag.len() + 1);
        let mut v = self.twos(n);
        let keep = bits as usize;
        for (i, l) in v.iter_mut().enumerate() {
            let lo = i * 32;
            if lo >= keep {
                *l = 0;
            } else if lo + 32 > keep {
                *l &= (1u32 << (keep - lo)) - 1;
            }
        }
        BigInt::make(false, v)
    }
    /// BigInt.asIntN (§21.2.2.1).
    pub fn as_int_n(&self, bits: u64) -> BigInt {
        if bits == 0 {
            return BigInt::zero();
        }
        let u = self.as_uint_n(bits);
        if u.bit_len() == bits {
            // Top bit set: subtract 2^bits.
            u.sub(&BigInt::from_i64(1).shl(bits))
        } else {
            u
        }
    }
    /// The low 64 bits as two's complement (BigInt64/BigUint64 storage).
    pub fn to_u64_wrapping(&self) -> u64 {
        let v = self.twos(self.mag.len().max(2) + 1);
        v[0] as u64 | (v[1] as u64) << 32
    }

    pub fn to_string_radix(&self, radix: u32) -> String {
        if self.is_zero() {
            return "0".into();
        }
        let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
        // Divide by the largest power of the radix in a u32 for speed.
        let mut chunk = radix;
        let mut per = 1;
        while (chunk as u64) * (radix as u64) <= u32::MAX as u64 {
            chunk *= radix;
            per += 1;
        }
        let mut mag = self.mag.clone();
        let mut parts: Vec<u32> = Vec::new();
        while !mag.is_empty() {
            let (q, r) = divrem_small(&mag, chunk);
            parts.push(r);
            mag = q;
        }
        let mut s = String::new();
        if self.neg {
            s.push('-');
        }
        for (i, p) in parts.iter().rev().enumerate() {
            let mut buf = Vec::new();
            let mut x = *p;
            for _ in 0..per {
                buf.push(digits[(x % radix) as usize]);
                x /= radix;
                if x == 0 && i == 0 {
                    break;
                }
            }
            buf.reverse();
            s.push_str(std::str::from_utf8(&buf).unwrap_or(""));
        }
        s
    }

    /// Digits in `radix` (no sign, no prefix); `None` if a character isn't a digit.
    pub fn parse_radix(s: &str, radix: u32) -> Option<BigInt> {
        if s.is_empty() {
            return None;
        }
        let mut mag: Vec<u32> = Vec::new();
        for ch in s.chars() {
            let d = ch.to_digit(radix)?;
            // mag = mag * radix + d
            let mut carry = d as u64;
            for l in mag.iter_mut() {
                let t = *l as u64 * radix as u64 + carry;
                *l = t as u32;
                carry = t >> 32;
            }
            if carry > 0 {
                mag.push(carry as u32);
            }
        }
        Some(BigInt::make(false, mag))
    }

    /// StringToBigInt (§7.1.14).
    pub fn parse_js(s: &[u16]) -> Option<BigInt> {
        let t = super::value::trim_js(s);
        let st = String::from_utf16(t).ok()?;
        if st.is_empty() {
            return Some(BigInt::zero());
        }
        let b = st.as_bytes();
        if b.len() > 2 && b[0] == b'0' {
            let radix = match b[1] {
                b'x' | b'X' => 16,
                b'o' | b'O' => 8,
                b'b' | b'B' => 2,
                _ => 0,
            };
            if radix != 0 {
                return BigInt::parse_radix(&st[2..], radix);
            }
        }
        let (neg, body) = match b[0] {
            b'-' => (true, &st[1..]),
            b'+' => (false, &st[1..]),
            _ => (false, &st[..]),
        };
        let v = BigInt::parse_radix(body, 10)?;
        Some(if neg { v.neg() } else { v })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> BigInt {
        BigInt::parse_js(&s.encode_utf16().collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn arithmetic_round_trips() {
        let x = b("123456789012345678901234567890");
        let y = b("-987654321098765432109876543210");
        assert_eq!(x.add(&y).to_string_radix(10), "-864197532086419753208641975320");
        assert_eq!(
            x.mul(&y).to_string_radix(10),
            "-121932631137021795226185032733622923332237463801111263526900"
        );
        let (q, r) = y.divrem(&x).unwrap();
        assert_eq!(q.to_string_radix(10), "-8");
        assert_eq!(r.to_string_radix(10), "-9000000000900000000090");
        assert_eq!(b("255").to_string_radix(16), "ff");
        assert_eq!(b("0x10").to_string_radix(10), "16");
        assert!(BigInt::parse_js(&"-0x10".encode_utf16().collect::<Vec<_>>()).is_none());
        assert!(BigInt::parse_js(&"1.5".encode_utf16().collect::<Vec<_>>()).is_none());
        assert_eq!(b("2").pow(100).to_string_radix(10), "1267650600228229401496703205376");
        let big = b("2").pow(200).add(&b("12345"));
        let (q2, r2) = big.divrem(&b("2").pow(70).add(&b("7"))).unwrap();
        assert_eq!(q2.mul(&b("2").pow(70).add(&b("7"))).add(&r2), big);
    }

    #[test]
    fn shifts_and_bits() {
        assert_eq!(b("-5").shr(1).to_string_radix(10), "-3");
        assert_eq!(b("-1").shl(65).to_string_radix(10), "-36893488147419103232");
        assert_eq!(b("-6").bitop(&b("3"), |x, y| x & y).to_string_radix(10), "2");
        assert_eq!(b("-6").bitop(&b("3"), |x, y| x | y).to_string_radix(10), "-5");
        assert_eq!(b("5").not().to_string_radix(10), "-6");
        assert_eq!(b("-1").as_uint_n(64).to_string_radix(10), "18446744073709551615");
        assert_eq!(b("18446744073709551615").as_int_n(64).to_string_radix(10), "-1");
        assert_eq!(b("255").as_int_n(8).to_string_radix(10), "-1");
    }

    #[test]
    fn number_conversion_rounds_to_even() {
        assert_eq!(b("9007199254740993").to_f64(), 9007199254740992.0);
        assert_eq!(b("9007199254740995").to_f64(), 9007199254740996.0);
        assert_eq!(BigInt::from_f64(1e20).unwrap().to_string_radix(10), "100000000000000000000");
        assert_eq!(b("12").cmp_f64(12.5), Some(Ordering::Less));
        assert_eq!(b("-12").cmp_f64(-12.5), Some(Ordering::Greater));
    }
}

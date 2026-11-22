//! Type conversion (ECMA-262 §7.1), testing and comparison (§7.2) and the operators (§13).

use super::code::Op;
use super::heap::*;
use super::value::{js_str, number_to_string, string_to_number, JsString, PropKey, Value};
use super::{JsResult, Vm};

pub fn nullish_name(v: &Value) -> &'static str {
    if matches!(v, Value::Null) {
        "null"
    } else {
        "undefined"
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    Default,
    Number,
    String,
}

impl Vm {
    pub fn is_callable(&self, v: &Value) -> bool {
        matches!(v, Value::Object(o) if self.heap.obj(*o).is_callable())
    }

    /// IsConstructor (§7.2.4).
    pub fn is_constructor(&self, v: &Value) -> bool {
        let Value::Object(o) = v else { return false };
        match &self.heap.obj(*o).kind {
            Kind::Function(c) => c.constructor,
            Kind::Native(n) => n.constructor,
            Kind::Bound(b) => self.is_constructor(&Value::Object(b.target)),
            Kind::Proxy(p) => p.constructor,
            _ => false,
        }
    }

    /// `typeof` (§13.5.3).
    pub fn type_of(&self, v: &Value) -> JsString {
        js_str(match v {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Symbol(_) => "symbol",
            Value::Object(o) => {
                if self.heap.obj(*o).is_callable() {
                    "function"
                } else {
                    "object"
                }
            }
        })
    }

    /// ToPrimitive (§7.1.1).
    pub fn to_primitive(&mut self, v: &Value, hint: Hint) -> JsResult<Value> {
        let Value::Object(o) = v else { return Ok(v.clone()) };
        let tp = PropKey::Sym(self.realm.sym_to_primitive.clone());
        if let Some(ex) = self.get_method(v, &tp)? {
            let h = match hint {
                Hint::Default => "default",
                Hint::Number => "number",
                Hint::String => "string",
            };
            let r = self.call(&ex, v.clone(), vec![Value::str(h)])?;
            if matches!(r, Value::Object(_)) {
                return self.type_error("Cannot convert object to primitive value");
            }
            return Ok(r);
        }
        self.ordinary_to_primitive(*o, if hint == Hint::String { Hint::String } else { Hint::Number })
    }

    /// OrdinaryToPrimitive (§7.1.1.1).
    pub fn ordinary_to_primitive(&mut self, o: ObjRef, hint: Hint) -> JsResult<Value> {
        let order = if hint == Hint::String { ["toString", "valueOf"] } else { ["valueOf", "toString"] };
        for name in order {
            let f = self.get(o, &PropKey::named(name))?;
            if self.is_callable(&f) {
                let r = self.call(&f, Value::Object(o), Vec::new())?;
                if !matches!(r, Value::Object(_)) {
                    return Ok(r);
                }
            }
        }
        self.type_error("Cannot convert object to primitive value")
    }

    /// ToNumber (§7.1.4).
    pub fn to_number(&mut self, v: &Value) -> JsResult<f64> {
        Ok(match v {
            Value::Undefined => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => *b as u8 as f64,
            Value::Number(n) => *n,
            Value::String(s) => string_to_number(s),
            Value::Symbol(_) => return self.type_error("Cannot convert a Symbol value to a number"),
            Value::Object(_) => {
                let p = self.to_primitive(v, Hint::Number)?;
                return self.to_number(&p);
            }
        })
    }

    /// ToIntegerOrInfinity (§7.1.5).
    pub fn to_integer_or_infinity(&mut self, v: &Value) -> JsResult<f64> {
        let n = self.to_number(v)?;
        Ok(if n.is_nan() {
            0.0
        } else if n.is_infinite() {
            n
        } else {
            n.trunc() + 0.0
        })
    }

    pub fn to_int32(&mut self, v: &Value) -> JsResult<i32> {
        let n = self.to_number(v)?;
        Ok(f64_to_int32(n))
    }

    pub fn to_uint32(&mut self, v: &Value) -> JsResult<u32> {
        let n = self.to_number(v)?;
        Ok(f64_to_int32(n) as u32)
    }

    pub fn to_uint16(&mut self, v: &Value) -> JsResult<u16> {
        let n = self.to_number(v)?;
        Ok(f64_to_int32(n) as u32 as u16)
    }

    /// ToLength (§7.1.20).
    pub fn to_length(&mut self, v: &Value) -> JsResult<f64> {
        let n = self.to_integer_or_infinity(v)?;
        Ok(n.clamp(0.0, 9007199254740991.0))
    }

    /// ToIndex (§7.1.22).
    pub fn to_index(&mut self, v: &Value) -> JsResult<f64> {
        let n = self.to_integer_or_infinity(v)?;
        if !(0.0..=9007199254740991.0).contains(&n) {
            return self.range_error("Invalid index");
        }
        Ok(n)
    }

    /// ToString (§7.1.17).
    pub fn to_string(&mut self, v: &Value) -> JsResult<JsString> {
        Ok(match v {
            Value::Undefined => js_str("undefined"),
            Value::Null => js_str("null"),
            Value::Bool(b) => js_str(if *b { "true" } else { "false" }),
            Value::Number(n) => js_str(&number_to_string(*n)),
            Value::String(s) => s.clone(),
            Value::Symbol(_) => return self.type_error("Cannot convert a Symbol value to a string"),
            Value::Object(_) => {
                let p = self.to_primitive(v, Hint::String)?;
                return self.to_string(&p);
            }
        })
    }

    /// ToPropertyKey (§7.1.19).
    pub fn to_property_key(&mut self, v: &Value) -> JsResult<PropKey> {
        match v {
            Value::String(s) => Ok(PropKey::from_js(s.clone())),
            Value::Symbol(s) => Ok(PropKey::Sym(s.clone())),
            Value::Number(n) => Ok(PropKey::from_f64(*n)),
            Value::Object(_) => {
                let p = self.to_primitive(v, Hint::String)?;
                self.to_property_key(&p)
            }
            _ => Ok(PropKey::from_js(self.to_string(v)?)),
        }
    }

    /// ToObject (§7.1.18).
    pub fn to_object(&mut self, v: &Value) -> JsResult<ObjRef> {
        let (proto, kind) = match v {
            Value::Object(o) => return Ok(*o),
            Value::Undefined | Value::Null => {
                return self.type_error(&format!("Cannot convert {} to object", nullish_name(v)));
            }
            Value::Bool(b) => (self.realm.boolean_proto, Kind::Boolean(*b)),
            Value::Number(n) => (self.realm.number_proto, Kind::Number(*n)),
            Value::String(s) => (self.realm.string_proto, Kind::String(s.clone())),
            Value::Symbol(s) => (self.realm.symbol_proto, Kind::Symbol(s.clone())),
        };
        let o = self.heap.alloc(Object::new(Some(proto), kind));
        if let Value::String(s) = v {
            self.heap.obj_mut(o).props.insert(
                PropKey::named("length"),
                Prop::data(Value::Number(s.len() as f64), false, false, false),
            );
        }
        Ok(o)
    }

    /// The `+` operator (§13.15.3 ApplyStringOrNumericBinaryOperator).
    pub fn add(&mut self, a: &Value, b: &Value) -> JsResult<Value> {
        if let (Value::Number(x), Value::Number(y)) = (a, b) {
            return Ok(Value::Number(x + y));
        }
        let pa = self.to_primitive(a, Hint::Default)?;
        let pb = self.to_primitive(b, Hint::Default)?;
        if matches!(pa, Value::String(_)) || matches!(pb, Value::String(_)) {
            let sa = self.to_string(&pa)?;
            let sb = self.to_string(&pb)?;
            if sa.len() + sb.len() > super::value::MAX_STRING_LEN {
                return self.range_error("Invalid string length");
            }
            let mut v = Vec::with_capacity(sa.len() + sb.len());
            v.extend_from_slice(&sa);
            v.extend_from_slice(&sb);
            return Ok(Value::String(v.into()));
        }
        let x = self.to_number(&pa)?;
        let y = self.to_number(&pb)?;
        Ok(Value::Number(x + y))
    }

    pub(super) fn arith(&mut self, op: Op, a: &Value, b: &Value) -> JsResult<Value> {
        let x = self.to_number(a)?;
        let y = self.to_number(b)?;
        Ok(Value::Number(match op {
            Op::Sub => x - y,
            Op::Mul => x * y,
            Op::Div => x / y,
            Op::Mod => js_rem(x, y),
            Op::Exp => js_pow(x, y),
            Op::Shl => (f64_to_int32(x).wrapping_shl(f64_to_int32(y) as u32 & 31)) as f64,
            Op::Shr => (f64_to_int32(x) >> (f64_to_int32(y) as u32 & 31)) as f64,
            Op::UShr => ((f64_to_int32(x) as u32) >> (f64_to_int32(y) as u32 & 31)) as f64,
            Op::BitAnd => (f64_to_int32(x) & f64_to_int32(y)) as f64,
            Op::BitOr => (f64_to_int32(x) | f64_to_int32(y)) as f64,
            Op::BitXor => (f64_to_int32(x) ^ f64_to_int32(y)) as f64,
            _ => f64::NAN,
        }))
    }

    /// Relational operators via IsLessThan (§7.2.13).
    pub(super) fn compare(&mut self, op: Op, a: &Value, b: &Value) -> JsResult<bool> {
        let (pa, pb) = if matches!(op, Op::Lt | Op::Le) {
            let pa = self.to_primitive(a, Hint::Number)?;
            let pb = self.to_primitive(b, Hint::Number)?;
            (pa, pb)
        } else {
            // `a > b` is `b < a` with left-first evaluation of a.
            let pa = self.to_primitive(a, Hint::Number)?;
            let pb = self.to_primitive(b, Hint::Number)?;
            (pa, pb)
        };
        let lt = |vm: &mut Vm, x: &Value, y: &Value| -> JsResult<Option<bool>> {
            if let (Value::String(s), Value::String(t)) = (x, y) {
                return Ok(Some(s[..] < t[..]));
            }
            let nx = vm.to_number(x)?;
            let ny = vm.to_number(y)?;
            if nx.is_nan() || ny.is_nan() {
                return Ok(None);
            }
            Ok(Some(nx < ny))
        };
        Ok(match op {
            Op::Lt => lt(self, &pa, &pb)? == Some(true),
            Op::Gt => lt(self, &pb, &pa)? == Some(true),
            Op::Le => lt(self, &pb, &pa)? == Some(false),
            Op::Ge => lt(self, &pa, &pb)? == Some(false),
            _ => false,
        })
    }

    /// IsLooselyEqual (§7.2.14).
    pub fn loosely_equal(&mut self, a: &Value, b: &Value) -> JsResult<bool> {
        use Value::*;
        Ok(match (a, b) {
            (Undefined | Null, Undefined | Null) => true,
            (Undefined | Null, _) | (_, Undefined | Null) => false,
            (Number(_), String(s)) => {
                let n = string_to_number(s);
                return self.loosely_equal(a, &Number(n));
            }
            (String(s), Number(_)) => {
                let n = string_to_number(s);
                return self.loosely_equal(&Number(n), b);
            }
            (Bool(x), _) => return self.loosely_equal(&Number(*x as u8 as f64), b),
            (_, Bool(y)) => return self.loosely_equal(a, &Number(*y as u8 as f64)),
            (Object(_), Number(_) | String(_) | Symbol(_)) => {
                let p = self.to_primitive(a, Hint::Default)?;
                return self.loosely_equal(&p, b);
            }
            (Number(_) | String(_) | Symbol(_), Object(_)) => {
                let p = self.to_primitive(b, Hint::Default)?;
                return self.loosely_equal(a, &p);
            }
            _ => super::value::strict_equals(a, b),
        })
    }

    /// InstanceofOperator (§13.10.2).
    pub fn instance_of(&mut self, v: &Value, target: &Value) -> JsResult<bool> {
        if !matches!(target, Value::Object(_)) {
            return self.type_error("Right-hand side of 'instanceof' is not an object");
        }
        let hi = PropKey::Sym(self.realm.sym_has_instance.clone());
        if let Some(h) = self.get_method(target, &hi)? {
            let r = self.call(&h, target.clone(), vec![v.clone()])?;
            return Ok(r.truthy());
        }
        if !self.is_callable(target) {
            return self.type_error("Right-hand side of 'instanceof' is not callable");
        }
        self.ordinary_has_instance(target, v)
    }

    /// OrdinaryHasInstance (§7.3.21).
    pub fn ordinary_has_instance(&mut self, c: &Value, o: &Value) -> JsResult<bool> {
        let Value::Object(co) = c else { return Ok(false) };
        if !self.is_callable(c) {
            return Ok(false);
        }
        if let Kind::Bound(b) = &self.heap.obj(*co).kind {
            let t = Value::Object(b.target);
            return self.instance_of(o, &t);
        }
        let Value::Object(mut obj) = o else { return Ok(false) };
        let p = self.get(*co, &PropKey::named("prototype"))?;
        let Value::Object(p) = p else {
            return self.type_error("Function has non-object prototype in instanceof check");
        };
        loop {
            match self.get_prototype_of(obj)? {
                None => return Ok(false),
                Some(x) if x == p => return Ok(true),
                Some(x) => obj = x,
            }
        }
    }
}

/// ToInt32's modular conversion (§7.1.6).
pub fn f64_to_int32(n: f64) -> i32 {
    if !n.is_finite() || n == 0.0 {
        return 0;
    }
    let t = n.trunc();
    let m = t.rem_euclid(4294967296.0);
    (m as u64 as u32) as i32
}

/// Number::remainder (§6.1.6.1.6): the sign of the dividend.
pub fn js_rem(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() || x.is_infinite() || y == 0.0 {
        return f64::NAN;
    }
    if y.is_infinite() || x == 0.0 {
        return x;
    }
    x % y
}

/// Number::exponentiate (§6.1.6.1.3): differs from IEEE pow for ±1 ** ±∞ and NaN exponents.
pub fn js_pow(x: f64, y: f64) -> f64 {
    if y.is_nan() {
        return f64::NAN;
    }
    if y == 0.0 {
        return 1.0;
    }
    if (x == 1.0 || x == -1.0) && y.is_infinite() {
        return f64::NAN;
    }
    x.powf(y)
}

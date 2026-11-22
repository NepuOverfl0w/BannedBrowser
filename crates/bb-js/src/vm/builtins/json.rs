//! The JSON object (§25.5): `parse` with revivers, `stringify` with replacers and indentation.

use super::super::heap::*;
use super::super::realm::{self, method};
use super::super::value::{js_str, number_to_string, JsString, PropKey, Value};
use super::super::{JsResult, Vm};
use super::object::is_array;

/// Deeper nesting is a SyntaxError (parse) or RangeError (stringify) instead of a crash.
const MAX_DEPTH: usize = 4000;

struct Parser<'a> {
    s: &'a [u16],
    i: usize,
    depth: usize,
    /// Build JSON Parse Records (for a reviver's `context.source`).
    track: bool,
}

/// JSON Parse Record (json-parse-with-source): the parsed value, a primitive's source
/// text, and the records of an array's elements or an object's entries.
#[derive(Default)]
struct Rec {
    value: Value,
    source: Option<JsString>,
    elements: Vec<Rec>,
    entries: Vec<(PropKey, Rec)>,
}

impl Rec {
    fn values(&self, out: &mut Vec<Value>) {
        out.push(self.value.clone());
        for e in &self.elements {
            e.values(out);
        }
        for (_, e) in &self.entries {
            e.values(out);
        }
    }
}

impl Parser<'_> {
    fn ws(&mut self) {
        while let Some(&c) = self.s.get(self.i) {
            if matches!(c, 0x20 | 0x09 | 0x0A | 0x0D) {
                self.i += 1;
            } else {
                break;
            }
        }
    }
    fn peek(&self) -> Option<u16> {
        self.s.get(self.i).copied()
    }
    fn lit(&mut self, w: &str) -> bool {
        let w: Vec<u16> = w.encode_utf16().collect();
        if self.s[self.i..].starts_with(&w) {
            self.i += w.len();
            true
        } else {
            false
        }
    }
    fn string(&mut self) -> Result<JsString, String> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek() else { return Err("Unterminated string in JSON".into()) };
            self.i += 1;
            match c {
                0x22 => return Ok(out.into()),
                0x5C => {
                    let Some(e) = self.peek() else { return Err("Bad escape in JSON".into()) };
                    self.i += 1;
                    match e {
                        0x22 | 0x5C | 0x2F => out.push(e),
                        0x62 => out.push(8),
                        0x66 => out.push(12),
                        0x6E => out.push(10),
                        0x72 => out.push(13),
                        0x74 => out.push(9),
                        0x75 => {
                            let mut v = 0u16;
                            for _ in 0..4 {
                                let d = self
                                    .peek()
                                    .and_then(|h| char::from_u32(h as u32))
                                    .and_then(|h| h.to_digit(16))
                                    .ok_or("Bad Unicode escape in JSON")?;
                                v = v * 16 + d as u16;
                                self.i += 1;
                            }
                            out.push(v);
                        }
                        _ => return Err("Bad escaped character in JSON".into()),
                    }
                }
                c if c < 0x20 => return Err("Bad control character in string literal in JSON".into()),
                c => out.push(c),
            }
        }
    }
    fn number(&mut self) -> Result<f64, String> {
        let start = self.i;
        if self.peek() == Some(b'-' as u16) {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let s = p.i;
            while p.peek().is_some_and(|c| (b'0' as u16..=b'9' as u16).contains(&c)) {
                p.i += 1;
            }
            p.i - s
        };
        if self.peek() == Some(b'0' as u16) {
            self.i += 1;
        } else if digits(self) == 0 {
            return Err("No number after minus sign in JSON".into());
        }
        if self.peek() == Some(b'.' as u16) {
            self.i += 1;
            if digits(self) == 0 {
                return Err("Unterminated fractional number in JSON".into());
            }
        }
        if matches!(self.peek(), Some(0x65) | Some(0x45)) {
            self.i += 1;
            if matches!(self.peek(), Some(0x2B) | Some(0x2D)) {
                self.i += 1;
            }
            if digits(self) == 0 {
                return Err("Exponent part is missing a number in JSON".into());
            }
        }
        let t: String = self.s[start..self.i].iter().map(|&c| c as u8 as char).collect();
        t.parse().map_err(|_| "Bad number in JSON".to_string())
    }
    fn value(&mut self, vm: &mut Vm) -> JsResult<Value> {
        Ok(self.value_rec(vm)?.value)
    }

    fn value_rec(&mut self, vm: &mut Vm) -> JsResult<Rec> {
        self.ws();
        let start = self.i;
        let mut rec = Rec::default();
        let v = self.value_inner(vm, &mut rec)?;
        if self.track && !matches!(v, Value::Object(_)) {
            rec.source = Some(self.s[start..self.i].into());
        }
        rec.value = v;
        Ok(rec)
    }

    fn value_inner(&mut self, vm: &mut Vm, rec: &mut Rec) -> JsResult<Value> {
        let err = |vm: &mut Vm, m: &str| vm.syntax_error::<Value>(m);
        match self.peek() {
            None => err(vm, "Unexpected end of JSON input"),
            Some(0x7B) => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return err(vm, "JSON nested too deeply");
                }
                self.i += 1;
                let o = vm.new_object();
                self.ws();
                if self.peek() == Some(0x7D) {
                    self.i += 1;
                    self.depth -= 1;
                    return Ok(Value::Object(o));
                }
                loop {
                    self.ws();
                    if self.peek() != Some(0x22) {
                        return err(vm, "Expected property name in JSON");
                    }
                    let k = match self.string() {
                        Ok(k) => k,
                        Err(m) => return err(vm, &m),
                    };
                    self.ws();
                    if self.peek() != Some(0x3A) {
                        return err(vm, "Expected ':' after property name in JSON");
                    }
                    self.i += 1;
                    let r = self.value_rec(vm)?;
                    let key = PropKey::from_js(k);
                    vm.create_data_property(o, key.clone(), r.value.clone())?;
                    if self.track {
                        // A duplicate key's last value wins.
                        rec.entries.retain(|(x, _)| *x != key);
                        rec.entries.push((key, r));
                    }
                    self.ws();
                    match self.peek() {
                        Some(0x2C) => self.i += 1,
                        Some(0x7D) => {
                            self.i += 1;
                            self.depth -= 1;
                            return Ok(Value::Object(o));
                        }
                        _ => return err(vm, "Expected ',' or '}' after property value in JSON"),
                    }
                }
            }
            Some(0x5B) => {
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return err(vm, "JSON nested too deeply");
                }
                self.i += 1;
                let mut vals = Vec::new();
                self.ws();
                if self.peek() == Some(0x5D) {
                    self.i += 1;
                    self.depth -= 1;
                    return Ok(Value::Object(vm.new_array(vals)));
                }
                loop {
                    let r = self.value_rec(vm)?;
                    vals.push(r.value.clone());
                    if self.track {
                        rec.elements.push(r);
                    }
                    self.ws();
                    match self.peek() {
                        Some(0x2C) => self.i += 1,
                        Some(0x5D) => {
                            self.i += 1;
                            self.depth -= 1;
                            return Ok(Value::Object(vm.new_array(vals)));
                        }
                        _ => return err(vm, "Expected ',' or ']' after array element in JSON"),
                    }
                }
            }
            Some(0x22) => match self.string() {
                Ok(s) => Ok(Value::String(s)),
                Err(m) => err(vm, &m),
            },
            Some(c) if c == b'-' as u16 || (b'0' as u16..=b'9' as u16).contains(&c) => match self.number() {
                Ok(n) => Ok(Value::Number(n)),
                Err(m) => err(vm, &m),
            },
            _ => {
                if self.lit("true") {
                    Ok(Value::Bool(true))
                } else if self.lit("false") {
                    Ok(Value::Bool(false))
                } else if self.lit("null") {
                    Ok(Value::Null)
                } else {
                    err(vm, "Unexpected token in JSON")
                }
            }
        }
    }
}

/// InternalizeJSONProperty (§25.5.1.1, with json-parse-with-source's parse records).
fn internalize(
    vm: &mut Vm,
    holder: ObjRef,
    name: PropKey,
    reviver: &Value,
    rec: Option<&Rec>,
    depth: usize,
) -> JsResult<Value> {
    if depth > MAX_DEPTH {
        return vm.range_error("JSON nested too deeply");
    }
    let val = vm.get(holder, &name)?;
    let context = vm.new_object();
    let rec = rec.filter(|r| super::super::value::same_value(&r.value, &val));
    if let Some(src) = rec.and_then(|r| r.source.clone()) {
        vm.create_data_property_or_throw(context, PropKey::lit("source"), Value::String(src))?;
    }
    if let Value::Object(o) = val {
        let arr = is_array(vm, &val)?;
        let keys: Vec<PropKey> = if arr {
            let len = vm.length_of_array_like(o)?;
            (0..len as u64).map(|i| PropKey::from_f64(i as f64)).collect()
        } else {
            let mut ks = Vec::new();
            for k in vm.own_property_keys(o)? {
                if matches!(k, PropKey::Sym(_)) {
                    continue;
                }
                if vm.get_own_property(o, &k)?.is_some_and(|d| d.enumerable == Some(true)) {
                    ks.push(k);
                }
            }
            ks
        };
        for (i, k) in keys.into_iter().enumerate() {
            let sub = rec.and_then(|r| {
                if arr {
                    r.elements.get(i)
                } else {
                    r.entries.iter().find(|(x, _)| *x == k).map(|(_, e)| e)
                }
            });
            let nv = internalize(vm, o, k.clone(), reviver, sub, depth + 1)?;
            if nv.is_undefined() {
                vm.delete(o, &k)?;
            } else {
                vm.create_data_property(o, k, nv)?;
            }
        }
    }
    vm.call(reviver, Value::Object(holder), vec![name.to_value(), val, Value::Object(context)])
}

fn parse(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let text = vm.to_string(&c.arg(0))?;
    let reviver = c.arg(1);
    let track = vm.is_callable(&reviver);
    let mut p = Parser { s: &text, i: 0, depth: 0, track };
    let rec = p.value_rec(vm)?;
    p.ws();
    if p.i != text.len() {
        return vm.syntax_error("Unexpected non-whitespace character after JSON");
    }
    if track {
        // The records' values stay alive while the reviver runs: identity is compared.
        let base = vm.roots.len();
        rec.values(&mut vm.roots);
        let root = vm.new_object();
        vm.roots.push(Value::Object(root));
        let r = vm
            .create_data_property_or_throw(root, PropKey::lit(""), rec.value.clone())
            .and_then(|_| internalize(vm, root, PropKey::lit(""), &reviver, Some(&rec), 0));
        vm.roots.truncate(base);
        return r;
    }
    Ok(rec.value)
}

struct Ser {
    replacer: Option<Value>,
    keys: Option<Vec<PropKey>>,
    gap: Vec<u16>,
    indent: Vec<u16>,
    stack: Vec<ObjRef>,
}

/// QuoteJSONString (§25.5.2.3).
fn quote(s: &[u16], out: &mut Vec<u16>) {
    out.push(b'"' as u16);
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        let esc: Option<&str> = match c {
            0x08 => Some("\\b"),
            0x09 => Some("\\t"),
            0x0A => Some("\\n"),
            0x0C => Some("\\f"),
            0x0D => Some("\\r"),
            0x22 => Some("\\\""),
            0x5C => Some("\\\\"),
            _ => None,
        };
        if let Some(e) = esc {
            out.extend(e.encode_utf16());
        } else if c < 0x20 {
            out.extend(format!("\\u{c:04x}").encode_utf16());
        } else if (0xD800..0xDC00).contains(&c) && s.get(i + 1).is_some_and(|d| (0xDC00..0xE000).contains(d))
        {
            out.push(c);
            out.push(s[i + 1]);
            i += 1;
        } else if (0xD800..0xE000).contains(&c) {
            out.extend(format!("\\u{c:04x}").encode_utf16());
        } else {
            out.push(c);
        }
        i += 1;
    }
    out.push(b'"' as u16);
}

impl Ser {
    /// SerializeJSONProperty (§25.5.2.2): `None` = undefined (skipped).
    fn property(&mut self, vm: &mut Vm, key: PropKey, holder: ObjRef) -> JsResult<Option<Vec<u16>>> {
        let mut v = vm.get(holder, &key)?;
        if matches!(v, Value::Object(_) | Value::BigInt(_)) {
            let tj = vm.get_value_key(&v, &PropKey::lit("toJSON"))?;
            if vm.is_callable(&tj) {
                v = vm.call(&tj, v, vec![key.to_value()])?;
            }
        }
        if let Some(r) = &self.replacer {
            let r = r.clone();
            v = vm.call(&r, Value::Object(holder), vec![key.to_value(), v])?;
        }
        if let Value::Object(o) = v {
            if matches!(vm.heap.obj(o).kind, Kind::RawJSON) {
                let raw = vm.get(o, &PropKey::lit("rawJSON"))?;
                return Ok(Some(vm.to_string(&raw)?.to_vec()));
            }
            v = match vm.heap.obj(o).kind {
                Kind::BigInt(ref b) => Value::BigInt(b.clone()),
                Kind::Number(_) => Value::Number(vm.to_number(&v)?),
                Kind::String(_) => Value::String(vm.to_string(&v)?),
                Kind::Boolean(b) => Value::Bool(b),
                _ => v,
            };
        }
        Ok(match &v {
            Value::Null => Some("null".encode_utf16().collect()),
            Value::Bool(b) => Some(if *b { "true" } else { "false" }.encode_utf16().collect()),
            Value::String(s) => {
                let mut out = Vec::with_capacity(s.len() + 2);
                quote(s, &mut out);
                Some(out)
            }
            Value::BigInt(_) => return vm.type_error("Do not know how to serialize a BigInt"),
            Value::Number(n) => Some(
                if n.is_finite() { number_to_string(*n) } else { "null".into() }.encode_utf16().collect(),
            ),
            Value::Object(o) if !vm.is_callable(&v) => {
                let o = *o;
                if self.stack.contains(&o) {
                    return vm.type_error("Converting circular structure to JSON");
                }
                if self.stack.len() > MAX_DEPTH {
                    return vm.range_error("JSON nested too deeply");
                }
                self.stack.push(o);
                let stepback = self.indent.clone();
                self.indent.extend_from_slice(&self.gap.clone());
                let r = if is_array(vm, &v)? { self.array(vm, o) } else { self.object(vm, o) };
                self.indent = stepback;
                self.stack.pop();
                Some(r?)
            }
            _ => None,
        })
    }

    fn wrap(&self, open: char, close: char, parts: Vec<Vec<u16>>, stepback_len: usize) -> Vec<u16> {
        let mut out: Vec<u16> = vec![open as u16];
        if parts.is_empty() {
            out.push(close as u16);
            return out;
        }
        if self.gap.is_empty() {
            for (i, p) in parts.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',' as u16);
                }
                out.extend(p);
            }
        } else {
            for (i, p) in parts.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',' as u16);
                }
                out.push(b'\n' as u16);
                out.extend_from_slice(&self.indent);
                out.extend(p);
            }
            out.push(b'\n' as u16);
            out.extend_from_slice(&self.indent[..stepback_len]);
        }
        out.push(close as u16);
        out
    }

    fn object(&mut self, vm: &mut Vm, o: ObjRef) -> JsResult<Vec<u16>> {
        let stepback = self.indent.len() - self.gap.len();
        let keys = match &self.keys {
            Some(k) => k.clone(),
            None => {
                let mut ks = Vec::new();
                for k in vm.own_property_keys(o)? {
                    if matches!(k, PropKey::Sym(_)) {
                        continue;
                    }
                    if vm.get_own_property(o, &k)?.is_some_and(|d| d.enumerable == Some(true)) {
                        ks.push(k);
                    }
                }
                ks
            }
        };
        let mut parts = Vec::new();
        for k in keys {
            if let Some(s) = self.property(vm, k.clone(), o)? {
                let mut m = Vec::new();
                let name = match k.to_value() {
                    Value::String(s) => s,
                    _ => js_str(""),
                };
                quote(&name, &mut m);
                m.push(b':' as u16);
                if !self.gap.is_empty() {
                    m.push(b' ' as u16);
                }
                m.extend(s);
                parts.push(m);
            }
        }
        Ok(self.wrap('{', '}', parts, stepback))
    }

    fn array(&mut self, vm: &mut Vm, o: ObjRef) -> JsResult<Vec<u16>> {
        let stepback = self.indent.len() - self.gap.len();
        let len = vm.length_of_array_like(o)?;
        let mut parts = Vec::new();
        let mut i = 0.0;
        while i < len {
            parts.push(
                self.property(vm, PropKey::from_f64(i), o)?
                    .unwrap_or_else(|| "null".encode_utf16().collect()),
            );
            i += 1.0;
        }
        Ok(self.wrap('[', ']', parts, stepback))
    }
}

fn stringify(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let mut ser = Ser { replacer: None, keys: None, gap: Vec::new(), indent: Vec::new(), stack: Vec::new() };
    let r = c.arg(1);
    if vm.is_callable(&r) {
        ser.replacer = Some(r);
    } else if is_array(vm, &r)? {
        let ro = r.as_object().expect("array");
        let len = vm.length_of_array_like(ro)?;
        let mut keys: Vec<PropKey> = Vec::new();
        let mut i = 0.0;
        while i < len {
            let v = vm.get(ro, &PropKey::from_f64(i))?;
            let item = match &v {
                Value::String(s) => Some(s.clone()),
                Value::Number(_) => Some(vm.to_string(&v)?),
                Value::Object(o) if matches!(vm.heap.obj(*o).kind, Kind::String(_) | Kind::Number(_)) => {
                    Some(vm.to_string(&v)?)
                }
                _ => None,
            };
            if let Some(s) = item {
                let k = PropKey::from_js(s);
                if !keys.contains(&k) {
                    keys.push(k);
                }
            }
            i += 1.0;
        }
        ser.keys = Some(keys);
    }
    let mut space = c.arg(2);
    if let Value::Object(o) = space {
        space = match vm.heap.obj(o).kind {
            Kind::Number(_) => Value::Number(vm.to_number(&space)?),
            Kind::String(_) => Value::String(vm.to_string(&space)?),
            _ => space,
        };
    }
    ser.gap = match &space {
        Value::Number(_) => {
            let n = vm.to_integer_or_infinity(&space)?.clamp(0.0, 10.0) as usize;
            vec![b' ' as u16; n]
        }
        Value::String(s) => s[..s.len().min(10)].to_vec(),
        _ => Vec::new(),
    };
    let wrapper = vm.new_object();
    vm.create_data_property_or_throw(wrapper, PropKey::lit(""), c.arg(0))?;
    Ok(match ser.property(vm, PropKey::lit(""), wrapper)? {
        Some(s) => Value::String(s.into()),
        None => Value::Undefined,
    })
}

pub(super) fn install(vm: &mut Vm) {
    let j = vm.new_object();
    let g = vm.realm.global;
    realm::value(vm, g, "JSON", Value::Object(j));
    realm::to_string_tag(vm, j, "JSON");
    method(vm, j, "parse", 2, parse);
    method(vm, j, "stringify", 3, stringify);
    method(vm, j, "rawJSON", 1, raw_json);
    method(vm, j, "isRawJSON", 1, |vm, c| {
        Ok(Value::Bool(matches!(c.arg(0), Value::Object(o) if matches!(vm.heap.obj(o).kind, Kind::RawJSON))))
    });
}

/// JSON.rawJSON (json-parse-with-source).
fn raw_json(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = vm.to_string(&c.arg(0))?;
    let bad_end = |u: Option<&u16>| u.is_none_or(|u| matches!(u, 0x09 | 0x0A | 0x0D | 0x20));
    if bad_end(s.first()) || bad_end(s.last()) {
        return vm.syntax_error("JSON.rawJSON: the text is empty or starts or ends with whitespace");
    }
    let mut p = Parser { s: &s, i: 0, depth: 0, track: false };
    if matches!(p.peek(), Some(0x7B | 0x5B)) {
        return vm.syntax_error("JSON.rawJSON: the text must be a primitive JSON value");
    }
    p.value(vm)?;
    p.ws();
    if p.i != s.len() {
        return vm.syntax_error("Unexpected non-whitespace character after JSON");
    }
    let o = vm.heap.alloc(Object::new(None, Kind::RawJSON));
    let obj = vm.heap.obj_mut(o);
    obj.props.insert(PropKey::lit("rawJSON"), Prop::data(Value::String(s), false, true, false));
    obj.extensible = false;
    Ok(Value::Object(o))
}

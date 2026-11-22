//! The built-in objects (ECMA-262 §19–§28).

mod array;
mod collections;
mod date;
pub(super) mod iterator;
mod json;
mod number;
mod object;
mod selfhost;
pub(super) use selfhost::MARK as SELF_HOSTED;
pub(super) mod regexp;
mod typed;
pub(super) use object::{from_property_descriptor, to_property_descriptor};
mod string;

use super::heap::*;
use super::realm::{self, accessor, constant, method, method_sym, value};
use super::value::{js_str, to_rust, JsString, PropKey, Sym, Value};
use super::{ErrorKind, JsResult, Vm};

pub(super) fn empty_fn(_: &mut Vm, _: NativeCall) -> JsResult<Value> {
    Ok(Value::Undefined)
}

pub(super) fn throw_type_error(vm: &mut Vm, _: NativeCall) -> JsResult<Value> {
    vm.type_error("'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions or the arguments objects for calls to them")
}

pub(super) fn install(vm: &mut Vm) {
    object::install(vm);
    install_errors(vm);
    array::install(vm);
    string::install(vm);
    number::install(vm);
    install_boolean(vm);
    install_symbol(vm);
    install_iterators(vm);
    super::generator::install(vm);
    super::async_gen::install(vm);
    object::install_dynamic_kinds(vm);
    install_reflect(vm);
    super::proxy::install(vm);
    super::promise::install(vm);
    install_aggregate_error(vm);
    super::dispose::install(vm);
    regexp::install(vm);
    collections::install(vm);
    json::install(vm);
    date::install(vm);
    iterator::install(vm);
    typed::install(vm);
    install_global(vm);
    selfhost::install(vm);
}

/// A string argument of a native, converted.
pub(super) fn arg_string(vm: &mut Vm, c: &NativeCall, i: usize) -> JsResult<JsString> {
    let v = c.arg(i);
    vm.to_string(&v)
}

// ---- global object (§19) ----

fn install_global(vm: &mut Vm) {
    let g = vm.realm.global;
    constant(vm, g, "NaN", Value::Number(f64::NAN));
    constant(vm, g, "Infinity", Value::Number(f64::INFINITY));
    constant(vm, g, "undefined", Value::Undefined);
    value(vm, g, "globalThis", Value::Object(g));
    let e = method(vm, g, "eval", 1, |vm, c| vm.indirect_eval(c.arg(0)));
    vm.realm.eval = e;
    method(vm, g, "isNaN", 1, |vm, c| Ok(Value::Bool(vm.to_number(&c.arg(0))?.is_nan())));
    method(vm, g, "isFinite", 1, |vm, c| Ok(Value::Bool(vm.to_number(&c.arg(0))?.is_finite())));
    let pf = method(vm, g, "parseFloat", 1, number::parse_float);
    let pi = method(vm, g, "parseInt", 2, number::parse_int);
    let nc = vm.get(g, &PropKey::lit("Number")).ok().and_then(|v| v.as_object());
    if let Some(n) = nc {
        value(vm, n, "parseFloat", Value::Object(pf));
        value(vm, n, "parseInt", Value::Object(pi));
    }
    method(vm, g, "print", 1, |vm, c| {
        let mut parts = Vec::new();
        for a in &c.args {
            parts.push(to_rust(&vm.to_string(a)?));
        }
        vm.output.push(parts.join(" "));
        Ok(Value::Undefined)
    });
    method(vm, g, "encodeURIComponent", 1, |vm, c| uri_encode(vm, &c, ";/?:@&=+$,#"));
    method(vm, g, "encodeURI", 1, |vm, c| uri_encode(vm, &c, ""));
    method(vm, g, "decodeURIComponent", 1, |vm, c| uri_decode(vm, &c, ""));
    method(vm, g, "decodeURI", 1, |vm, c| uri_decode(vm, &c, ";/?:@&=+$,#"));
}

/// Encode (§19.2.6.5): `extra_unescaped` is what encodeURI keeps besides the unreserved set.
fn uri_encode(vm: &mut Vm, c: &NativeCall, reserved_kept_by_uri: &str) -> JsResult<Value> {
    let s = arg_string(vm, c, 0)?;
    // encodeURIComponent passes the reserved set as the characters it *escapes*;
    // encodeURI keeps them. Unreserved marks always stay.
    let keep_reserved = reserved_kept_by_uri.is_empty();
    let unreserved = |ch: u16| {
        let c = ch as u8 as char;
        ch < 128 && (c.is_ascii_alphanumeric() || "-_.!~*'()".contains(c))
    };
    let reserved = |ch: u16| ch < 128 && ";/?:@&=+$,#".contains(ch as u8 as char);
    let mut out = String::new();
    let mut i = 0;
    while i < s.len() {
        let ch = s[i];
        if unreserved(ch) || (keep_reserved && reserved(ch)) {
            out.push(ch as u8 as char);
            i += 1;
            continue;
        }
        let cp = if (0xD800..0xDC00).contains(&ch) {
            match s.get(i + 1) {
                Some(&lo) if (0xDC00..0xE000).contains(&lo) => {
                    i += 1;
                    0x10000 + ((ch as u32 - 0xD800) << 10) + (lo as u32 - 0xDC00)
                }
                _ => return Err(vm.error(ErrorKind::Uri, "URI malformed")),
            }
        } else if (0xDC00..0xE000).contains(&ch) {
            return Err(vm.error(ErrorKind::Uri, "URI malformed"));
        } else {
            ch as u32
        };
        let mut buf = [0u8; 4];
        let enc = char::from_u32(cp).map(|c| c.encode_utf8(&mut buf).len()).unwrap_or(0);
        for b in &buf[..enc] {
            out.push_str(&format!("%{b:02X}"));
        }
        i += 1;
    }
    Ok(Value::str(&out))
}

/// Decode (§19.2.6.6): escapes of characters in `keep` stay escaped.
fn uri_decode(vm: &mut Vm, c: &NativeCall, keep: &str) -> JsResult<Value> {
    let s = arg_string(vm, c, 0)?;
    let mut out: Vec<u16> = Vec::with_capacity(s.len());
    let hex = |s: &[u16], i: usize| -> Option<u8> {
        let h = char::from_u32(*s.get(i + 1)? as u32)?.to_digit(16)?;
        let l = char::from_u32(*s.get(i + 2)? as u32)?.to_digit(16)?;
        Some((h * 16 + l) as u8)
    };
    let mut i = 0;
    while i < s.len() {
        if s[i] != b'%' as u16 {
            out.push(s[i]);
            i += 1;
            continue;
        }
        let Some(b) = hex(&s, i) else { return Err(vm.error(ErrorKind::Uri, "URI malformed")) };
        if b < 0x80 {
            if keep.contains(b as char) {
                out.extend_from_slice(&s[i..i + 3]);
            } else {
                out.push(b as u16);
            }
            i += 3;
            continue;
        }
        let n = if b & 0xE0 == 0xC0 {
            2
        } else if b & 0xF0 == 0xE0 {
            3
        } else if b & 0xF8 == 0xF0 {
            4
        } else {
            return Err(vm.error(ErrorKind::Uri, "URI malformed"));
        };
        let mut bytes = vec![b];
        let mut j = i + 3;
        for _ in 1..n {
            if s.get(j) != Some(&(b'%' as u16)) {
                return Err(vm.error(ErrorKind::Uri, "URI malformed"));
            }
            let Some(x) = hex(&s, j) else { return Err(vm.error(ErrorKind::Uri, "URI malformed")) };
            bytes.push(x);
            j += 3;
        }
        match std::str::from_utf8(&bytes) {
            Ok(st) => out.extend(st.encode_utf16()),
            Err(_) => return Err(vm.error(ErrorKind::Uri, "URI malformed")),
        }
        i = j;
    }
    Ok(Value::String(out.into()))
}

// ---- Error objects (§20.5) ----

const ERROR_NAMES: [(&str, ErrorKind); 7] = [
    ("Error", ErrorKind::Error),
    ("TypeError", ErrorKind::Type),
    ("RangeError", ErrorKind::Range),
    ("ReferenceError", ErrorKind::Reference),
    ("SyntaxError", ErrorKind::Syntax),
    ("EvalError", ErrorKind::Eval),
    ("URIError", ErrorKind::Uri),
];

fn error_ctor(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let kind = match &vm.heap.obj(c.callee).kind {
        Kind::Native(n) => match n.slots.first() {
            Some(Value::Number(k)) => *k as usize,
            _ => 0,
        },
        _ => 0,
    };
    let nt = c.new_target.unwrap_or(c.callee);
    let ek = ERROR_NAMES[kind].1;
    let proto = vm.get(nt, &PropKey::lit("prototype"))?;
    let proto = match proto {
        Value::Object(p) => p,
        _ => vm.realm.error_proto(ek),
    };
    let o = vm.heap.alloc(Object::new(Some(proto), Kind::Error));
    let msg = c.arg(0);
    if !msg.is_undefined() {
        let m = vm.to_string(&msg)?;
        vm.heap.obj_mut(o).props.insert(PropKey::lit("message"), Prop::method(Value::String(m)));
    }
    // InstallErrorCause (§20.5.8.1).
    if let Value::Object(opts) = c.arg(1) {
        let ck = PropKey::lit("cause");
        if vm.has_property(opts, &ck)? {
            let cause = vm.get(opts, &ck)?;
            vm.heap.obj_mut(o).props.insert(ck, Prop::method(cause));
        }
    }
    Ok(Value::Object(o))
}

fn install_errors(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let mut base_ctor = None;
    for (i, (name, kind)) in ERROR_NAMES.iter().enumerate() {
        let proto_parent = if i == 0 { op } else { vm.realm.error_proto(ErrorKind::Error) };
        let proto = vm.new_object_with_proto(Some(proto_parent));
        vm.realm.error_protos[*kind as usize] = proto;
        let c = realm::native_with(vm, name, 1, error_ctor, true, vec![Value::Number(i as f64)]);
        if let Some(b) = base_ctor {
            vm.heap.obj_mut(c).proto = Some(b);
        }
        vm.heap
            .obj_mut(c)
            .props
            .insert(PropKey::lit("prototype"), Prop::data(Value::Object(proto), false, false, false));
        value(vm, proto, "constructor", Value::Object(c));
        value(vm, proto, "name", Value::str(name));
        value(vm, proto, "message", Value::str(""));
        let g = vm.realm.global;
        value(vm, g, name, Value::Object(c));
        if i == 0 {
            base_ctor = Some(c);
            method(vm, proto, "toString", 0, |vm, c| {
                let Value::Object(o) = c.this else {
                    return vm.type_error("Error.prototype.toString called on non-object");
                };
                let n = vm.get(o, &PropKey::lit("name"))?;
                let n = if n.is_undefined() { js_str("Error") } else { vm.to_string(&n)? };
                let m = vm.get(o, &PropKey::lit("message"))?;
                let m = if m.is_undefined() { js_str("") } else { vm.to_string(&m)? };
                if n.is_empty() {
                    return Ok(Value::String(m));
                }
                if m.is_empty() {
                    return Ok(Value::String(n));
                }
                let mut v = n.to_vec();
                v.extend(": ".encode_utf16());
                v.extend_from_slice(&m);
                Ok(Value::String(v.into()))
            });
        }
    }
}

// ---- Boolean (§20.3) ----

fn this_boolean(vm: &mut Vm, v: &Value) -> JsResult<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Object(o) => match vm.heap.obj(*o).kind {
            Kind::Boolean(b) => Ok(b),
            _ => vm.type_error("not a Boolean"),
        },
        _ => vm.type_error("not a Boolean"),
    }
}

fn install_boolean(vm: &mut Vm) {
    let bp = vm.realm.boolean_proto;
    realm::constructor(
        vm,
        "Boolean",
        1,
        |vm, c| {
            let b = c.arg(0).truthy();
            let Some(nt) = c.new_target else { return Ok(Value::Bool(b)) };
            let proto = vm.prototype_from_ctor(nt, |r| r.boolean_proto)?;
            Ok(Value::Object(vm.heap.alloc(Object::new(Some(proto), Kind::Boolean(b)))))
        },
        bp,
    );
    method(vm, bp, "toString", 0, |vm, c| {
        let b = this_boolean(vm, &c.this)?;
        Ok(Value::str(if b { "true" } else { "false" }))
    });
    method(vm, bp, "valueOf", 0, |vm, c| Ok(Value::Bool(this_boolean(vm, &c.this)?)));
}

// ---- Symbol (§20.4) ----

fn this_symbol(vm: &mut Vm, v: &Value) -> JsResult<Sym> {
    match v {
        Value::Symbol(s) => Ok(s.clone()),
        Value::Object(o) => match &vm.heap.obj(*o).kind {
            Kind::Symbol(s) => Ok(s.clone()),
            _ => vm.type_error("not a Symbol"),
        },
        _ => vm.type_error("not a Symbol"),
    }
}

fn symbol_descriptive_string(s: &Sym) -> JsString {
    let mut v: Vec<u16> = "Symbol(".encode_utf16().collect();
    if let Some(d) = &s.0.description {
        v.extend_from_slice(d);
    }
    v.push(b')' as u16);
    v.into()
}

fn install_symbol(vm: &mut Vm) {
    let sp = vm.realm.symbol_proto;
    let ctor = realm::constructor(
        vm,
        "Symbol",
        0,
        |vm, c| {
            if c.new_target.is_some() {
                return vm.type_error("Symbol is not a constructor");
            }
            let d = c.arg(0);
            let desc = if d.is_undefined() { None } else { Some(vm.to_string(&d)?) };
            Ok(Value::Symbol(Sym::new(desc)))
        },
        sp,
    );
    for (name, s) in vm.realm.well_known() {
        constant(vm, ctor, name, Value::Symbol(s));
    }
    method(vm, ctor, "for", 1, |vm, c| {
        let k = arg_string(vm, &c, 0)?;
        if let Some(s) = vm.registry.get(&k[..]) {
            return Ok(Value::Symbol(s.clone()));
        }
        let s = Sym(std::rc::Rc::new(super::value::SymbolData {
            description: Some(k.clone()),
            registered: true,
            private: false,
        }));
        vm.registry.insert(k.to_vec(), s.clone());
        Ok(Value::Symbol(s))
    });
    method(vm, ctor, "keyFor", 1, |vm, c| {
        let Value::Symbol(s) = c.arg(0) else { return vm.type_error("not a symbol") };
        Ok(if s.0.registered {
            s.0.description.clone().map(Value::String).unwrap_or_default()
        } else {
            Value::Undefined
        })
    });
    method(vm, sp, "toString", 0, |vm, c| {
        let s = this_symbol(vm, &c.this)?;
        Ok(Value::String(symbol_descriptive_string(&s)))
    });
    method(vm, sp, "valueOf", 0, |vm, c| Ok(Value::Symbol(this_symbol(vm, &c.this)?)));
    accessor(
        vm,
        sp,
        PropKey::lit("description"),
        Some(|vm, c| {
            let s = this_symbol(vm, &c.this)?;
            Ok(s.0.description.clone().map(Value::String).unwrap_or_default())
        }),
        None,
    );
    let tp = vm.realm.sym_to_primitive.clone();
    let f = method_sym(vm, sp, tp.clone(), "[Symbol.toPrimitive]", 1, |vm, c| {
        Ok(Value::Symbol(this_symbol(vm, &c.this)?))
    });
    if let Some(p) = vm.heap.obj_mut(sp).props.get_mut(&PropKey::Sym(tp)) {
        p.writable = false;
    }
    let _ = f;
    realm::to_string_tag(vm, sp, "Symbol");
}

// ---- iterators (§27.1) ----

fn install_iterators(vm: &mut Vm) {
    let ip = vm.realm.iterator_proto;
    let si = vm.realm.sym_iterator.clone();
    method_sym(vm, ip, si, "[Symbol.iterator]", 0, |_, c| Ok(c.this));
    let aip = vm.new_object_with_proto(Some(ip));
    vm.realm.array_iterator_proto = aip;
    method(vm, aip, "next", 0, array_iterator_next);
    realm::to_string_tag(vm, aip, "Array Iterator");
    let sip = vm.new_object_with_proto(Some(ip));
    vm.realm.string_iterator_proto = sip;
    method(vm, sip, "next", 0, |vm, c| {
        let Value::Object(o) = c.this else { return vm.type_error("not a String Iterator") };
        let Kind::StringIter { s, pos, done } = &mut vm.heap.obj_mut(o).kind else {
            return vm.type_error("not a String Iterator");
        };
        if *done || *pos >= s.len() {
            *done = true;
            return Ok(vm.iter_result(Value::Undefined, true));
        }
        let a = s[*pos];
        let n = if (0xD800..0xDC00).contains(&a)
            && s.get(*pos + 1).is_some_and(|b| (0xDC00..0xE000).contains(b))
        {
            2
        } else {
            1
        };
        let piece: JsString = s[*pos..*pos + n].into();
        *pos += n;
        Ok(vm.iter_result(Value::String(piece), false))
    });
    realm::to_string_tag(vm, sip, "String Iterator");
}

/// %ArrayIteratorPrototype%.next (§23.1.5.2.1).
fn array_iterator_next(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let Value::Object(o) = c.this else { return vm.type_error("not an Array Iterator") };
    let Kind::ArrayIter { target, index, kind, .. } = &vm.heap.obj(o).kind else {
        return vm.type_error("not an Array Iterator");
    };
    let (Some(t), i, k) = (*target, *index, *kind) else {
        return Ok(vm.iter_result(Value::Undefined, true));
    };
    let len = if vm.ta(t).is_some() {
        match vm.ta_length(t) {
            Some(l) => l as f64,
            None => return vm.type_error("typed array is detached or out of bounds"),
        }
    } else {
        vm.length_of_array_like(t)?
    };
    if i as f64 >= len {
        if let Kind::ArrayIter { target, .. } = &mut vm.heap.obj_mut(o).kind {
            *target = None;
        }
        return Ok(vm.iter_result(Value::Undefined, true));
    }
    if let Kind::ArrayIter { index, .. } = &mut vm.heap.obj_mut(o).kind {
        *index = i + 1;
    }
    let v = match k {
        IterKind::Keys => Value::Number(i as f64),
        IterKind::Values => vm.get(t, &PropKey::from_u32(i))?,
        IterKind::Entries => {
            let e = vm.get(t, &PropKey::from_u32(i))?;
            Value::Object(vm.new_array(vec![Value::Number(i as f64), e]))
        }
    };
    Ok(vm.iter_result(v, false))
}

pub(super) fn create_array_iterator(vm: &mut Vm, target: ObjRef, kind: IterKind) -> Value {
    let p = vm.realm.array_iterator_proto;
    Value::Object(
        vm.heap.alloc(Object::new(
            Some(p),
            Kind::ArrayIter { target: Some(target), index: 0, kind, typed: false },
        )),
    )
}

// ---- Reflect (§28.1) ----

fn object_arg(vm: &mut Vm, v: &Value) -> JsResult<ObjRef> {
    match v {
        Value::Object(o) => Ok(*o),
        _ => vm.type_error("argument is not an object"),
    }
}

fn install_reflect(vm: &mut Vm) {
    let r = vm.new_object();
    let g = vm.realm.global;
    value(vm, g, "Reflect", Value::Object(r));
    realm::to_string_tag(vm, r, "Reflect");
    method(vm, r, "apply", 3, |vm, c| {
        let f = c.arg(0);
        if !vm.is_callable(&f) {
            return vm.type_error("Reflect.apply target is not callable");
        }
        let args = vm.list_from_array_like(&c.arg(2))?;
        vm.call(&f, c.arg(1), args)
    });
    method(vm, r, "construct", 2, |vm, c| {
        let f = c.arg(0);
        if !vm.is_constructor(&f) {
            return vm.type_error("Reflect.construct target is not a constructor");
        }
        let nt = if c.args.len() > 2 { c.arg(2) } else { f.clone() };
        if !vm.is_constructor(&nt) {
            return vm.type_error("Reflect.construct newTarget is not a constructor");
        }
        let args = vm.list_from_array_like(&c.arg(1))?;
        vm.construct(f.as_object().expect("object"), args, nt.as_object())
    });
    method(vm, r, "defineProperty", 3, |vm, c| {
        let o = object_arg(vm, &c.arg(0))?;
        let k = vm.to_property_key(&c.arg(1))?;
        let d = object::to_property_descriptor(vm, &c.arg(2))?;
        Ok(Value::Bool(vm.define_own_property(o, k, d)?))
    });
    method(vm, r, "deleteProperty", 2, |vm, c| {
        let o = object_arg(vm, &c.arg(0))?;
        let k = vm.to_property_key(&c.arg(1))?;
        Ok(Value::Bool(vm.delete(o, &k)?))
    });
    method(vm, r, "get", 2, |vm, c| {
        let o = object_arg(vm, &c.arg(0))?;
        let k = vm.to_property_key(&c.arg(1))?;
        let recv = if c.args.len() > 2 { c.arg(2) } else { Value::Object(o) };
        vm.get_with_receiver(o, &k, recv)
    });
    method(vm, r, "getOwnPropertyDescriptor", 2, |vm, c| {
        let o = object_arg(vm, &c.arg(0))?;
        let k = vm.to_property_key(&c.arg(1))?;
        let d = vm.get_own_property(o, &k)?;
        Ok(object::from_property_descriptor(vm, d))
    });
    method(vm, r, "getPrototypeOf", 1, |vm, c| {
        let o = object_arg(vm, &c.arg(0))?;
        Ok(vm.get_prototype_of(o)?.map(Value::Object).unwrap_or(Value::Null))
    });
    method(vm, r, "has", 2, |vm, c| {
        let o = object_arg(vm, &c.arg(0))?;
        let k = vm.to_property_key(&c.arg(1))?;
        Ok(Value::Bool(vm.has_property(o, &k)?))
    });
    method(vm, r, "isExtensible", 1, |vm, c| {
        let o = object_arg(vm, &c.arg(0))?;
        Ok(Value::Bool(vm.is_extensible(o)?))
    });
    method(vm, r, "ownKeys", 1, |vm, c| {
        let o = object_arg(vm, &c.arg(0))?;
        let keys: Vec<Value> = vm.own_property_keys(o)?.iter().map(|k| k.to_value()).collect();
        Ok(Value::Object(vm.new_array(keys)))
    });
    method(vm, r, "preventExtensions", 1, |vm, c| {
        let o = object_arg(vm, &c.arg(0))?;
        Ok(Value::Bool(vm.prevent_extensions(o)?))
    });
    method(vm, r, "set", 3, |vm, c| {
        let o = object_arg(vm, &c.arg(0))?;
        let k = vm.to_property_key(&c.arg(1))?;
        let recv = if c.args.len() > 3 { c.arg(3) } else { Value::Object(o) };
        Ok(Value::Bool(vm.set(o, k, c.arg(2), recv)?))
    });
    method(vm, r, "setPrototypeOf", 2, |vm, c| {
        let o = object_arg(vm, &c.arg(0))?;
        let p = match c.arg(1) {
            Value::Object(p) => Some(p),
            Value::Null => None,
            _ => return vm.type_error("Object prototype may only be an Object or null"),
        };
        Ok(Value::Bool(vm.set_prototype_of(o, p)?))
    });
}

/// AggregateError (§20.5.7).
fn install_aggregate_error(vm: &mut Vm) {
    let ep = vm.realm.error_proto(ErrorKind::Error);
    let proto = vm.new_object_with_proto(Some(ep));
    vm.realm.aggregate_error_proto = proto;
    let c = realm::constructor(
        vm,
        "AggregateError",
        2,
        |vm, c| {
            let nt = c.new_target.unwrap_or(c.callee);
            let proto = vm.prototype_from_ctor(nt, |r| r.aggregate_error_proto)?;
            let o = vm.heap.alloc(Object::new(Some(proto), Kind::Error));
            let msg = c.arg(1);
            if !msg.is_undefined() {
                let m = vm.to_string(&msg)?;
                vm.heap.obj_mut(o).props.insert(PropKey::lit("message"), Prop::method(Value::String(m)));
            }
            if let Value::Object(opts) = c.arg(2) {
                let ck = PropKey::lit("cause");
                if vm.has_property(opts, &ck)? {
                    let cause = vm.get(opts, &ck)?;
                    vm.heap.obj_mut(o).props.insert(ck, Prop::method(cause));
                }
            }
            let errors = vm.iterate_to_vec(&c.arg(0))?;
            let arr = vm.new_array(errors);
            vm.heap.obj_mut(o).props.insert(PropKey::lit("errors"), Prop::method(Value::Object(arr)));
            Ok(Value::Object(o))
        },
        proto,
    );
    let g = vm.realm.global;
    let ec = vm.get(g, &PropKey::lit("Error")).ok().and_then(|v| v.as_object());
    vm.heap.obj_mut(c).proto = ec;
    value(vm, proto, "name", Value::str("AggregateError"));
    value(vm, proto, "message", Value::str(""));
}

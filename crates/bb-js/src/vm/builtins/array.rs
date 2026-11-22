//! Array (§23.1).

use super::super::heap::*;
use super::super::realm::{self, accessor, method, method_sym};
use super::super::value::{js_str, same_value_zero, strict_equals, JsString, PropKey, Value};
use super::super::{JsResult, Vm};
use super::create_array_iterator;
use super::object::is_array;

fn key(i: f64) -> PropKey {
    PropKey::from_f64(i)
}

/// ArrayCreate (§10.4.2.2).
pub(super) fn array_create(vm: &mut Vm, len: f64, proto: Option<ObjRef>) -> JsResult<ObjRef> {
    if len > 4294967295.0 {
        return vm.range_error("Invalid array length");
    }
    let a = vm.new_array(Vec::new());
    vm.set_array_length_raw(a, len as u32);
    if let Some(p) = proto {
        vm.heap.obj_mut(a).proto = Some(p);
    }
    Ok(a)
}

/// ArraySpeciesCreate (§10.4.2.3).
fn species_create(vm: &mut Vm, original: ObjRef, len: f64) -> JsResult<ObjRef> {
    if !is_array(vm, &Value::Object(original))? {
        return array_create(vm, len, None);
    }
    let mut c = vm.get(original, &PropKey::lit("constructor"))?;
    // Another realm's %Array% counts as no species (step 4.c).
    if let Value::Object(co) = c {
        if vm.is_constructor(&c) {
            let rid = vm.function_realm(co)?;
            if rid != vm.cur_realm && vm.realm_by_id(rid).array_ctor == co {
                c = Value::Undefined;
            }
        }
    }
    if let Value::Object(co) = c {
        let sk = PropKey::Sym(vm.realm.sym_species.clone());
        c = vm.get(co, &sk)?;
        if matches!(c, Value::Null) {
            c = Value::Undefined;
        }
    }
    if c.is_undefined() {
        return array_create(vm, len, None);
    }
    if !vm.is_constructor(&c) {
        return vm.type_error("object.constructor[Symbol.species] is not a constructor");
    }
    let r = vm.construct(c.as_object().expect("object"), vec![Value::Number(len)], None)?;
    match r {
        Value::Object(o) => Ok(o),
        _ => vm.type_error("species constructor returned a non-object"),
    }
}

fn this_obj(vm: &mut Vm, c: &NativeCall) -> JsResult<(ObjRef, f64)> {
    let o = vm.to_object(&c.this)?;
    let len = vm.length_of_array_like(o)?;
    Ok((o, len))
}

fn callback(vm: &mut Vm, c: &NativeCall) -> JsResult<Value> {
    let f = c.arg(0);
    if !vm.is_callable(&f) {
        return vm.type_error(&format!("{} is not a function", vm.describe_short(&f)));
    }
    Ok(f)
}

/// Relative index argument (`slice`, `at`, `fill`…): clamp into [0, len].
fn relative(vm: &mut Vm, v: &Value, len: f64, default: f64) -> JsResult<f64> {
    if v.is_undefined() {
        return Ok(default);
    }
    let r = vm.to_integer_or_infinity(v)?;
    Ok(if r < 0.0 { (len + r).max(0.0) } else { r.min(len) })
}

fn set_length(vm: &mut Vm, o: ObjRef, n: f64) -> JsResult<()> {
    vm.set_or_throw(o, PropKey::lit("length"), Value::Number(n))
}

/// The default comparison of `sort` (§23.1.3.30.2 SortCompare without comparefn).
fn sort_compare(vm: &mut Vm, cmp: &Value, x: &Value, y: &Value) -> JsResult<std::cmp::Ordering> {
    use std::cmp::Ordering::*;
    match (x.is_undefined(), y.is_undefined()) {
        (true, true) => return Ok(Equal),
        (true, false) => return Ok(Greater),
        (false, true) => return Ok(Less),
        _ => {}
    }
    if !cmp.is_undefined() {
        let r = vm.call(cmp, Value::Undefined, vec![x.clone(), y.clone()])?;
        let n = vm.to_number(&r)?;
        return Ok(if n < 0.0 {
            Less
        } else if n > 0.0 {
            Greater
        } else {
            Equal
        });
    }
    let xs = vm.to_string(x)?;
    let ys = vm.to_string(y)?;
    Ok(xs[..].cmp(&ys[..]))
}

/// A stable merge sort whose comparator may throw.
pub(super) fn sort_values(vm: &mut Vm, v: Vec<Value>, cmp: &Value) -> JsResult<Vec<Value>> {
    if v.len() <= 1 {
        return Ok(v);
    }
    let mid = v.len() / 2;
    let mut left = v;
    let right = left.split_off(mid);
    let l = sort_values(vm, left, cmp)?;
    let r = sort_values(vm, right, cmp)?;
    let mut out = Vec::with_capacity(l.len() + r.len());
    let (mut i, mut j) = (0, 0);
    while i < l.len() && j < r.len() {
        if sort_compare(vm, cmp, &r[j], &l[i])? == std::cmp::Ordering::Less {
            out.push(r[j].clone());
            j += 1;
        } else {
            out.push(l[i].clone());
            i += 1;
        }
    }
    out.extend_from_slice(&l[i..]);
    out.extend_from_slice(&r[j..]);
    Ok(out)
}

/// SortIndexedProperties (§23.1.3.30.1) with holes skipped.
fn collect_present(vm: &mut Vm, o: ObjRef, len: f64, skip_holes: bool) -> JsResult<Vec<Value>> {
    let mut out = Vec::new();
    let mut i = 0.0;
    while i < len {
        let k = key(i);
        if !skip_holes || vm.has_property(o, &k)? {
            out.push(vm.get(o, &k)?);
        }
        i += 1.0;
    }
    Ok(out)
}

fn comparefn(vm: &mut Vm, c: &NativeCall) -> JsResult<Value> {
    let f = c.arg(0);
    if !f.is_undefined() && !vm.is_callable(&f) {
        return vm.type_error("The comparison function must be either a function or undefined");
    }
    Ok(f)
}

/// FlattenIntoArray (§23.1.3.13.1).
fn flatten(
    vm: &mut Vm,
    target: ObjRef,
    src: ObjRef,
    len: f64,
    mut start: f64,
    depth: f64,
    mapper: Option<(&Value, &Value)>,
) -> JsResult<f64> {
    let mut i = 0.0;
    while i < len {
        let k = key(i);
        if vm.has_property(src, &k)? {
            let mut el = vm.get(src, &k)?;
            if let Some((f, this)) = mapper {
                el = vm.call(f, this.clone(), vec![el, Value::Number(i), Value::Object(src)])?;
            }
            if depth > 0.0 && is_array(vm, &el)? {
                let eo = el.as_object().expect("array");
                let el_len = vm.length_of_array_like(eo)?;
                start = flatten(vm, target, eo, el_len, start, depth - 1.0, None)?;
            } else {
                if start >= 9007199254740991.0 {
                    return vm.type_error("array too long");
                }
                vm.create_data_property_or_throw(target, key(start), el)?;
                start += 1.0;
            }
        }
        i += 1.0;
    }
    Ok(start)
}

pub(super) fn install(vm: &mut Vm) {
    let ap = vm.realm.array_proto;
    let ctor = realm::constructor(
        vm,
        "Array",
        1,
        |vm, c| {
            let nt = c.new_target.unwrap_or(c.callee);
            let proto = vm.prototype_from_ctor(nt, |r| r.array_proto)?;
            match c.args.len() {
                0 => Ok(Value::Object(array_create(vm, 0.0, Some(proto))?)),
                1 => {
                    let l = c.arg(0);
                    if let Value::Number(n) = l {
                        let u = super::super::ops::f64_to_int32(n) as u32;
                        if u as f64 != n {
                            return vm.range_error("Invalid array length");
                        }
                        Ok(Value::Object(array_create(vm, n, Some(proto))?))
                    } else {
                        let a = vm.new_array(vec![l]);
                        vm.heap.obj_mut(a).proto = Some(proto);
                        Ok(Value::Object(a))
                    }
                }
                _ => {
                    let a = vm.new_array(c.args);
                    vm.heap.obj_mut(a).proto = Some(proto);
                    Ok(Value::Object(a))
                }
            }
        },
        ap,
    );
    vm.realm.array_ctor = ctor;
    let sp = vm.realm.sym_species.clone();
    accessor(vm, ctor, PropKey::Sym(sp), Some(|_, c| Ok(c.this)), None);
    method(vm, ctor, "isArray", 1, |vm, c| Ok(Value::Bool(is_array(vm, &c.arg(0))?)));
    method(vm, ctor, "of", 0, |vm, c| {
        let n = c.args.len() as f64;
        let a = if vm.is_constructor(&c.this) {
            match vm.construct(c.this.as_object().expect("ctor"), vec![Value::Number(n)], None)? {
                Value::Object(o) => o,
                _ => return vm.type_error("not an object"),
            }
        } else {
            array_create(vm, n, None)?
        };
        for (i, v) in c.args.into_iter().enumerate() {
            vm.create_data_property_or_throw(a, key(i as f64), v)?;
        }
        set_length(vm, a, n)?;
        Ok(Value::Object(a))
    });
    method(vm, ctor, "from", 1, |vm, c| {
        let items = c.arg(0);
        let mapfn = c.arg(1);
        let this_arg = c.arg(2);
        if !mapfn.is_undefined() && !vm.is_callable(&mapfn) {
            return vm.type_error("Array.from: mapper is not a function");
        }
        let ik = PropKey::Sym(vm.realm.sym_iterator.clone());
        let using = vm.get_method(&items, &ik)?;
        let make = |vm: &mut Vm, len: Option<f64>| -> JsResult<ObjRef> {
            if vm.is_constructor(&c.this) {
                let args = len.map(|n| vec![Value::Number(n)]).unwrap_or_default();
                match vm.construct(c.this.as_object().expect("ctor"), args, None)? {
                    Value::Object(o) => Ok(o),
                    _ => vm.type_error("not an object"),
                }
            } else {
                array_create(vm, len.unwrap_or(0.0), None)
            }
        };
        if let Some(m) = using {
            let a = make(vm, None)?;
            let r = vm.get_iterator_from_method(&items, &m)?;
            let mut k = 0.0;
            while let Some(v) = vm.iter_step(r)? {
                let step = (|| -> JsResult<()> {
                    let v = if mapfn.is_undefined() {
                        v
                    } else {
                        vm.call(&mapfn, this_arg.clone(), vec![v, Value::Number(k)])?
                    };
                    vm.create_data_property_or_throw(a, key(k), v)
                })();
                if let Err(e) = step {
                    let _ = vm.iterator_close(r);
                    return Err(e);
                }
                k += 1.0;
            }
            set_length(vm, a, k)?;
            return Ok(Value::Object(a));
        }
        let src = vm.to_object(&items)?;
        let len = vm.length_of_array_like(src)?;
        let a = make(vm, Some(len))?;
        let mut k = 0.0;
        while k < len {
            let v = vm.get(src, &key(k))?;
            let v = if mapfn.is_undefined() {
                v
            } else {
                vm.call(&mapfn, this_arg.clone(), vec![v, Value::Number(k)])?
            };
            vm.create_data_property_or_throw(a, key(k), v)?;
            k += 1.0;
        }
        set_length(vm, a, len)?;
        Ok(Value::Object(a))
    });

    method(vm, ap, "push", 1, |vm, c| {
        let (o, mut len) = this_obj(vm, &c)?;
        if len + c.args.len() as f64 > 9007199254740991.0 {
            return vm.type_error("Pushing elements past 2^53-1");
        }
        for v in c.args {
            vm.set_or_throw(o, key(len), v)?;
            len += 1.0;
        }
        set_length(vm, o, len)?;
        Ok(Value::Number(len))
    });
    method(vm, ap, "pop", 0, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        if len == 0.0 {
            set_length(vm, o, 0.0)?;
            return Ok(Value::Undefined);
        }
        let k = key(len - 1.0);
        let v = vm.get(o, &k)?;
        vm.delete_or_throw(o, &k)?;
        set_length(vm, o, len - 1.0)?;
        Ok(v)
    });
    method(vm, ap, "shift", 0, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        if len == 0.0 {
            set_length(vm, o, 0.0)?;
            return Ok(Value::Undefined);
        }
        let first = vm.get(o, &key(0.0))?;
        let mut k = 1.0;
        while k < len {
            let from = key(k);
            let to = key(k - 1.0);
            if vm.has_property(o, &from)? {
                let v = vm.get(o, &from)?;
                vm.set_or_throw(o, to, v)?;
            } else {
                vm.delete_or_throw(o, &to)?;
            }
            k += 1.0;
        }
        vm.delete_or_throw(o, &key(len - 1.0))?;
        set_length(vm, o, len - 1.0)?;
        Ok(first)
    });
    method(vm, ap, "unshift", 1, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let n = c.args.len() as f64;
        if n > 0.0 {
            if len + n > 9007199254740991.0 {
                return vm.type_error("Unshift past 2^53-1");
            }
            let mut k = len;
            while k > 0.0 {
                let from = key(k - 1.0);
                let to = key(k + n - 1.0);
                if vm.has_property(o, &from)? {
                    let v = vm.get(o, &from)?;
                    vm.set_or_throw(o, to, v)?;
                } else {
                    vm.delete_or_throw(o, &to)?;
                }
                k -= 1.0;
            }
            for (j, v) in c.args.into_iter().enumerate() {
                vm.set_or_throw(o, key(j as f64), v)?;
            }
        }
        set_length(vm, o, len + n)?;
        Ok(Value::Number(len + n))
    });
    method(vm, ap, "slice", 2, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let k = relative(vm, &c.arg(0), len, 0.0)?;
        let fin = relative(vm, &c.arg(1), len, len)?;
        let count = (fin - k).max(0.0);
        let a = species_create(vm, o, count)?;
        let mut n = 0.0;
        let mut i = k;
        while i < fin {
            let pk = key(i);
            if vm.has_property(o, &pk)? {
                let v = vm.get(o, &pk)?;
                vm.create_data_property_or_throw(a, key(n), v)?;
            }
            i += 1.0;
            n += 1.0;
        }
        set_length(vm, a, n)?;
        Ok(Value::Object(a))
    });
    method(vm, ap, "splice", 2, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let start = relative(vm, &c.arg(0), len, 0.0)?;
        let items: Vec<Value> = c.args.iter().skip(2).cloned().collect();
        let del = match c.args.len() {
            0 => 0.0,
            1 => len - start,
            _ => {
                let dc = vm.to_integer_or_infinity(&c.arg(1))?;
                dc.clamp(0.0, len - start)
            }
        };
        let ic = items.len() as f64;
        if len + ic - del > 9007199254740991.0 {
            return vm.type_error("splice result too long");
        }
        let a = species_create(vm, o, del)?;
        let mut k = 0.0;
        while k < del {
            let from = key(start + k);
            if vm.has_property(o, &from)? {
                let v = vm.get(o, &from)?;
                vm.create_data_property_or_throw(a, key(k), v)?;
            }
            k += 1.0;
        }
        set_length(vm, a, del)?;
        if ic < del {
            let mut k = start;
            while k < len - del {
                let from = key(k + del);
                let to = key(k + ic);
                if vm.has_property(o, &from)? {
                    let v = vm.get(o, &from)?;
                    vm.set_or_throw(o, to, v)?;
                } else {
                    vm.delete_or_throw(o, &to)?;
                }
                k += 1.0;
            }
            let mut k = len;
            while k > len - del + ic {
                vm.delete_or_throw(o, &key(k - 1.0))?;
                k -= 1.0;
            }
        } else if ic > del {
            let mut k = len - del;
            while k > start {
                let from = key(k + del - 1.0);
                let to = key(k + ic - 1.0);
                if vm.has_property(o, &from)? {
                    let v = vm.get(o, &from)?;
                    vm.set_or_throw(o, to, v)?;
                } else {
                    vm.delete_or_throw(o, &to)?;
                }
                k -= 1.0;
            }
        }
        for (j, v) in items.into_iter().enumerate() {
            vm.set_or_throw(o, key(start + j as f64), v)?;
        }
        set_length(vm, o, len - del + ic)?;
        Ok(Value::Object(a))
    });
    method(vm, ap, "concat", 1, |vm, c| {
        let o = vm.to_object(&c.this)?;
        let a = species_create(vm, o, 0.0)?;
        let mut n = 0.0;
        let mut items = vec![Value::Object(o)];
        items.extend(c.args);
        let spreadable = PropKey::Sym(vm.realm.sym_is_concat_spreadable.clone());
        for e in items {
            let spread = match &e {
                Value::Object(eo) => {
                    let s = vm.get(*eo, &spreadable)?;
                    if s.is_undefined() {
                        is_array(vm, &e)?
                    } else {
                        s.truthy()
                    }
                }
                _ => false,
            };
            if spread {
                let eo = e.as_object().expect("object");
                let len = vm.length_of_array_like(eo)?;
                if n + len > 9007199254740991.0 {
                    return vm.type_error("concat result too long");
                }
                let mut k = 0.0;
                while k < len {
                    let pk = key(k);
                    if vm.has_property(eo, &pk)? {
                        let v = vm.get(eo, &pk)?;
                        vm.create_data_property_or_throw(a, key(n), v)?;
                    }
                    n += 1.0;
                    k += 1.0;
                }
            } else {
                if n >= 9007199254740991.0 {
                    return vm.type_error("concat result too long");
                }
                vm.create_data_property_or_throw(a, key(n), e)?;
                n += 1.0;
            }
        }
        set_length(vm, a, n)?;
        Ok(Value::Object(a))
    });
    method(vm, ap, "join", 1, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let sep: JsString = if c.arg(0).is_undefined() { js_str(",") } else { vm.to_string(&c.arg(0))? };
        let mut out: Vec<u16> = Vec::new();
        let mut k = 0.0;
        while k < len {
            if k > 0.0 {
                out.extend_from_slice(&sep);
            }
            let v = vm.get(o, &key(k))?;
            if !v.is_nullish() {
                out.extend_from_slice(&vm.to_string(&v)?);
            }
            if out.len() > super::super::value::MAX_STRING_LEN {
                return vm.range_error("Invalid string length");
            }
            k += 1.0;
        }
        Ok(Value::String(out.into()))
    });
    method(vm, ap, "toString", 0, |vm, c| {
        let o = vm.to_object(&c.this)?;
        let f = vm.get(o, &PropKey::lit("join"))?;
        if vm.is_callable(&f) {
            return vm.call(&f, Value::Object(o), Vec::new());
        }
        let op = vm.realm.object_proto;
        let ts = vm.get(op, &PropKey::lit("toString"))?;
        vm.call(&ts, Value::Object(o), Vec::new())
    });
    method(vm, ap, "toLocaleString", 0, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let mut out: Vec<u16> = Vec::new();
        let mut k = 0.0;
        while k < len {
            if k > 0.0 {
                out.push(b',' as u16);
            }
            let v = vm.get(o, &key(k))?;
            if !v.is_nullish() {
                let f = vm.get_value_key(&v, &PropKey::lit("toLocaleString"))?;
                let r = vm.call(&f, v, Vec::new())?;
                out.extend_from_slice(&vm.to_string(&r)?);
            }
            k += 1.0;
        }
        Ok(Value::String(out.into()))
    });
    method(vm, ap, "reverse", 0, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let mid = (len / 2.0).floor();
        let mut lower = 0.0;
        while lower != mid {
            let upper = len - lower - 1.0;
            let (lk, uk) = (key(lower), key(upper));
            let le = vm.has_property(o, &lk)?;
            let lv = if le { vm.get(o, &lk)? } else { Value::Undefined };
            let ue = vm.has_property(o, &uk)?;
            let uv = if ue { vm.get(o, &uk)? } else { Value::Undefined };
            match (le, ue) {
                (true, true) => {
                    vm.set_or_throw(o, lk, uv)?;
                    vm.set_or_throw(o, uk, lv)?;
                }
                (false, true) => {
                    vm.set_or_throw(o, lk.clone(), uv)?;
                    vm.delete_or_throw(o, &uk)?;
                }
                (true, false) => {
                    vm.delete_or_throw(o, &lk)?;
                    vm.set_or_throw(o, uk, lv)?;
                }
                _ => {}
            }
            lower += 1.0;
        }
        Ok(Value::Object(o))
    });
    method(vm, ap, "indexOf", 1, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        if len == 0.0 {
            return Ok(Value::Number(-1.0));
        }
        let mut n = vm.to_integer_or_infinity(&c.arg(1))?;
        if n == f64::INFINITY {
            return Ok(Value::Number(-1.0));
        }
        if n < 0.0 {
            n = (len + n).max(0.0);
        }
        let target = c.arg(0);
        while n < len {
            let k = key(n);
            if vm.has_property(o, &k)? && strict_equals(&vm.get(o, &k)?, &target) {
                return Ok(Value::Number(n));
            }
            n += 1.0;
        }
        Ok(Value::Number(-1.0))
    });
    method(vm, ap, "lastIndexOf", 1, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        if len == 0.0 {
            return Ok(Value::Number(-1.0));
        }
        let n = if c.args.len() > 1 { vm.to_integer_or_infinity(&c.arg(1))? } else { len - 1.0 };
        let mut k = if n == f64::NEG_INFINITY {
            return Ok(Value::Number(-1.0));
        } else if n >= 0.0 {
            n.min(len - 1.0)
        } else {
            len + n
        };
        let target = c.arg(0);
        while k >= 0.0 {
            let pk = key(k);
            if vm.has_property(o, &pk)? && strict_equals(&vm.get(o, &pk)?, &target) {
                return Ok(Value::Number(k));
            }
            k -= 1.0;
        }
        Ok(Value::Number(-1.0))
    });
    method(vm, ap, "includes", 1, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        if len == 0.0 {
            return Ok(Value::Bool(false));
        }
        let mut n = vm.to_integer_or_infinity(&c.arg(1))?;
        if n == f64::INFINITY {
            return Ok(Value::Bool(false));
        }
        if n < 0.0 {
            n = (len + n).max(0.0);
        }
        let target = c.arg(0);
        while n < len {
            if same_value_zero(&vm.get(o, &key(n))?, &target) {
                return Ok(Value::Bool(true));
            }
            n += 1.0;
        }
        Ok(Value::Bool(false))
    });
    // Iteration methods with callbacks (§23.1.3.*).
    method(vm, ap, "forEach", 1, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let f = callback(vm, &c)?;
        let mut k = 0.0;
        while k < len {
            let pk = key(k);
            if vm.has_property(o, &pk)? {
                let v = vm.get(o, &pk)?;
                vm.call(&f, c.arg(1), vec![v, Value::Number(k), Value::Object(o)])?;
            }
            k += 1.0;
        }
        Ok(Value::Undefined)
    });
    method(vm, ap, "map", 1, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let f = callback(vm, &c)?;
        let a = species_create(vm, o, len)?;
        let mut k = 0.0;
        while k < len {
            let pk = key(k);
            if vm.has_property(o, &pk)? {
                let v = vm.get(o, &pk)?;
                let r = vm.call(&f, c.arg(1), vec![v, Value::Number(k), Value::Object(o)])?;
                vm.create_data_property_or_throw(a, pk, r)?;
            }
            k += 1.0;
        }
        Ok(Value::Object(a))
    });
    method(vm, ap, "filter", 1, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let f = callback(vm, &c)?;
        let a = species_create(vm, o, 0.0)?;
        let mut k = 0.0;
        let mut to = 0.0;
        while k < len {
            let pk = key(k);
            if vm.has_property(o, &pk)? {
                let v = vm.get(o, &pk)?;
                if vm.call(&f, c.arg(1), vec![v.clone(), Value::Number(k), Value::Object(o)])?.truthy() {
                    vm.create_data_property_or_throw(a, key(to), v)?;
                    to += 1.0;
                }
            }
            k += 1.0;
        }
        Ok(Value::Object(a))
    });
    for (name, want, result_found, result_none) in
        [("some", true, true, false), ("every", false, false, true)]
    {
        let f: NativeFn =
            if name == "some" { |vm, c| some_every(vm, c, true) } else { |vm, c| some_every(vm, c, false) };
        let _ = (want, result_found, result_none);
        method(vm, ap, name, 1, f);
    }
    method(vm, ap, "reduce", 1, |vm, c| reduce(vm, c, false));
    method(vm, ap, "reduceRight", 1, |vm, c| reduce(vm, c, true));
    method(vm, ap, "find", 1, |vm, c| find(vm, c, false, false));
    method(vm, ap, "findIndex", 1, |vm, c| find(vm, c, false, true));
    method(vm, ap, "findLast", 1, |vm, c| find(vm, c, true, false));
    method(vm, ap, "findLastIndex", 1, |vm, c| find(vm, c, true, true));
    method(vm, ap, "fill", 1, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let mut k = relative(vm, &c.arg(1), len, 0.0)?;
        let fin = relative(vm, &c.arg(2), len, len)?;
        while k < fin {
            vm.set_or_throw(o, key(k), c.arg(0))?;
            k += 1.0;
        }
        Ok(Value::Object(o))
    });
    method(vm, ap, "copyWithin", 2, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let mut to = relative(vm, &c.arg(0), len, 0.0)?;
        let mut from = relative(vm, &c.arg(1), len, 0.0)?;
        let fin = relative(vm, &c.arg(2), len, len)?;
        let mut count = (fin - from).min(len - to);
        let dir = if from < to && to < from + count {
            from += count - 1.0;
            to += count - 1.0;
            -1.0
        } else {
            1.0
        };
        while count > 0.0 {
            let (fk, tk) = (key(from), key(to));
            if vm.has_property(o, &fk)? {
                let v = vm.get(o, &fk)?;
                vm.set_or_throw(o, tk, v)?;
            } else {
                vm.delete_or_throw(o, &tk)?;
            }
            from += dir;
            to += dir;
            count -= 1.0;
        }
        Ok(Value::Object(o))
    });
    method(vm, ap, "at", 1, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let r = vm.to_integer_or_infinity(&c.arg(0))?;
        let k = if r >= 0.0 { r } else { len + r };
        if k < 0.0 || k >= len {
            return Ok(Value::Undefined);
        }
        vm.get(o, &key(k))
    });
    method(vm, ap, "sort", 1, |vm, c| {
        let cmp = comparefn(vm, &c)?;
        let (o, len) = this_obj(vm, &c)?;
        let vals = collect_present(vm, o, len, true)?;
        let n = vals.len() as f64;
        let sorted = sort_values(vm, vals, &cmp)?;
        for (i, v) in sorted.into_iter().enumerate() {
            vm.set_or_throw(o, key(i as f64), v)?;
        }
        let mut k = n;
        while k < len {
            vm.delete_or_throw(o, &key(k))?;
            k += 1.0;
        }
        Ok(Value::Object(o))
    });
    method(vm, ap, "toSorted", 1, |vm, c| {
        let cmp = comparefn(vm, &c)?;
        let (o, len) = this_obj(vm, &c)?;
        let a = array_create(vm, len, None)?;
        let vals = collect_present(vm, o, len, false)?;
        let sorted = sort_values(vm, vals, &cmp)?;
        for (i, v) in sorted.into_iter().enumerate() {
            vm.create_data_property_or_throw(a, key(i as f64), v)?;
        }
        Ok(Value::Object(a))
    });
    method(vm, ap, "toReversed", 0, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let a = array_create(vm, len, None)?;
        let mut k = 0.0;
        while k < len {
            let v = vm.get(o, &key(len - k - 1.0))?;
            vm.create_data_property_or_throw(a, key(k), v)?;
            k += 1.0;
        }
        Ok(Value::Object(a))
    });
    method(vm, ap, "with", 2, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let r = vm.to_integer_or_infinity(&c.arg(0))?;
        let idx = if r >= 0.0 { r } else { len + r };
        if idx >= len || idx < 0.0 {
            return vm.range_error("Invalid index");
        }
        let a = array_create(vm, len, None)?;
        let mut k = 0.0;
        while k < len {
            let v = if k == idx { c.arg(1) } else { vm.get(o, &key(k))? };
            vm.create_data_property_or_throw(a, key(k), v)?;
            k += 1.0;
        }
        Ok(Value::Object(a))
    });
    method(vm, ap, "toSpliced", 2, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let start = relative(vm, &c.arg(0), len, 0.0)?;
        let items: Vec<Value> = c.args.iter().skip(2).cloned().collect();
        let skip = match c.args.len() {
            0 => 0.0,
            1 => len - start,
            _ => vm.to_integer_or_infinity(&c.arg(1))?.clamp(0.0, len - start),
        };
        let new_len = len + items.len() as f64 - skip;
        if new_len > 9007199254740991.0 {
            return vm.type_error("toSpliced result too long");
        }
        let a = array_create(vm, new_len, None)?;
        let mut i = 0.0;
        while i < start {
            let v = vm.get(o, &key(i))?;
            vm.create_data_property_or_throw(a, key(i), v)?;
            i += 1.0;
        }
        for v in items {
            vm.create_data_property_or_throw(a, key(i), v)?;
            i += 1.0;
        }
        let mut r = start + skip;
        while i < new_len {
            let v = vm.get(o, &key(r))?;
            vm.create_data_property_or_throw(a, key(i), v)?;
            i += 1.0;
            r += 1.0;
        }
        Ok(Value::Object(a))
    });
    method(vm, ap, "flat", 0, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let d = c.arg(0);
        let depth = if d.is_undefined() { 1.0 } else { vm.to_integer_or_infinity(&d)?.max(0.0) };
        let a = species_create(vm, o, 0.0)?;
        flatten(vm, a, o, len, 0.0, depth, None)?;
        Ok(Value::Object(a))
    });
    method(vm, ap, "flatMap", 1, |vm, c| {
        let (o, len) = this_obj(vm, &c)?;
        let f = callback(vm, &c)?;
        let a = species_create(vm, o, 0.0)?;
        let this = c.arg(1);
        flatten(vm, a, o, len, 0.0, 1.0, Some((&f, &this)))?;
        Ok(Value::Object(a))
    });
    method(vm, ap, "keys", 0, |vm, c| {
        let o = vm.to_object(&c.this)?;
        Ok(create_array_iterator(vm, o, IterKind::Keys))
    });
    method(vm, ap, "entries", 0, |vm, c| {
        let o = vm.to_object(&c.this)?;
        Ok(create_array_iterator(vm, o, IterKind::Entries))
    });
    let values = method(vm, ap, "values", 0, |vm, c| {
        let o = vm.to_object(&c.this)?;
        Ok(create_array_iterator(vm, o, IterKind::Values))
    });
    vm.realm.array_proto_values = values;
    let si = vm.realm.sym_iterator.clone();
    vm.heap.obj_mut(ap).props.insert(PropKey::Sym(si), Prop::method(Value::Object(values)));
    let _ = method_sym;
    // %Array.prototype%[@@unscopables] (§23.1.3.41).
    let u = vm.new_object_with_proto(None);
    for n in [
        "at",
        "copyWithin",
        "entries",
        "fill",
        "find",
        "findIndex",
        "findLast",
        "findLastIndex",
        "flat",
        "flatMap",
        "includes",
        "keys",
        "toReversed",
        "toSorted",
        "toSpliced",
        "values",
    ] {
        vm.heap.obj_mut(u).props.insert(PropKey::named(n), Prop::data(Value::Bool(true), true, true, true));
    }
    let uk = PropKey::Sym(vm.realm.sym_unscopables.clone());
    vm.heap.obj_mut(ap).props.insert(uk, Prop::data(Value::Object(u), false, false, true));
}

fn some_every(vm: &mut Vm, c: NativeCall, some: bool) -> JsResult<Value> {
    let (o, len) = this_obj(vm, &c)?;
    let f = callback(vm, &c)?;
    let mut k = 0.0;
    while k < len {
        let pk = key(k);
        if vm.has_property(o, &pk)? {
            let v = vm.get(o, &pk)?;
            let r = vm.call(&f, c.arg(1), vec![v, Value::Number(k), Value::Object(o)])?.truthy();
            if r == some {
                return Ok(Value::Bool(some));
            }
        }
        k += 1.0;
    }
    Ok(Value::Bool(!some))
}

fn reduce(vm: &mut Vm, c: NativeCall, right: bool) -> JsResult<Value> {
    let (o, len) = this_obj(vm, &c)?;
    let f = callback(vm, &c)?;
    let mut k: f64 = if right { len - 1.0 } else { 0.0 };
    let step = if right { -1.0 } else { 1.0 };
    let in_range = |k: f64| if right { k >= 0.0 } else { k < len };
    let mut acc = if c.args.len() >= 2 {
        c.arg(1)
    } else {
        loop {
            if !in_range(k) {
                return vm.type_error("Reduce of empty array with no initial value");
            }
            let pk = key(k);
            k += step;
            if vm.has_property(o, &pk)? {
                break vm.get(o, &pk)?;
            }
        }
    };
    while in_range(k) {
        let pk = key(k);
        if vm.has_property(o, &pk)? {
            let v = vm.get(o, &pk)?;
            acc = vm.call(&f, Value::Undefined, vec![acc, v, Value::Number(k), Value::Object(o)])?;
        }
        k += step;
    }
    Ok(acc)
}

fn find(vm: &mut Vm, c: NativeCall, last: bool, index: bool) -> JsResult<Value> {
    let (o, len) = this_obj(vm, &c)?;
    let f = callback(vm, &c)?;
    let mut i = 0.0;
    while i < len {
        let k = if last { len - 1.0 - i } else { i };
        let v = vm.get(o, &key(k))?;
        if vm.call(&f, c.arg(1), vec![v.clone(), Value::Number(k), Value::Object(o)])?.truthy() {
            return Ok(if index { Value::Number(k) } else { v });
        }
        i += 1.0;
    }
    Ok(if index { Value::Number(-1.0) } else { Value::Undefined })
}

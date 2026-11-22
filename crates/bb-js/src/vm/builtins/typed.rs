//! ArrayBuffer, SharedArrayBuffer, DataView, %TypedArray% and the concrete typed array
//! constructors (§25.1–3, §23.2).

use super::super::heap::*;
use super::super::realm::{self, accessor, method};
use super::super::typed::{
    read_big, read_elem, write_big, write_elem, Buffer, DataView, TaKind, TypedArray, MAX_BUFFER, TA_KINDS,
};
use super::super::value::{js_str, same_value_zero, strict_equals, JsString, PropKey, Value};
use super::super::{JsResult, Vm};
use super::array::sort_values;
use super::create_array_iterator;

fn key(i: usize) -> PropKey {
    PropKey::from_f64(i as f64)
}

// ---- ArrayBuffer / SharedArrayBuffer ----

fn buffer_of(vm: &mut Vm, v: &Value, shared: bool) -> JsResult<ObjRef> {
    match v {
        Value::Object(o) => match &vm.heap.obj(*o).kind {
            Kind::ArrayBuffer(b) if b.shared == shared => Ok(*o),
            _ => vm.type_error(if shared { "not a SharedArrayBuffer" } else { "not an ArrayBuffer" }),
        },
        _ => vm.type_error(if shared { "not a SharedArrayBuffer" } else { "not an ArrayBuffer" }),
    }
}

fn buf_data(vm: &Vm, b: ObjRef) -> (Option<usize>, Option<usize>) {
    match &vm.heap.obj(b).kind {
        Kind::ArrayBuffer(d) => (d.bytes.as_ref().map(|x| x.len()), d.max),
        _ => (None, None),
    }
}

/// AllocateArrayBuffer / AllocateSharedArrayBuffer (§25.1.3.1, §25.2.2.1).
pub(super) fn allocate_buffer(
    vm: &mut Vm,
    proto: ObjRef,
    len: f64,
    max: Option<f64>,
    shared: bool,
) -> JsResult<ObjRef> {
    if let Some(m) = max {
        if len > m {
            return vm.range_error("byteLength exceeds maxByteLength");
        }
        if m > MAX_BUFFER as f64 {
            return vm.range_error("Array buffer allocation failed");
        }
    }
    if len > MAX_BUFFER as f64 {
        return vm.range_error("Array buffer allocation failed");
    }
    Ok(vm.heap.alloc(Object::new(
        Some(proto),
        Kind::ArrayBuffer(Box::new(Buffer {
            bytes: Some(vec![0; len as usize]),
            max: max.map(|m| m as usize),
            shared,
        })),
    )))
}

fn max_option(vm: &mut Vm, opts: &Value) -> JsResult<Option<f64>> {
    let Value::Object(o) = opts else { return Ok(None) };
    let m = vm.get(*o, &PropKey::lit("maxByteLength"))?;
    if m.is_undefined() {
        return Ok(None);
    }
    Ok(Some(vm.to_index(&m)?))
}

fn buffer_ctor(vm: &mut Vm, c: NativeCall, shared: bool) -> JsResult<Value> {
    let Some(nt) = c.new_target else { return vm.type_error("Constructor requires 'new'") };
    let len = vm.to_index(&c.arg(0))?;
    let max = max_option(vm, &c.arg(1))?;
    let proto = if shared {
        vm.prototype_from_ctor(nt, |r| r.shared_array_buffer_proto)?
    } else {
        vm.prototype_from_ctor(nt, |r| r.array_buffer_proto)?
    };
    Ok(Value::Object(allocate_buffer(vm, proto, len, max, shared)?))
}

fn rel(vm: &mut Vm, v: &Value, len: f64, default: f64) -> JsResult<f64> {
    if v.is_undefined() {
        return Ok(default);
    }
    let r = vm.to_integer_or_infinity(v)?;
    Ok(if r < 0.0 { (len + r).max(0.0) } else { r.min(len) })
}

/// ArrayBuffer.prototype.slice / SharedArrayBuffer.prototype.slice (§25.1.6.7).
fn buffer_slice(vm: &mut Vm, c: NativeCall, shared: bool) -> JsResult<Value> {
    let o = buffer_of(vm, &c.this, shared)?;
    if vm.is_detached(o) {
        return vm.type_error("ArrayBuffer is detached");
    }
    let len = buf_data(vm, o).0.unwrap_or(0) as f64;
    let first = rel(vm, &c.arg(0), len, 0.0)?;
    let fin = rel(vm, &c.arg(1), len, len)?;
    let new_len = (fin - first).max(0.0);
    let default = if shared {
        vm.get(vm.realm.global, &PropKey::lit("SharedArrayBuffer"))?
    } else {
        vm.get(vm.realm.global, &PropKey::lit("ArrayBuffer"))?
    };
    let ctor = vm.species_constructor(o, default.as_object().expect("constructor"))?;
    let new = vm.construct(ctor.as_object().expect("constructor"), vec![Value::Number(new_len)], None)?;
    let n = buffer_of(vm, &new, shared)?;
    if vm.is_detached(n) {
        return vm.type_error("species constructor returned a detached ArrayBuffer");
    }
    if n == o {
        return vm.type_error("species constructor returned the same ArrayBuffer");
    }
    if (buf_data(vm, n).0.unwrap_or(0) as f64) < new_len {
        return vm.type_error("species constructor returned a too small ArrayBuffer");
    }
    if vm.is_detached(o) {
        return vm.type_error("ArrayBuffer is detached");
    }
    let cur_len = buf_data(vm, o).0.unwrap_or(0);
    let first = first as usize;
    let fin = (fin as usize).min(cur_len);
    if first < fin {
        let bytes = vm.buffer_bytes(o).map(|b| b[first..fin].to_vec()).unwrap_or_default();
        if let Some(t) = vm.buffer_bytes_mut(n) {
            t[..bytes.len()].copy_from_slice(&bytes);
        }
    }
    Ok(new)
}

/// ArrayBufferCopyAndDetach (§25.1.3.3): transfer / transferToFixedLength.
fn transfer(vm: &mut Vm, c: NativeCall, fixed: bool) -> JsResult<Value> {
    let o = buffer_of(vm, &c.this, false)?;
    let (cur, max) = buf_data(vm, o);
    let new_len = if c.arg(0).is_undefined() { cur.unwrap_or(0) as f64 } else { vm.to_index(&c.arg(0))? };
    let Some(_) = cur else { return vm.type_error("ArrayBuffer is detached") };
    let new_max = if fixed { None } else { max.map(|m| m as f64) };
    if let Some(m) = new_max {
        if new_len > m {
            return vm.range_error("new length exceeds maxByteLength");
        }
    }
    let proto = vm.realm.array_buffer_proto;
    let n = allocate_buffer(vm, proto, new_len, new_max, false)?;
    let bytes = vm.buffer_bytes(o).cloned().unwrap_or_default();
    if let Some(t) = vm.buffer_bytes_mut(n) {
        let k = t.len().min(bytes.len());
        t[..k].copy_from_slice(&bytes[..k]);
    }
    vm.detach_array_buffer(o)?;
    Ok(Value::Object(n))
}

fn install_buffers(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let species = PropKey::Sym(vm.realm.sym_species.clone());
    let abp = vm.new_object_with_proto(Some(op));
    vm.realm.array_buffer_proto = abp;
    let abc = realm::constructor(vm, "ArrayBuffer", 1, |vm, c| buffer_ctor(vm, c, false), abp);
    accessor(vm, abc, species.clone(), Some(|_, c| Ok(c.this)), None);
    method(vm, abc, "isView", 1, |vm, c| {
        Ok(Value::Bool(
            matches!(c.arg(0), Value::Object(o) if matches!(vm.heap.obj(o).kind, Kind::TypedArray(_) | Kind::DataView(_))),
        ))
    });
    accessor(
        vm,
        abp,
        PropKey::lit("byteLength"),
        Some(|vm, c| {
            let o = buffer_of(vm, &c.this, false)?;
            Ok(Value::Number(buf_data(vm, o).0.unwrap_or(0) as f64))
        }),
        None,
    );
    accessor(
        vm,
        abp,
        PropKey::lit("maxByteLength"),
        Some(|vm, c| {
            let o = buffer_of(vm, &c.this, false)?;
            let (len, max) = buf_data(vm, o);
            Ok(Value::Number(match len {
                None => 0.0,
                Some(l) => max.unwrap_or(l) as f64,
            }))
        }),
        None,
    );
    accessor(
        vm,
        abp,
        PropKey::lit("resizable"),
        Some(|vm, c| {
            let o = buffer_of(vm, &c.this, false)?;
            Ok(Value::Bool(buf_data(vm, o).1.is_some()))
        }),
        None,
    );
    accessor(
        vm,
        abp,
        PropKey::lit("detached"),
        Some(|vm, c| {
            let o = buffer_of(vm, &c.this, false)?;
            Ok(Value::Bool(vm.is_detached(o)))
        }),
        None,
    );
    method(vm, abp, "slice", 2, |vm, c| buffer_slice(vm, c, false));
    method(vm, abp, "resize", 1, |vm, c| {
        let o = buffer_of(vm, &c.this, false)?;
        let (_, max) = buf_data(vm, o);
        let Some(max) = max else { return vm.type_error("ArrayBuffer is not resizable") };
        let n = vm.to_index(&c.arg(0))?;
        if vm.is_detached(o) {
            return vm.type_error("ArrayBuffer is detached");
        }
        if n > max as f64 {
            return vm.range_error("new length exceeds maxByteLength");
        }
        if let Some(b) = vm.buffer_bytes_mut(o) {
            b.resize(n as usize, 0);
        }
        Ok(Value::Undefined)
    });
    method(vm, abp, "transfer", 0, |vm, c| transfer(vm, c, false));
    method(vm, abp, "transferToFixedLength", 0, |vm, c| transfer(vm, c, true));
    realm::to_string_tag(vm, abp, "ArrayBuffer");

    let sbp = vm.new_object_with_proto(Some(op));
    vm.realm.shared_array_buffer_proto = sbp;
    let sbc = realm::constructor(vm, "SharedArrayBuffer", 1, |vm, c| buffer_ctor(vm, c, true), sbp);
    accessor(vm, sbc, species, Some(|_, c| Ok(c.this)), None);
    accessor(
        vm,
        sbp,
        PropKey::lit("byteLength"),
        Some(|vm, c| {
            let o = buffer_of(vm, &c.this, true)?;
            Ok(Value::Number(buf_data(vm, o).0.unwrap_or(0) as f64))
        }),
        None,
    );
    accessor(
        vm,
        sbp,
        PropKey::lit("maxByteLength"),
        Some(|vm, c| {
            let o = buffer_of(vm, &c.this, true)?;
            let (len, max) = buf_data(vm, o);
            Ok(Value::Number(max.unwrap_or(len.unwrap_or(0)) as f64))
        }),
        None,
    );
    accessor(
        vm,
        sbp,
        PropKey::lit("growable"),
        Some(|vm, c| {
            let o = buffer_of(vm, &c.this, true)?;
            Ok(Value::Bool(buf_data(vm, o).1.is_some()))
        }),
        None,
    );
    method(vm, sbp, "grow", 1, |vm, c| {
        let o = buffer_of(vm, &c.this, true)?;
        let (len, max) = buf_data(vm, o);
        let Some(max) = max else { return vm.type_error("SharedArrayBuffer is not growable") };
        let n = vm.to_index(&c.arg(0))?;
        if n > max as f64 || n < len.unwrap_or(0) as f64 {
            return vm.range_error("Invalid length for SharedArrayBuffer.prototype.grow");
        }
        if let Some(b) = vm.buffer_bytes_mut(o) {
            b.resize(n as usize, 0);
        }
        Ok(Value::Undefined)
    });
    method(vm, sbp, "slice", 2, |vm, c| buffer_slice(vm, c, true));
    realm::to_string_tag(vm, sbp, "SharedArrayBuffer");
}

// ---- DataView ----

fn view_parts(vm: &mut Vm, v: &Value) -> JsResult<(ObjRef, usize, Option<usize>)> {
    match v {
        Value::Object(o) => match &vm.heap.obj(*o).kind {
            Kind::DataView(d) => Ok((d.buffer, d.offset, d.length)),
            _ => vm.type_error("not a DataView"),
        },
        _ => vm.type_error("not a DataView"),
    }
}

/// GetViewByteLength with the out-of-bounds check (§25.3.1.3–4): `None` when OOB/detached.
fn view_length(vm: &Vm, buf: ObjRef, off: usize, len: Option<usize>) -> Option<usize> {
    let blen = vm.buffer_bytes(buf)?.len();
    if off > blen {
        return None;
    }
    match len {
        Some(l) => (off + l <= blen).then_some(l),
        None => Some(blen - off),
    }
}

fn view_get(vm: &mut Vm, c: NativeCall, kind: TaKind) -> JsResult<Value> {
    let (buf, off, len) = view_parts(vm, &c.this)?;
    let idx = vm.to_index(&c.arg(0))?;
    let little = c.arg(1).truthy();
    let Some(size) = view_length(vm, buf, off, len) else {
        return vm.type_error("DataView is detached or out of bounds");
    };
    if idx + kind.size() as f64 > size as f64 {
        return vm.range_error("Offset is outside the bounds of the DataView");
    }
    let at = off + idx as usize;
    Ok(Value::Number(vm.buffer_bytes(buf).map(|b| read_elem(b, kind, at, !little)).unwrap_or(0.0)))
}

fn view_set(vm: &mut Vm, c: NativeCall, kind: TaKind) -> JsResult<Value> {
    let (buf, off, len) = view_parts(vm, &c.this)?;
    let idx = vm.to_index(&c.arg(0))?;
    let x = vm.to_number(&c.arg(1))?;
    let little = c.arg(2).truthy();
    let Some(size) = view_length(vm, buf, off, len) else {
        return vm.type_error("DataView is detached or out of bounds");
    };
    if idx + kind.size() as f64 > size as f64 {
        return vm.range_error("Offset is outside the bounds of the DataView");
    }
    let at = off + idx as usize;
    if let Some(b) = vm.buffer_bytes_mut(buf) {
        write_elem(b, kind, at, x, !little);
    }
    Ok(Value::Undefined)
}

fn view_get_big(vm: &mut Vm, c: NativeCall, kind: TaKind) -> JsResult<Value> {
    let (buf, off, len) = view_parts(vm, &c.this)?;
    let idx = vm.to_index(&c.arg(0))?;
    let little = c.arg(1).truthy();
    let Some(size) = view_length(vm, buf, off, len) else {
        return vm.type_error("DataView is detached or out of bounds");
    };
    if idx + 8.0 > size as f64 {
        return vm.range_error("Offset is outside the bounds of the DataView");
    }
    let at = off + idx as usize;
    let b = vm.buffer_bytes(buf).map(|b| read_big(b, kind, at, !little)).unwrap_or_default();
    Ok(Value::BigInt(std::rc::Rc::new(b)))
}

fn view_set_big(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let (buf, off, len) = view_parts(vm, &c.this)?;
    let idx = vm.to_index(&c.arg(0))?;
    let x = vm.to_bigint(&c.arg(1))?;
    let little = c.arg(2).truthy();
    let Some(size) = view_length(vm, buf, off, len) else {
        return vm.type_error("DataView is detached or out of bounds");
    };
    if idx + 8.0 > size as f64 {
        return vm.range_error("Offset is outside the bounds of the DataView");
    }
    let at = off + idx as usize;
    if let Some(b) = vm.buffer_bytes_mut(buf) {
        write_big(b, at, &x, !little);
    }
    Ok(Value::Undefined)
}

fn install_dataview(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let dvp = vm.new_object_with_proto(Some(op));
    vm.realm.dataview_proto = dvp;
    realm::constructor(
        vm,
        "DataView",
        1,
        |vm, c| {
            let Some(nt) = c.new_target else { return vm.type_error("Constructor DataView requires 'new'") };
            let buf = match c.arg(0) {
                Value::Object(o) if matches!(vm.heap.obj(o).kind, Kind::ArrayBuffer(_)) => o,
                _ => return vm.type_error("First argument to DataView constructor must be an ArrayBuffer"),
            };
            let off = vm.to_index(&c.arg(1))?;
            if vm.is_detached(buf) {
                return vm.type_error("ArrayBuffer is detached");
            }
            let (blen, max) = buf_data(vm, buf);
            let blen = blen.unwrap_or(0) as f64;
            if off > blen {
                return vm.range_error("Start offset is outside the bounds of the buffer");
            }
            let tracking = c.arg(2).is_undefined() && max.is_some();
            let view_len = if c.arg(2).is_undefined() {
                if tracking {
                    None
                } else {
                    Some(blen - off)
                }
            } else {
                let l = vm.to_index(&c.arg(2))?;
                if off + l > blen {
                    return vm.range_error("Invalid DataView length");
                }
                Some(l)
            };
            let proto = vm.prototype_from_ctor(nt, |r| r.dataview_proto)?;
            if vm.is_detached(buf) {
                return vm.type_error("ArrayBuffer is detached");
            }
            // The buffer may have shrunk during the prototype lookup (§25.3.2.1 steps 12–13).
            let blen2 = buf_data(vm, buf).0.unwrap_or(0) as f64;
            if off > blen2 || view_len.is_some_and(|l| off + l > blen2) {
                return vm.range_error("Invalid DataView length");
            }
            Ok(Value::Object(vm.heap.alloc(Object::new(
                Some(proto),
                Kind::DataView(Box::new(DataView {
                    buffer: buf,
                    offset: off as usize,
                    length: view_len.map(|l| l as usize),
                })),
            ))))
        },
        dvp,
    );
    accessor(
        vm,
        dvp,
        PropKey::lit("buffer"),
        Some(|vm, c| Ok(Value::Object(view_parts(vm, &c.this)?.0))),
        None,
    );
    accessor(
        vm,
        dvp,
        PropKey::lit("byteLength"),
        Some(|vm, c| {
            let (b, o, l) = view_parts(vm, &c.this)?;
            match view_length(vm, b, o, l) {
                Some(n) => Ok(Value::Number(n as f64)),
                None => vm.type_error("DataView is detached or out of bounds"),
            }
        }),
        None,
    );
    accessor(
        vm,
        dvp,
        PropKey::lit("byteOffset"),
        Some(|vm, c| {
            let (b, o, l) = view_parts(vm, &c.this)?;
            match view_length(vm, b, o, l) {
                Some(_) => Ok(Value::Number(o as f64)),
                None => vm.type_error("DataView is detached or out of bounds"),
            }
        }),
        None,
    );
    macro_rules! view_methods {
        ($($get:literal, $set:literal, $k:expr);* $(;)?) => {
            $(
                method(vm, dvp, $get, 1, |vm, c| view_get(vm, c, $k));
                method(vm, dvp, $set, 2, |vm, c| view_set(vm, c, $k));
            )*
        };
    }
    view_methods!(
        "getInt8", "setInt8", TaKind::Int8;
        "getUint8", "setUint8", TaKind::Uint8;
        "getInt16", "setInt16", TaKind::Int16;
        "getUint16", "setUint16", TaKind::Uint16;
        "getInt32", "setInt32", TaKind::Int32;
        "getUint32", "setUint32", TaKind::Uint32;
        "getFloat16", "setFloat16", TaKind::Float16;
        "getFloat32", "setFloat32", TaKind::Float32;
        "getFloat64", "setFloat64", TaKind::Float64;
    );
    method(vm, dvp, "getBigInt64", 1, |vm, c| view_get_big(vm, c, TaKind::BigInt64));
    method(vm, dvp, "getBigUint64", 1, |vm, c| view_get_big(vm, c, TaKind::BigUint64));
    method(vm, dvp, "setBigInt64", 2, view_set_big);
    method(vm, dvp, "setBigUint64", 2, view_set_big);
    realm::to_string_tag(vm, dvp, "DataView");
}

// ---- typed arrays ----

fn new_ta(
    vm: &mut Vm,
    kind: TaKind,
    proto: ObjRef,
    buffer: ObjRef,
    offset: usize,
    length: Option<usize>,
) -> ObjRef {
    vm.heap.alloc(Object::new(
        Some(proto),
        Kind::TypedArray(Box::new(TypedArray { kind, buffer, offset, length })),
    ))
}

/// AllocateTypedArray with a new zeroed buffer of `len` elements.
fn alloc_ta(vm: &mut Vm, kind: TaKind, proto: ObjRef, len: f64) -> JsResult<ObjRef> {
    let bytes = len * kind.size() as f64;
    let abp = vm.realm.array_buffer_proto;
    let buf = allocate_buffer(vm, abp, bytes, None, false)?;
    Ok(new_ta(vm, kind, proto, buf, 0, Some(len as usize)))
}

fn ta_kind_of_ctor(vm: &Vm, c: ObjRef) -> Option<TaKind> {
    vm.realm.ta_ctors.iter().position(|&x| x == c).map(|i| TA_KINDS[i].0)
}

/// The concrete constructors (§23.2.5.1).
fn ta_ctor(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let Some(nt) = c.new_target else { return vm.type_error("Constructor requires 'new'") };
    let Some(kind) = ta_kind_of_ctor(vm, c.callee) else {
        return vm.type_error("not a typed array constructor");
    };
    let default_proto = vm.realm.ta_protos[kind.index()];
    let first = c.arg(0);
    let Value::Object(fo) = first else {
        let len = vm.to_index(&first)?;
        let proto = proto_from(vm, nt, default_proto)?;
        return Ok(Value::Object(alloc_ta(vm, kind, proto, len)?));
    };
    let proto = proto_from(vm, nt, default_proto)?;
    if let Some((src_kind, _, _, _)) = vm.ta(fo) {
        // From a typed array (§23.2.5.1.2).
        if src_kind.is_bigint() != kind.is_bigint() {
            return vm.type_error("Content types of the source and target typed arrays differ");
        }
        let Some(src_len) = vm.ta_length(fo) else {
            return vm.type_error("source typed array is detached or out of bounds");
        };
        let o = alloc_ta(vm, kind, proto, src_len as f64)?;
        for i in 0..src_len {
            let v = vm.ta_get(fo, i as f64);
            vm.ta_set(o, i as f64, &v)?;
        }
        return Ok(Value::Object(o));
    }
    if let Kind::ArrayBuffer(_) = &vm.heap.obj(fo).kind {
        // From a buffer (§23.2.5.1.3).
        let size = kind.size() as f64;
        let off = vm.to_index(&c.arg(1))?;
        if off % size != 0.0 {
            return vm.range_error(&format!(
                "start offset of {} should be a multiple of {}",
                kind.name(),
                size
            ));
        }
        let new_len = if c.arg(2).is_undefined() { None } else { Some(vm.to_index(&c.arg(2))?) };
        if vm.is_detached(fo) {
            return vm.type_error("ArrayBuffer is detached");
        }
        let (blen, max) = buf_data(vm, fo);
        let blen = blen.unwrap_or(0) as f64;
        let length = match new_len {
            None if max.is_some() => {
                if off > blen {
                    return vm.range_error("Start offset is outside the bounds of the buffer");
                }
                None
            }
            None => {
                if blen % size != 0.0 {
                    return vm.range_error(&format!(
                        "byte length of {} should be a multiple of {}",
                        kind.name(),
                        size
                    ));
                }
                if blen - off < 0.0 {
                    return vm.range_error("Start offset is outside the bounds of the buffer");
                }
                Some(((blen - off) / size) as usize)
            }
            Some(l) => {
                if off + l * size > blen {
                    return vm.range_error("Invalid typed array length");
                }
                Some(l as usize)
            }
        };
        return Ok(Value::Object(new_ta(vm, kind, proto, fo, off as usize, length)));
    }
    // From an iterable or an array-like (§23.2.5.1.4–5).
    let ik = PropKey::Sym(vm.realm.sym_iterator.clone());
    let vals = match vm.get_method(&first, &ik)? {
        Some(m) => {
            let r = vm.get_iterator_from_method(&first, &m)?;
            let mut v = Vec::new();
            while let Some(x) = vm.iter_step(r)? {
                v.push(x);
            }
            v
        }
        None => {
            let len = vm.length_of_array_like(fo)?;
            let o = alloc_ta(vm, kind, proto, len)?;
            let mut i = 0.0;
            while i < len {
                let v = vm.get(fo, &PropKey::from_f64(i))?;
                vm.set_or_throw(o, PropKey::from_f64(i), v)?;
                i += 1.0;
            }
            return Ok(Value::Object(o));
        }
    };
    let o = alloc_ta(vm, kind, proto, vals.len() as f64)?;
    for (i, v) in vals.into_iter().enumerate() {
        vm.set_or_throw(o, key(i), v)?;
    }
    Ok(Value::Object(o))
}

fn proto_from(vm: &mut Vm, nt: ObjRef, default: ObjRef) -> JsResult<ObjRef> {
    match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => Ok(p),
        _ => Ok(default),
    }
}

/// TypedArrayCreateFromConstructor (§23.2.4.2).
fn ta_create(vm: &mut Vm, ctor: &Value, args: Vec<Value>) -> JsResult<ObjRef> {
    let Some(co) = ctor.as_object() else { return vm.type_error("not a constructor") };
    let want = match args.as_slice() {
        [Value::Number(n)] => Some(*n),
        _ => None,
    };
    let r = vm.construct(co, args, None)?;
    let (o, len) = vm.ta_validate(&r)?;
    if let Some(n) = want {
        if (len as f64) < n {
            return vm.type_error("species constructor returned a too short typed array");
        }
    }
    Ok(o)
}

/// TypedArraySpeciesCreate (§23.2.4.1).
fn species_create(vm: &mut Vm, exemplar: ObjRef, args: Vec<Value>) -> JsResult<ObjRef> {
    let kind = vm.ta(exemplar).expect("typed array").0;
    let default = vm.realm.ta_ctors[kind.index()];
    let c = vm.species_constructor(exemplar, default)?;
    let r = ta_create(vm, &c, args)?;
    if vm.ta(r).is_some_and(|t| t.0.is_bigint() != kind.is_bigint()) {
        return vm.type_error("Content types of the species-created and source typed arrays differ");
    }
    Ok(r)
}

fn this_ta(vm: &mut Vm, c: &NativeCall) -> JsResult<(ObjRef, usize)> {
    vm.ta_validate(&c.this)
}

fn callback(vm: &mut Vm, f: &Value) -> JsResult<()> {
    if !vm.is_callable(f) {
        return vm.type_error(&format!("{} is not a function", vm.describe_short(f)));
    }
    Ok(())
}

fn ta_values(vm: &Vm, o: ObjRef) -> Vec<Value> {
    let n = vm.ta_length(o).unwrap_or(0);
    (0..n).map(|i| vm.ta_get(o, i as f64)).collect()
}

/// The default numeric order of %TypedArray%.prototype.sort (§23.2.4.7).
fn numeric_cmp(a: f64, b: f64) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Equal,
        (true, false) => Greater,
        (false, true) => Less,
        _ => {
            if a < b {
                Less
            } else if a > b {
                Greater
            } else if a == 0.0 && b == 0.0 {
                b.is_sign_negative().cmp(&a.is_sign_negative())
            } else {
                Equal
            }
        }
    }
}

fn sorted(vm: &mut Vm, vals: Vec<Value>, cmp: &Value) -> JsResult<Vec<Value>> {
    if cmp.is_undefined() && vals.iter().any(|v| matches!(v, Value::BigInt(_))) {
        let mut v = vals;
        v.sort_by(|a, b| match (a, b) {
            (Value::BigInt(x), Value::BigInt(y)) => x.cmp(y),
            _ => std::cmp::Ordering::Equal,
        });
        return Ok(v);
    }
    if cmp.is_undefined() {
        let mut nums: Vec<f64> =
            vals.iter().map(|v| if let Value::Number(n) = v { *n } else { f64::NAN }).collect();
        nums.sort_by(|a, b| numeric_cmp(*a, *b));
        return Ok(nums.into_iter().map(Value::Number).collect());
    }
    // A comparator: a stable merge sort through it (ToNumber of the result as for Array).
    let wrapped = cmp.clone();
    let f = realm::native_with(
        vm,
        "",
        2,
        |vm, c| {
            let cmp = match &vm.heap.obj(c.callee).kind {
                Kind::Native(n) => n.slots[0].clone(),
                _ => Value::Undefined,
            };
            let r = vm.call(&cmp, Value::Undefined, vec![c.arg(0), c.arg(1)])?;
            let n = vm.to_number(&r)?;
            Ok(Value::Number(if n.is_nan() { 0.0 } else { n }))
        },
        false,
        vec![wrapped],
    );
    sort_values(vm, vals, &Value::Object(f))
}

fn find(vm: &mut Vm, c: NativeCall, last: bool, index: bool) -> JsResult<Value> {
    let (o, len) = this_ta(vm, &c)?;
    let f = c.arg(0);
    callback(vm, &f)?;
    for j in 0..len {
        let i = if last { len - 1 - j } else { j };
        let v = vm.ta_get(o, i as f64);
        if vm.call(&f, c.arg(1), vec![v.clone(), Value::Number(i as f64), Value::Object(o)])?.truthy() {
            return Ok(if index { Value::Number(i as f64) } else { v });
        }
    }
    Ok(if index { Value::Number(-1.0) } else { Value::Undefined })
}

fn install_typed_arrays(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let fp = vm.realm.function_proto;
    let tap = vm.new_object_with_proto(Some(op));
    vm.realm.typed_array_proto = tap;
    let tac = realm::native_with(
        vm,
        "TypedArray",
        0,
        |vm, _| vm.type_error("Abstract class TypedArray not directly constructable"),
        true,
        Vec::new(),
    );
    vm.realm.typed_array_ctor = tac;
    vm.heap
        .obj_mut(tac)
        .props
        .insert(PropKey::lit("prototype"), Prop::data(Value::Object(tap), false, false, false));
    vm.heap.obj_mut(tap).props.insert(PropKey::lit("constructor"), Prop::method(Value::Object(tac)));
    let species = PropKey::Sym(vm.realm.sym_species.clone());
    accessor(vm, tac, species, Some(|_, c| Ok(c.this)), None);
    let _ = fp;

    method(vm, tac, "of", 0, |vm, c| {
        if !vm.is_constructor(&c.this) {
            return vm.type_error("TypedArray.of: this is not a constructor");
        }
        let n = c.args.len();
        let o = ta_create(vm, &c.this, vec![Value::Number(n as f64)])?;
        for (i, v) in c.args.into_iter().enumerate() {
            vm.set_or_throw(o, key(i), v)?;
        }
        Ok(Value::Object(o))
    });
    method(vm, tac, "from", 1, |vm, c| {
        if !vm.is_constructor(&c.this) {
            return vm.type_error("TypedArray.from: this is not a constructor");
        }
        let mapfn = c.arg(1);
        if !mapfn.is_undefined() {
            callback(vm, &mapfn)?;
        }
        let src = c.arg(0);
        let ik = PropKey::Sym(vm.realm.sym_iterator.clone());
        let vals: Vec<Value> = match vm.get_method(&src, &ik)? {
            Some(m) => {
                let r = vm.get_iterator_from_method(&src, &m)?;
                let mut v = Vec::new();
                while let Some(x) = vm.iter_step(r)? {
                    v.push(x);
                }
                v
            }
            None => {
                let so = vm.to_object(&src)?;
                let len = vm.length_of_array_like(so)?;
                let o = ta_create(vm, &c.this, vec![Value::Number(len)])?;
                let mut i = 0.0;
                while i < len {
                    let mut v = vm.get(so, &PropKey::from_f64(i))?;
                    if !mapfn.is_undefined() {
                        v = vm.call(&mapfn, c.arg(2), vec![v, Value::Number(i)])?;
                    }
                    vm.set_or_throw(o, PropKey::from_f64(i), v)?;
                    i += 1.0;
                }
                return Ok(Value::Object(o));
            }
        };
        let o = ta_create(vm, &c.this, vec![Value::Number(vals.len() as f64)])?;
        for (i, mut v) in vals.into_iter().enumerate() {
            if !mapfn.is_undefined() {
                v = vm.call(&mapfn, c.arg(2), vec![v, Value::Number(i as f64)])?;
            }
            vm.set_or_throw(o, key(i), v)?;
        }
        Ok(Value::Object(o))
    });

    // Getters (§23.2.3.*): out of bounds reads as 0.
    accessor(
        vm,
        tap,
        PropKey::lit("buffer"),
        Some(|vm, c| match c.this.as_object().and_then(|o| vm.ta(o)) {
            Some((_, b, _, _)) => Ok(Value::Object(b)),
            None => vm.type_error("not a typed array"),
        }),
        None,
    );
    accessor(
        vm,
        tap,
        PropKey::lit("byteLength"),
        Some(|vm, c| match c.this.as_object().and_then(|o| vm.ta(o).map(|t| (o, t))) {
            Some((o, (k, ..))) => Ok(Value::Number((vm.ta_length(o).unwrap_or(0) * k.size()) as f64)),
            None => vm.type_error("not a typed array"),
        }),
        None,
    );
    accessor(
        vm,
        tap,
        PropKey::lit("byteOffset"),
        Some(|vm, c| match c.this.as_object().and_then(|o| vm.ta(o).map(|t| (o, t))) {
            Some((o, (_, _, off, _))) => {
                Ok(Value::Number(if vm.ta_length(o).is_some() { off as f64 } else { 0.0 }))
            }
            None => vm.type_error("not a typed array"),
        }),
        None,
    );
    accessor(
        vm,
        tap,
        PropKey::lit("length"),
        Some(|vm, c| match c.this.as_object().filter(|o| vm.ta(*o).is_some()) {
            Some(o) => Ok(Value::Number(vm.ta_length(o).unwrap_or(0) as f64)),
            None => vm.type_error("not a typed array"),
        }),
        None,
    );
    let tag = PropKey::Sym(vm.realm.sym_to_string_tag.clone());
    accessor(
        vm,
        tap,
        tag,
        Some(|vm, c| {
            Ok(match c.this.as_object().and_then(|o| vm.ta(o)) {
                Some((k, ..)) => Value::str(k.name()),
                None => Value::Undefined,
            })
        }),
        None,
    );

    method(vm, tap, "at", 1, |vm, c| {
        let (o, len) = this_ta(vm, &c)?;
        let r = vm.to_integer_or_infinity(&c.arg(0))?;
        let k = if r >= 0.0 { r } else { len as f64 + r };
        if k < 0.0 || k >= len as f64 {
            return Ok(Value::Undefined);
        }
        Ok(vm.ta_get(o, k))
    });
    method(vm, tap, "copyWithin", 2, |vm, c| {
        let (o, len) = this_ta(vm, &c)?;
        let lf = len as f64;
        let to = rel(vm, &c.arg(0), lf, 0.0)?;
        let from = rel(vm, &c.arg(1), lf, 0.0)?;
        let fin = rel(vm, &c.arg(2), lf, lf)?;
        let count = (fin - from).min(lf - to);
        if count > 0.0 {
            let Some(len2) = vm.ta_length(o) else {
                return vm.type_error("typed array is detached or out of bounds");
            };
            let (kind, buf, off, _) = vm.ta(o).expect("typed array");
            let size = kind.size();
            let end_byte = off + len2 * size;
            let to_b = off + to as usize * size;
            let from_b = off + from as usize * size;
            let mut n = count as usize * size;
            if from_b + n > end_byte {
                n = end_byte.saturating_sub(from_b);
            }
            if to_b + n > end_byte {
                n = end_byte.saturating_sub(to_b);
            }
            if let Some(b) = vm.buffer_bytes_mut(buf) {
                if n > 0 {
                    b.copy_within(from_b..from_b + n, to_b);
                }
            }
        }
        Ok(Value::Object(o))
    });
    method(vm, tap, "entries", 0, |vm, c| {
        let (o, _) = this_ta(vm, &c)?;
        Ok(create_array_iterator(vm, o, IterKind::Entries))
    });
    method(vm, tap, "keys", 0, |vm, c| {
        let (o, _) = this_ta(vm, &c)?;
        Ok(create_array_iterator(vm, o, IterKind::Keys))
    });
    let values = method(vm, tap, "values", 0, |vm, c| {
        let (o, _) = this_ta(vm, &c)?;
        Ok(create_array_iterator(vm, o, IterKind::Values))
    });
    let si = PropKey::Sym(vm.realm.sym_iterator.clone());
    vm.heap.obj_mut(tap).props.insert(si, Prop::method(Value::Object(values)));
    for (name, some) in [("some", true), ("every", false)] {
        let f: NativeFn =
            if some { |vm, c| some_every(vm, c, true) } else { |vm, c| some_every(vm, c, false) };
        method(vm, tap, name, 1, f);
    }
    method(vm, tap, "fill", 1, |vm, c| {
        let (o, len) = this_ta(vm, &c)?;
        let v = vm.ta_coerce(o, &c.arg(0))?;
        let lf = len as f64;
        let k = rel(vm, &c.arg(1), lf, 0.0)?;
        let fin = rel(vm, &c.arg(2), lf, lf)?;
        if vm.ta_length(o).is_none() {
            return vm.type_error("typed array is detached or out of bounds");
        }
        let mut i = k;
        while i < fin {
            vm.ta_set(o, i, &v)?;
            i += 1.0;
        }
        Ok(Value::Object(o))
    });
    method(vm, tap, "filter", 1, |vm, c| {
        let (o, len) = this_ta(vm, &c)?;
        let f = c.arg(0);
        callback(vm, &f)?;
        let mut kept = Vec::new();
        for i in 0..len {
            let v = vm.ta_get(o, i as f64);
            if vm.call(&f, c.arg(1), vec![v.clone(), Value::Number(i as f64), Value::Object(o)])?.truthy() {
                kept.push(v);
            }
        }
        let a = species_create(vm, o, vec![Value::Number(kept.len() as f64)])?;
        for (i, v) in kept.into_iter().enumerate() {
            vm.set_or_throw(a, key(i), v)?;
        }
        Ok(Value::Object(a))
    });
    method(vm, tap, "find", 1, |vm, c| find(vm, c, false, false));
    method(vm, tap, "findIndex", 1, |vm, c| find(vm, c, false, true));
    method(vm, tap, "findLast", 1, |vm, c| find(vm, c, true, false));
    method(vm, tap, "findLastIndex", 1, |vm, c| find(vm, c, true, true));
    method(vm, tap, "forEach", 1, |vm, c| {
        let (o, len) = this_ta(vm, &c)?;
        let f = c.arg(0);
        callback(vm, &f)?;
        for i in 0..len {
            let v = vm.ta_get(o, i as f64);
            vm.call(&f, c.arg(1), vec![v, Value::Number(i as f64), Value::Object(o)])?;
        }
        Ok(Value::Undefined)
    });
    method(vm, tap, "includes", 1, |vm, c| search(vm, c, 0));
    method(vm, tap, "indexOf", 1, |vm, c| search(vm, c, 1));
    method(vm, tap, "lastIndexOf", 1, |vm, c| search(vm, c, 2));
    method(vm, tap, "join", 1, |vm, c| {
        let (o, len) = this_ta(vm, &c)?;
        let sep: JsString = if c.arg(0).is_undefined() { js_str(",") } else { vm.to_string(&c.arg(0))? };
        let mut out: Vec<u16> = Vec::new();
        for i in 0..len {
            if i > 0 {
                out.extend_from_slice(&sep);
            }
            let v = vm.ta_get(o, i as f64);
            if !v.is_undefined() {
                out.extend_from_slice(&vm.to_string(&v)?);
            }
        }
        Ok(Value::String(out.into()))
    });
    method(vm, tap, "map", 1, |vm, c| {
        let (o, len) = this_ta(vm, &c)?;
        let f = c.arg(0);
        callback(vm, &f)?;
        let a = species_create(vm, o, vec![Value::Number(len as f64)])?;
        for i in 0..len {
            let v = vm.ta_get(o, i as f64);
            let r = vm.call(&f, c.arg(1), vec![v, Value::Number(i as f64), Value::Object(o)])?;
            vm.set_or_throw(a, key(i), r)?;
        }
        Ok(Value::Object(a))
    });
    method(vm, tap, "reduce", 1, |vm, c| reduce(vm, c, false));
    method(vm, tap, "reduceRight", 1, |vm, c| reduce(vm, c, true));
    method(vm, tap, "reverse", 0, |vm, c| {
        let (o, len) = this_ta(vm, &c)?;
        let vals = ta_values(vm, o);
        for (i, v) in vals.into_iter().rev().enumerate().take(len) {
            vm.ta_set(o, i as f64, &v)?;
        }
        Ok(Value::Object(o))
    });
    method(vm, tap, "set", 1, |vm, c| {
        let Value::Object(o) = c.this else { return vm.type_error("not a typed array") };
        if vm.ta(o).is_none() {
            return vm.type_error("not a typed array");
        }
        let off = vm.to_integer_or_infinity(&c.arg(1))?;
        if off < 0.0 {
            return vm.range_error("offset is out of bounds");
        }
        let Some(target_len) = vm.ta_length(o) else {
            return vm.type_error("typed array is detached or out of bounds");
        };
        let src = c.arg(0);
        let vals: Vec<Value> = match &src {
            Value::Object(s) if vm.ta(*s).is_some() => {
                let Some(_) = vm.ta_length(*s) else {
                    return vm.type_error("source typed array is detached or out of bounds");
                };
                ta_values(vm, *s)
            }
            _ => {
                let so = vm.to_object(&src)?;
                let len = vm.length_of_array_like(so)?;
                if off == f64::INFINITY || len + off > target_len as f64 {
                    return vm.range_error("offset is out of bounds");
                }
                let mut i = 0.0;
                while i < len {
                    let v = vm.get(so, &PropKey::from_f64(i))?;
                    vm.ta_set(o, off + i, &v)?;
                    i += 1.0;
                }
                return Ok(Value::Undefined);
            }
        };
        if off == f64::INFINITY || vals.len() as f64 + off > target_len as f64 {
            return vm.range_error("offset is out of bounds");
        }
        for (i, v) in vals.into_iter().enumerate() {
            vm.ta_set(o, off + i as f64, &v)?;
        }
        Ok(Value::Undefined)
    });
    method(vm, tap, "slice", 2, |vm, c| {
        let (o, len) = this_ta(vm, &c)?;
        let lf = len as f64;
        let k = rel(vm, &c.arg(0), lf, 0.0)?;
        let fin = rel(vm, &c.arg(1), lf, lf)?;
        let count = (fin - k).max(0.0);
        let a = species_create(vm, o, vec![Value::Number(count)])?;
        if count > 0.0 {
            let Some(len2) = vm.ta_length(o) else {
                return vm.type_error("typed array is detached or out of bounds");
            };
            let fin = fin.min(len2 as f64);
            let mut n = 0.0;
            let mut i = k;
            while i < fin {
                let v = vm.ta_get(o, i);
                vm.ta_set(a, n, &v)?;
                i += 1.0;
                n += 1.0;
            }
        }
        Ok(Value::Object(a))
    });
    method(vm, tap, "sort", 1, |vm, c| {
        let cmp = c.arg(0);
        if !cmp.is_undefined() {
            callback(vm, &cmp)?;
        }
        let (o, _) = this_ta(vm, &c)?;
        let vals = ta_values(vm, o);
        let s = sorted(vm, vals, &cmp)?;
        for (i, v) in s.into_iter().enumerate() {
            vm.ta_set(o, i as f64, &v)?;
        }
        Ok(Value::Object(o))
    });
    method(vm, tap, "subarray", 2, |vm, c| {
        let Value::Object(o) = c.this else { return vm.type_error("not a typed array") };
        let Some((kind, buf, off, length)) = vm.ta(o) else { return vm.type_error("not a typed array") };
        let src_len = vm.ta_length(o).unwrap_or(0) as f64;
        let begin = rel(vm, &c.arg(0), src_len, 0.0)?;
        let begin_byte = off as f64 + begin * kind.size() as f64;
        let mut args = vec![Value::Object(buf), Value::Number(begin_byte)];
        if length.is_some() || !c.arg(1).is_undefined() {
            let fin = rel(vm, &c.arg(1), src_len, src_len)?;
            args.push(Value::Number((fin - begin).max(0.0)));
        }
        let default = vm.realm.ta_ctors[kind.index()];
        let ctor = vm.species_constructor(o, default)?;
        Ok(Value::Object(ta_create(vm, &ctor, args)?))
    });
    method(vm, tap, "toLocaleString", 0, |vm, c| {
        let (o, len) = this_ta(vm, &c)?;
        let mut out: Vec<u16> = Vec::new();
        for i in 0..len {
            if i > 0 {
                out.push(b',' as u16);
            }
            let v = vm.ta_get(o, i as f64);
            if !v.is_nullish() {
                let f = vm.get_value_key(&v, &PropKey::lit("toLocaleString"))?;
                let r = vm.call(&f, v, Vec::new())?;
                out.extend_from_slice(&vm.to_string(&r)?);
            }
        }
        Ok(Value::String(out.into()))
    });
    method(vm, tap, "toReversed", 0, |vm, c| {
        let (o, len) = this_ta(vm, &c)?;
        let kind = vm.ta(o).expect("typed array").0;
        let p = vm.realm.ta_protos[kind.index()];
        let a = alloc_ta(vm, kind, p, len as f64)?;
        for (i, v) in ta_values(vm, o).into_iter().rev().enumerate() {
            vm.ta_set(a, i as f64, &v)?;
        }
        Ok(Value::Object(a))
    });
    method(vm, tap, "toSorted", 1, |vm, c| {
        let cmp = c.arg(0);
        if !cmp.is_undefined() {
            callback(vm, &cmp)?;
        }
        let (o, len) = this_ta(vm, &c)?;
        let kind = vm.ta(o).expect("typed array").0;
        let p = vm.realm.ta_protos[kind.index()];
        let a = alloc_ta(vm, kind, p, len as f64)?;
        let vals = ta_values(vm, o);
        for (i, v) in sorted(vm, vals, &cmp)?.into_iter().enumerate() {
            vm.ta_set(a, i as f64, &v)?;
        }
        Ok(Value::Object(a))
    });
    method(vm, tap, "with", 2, |vm, c| {
        let (o, len) = this_ta(vm, &c)?;
        let r = vm.to_integer_or_infinity(&c.arg(0))?;
        let idx = if r >= 0.0 { r } else { len as f64 + r };
        let v = vm.ta_coerce(o, &c.arg(1))?;
        if !vm.ta_length(o).is_some_and(|l| idx >= 0.0 && idx < l as f64) {
            return vm.range_error("Invalid typed array index");
        }
        let kind = vm.ta(o).expect("typed array").0;
        let p = vm.realm.ta_protos[kind.index()];
        let a = alloc_ta(vm, kind, p, len as f64)?;
        for i in 0..len {
            let x = if i as f64 == idx { v.clone() } else { vm.ta_get(o, i as f64) };
            vm.ta_set(a, i as f64, &x)?;
        }
        Ok(Value::Object(a))
    });
    let ap = vm.realm.array_proto;
    if let Ok(ts) = vm.get(ap, &PropKey::lit("toString")) {
        realm::value(vm, tap, "toString", ts);
    }

    for (i, (kind, name)) in TA_KINDS.iter().enumerate() {
        let p = vm.new_object_with_proto(Some(tap));
        vm.realm.ta_protos[i] = p;
        let c = realm::constructor(vm, name, 3, ta_ctor, p);
        vm.realm.ta_ctors[i] = c;
        vm.heap.obj_mut(c).proto = Some(tac);
        let bpe = Value::Number(kind.size() as f64);
        realm::constant(vm, c, "BYTES_PER_ELEMENT", bpe.clone());
        realm::constant(vm, p, "BYTES_PER_ELEMENT", bpe);
    }
}

fn some_every(vm: &mut Vm, c: NativeCall, some: bool) -> JsResult<Value> {
    let (o, len) = this_ta(vm, &c)?;
    let f = c.arg(0);
    callback(vm, &f)?;
    for i in 0..len {
        let v = vm.ta_get(o, i as f64);
        if vm.call(&f, c.arg(1), vec![v, Value::Number(i as f64), Value::Object(o)])?.truthy() == some {
            return Ok(Value::Bool(some));
        }
    }
    Ok(Value::Bool(!some))
}

fn reduce(vm: &mut Vm, c: NativeCall, right: bool) -> JsResult<Value> {
    let (o, len) = this_ta(vm, &c)?;
    let f = c.arg(0);
    callback(vm, &f)?;
    let order: Vec<usize> = if right { (0..len).rev().collect() } else { (0..len).collect() };
    let mut it = order.into_iter();
    let mut acc = if c.args.len() >= 2 {
        c.arg(1)
    } else {
        match it.next() {
            Some(i) => vm.ta_get(o, i as f64),
            None => return vm.type_error("Reduce of empty array with no initial value"),
        }
    };
    for i in it {
        let v = vm.ta_get(o, i as f64);
        acc = vm.call(&f, Value::Undefined, vec![acc, v, Value::Number(i as f64), Value::Object(o)])?;
    }
    Ok(acc)
}

/// includes (0), indexOf (1), lastIndexOf (2).
fn search(vm: &mut Vm, c: NativeCall, which: u8) -> JsResult<Value> {
    let (o, len) = this_ta(vm, &c)?;
    let not_found = if which == 0 { Value::Bool(false) } else { Value::Number(-1.0) };
    if len == 0 {
        return Ok(not_found);
    }
    let lf = len as f64;
    let target = c.arg(0);
    if which == 2 {
        let n = if c.args.len() > 1 { vm.to_integer_or_infinity(&c.arg(1))? } else { lf - 1.0 };
        if n == f64::NEG_INFINITY {
            return Ok(not_found);
        }
        let mut k = if n >= 0.0 { n.min(lf - 1.0) } else { lf + n };
        while k >= 0.0 {
            if vm.ta_has(o, k) && strict_equals(&vm.ta_get(o, k), &target) {
                return Ok(Value::Number(k));
            }
            k -= 1.0;
        }
        return Ok(not_found);
    }
    let mut n = vm.to_integer_or_infinity(&c.arg(1))?;
    if n == f64::INFINITY {
        return Ok(not_found);
    }
    if n < 0.0 {
        n = (lf + n).max(0.0);
    }
    while n < lf {
        let v = vm.ta_get(o, n);
        if which == 0 {
            if same_value_zero(&v, &target) {
                return Ok(Value::Bool(true));
            }
        } else if vm.ta_has(o, n) && strict_equals(&v, &target) {
            return Ok(Value::Number(n));
        }
        n += 1.0;
    }
    Ok(not_found)
}

pub(super) fn install(vm: &mut Vm) {
    install_buffers(vm);
    install_dataview(vm);
    install_typed_arrays(vm);
    install_atomics(vm);
}

// ---- Atomics (§25.4): one agent, so operations are plain reads and writes ----

/// ValidateIntegerTypedArray + ValidateAtomicAccess (§25.4.3.1–2): (array, element index).
fn atomic_target(vm: &mut Vm, ta: &Value, idx: &Value, waitable: bool) -> JsResult<(ObjRef, usize)> {
    let (o, len) = vm.ta_validate(ta)?;
    let kind = vm.ta(o).expect("typed array").0;
    let ok = if waitable {
        matches!(kind, TaKind::Int32 | TaKind::BigInt64)
    } else {
        !matches!(kind, TaKind::Float16 | TaKind::Float32 | TaKind::Float64 | TaKind::Uint8Clamped)
    };
    if !ok {
        return vm.type_error("Atomics operations need an integer typed array");
    }
    let i = vm.to_index(idx)?;
    if i >= len as f64 {
        return vm.range_error("Invalid atomic access index");
    }
    Ok((o, i as usize))
}

fn revalidate(vm: &mut Vm, o: ObjRef, i: usize) -> JsResult<()> {
    match vm.ta_length(o) {
        None => vm.type_error("typed array is detached or out of bounds"),
        Some(l) if i >= l => vm.range_error("Invalid atomic access index"),
        _ => Ok(()),
    }
}

/// An atomic read-modify-write: returns the old value.
fn atomic_rmw(vm: &mut Vm, c: NativeCall, op: fn(&Value, &Value) -> Value) -> JsResult<Value> {
    let (o, i) = atomic_target(vm, &c.arg(0), &c.arg(1), false)?;
    let v = coerce_integer(vm, o, &c.arg(2))?;
    revalidate(vm, o, i)?;
    let old = vm.ta_get(o, i as f64);
    let new = op(&old, &v);
    vm.ta_set(o, i as f64, &new)?;
    Ok(old)
}

/// ToBigInt or ToIntegerOrInfinity (Atomics convert integers, not ToNumber).
fn coerce_integer(vm: &mut Vm, o: ObjRef, v: &Value) -> JsResult<Value> {
    if vm.ta(o).is_some_and(|t| t.0.is_bigint()) {
        Ok(Value::BigInt(vm.to_bigint(v)?))
    } else {
        Ok(Value::Number(vm.to_integer_or_infinity(v)?))
    }
}

fn num_or_big(
    a: &Value,
    b: &Value,
    n: fn(f64, f64) -> f64,
    big: fn(&super::super::bigint::BigInt, &super::super::bigint::BigInt) -> super::super::bigint::BigInt,
) -> Value {
    match (a, b) {
        (Value::BigInt(x), Value::BigInt(y)) => Value::BigInt(std::rc::Rc::new(big(x, y))),
        (Value::Number(x), Value::Number(y)) => Value::Number(n(*x, *y)),
        _ => b.clone(),
    }
}

fn int(x: f64) -> i64 {
    super::super::ops::f64_to_int32(x) as i64
        | ((if x.is_finite() { (x.trunc() / 4294967296.0).trunc() as i64 } else { 0 }) << 32)
}

fn install_atomics(vm: &mut Vm) {
    let a = vm.new_object();
    let g = vm.realm.global;
    realm::value(vm, g, "Atomics", Value::Object(a));
    realm::to_string_tag(vm, a, "Atomics");
    method(vm, a, "add", 3, |vm, c| {
        atomic_rmw(vm, c, |x, y| num_or_big(x, y, |p, q| p + q, |p, q| p.add(q)))
    });
    method(vm, a, "sub", 3, |vm, c| {
        atomic_rmw(vm, c, |x, y| num_or_big(x, y, |p, q| p - q, |p, q| p.sub(q)))
    });
    method(vm, a, "and", 3, |vm, c| {
        atomic_rmw(vm, c, |x, y| {
            num_or_big(x, y, |p, q| (int(p) & int(q)) as f64, |p, q| p.bitop(q, |m, n| m & n))
        })
    });
    method(vm, a, "or", 3, |vm, c| {
        atomic_rmw(vm, c, |x, y| {
            num_or_big(x, y, |p, q| (int(p) | int(q)) as f64, |p, q| p.bitop(q, |m, n| m | n))
        })
    });
    method(vm, a, "xor", 3, |vm, c| {
        atomic_rmw(vm, c, |x, y| {
            num_or_big(x, y, |p, q| (int(p) ^ int(q)) as f64, |p, q| p.bitop(q, |m, n| m ^ n))
        })
    });
    method(vm, a, "exchange", 3, |vm, c| atomic_rmw(vm, c, |_, y| y.clone()));
    method(vm, a, "compareExchange", 4, |vm, c| {
        let (o, i) = atomic_target(vm, &c.arg(0), &c.arg(1), false)?;
        let expected = coerce_integer(vm, o, &c.arg(2))?;
        let replacement = coerce_integer(vm, o, &c.arg(3))?;
        revalidate(vm, o, i)?;
        let old = vm.ta_get(o, i as f64);
        // Compare as stored bytes: write the expected value to a scratch element.
        let kind = vm.ta(o).expect("typed array").0;
        let p = vm.realm.ta_protos[kind.index()];
        let scratch = alloc_ta(vm, kind, p, 1.0)?;
        vm.ta_set(scratch, 0.0, &expected)?;
        let expected_stored = vm.ta_get(scratch, 0.0);
        if strict_equals(&old, &expected_stored) {
            vm.ta_set(o, i as f64, &replacement)?;
        }
        Ok(old)
    });
    method(vm, a, "load", 2, |vm, c| {
        let (o, i) = atomic_target(vm, &c.arg(0), &c.arg(1), false)?;
        revalidate(vm, o, i)?;
        Ok(vm.ta_get(o, i as f64))
    });
    method(vm, a, "store", 3, |vm, c| {
        let (o, i) = atomic_target(vm, &c.arg(0), &c.arg(1), false)?;
        let v = coerce_integer(vm, o, &c.arg(2))?;
        let v = match v {
            Value::Number(n) => Value::Number(n + 0.0),
            v => v,
        };
        revalidate(vm, o, i)?;
        vm.ta_set(o, i as f64, &v)?;
        Ok(v)
    });
    method(vm, a, "isLockFree", 1, |vm, c| {
        let n = vm.to_integer_or_infinity(&c.arg(0))?;
        Ok(Value::Bool(matches!(n as i64, 1 | 2 | 4 | 8) && n.fract() == 0.0))
    });
    method(vm, a, "pause", 0, |vm, c| match c.arg(0) {
        Value::Undefined => Ok(Value::Undefined),
        Value::Number(n) if n.is_finite() && n.fract() == 0.0 => Ok(Value::Undefined),
        _ => vm.type_error("Atomics.pause: argument must be an integral Number"),
    });
    method(vm, a, "notify", 3, |vm, c| {
        let (o, i) = atomic_target(vm, &c.arg(0), &c.arg(1), true)?;
        if !c.arg(2).is_undefined() {
            let n = vm.to_integer_or_infinity(&c.arg(2))?;
            let _ = n.max(0.0);
        }
        revalidate(vm, o, i)?;
        Ok(Value::Number(0.0))
    });
    method(vm, a, "wait", 4, |vm, c| {
        let (o, i) = atomic_target(vm, &c.arg(0), &c.arg(1), true)?;
        let buf = vm.ta(o).expect("typed array").1;
        if !matches!(&vm.heap.obj(buf).kind, Kind::ArrayBuffer(b) if b.shared) {
            return vm.type_error("Atomics.wait needs a shared typed array");
        }
        let v = coerce_integer(vm, o, &c.arg(2))?;
        let t = vm.to_number(&c.arg(3))?;
        let _timeout = if t.is_nan() { f64::INFINITY } else { t.max(0.0) };
        let cur = vm.ta_get(o, i as f64);
        let expected = match (&cur, &v) {
            (Value::Number(_), Value::Number(n)) => Value::Number(super::super::ops::f64_to_int32(*n) as f64),
            (Value::BigInt(_), Value::BigInt(b)) => Value::BigInt(std::rc::Rc::new(b.as_int_n(64))),
            _ => v.clone(),
        };
        if !strict_equals(&cur, &expected) {
            return Ok(Value::str("not-equal"));
        }
        // No other agent can notify us: the wait always times out.
        Ok(Value::str("timed-out"))
    });
}

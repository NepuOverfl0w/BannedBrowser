//! Keyed collections (§24): Map, Set, WeakMap, WeakSet, and WeakRef/FinalizationRegistry
//! (§26.1–26.2).

use super::super::heap::*;
use super::super::realm::{self, accessor, method};
use super::super::value::{PropKey, Value};
use super::super::{JsResult, Vm};

#[derive(Clone, Copy, PartialEq)]
enum Coll {
    Map,
    Set,
    WeakMap,
    WeakSet,
}

fn data<'a>(vm: &'a mut Vm, this: &Value, want: Coll) -> Option<&'a mut OrderedMap> {
    let Value::Object(o) = this else { return None };
    match (&mut vm.heap.obj_mut(*o).kind, want) {
        (Kind::Map(m), Coll::Map)
        | (Kind::Set(m), Coll::Set)
        | (Kind::WeakMap(m), Coll::WeakMap)
        | (Kind::WeakSet(m), Coll::WeakSet) => Some(m),
        _ => None,
    }
}

fn check(vm: &mut Vm, this: &Value, want: Coll) -> JsResult<()> {
    if data(vm, this, want).is_some() {
        Ok(())
    } else {
        let n = match want {
            Coll::Map => "Map",
            Coll::Set => "Set",
            Coll::WeakMap => "WeakMap",
            Coll::WeakSet => "WeakSet",
        };
        vm.type_error(&format!("Method called on incompatible receiver: not a {n}"))
    }
}

/// CanBeHeldWeakly (§9.13): objects and unregistered symbols.
pub(super) fn can_be_held_weakly(v: &Value) -> bool {
    match v {
        Value::Object(_) => true,
        Value::Symbol(s) => !s.0.registered,
        _ => false,
    }
}

fn ctor(vm: &mut Vm, c: NativeCall, which: Coll) -> JsResult<Value> {
    let Some(nt) = c.new_target else { return vm.type_error("Constructor requires 'new'") };
    let (proto, kind) = match which {
        Coll::Map => (vm.prototype_from_ctor(nt, |r| r.map_proto)?, Kind::Map(Default::default())),
        Coll::Set => (vm.prototype_from_ctor(nt, |r| r.set_proto)?, Kind::Set(Default::default())),
        Coll::WeakMap => {
            (vm.prototype_from_ctor(nt, |r| r.weakmap_proto)?, Kind::WeakMap(Default::default()))
        }
        Coll::WeakSet => {
            (vm.prototype_from_ctor(nt, |r| r.weakset_proto)?, Kind::WeakSet(Default::default()))
        }
    };
    let o = vm.heap.alloc(Object::new(Some(proto), kind));
    let it = c.arg(0);
    if it.is_nullish() {
        return Ok(Value::Object(o));
    }
    let adder_name = if matches!(which, Coll::Map | Coll::WeakMap) { "set" } else { "add" };
    let adder = vm.get(o, &PropKey::named(adder_name))?;
    if !vm.is_callable(&adder) {
        return vm.type_error(&format!("'{adder_name}' is not a function"));
    }
    let rec = vm.get_iterator(&it)?;
    while let Some(next) = vm.iter_step(rec)? {
        let r = (|| -> JsResult<()> {
            if matches!(which, Coll::Map | Coll::WeakMap) {
                let Value::Object(e) = next else {
                    return vm.type_error("Iterator value is not an entry object");
                };
                let k = vm.get(e, &PropKey::Index(0))?;
                let v = vm.get(e, &PropKey::Index(1))?;
                vm.call(&adder, Value::Object(o), vec![k, v])?;
            } else {
                vm.call(&adder, Value::Object(o), vec![next])?;
            }
            Ok(())
        })();
        if let Err(e) = r {
            if !vm.terminated {
                let _ = vm.iterator_close(rec);
            }
            return Err(e);
        }
    }
    Ok(Value::Object(o))
}

fn iter(vm: &mut Vm, this: &Value, which: Coll, kind: IterKind) -> JsResult<Value> {
    check(vm, this, which)?;
    let proto = if which == Coll::Map { vm.realm.map_iterator_proto } else { vm.realm.set_iterator_proto };
    Ok(Value::Object(
        vm.heap.alloc(Object::new(Some(proto), Kind::MapIter { map: this.as_object(), index: 0, kind })),
    ))
}

/// %MapIteratorPrototype%.next / %SetIteratorPrototype%.next.
fn iter_next(vm: &mut Vm, c: NativeCall, set: bool) -> JsResult<Value> {
    let Value::Object(it) = c.this else { return vm.type_error("not a collection iterator") };
    let (map, index, kind) = match &vm.heap.obj(it).kind {
        Kind::MapIter { map, index, kind } => (*map, *index, *kind),
        _ => return vm.type_error("not a collection iterator"),
    };
    let proto_ok = matches!(vm.heap.obj(it).kind, Kind::MapIter { .. });
    if !proto_ok {
        return vm.type_error("not a collection iterator");
    }
    let Some(m) = map else { return Ok(vm.iter_result(Value::Undefined, true)) };
    let entries_len = match &vm.heap.obj(m).kind {
        Kind::Map(d) | Kind::Set(d) => d.entries.len(),
        _ => 0,
    };
    let mut i = index;
    while i < entries_len {
        let e = match &vm.heap.obj(m).kind {
            Kind::Map(d) | Kind::Set(d) => d.entries[i].clone(),
            _ => None,
        };
        i += 1;
        if let Some((k, v)) = e {
            if let Kind::MapIter { index, .. } = &mut vm.heap.obj_mut(it).kind {
                *index = i;
            }
            let r = match (kind, set) {
                (IterKind::Keys, _) | (IterKind::Values, true) => k,
                (IterKind::Values, false) => v,
                (IterKind::Entries, true) => Value::Object(vm.new_array(vec![k.clone(), k])),
                (IterKind::Entries, false) => Value::Object(vm.new_array(vec![k, v])),
            };
            return Ok(vm.iter_result(r, false));
        }
    }
    if let Kind::MapIter { map, .. } = &mut vm.heap.obj_mut(it).kind {
        *map = None;
    }
    Ok(vm.iter_result(Value::Undefined, true))
}

fn for_each(vm: &mut Vm, c: NativeCall, which: Coll) -> JsResult<Value> {
    check(vm, &c.this, which)?;
    let f = c.arg(0);
    if !vm.is_callable(&f) {
        return vm.type_error("callback is not a function");
    }
    let mut i = 0;
    loop {
        let e = match data(vm, &c.this, which) {
            Some(d) if i < d.entries.len() => d.entries[i].clone(),
            _ => break,
        };
        i += 1;
        if let Some((k, v)) = e {
            let args = if which == Coll::Set {
                vec![k.clone(), k, c.this.clone()]
            } else {
                vec![v, k, c.this.clone()]
            };
            vm.call(&f, c.arg(1), args)?;
        }
    }
    Ok(Value::Undefined)
}

/// GetSetRecord (§24.2.1.2): (object, size, has, keys).
fn set_record(vm: &mut Vm, v: &Value) -> JsResult<(Value, f64, Value, Value)> {
    let Value::Object(o) = v else { return vm.type_error("set-like argument is not an object") };
    let raw = vm.get(*o, &PropKey::lit("size"))?;
    let num = vm.to_number(&raw)?;
    if num.is_nan() {
        return vm.type_error("set-like size is NaN");
    }
    let size = vm.to_integer_or_infinity(&Value::Number(num))?;
    if size < 0.0 {
        return vm.range_error("set-like size is negative");
    }
    let has = vm.get(*o, &PropKey::lit("has"))?;
    if !vm.is_callable(&has) {
        return vm.type_error("set-like 'has' is not a function");
    }
    let keys = vm.get(*o, &PropKey::lit("keys"))?;
    if !vm.is_callable(&keys) {
        return vm.type_error("set-like 'keys' is not a function");
    }
    Ok((v.clone(), size, has, keys))
}

fn set_values(vm: &mut Vm, this: &Value) -> Vec<Value> {
    data(vm, this, Coll::Set)
        .map(|d| d.entries.iter().flatten().map(|e| e.0.clone()).collect())
        .unwrap_or_default()
}

fn new_set(vm: &mut Vm, vals: Vec<Value>) -> Value {
    let mut m = OrderedMap::default();
    for v in vals {
        m.set(v, Value::Undefined);
    }
    let p = vm.realm.set_proto;
    Value::Object(vm.heap.alloc(Object::new(Some(p), Kind::Set(Box::new(m)))))
}

/// Iterate a set-like's `keys()` (GetIteratorFromMethod over the record).
fn other_keys(vm: &mut Vm, rec: &(Value, f64, Value, Value)) -> JsResult<ObjRef> {
    let it = vm.call(&rec.3, rec.0.clone(), Vec::new())?;
    let Value::Object(_) = it else { return vm.type_error("keys() result is not an object") };
    let next = vm.get_value_key(&it, &PropKey::lit("next"))?;
    Ok(vm
        .heap
        .alloc(Object::new(None, Kind::IterRecord(Box::new(IterRecord { iterator: it, next, done: false })))))
}

fn norm(v: Value) -> Value {
    match v {
        Value::Number(n) if n == 0.0 && n.is_sign_negative() => Value::Number(0.0),
        v => v,
    }
}

#[derive(Clone, Copy)]
enum SetOp {
    Union,
    Intersection,
    Difference,
    SymDiff,
    Subset,
    Superset,
    Disjoint,
}

/// The ES2025 Set methods (§24.2.4.*).
fn set_op(vm: &mut Vm, c: NativeCall, op: SetOp) -> JsResult<Value> {
    check(vm, &c.this, Coll::Set)?;
    let rec = set_record(vm, &c.arg(0))?;
    let this_size = data(vm, &c.this, Coll::Set).map(|d| d.size()).unwrap_or(0) as f64;
    let has_this = |vm: &mut Vm, k: &Value| data(vm, &c.this, Coll::Set).is_some_and(|d| d.has(k));
    let other_has = |vm: &mut Vm, k: &Value| -> JsResult<bool> {
        Ok(vm.call(&rec.2, rec.0.clone(), vec![k.clone()])?.truthy())
    };
    match op {
        SetOp::Union | SetOp::SymDiff => {
            let mut result = set_values(vm, &c.this);
            let mut m = OrderedMap::default();
            for v in result.drain(..) {
                m.set(v, Value::Undefined);
            }
            let keys = other_keys(vm, &rec)?;
            while let Some(k) = vm.iter_step(keys)? {
                let k = norm(k);
                if let SetOp::SymDiff = op {
                    if has_this(vm, &k) {
                        m.delete(&k);
                    } else if !m.has(&k) {
                        m.set(k, Value::Undefined);
                    }
                } else {
                    m.set(k, Value::Undefined);
                }
            }
            let p = vm.realm.set_proto;
            Ok(Value::Object(vm.heap.alloc(Object::new(Some(p), Kind::Set(Box::new(m))))))
        }
        SetOp::Intersection => {
            let mut out = Vec::new();
            if this_size <= rec.1 {
                let mut i = 0;
                loop {
                    let e = match data(vm, &c.this, Coll::Set) {
                        Some(d) if i < d.entries.len() => d.entries[i].clone(),
                        _ => break,
                    };
                    i += 1;
                    if let Some((k, _)) = e {
                        if other_has(vm, &k)? {
                            out.push(k);
                        }
                    }
                }
            } else {
                let keys = other_keys(vm, &rec)?;
                while let Some(k) = vm.iter_step(keys)? {
                    let k = norm(k);
                    if has_this(vm, &k) && !out.iter().any(|x| super::super::value::same_value_zero(x, &k)) {
                        out.push(k);
                    }
                }
            }
            Ok(new_set(vm, out))
        }
        SetOp::Difference => {
            let mut m = OrderedMap::default();
            for v in set_values(vm, &c.this) {
                m.set(v, Value::Undefined);
            }
            if this_size <= rec.1 {
                let mut i = 0;
                loop {
                    let e = match data(vm, &c.this, Coll::Set) {
                        Some(d) if i < d.entries.len() => d.entries[i].clone(),
                        _ => break,
                    };
                    i += 1;
                    if let Some((k, _)) = e {
                        if other_has(vm, &k)? {
                            m.delete(&k);
                        }
                    }
                }
            } else {
                let keys = other_keys(vm, &rec)?;
                while let Some(k) = vm.iter_step(keys)? {
                    m.delete(&norm(k));
                }
            }
            let p = vm.realm.set_proto;
            Ok(Value::Object(vm.heap.alloc(Object::new(Some(p), Kind::Set(Box::new(m))))))
        }
        SetOp::Subset => {
            if this_size > rec.1 {
                return Ok(Value::Bool(false));
            }
            let mut i = 0;
            loop {
                let e = match data(vm, &c.this, Coll::Set) {
                    Some(d) if i < d.entries.len() => d.entries[i].clone(),
                    _ => break,
                };
                i += 1;
                if let Some((k, _)) = e {
                    if !other_has(vm, &k)? {
                        return Ok(Value::Bool(false));
                    }
                }
            }
            Ok(Value::Bool(true))
        }
        SetOp::Superset => {
            if this_size < rec.1 {
                return Ok(Value::Bool(false));
            }
            let keys = other_keys(vm, &rec)?;
            while let Some(k) = vm.iter_step(keys)? {
                if !has_this(vm, &k) {
                    vm.iterator_close(keys)?;
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        SetOp::Disjoint => {
            if this_size <= rec.1 {
                let mut i = 0;
                loop {
                    let e = match data(vm, &c.this, Coll::Set) {
                        Some(d) if i < d.entries.len() => d.entries[i].clone(),
                        _ => break,
                    };
                    i += 1;
                    if let Some((k, _)) = e {
                        if other_has(vm, &k)? {
                            return Ok(Value::Bool(false));
                        }
                    }
                }
            } else {
                let keys = other_keys(vm, &rec)?;
                while let Some(k) = vm.iter_step(keys)? {
                    if has_this(vm, &k) {
                        vm.iterator_close(keys)?;
                        return Ok(Value::Bool(false));
                    }
                }
            }
            Ok(Value::Bool(true))
        }
    }
}

/// Map.groupBy / Object.groupBy (§7.3.35 GroupBy).
pub(super) fn group_by(
    vm: &mut Vm,
    items: &Value,
    f: &Value,
    property_keys: bool,
) -> JsResult<Vec<(Value, Vec<Value>)>> {
    if items.is_nullish() {
        return vm.type_error("groupBy called on null or undefined");
    }
    if !vm.is_callable(f) {
        return vm.type_error("groupBy callback is not a function");
    }
    let rec = vm.get_iterator(items)?;
    let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
    let mut k = 0.0;
    while let Some(v) = vm.iter_step(rec)? {
        let key = match vm.call(f, Value::Undefined, vec![v.clone(), Value::Number(k)]) {
            Ok(key) => key,
            Err(e) => {
                if !vm.terminated {
                    let _ = vm.iterator_close(rec);
                }
                return Err(e);
            }
        };
        let key = if property_keys {
            match vm.to_property_key(&key) {
                Ok(pk) => pk.to_value(),
                Err(e) => {
                    let _ = vm.iterator_close(rec);
                    return Err(e);
                }
            }
        } else {
            norm(key)
        };
        match groups.iter_mut().find(|g| super::super::value::same_value(&g.0, &key)) {
            Some(g) => g.1.push(v),
            None => groups.push((key, vec![v])),
        }
        k += 1.0;
    }
    Ok(groups)
}

/// Map/WeakMap.prototype.getOrInsert(Computed) (the upsert proposal, stage 3).
fn get_or_insert(vm: &mut Vm, c: NativeCall, which: Coll, computed: bool) -> JsResult<Value> {
    check(vm, &c.this, which)?;
    let key = norm(c.arg(0));
    if which == Coll::WeakMap && !can_be_held_weakly(&key) {
        return vm.type_error("Invalid value used as weak map key");
    }
    if computed && !vm.is_callable(&c.arg(1)) {
        return vm.type_error("getOrInsertComputed: callback is not a function");
    }
    if let Some(v) = data(vm, &c.this, which).and_then(|d| d.get(&key).cloned()) {
        return Ok(v);
    }
    let v = if computed { vm.call(&c.arg(1), Value::Undefined, vec![key.clone()])? } else { c.arg(1) };
    if let Some(d) = data(vm, &c.this, which) {
        d.set(key, v.clone());
    }
    Ok(v)
}

pub(super) fn install(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let ip = vm.realm.iterator_proto;
    let species = vm.realm.sym_species.clone();
    let si = vm.realm.sym_iterator.clone();

    // Map
    let mp = vm.new_object_with_proto(Some(op));
    vm.realm.map_proto = mp;
    let mc = realm::constructor(vm, "Map", 0, |vm, c| ctor(vm, c, Coll::Map), mp);
    accessor(vm, mc, PropKey::Sym(species.clone()), Some(|_, c| Ok(c.this)), None);
    method(vm, mc, "groupBy", 2, |vm, c| {
        let groups = group_by(vm, &c.arg(0), &c.arg(1), false)?;
        let mut m = OrderedMap::default();
        for (k, vals) in groups {
            let a = vm.new_array(vals);
            m.set(k, Value::Object(a));
        }
        let p = vm.realm.map_proto;
        Ok(Value::Object(vm.heap.alloc(Object::new(Some(p), Kind::Map(Box::new(m))))))
    });
    method(vm, mp, "get", 1, |vm, c| {
        check(vm, &c.this, Coll::Map)?;
        Ok(data(vm, &c.this, Coll::Map).and_then(|d| d.get(&c.arg(0)).cloned()).unwrap_or_default())
    });
    method(vm, mp, "set", 2, |vm, c| {
        check(vm, &c.this, Coll::Map)?;
        if let Some(d) = data(vm, &c.this, Coll::Map) {
            d.set(c.arg(0), c.arg(1));
        }
        Ok(c.this)
    });
    method(vm, mp, "has", 1, |vm, c| {
        check(vm, &c.this, Coll::Map)?;
        Ok(Value::Bool(data(vm, &c.this, Coll::Map).is_some_and(|d| d.has(&c.arg(0)))))
    });
    method(vm, mp, "delete", 1, |vm, c| {
        check(vm, &c.this, Coll::Map)?;
        Ok(Value::Bool(data(vm, &c.this, Coll::Map).is_some_and(|d| d.delete(&c.arg(0)))))
    });
    method(vm, mp, "clear", 0, |vm, c| {
        check(vm, &c.this, Coll::Map)?;
        if let Some(d) = data(vm, &c.this, Coll::Map) {
            d.clear();
        }
        Ok(Value::Undefined)
    });
    method(vm, mp, "forEach", 1, |vm, c| for_each(vm, c, Coll::Map));
    method(vm, mp, "getOrInsert", 2, |vm, c| get_or_insert(vm, c, Coll::Map, false));
    method(vm, mp, "getOrInsertComputed", 2, |vm, c| get_or_insert(vm, c, Coll::Map, true));
    accessor(
        vm,
        mp,
        PropKey::lit("size"),
        Some(|vm, c| {
            check(vm, &c.this, Coll::Map)?;
            Ok(Value::Number(data(vm, &c.this, Coll::Map).map(|d| d.size()).unwrap_or(0) as f64))
        }),
        None,
    );
    method(vm, mp, "keys", 0, |vm, c| iter(vm, &c.this, Coll::Map, IterKind::Keys));
    method(vm, mp, "values", 0, |vm, c| iter(vm, &c.this, Coll::Map, IterKind::Values));
    let me = method(vm, mp, "entries", 0, |vm, c| iter(vm, &c.this, Coll::Map, IterKind::Entries));
    vm.heap.obj_mut(mp).props.insert(PropKey::Sym(si.clone()), Prop::method(Value::Object(me)));
    realm::to_string_tag(vm, mp, "Map");
    let mip = vm.new_object_with_proto(Some(ip));
    vm.realm.map_iterator_proto = mip;
    method(vm, mip, "next", 0, |vm, c| iter_next(vm, c, false));
    realm::to_string_tag(vm, mip, "Map Iterator");

    // Set
    let sp = vm.new_object_with_proto(Some(op));
    vm.realm.set_proto = sp;
    let sc = realm::constructor(vm, "Set", 0, |vm, c| ctor(vm, c, Coll::Set), sp);
    accessor(vm, sc, PropKey::Sym(species), Some(|_, c| Ok(c.this)), None);
    method(vm, sp, "add", 1, |vm, c| {
        check(vm, &c.this, Coll::Set)?;
        if let Some(d) = data(vm, &c.this, Coll::Set) {
            let k = c.arg(0);
            if !d.has(&k) {
                d.set(k, Value::Undefined);
            }
        }
        Ok(c.this)
    });
    method(vm, sp, "has", 1, |vm, c| {
        check(vm, &c.this, Coll::Set)?;
        Ok(Value::Bool(data(vm, &c.this, Coll::Set).is_some_and(|d| d.has(&c.arg(0)))))
    });
    method(vm, sp, "delete", 1, |vm, c| {
        check(vm, &c.this, Coll::Set)?;
        Ok(Value::Bool(data(vm, &c.this, Coll::Set).is_some_and(|d| d.delete(&c.arg(0)))))
    });
    method(vm, sp, "clear", 0, |vm, c| {
        check(vm, &c.this, Coll::Set)?;
        if let Some(d) = data(vm, &c.this, Coll::Set) {
            d.clear();
        }
        Ok(Value::Undefined)
    });
    method(vm, sp, "forEach", 1, |vm, c| for_each(vm, c, Coll::Set));
    accessor(
        vm,
        sp,
        PropKey::lit("size"),
        Some(|vm, c| {
            check(vm, &c.this, Coll::Set)?;
            Ok(Value::Number(data(vm, &c.this, Coll::Set).map(|d| d.size()).unwrap_or(0) as f64))
        }),
        None,
    );
    method(vm, sp, "entries", 0, |vm, c| iter(vm, &c.this, Coll::Set, IterKind::Entries));
    let sv = method(vm, sp, "values", 0, |vm, c| iter(vm, &c.this, Coll::Set, IterKind::Values));
    vm.heap.obj_mut(sp).props.insert(PropKey::lit("keys"), Prop::method(Value::Object(sv)));
    vm.heap.obj_mut(sp).props.insert(PropKey::Sym(si), Prop::method(Value::Object(sv)));
    for (name, o) in [
        ("union", SetOp::Union),
        ("intersection", SetOp::Intersection),
        ("difference", SetOp::Difference),
        ("symmetricDifference", SetOp::SymDiff),
        ("isSubsetOf", SetOp::Subset),
        ("isSupersetOf", SetOp::Superset),
        ("isDisjointFrom", SetOp::Disjoint),
    ] {
        let f: NativeFn = match o {
            SetOp::Union => |vm, c| set_op(vm, c, SetOp::Union),
            SetOp::Intersection => |vm, c| set_op(vm, c, SetOp::Intersection),
            SetOp::Difference => |vm, c| set_op(vm, c, SetOp::Difference),
            SetOp::SymDiff => |vm, c| set_op(vm, c, SetOp::SymDiff),
            SetOp::Subset => |vm, c| set_op(vm, c, SetOp::Subset),
            SetOp::Superset => |vm, c| set_op(vm, c, SetOp::Superset),
            SetOp::Disjoint => |vm, c| set_op(vm, c, SetOp::Disjoint),
        };
        method(vm, sp, name, 1, f);
    }
    realm::to_string_tag(vm, sp, "Set");
    let sip = vm.new_object_with_proto(Some(ip));
    vm.realm.set_iterator_proto = sip;
    method(vm, sip, "next", 0, |vm, c| iter_next(vm, c, true));
    realm::to_string_tag(vm, sip, "Set Iterator");

    // WeakMap
    let wmp = vm.new_object_with_proto(Some(op));
    vm.realm.weakmap_proto = wmp;
    realm::constructor(vm, "WeakMap", 0, |vm, c| ctor(vm, c, Coll::WeakMap), wmp);
    method(vm, wmp, "get", 1, |vm, c| {
        check(vm, &c.this, Coll::WeakMap)?;
        Ok(data(vm, &c.this, Coll::WeakMap).and_then(|d| d.get(&c.arg(0)).cloned()).unwrap_or_default())
    });
    method(vm, wmp, "set", 2, |vm, c| {
        check(vm, &c.this, Coll::WeakMap)?;
        if !can_be_held_weakly(&c.arg(0)) {
            return vm.type_error("Invalid value used as weak map key");
        }
        if let Some(d) = data(vm, &c.this, Coll::WeakMap) {
            d.set(c.arg(0), c.arg(1));
        }
        Ok(c.this)
    });
    method(vm, wmp, "has", 1, |vm, c| {
        check(vm, &c.this, Coll::WeakMap)?;
        Ok(Value::Bool(data(vm, &c.this, Coll::WeakMap).is_some_and(|d| d.has(&c.arg(0)))))
    });
    method(vm, wmp, "delete", 1, |vm, c| {
        check(vm, &c.this, Coll::WeakMap)?;
        Ok(Value::Bool(data(vm, &c.this, Coll::WeakMap).is_some_and(|d| d.delete(&c.arg(0)))))
    });
    method(vm, wmp, "getOrInsert", 2, |vm, c| get_or_insert(vm, c, Coll::WeakMap, false));
    method(vm, wmp, "getOrInsertComputed", 2, |vm, c| get_or_insert(vm, c, Coll::WeakMap, true));
    realm::to_string_tag(vm, wmp, "WeakMap");

    // WeakSet
    let wsp = vm.new_object_with_proto(Some(op));
    vm.realm.weakset_proto = wsp;
    realm::constructor(vm, "WeakSet", 0, |vm, c| ctor(vm, c, Coll::WeakSet), wsp);
    method(vm, wsp, "add", 1, |vm, c| {
        check(vm, &c.this, Coll::WeakSet)?;
        if !can_be_held_weakly(&c.arg(0)) {
            return vm.type_error("Invalid value used in weak set");
        }
        if let Some(d) = data(vm, &c.this, Coll::WeakSet) {
            d.set(c.arg(0), Value::Undefined);
        }
        Ok(c.this)
    });
    method(vm, wsp, "has", 1, |vm, c| {
        check(vm, &c.this, Coll::WeakSet)?;
        Ok(Value::Bool(data(vm, &c.this, Coll::WeakSet).is_some_and(|d| d.has(&c.arg(0)))))
    });
    method(vm, wsp, "delete", 1, |vm, c| {
        check(vm, &c.this, Coll::WeakSet)?;
        Ok(Value::Bool(data(vm, &c.this, Coll::WeakSet).is_some_and(|d| d.delete(&c.arg(0)))))
    });
    realm::to_string_tag(vm, wsp, "WeakSet");

    // WeakRef (§26.1)
    let wrp = vm.new_object_with_proto(Some(op));
    vm.realm.weakref_proto = wrp;
    realm::constructor(
        vm,
        "WeakRef",
        1,
        |vm, c| {
            let Some(nt) = c.new_target else { return vm.type_error("Constructor WeakRef requires 'new'") };
            if !can_be_held_weakly(&c.arg(0)) {
                return vm.type_error("WeakRef: invalid target");
            }
            let p = vm.prototype_from_ctor(nt, |r| r.weakref_proto)?;
            Ok(Value::Object(vm.heap.alloc(Object::new(Some(p), Kind::WeakRef(c.arg(0))))))
        },
        wrp,
    );
    method(vm, wrp, "deref", 0, |vm, c| match &c.this {
        Value::Object(o) => match &vm.heap.obj(*o).kind {
            Kind::WeakRef(v) => Ok(v.clone()),
            _ => vm.type_error("not a WeakRef"),
        },
        _ => vm.type_error("not a WeakRef"),
    });
    realm::to_string_tag(vm, wrp, "WeakRef");

    // FinalizationRegistry (§26.2)
    let frp = vm.new_object_with_proto(Some(op));
    vm.realm.finreg_proto = frp;
    realm::constructor(
        vm,
        "FinalizationRegistry",
        1,
        |vm, c| {
            let Some(nt) = c.new_target else {
                return vm.type_error("Constructor FinalizationRegistry requires 'new'");
            };
            if !vm.is_callable(&c.arg(0)) {
                return vm.type_error("FinalizationRegistry: cleanup must be callable");
            }
            let p = vm.prototype_from_ctor(nt, |r| r.finreg_proto)?;
            Ok(Value::Object(
                vm.heap.alloc(Object::new(Some(p), Kind::FinalizationRegistry(c.arg(0), Vec::new()))),
            ))
        },
        frp,
    );
    fn finreg(vm: &mut Vm, this: &Value) -> JsResult<ObjRef> {
        match this {
            Value::Object(o) if matches!(vm.heap.obj(*o).kind, Kind::FinalizationRegistry(..)) => Ok(*o),
            _ => vm.type_error("not a FinalizationRegistry"),
        }
    }
    method(vm, frp, "register", 2, |vm, c| {
        let o = finreg(vm, &c.this)?;
        let (target, held, token) = (c.arg(0), c.arg(1), c.arg(2));
        if !can_be_held_weakly(&target) {
            return vm.type_error("FinalizationRegistry.register: invalid target");
        }
        if super::super::value::same_value(&target, &held) {
            return vm.type_error("FinalizationRegistry.register: target and holdings must not be the same");
        }
        if !token.is_undefined() && !can_be_held_weakly(&token) {
            return vm.type_error("FinalizationRegistry.register: invalid unregister token");
        }
        if let Kind::FinalizationRegistry(_, cells) = &mut vm.heap.obj_mut(o).kind {
            cells.push((target, held, token));
        }
        Ok(Value::Undefined)
    });
    method(vm, frp, "unregister", 1, |vm, c| {
        let o = finreg(vm, &c.this)?;
        let token = c.arg(0);
        if !can_be_held_weakly(&token) {
            return vm.type_error("FinalizationRegistry.unregister: invalid token");
        }
        let mut removed = false;
        if let Kind::FinalizationRegistry(_, cells) = &mut vm.heap.obj_mut(o).kind {
            let before = cells.len();
            cells.retain(|cell| !super::super::value::same_value(&cell.2, &token));
            removed = cells.len() != before;
        }
        Ok(Value::Bool(removed))
    });
    realm::to_string_tag(vm, frp, "FinalizationRegistry");

    // Object.groupBy (§20.1.2.12)
    let oc = vm.realm.object_ctor;
    method(vm, oc, "groupBy", 2, |vm, c| {
        let groups = group_by(vm, &c.arg(0), &c.arg(1), true)?;
        let o = vm.new_object_with_proto(None);
        for (k, vals) in groups {
            let a = vm.new_array(vals);
            let pk = vm.to_property_key(&k)?;
            vm.create_data_property_or_throw(o, pk, Value::Object(a))?;
        }
        Ok(Value::Object(o))
    });
}

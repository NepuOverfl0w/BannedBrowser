//! The Iterator constructor and the iterator helpers (§27.1.3–4, ES2025), plus
//! Iterator.concat. Helpers are native state machines over the underlying iterator.

use super::super::heap::*;
use super::super::realm::{self, accessor, method, native_with};
use super::super::value::{PropKey, Value};
use super::super::{ErrorKind, JsResult, Vm};

pub enum HelperKind {
    Map(Value),
    Filter(Value),
    Take(f64),
    Drop(f64),
    FlatMap(Value, Option<(Value, Value)>),
    /// Iterator.concat: (iterable, method) pairs still to go, and the current inner iterator.
    Concat(Vec<(Value, Value)>, Option<(Value, Value)>),
    /// Iterator.prototype.chunks (iterator-chunking): size; the source is exhausted.
    Chunks(usize, bool),
    /// Iterator.prototype.windows: size, "allow-partial", the window, exhausted.
    Windows(usize, bool, Vec<Value>, bool),
    /// Iterator.zip / Iterator.zipKeyed (joint-iteration, IteratorZip).
    Zip(Box<Zip>),
}

#[derive(Clone, Copy, PartialEq)]
pub enum ZipMode {
    Shortest,
    Longest,
    Strict,
}

pub struct Zip {
    /// The iterators' (iterator, next) records, in order.
    iters: Vec<(Value, Value)>,
    /// openIters membership by index.
    open: Vec<bool>,
    /// A finished iterator in "longest" mode (its slot uses the padding).
    finished: Vec<bool>,
    mode: ZipMode,
    padding: Vec<Value>,
    /// zipKeyed: the result objects' keys.
    keys: Option<Vec<PropKey>>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum HelperState {
    Start,
    Suspended,
    Running,
    Done,
}

pub struct Helper {
    pub iterator: Value,
    pub next: Value,
    pub kind: HelperKind,
    pub counter: f64,
    pub state: HelperState,
}

impl Helper {
    pub fn trace(&self, os: &mut Vec<ObjRef>) {
        let mut push = |v: &Value| {
            if let Value::Object(o) = v {
                os.push(*o)
            }
        };
        push(&self.iterator);
        push(&self.next);
        match &self.kind {
            HelperKind::Map(f) | HelperKind::Filter(f) => push(f),
            HelperKind::FlatMap(f, inner) => {
                push(f);
                if let Some((a, b)) = inner {
                    push(a);
                    push(b);
                }
            }
            HelperKind::Windows(_, _, buf, _) => buf.iter().for_each(push),
            HelperKind::Zip(z) => {
                for (a, b) in &z.iters {
                    push(a);
                    push(b);
                }
                z.padding.iter().for_each(push);
            }
            HelperKind::Concat(rest, inner) => {
                for (a, b) in rest {
                    push(a);
                    push(b);
                }
                if let Some((a, b)) = inner {
                    push(a);
                    push(b);
                }
            }
            _ => {}
        }
    }
}

/// IteratorStepValue on a plain (iterator, next) pair.
fn step(vm: &mut Vm, it: &Value, next: &Value) -> JsResult<Option<Value>> {
    let r = vm.call(next, it.clone(), Vec::new())?;
    let Value::Object(ro) = r else { return vm.type_error("Iterator result is not an object") };
    let cond = vm.get(ro, &PropKey::lit("done"))?;
    if vm.to_boolean(&cond) {
        return Ok(None);
    }
    Ok(Some(vm.get(ro, &PropKey::lit("value"))?))
}

/// IteratorClose with a normal completion.
fn close(vm: &mut Vm, it: &Value) -> JsResult<()> {
    if let Some(f) = vm.get_method(it, &PropKey::lit("return"))? {
        let r = vm.call(&f, it.clone(), Vec::new())?;
        if !matches!(r, Value::Object(_)) {
            return vm.type_error("Iterator result is not an object");
        }
    }
    Ok(())
}

/// IteratorClose with a throw completion: errors from `return` are dropped.
fn close_throw<T>(vm: &mut Vm, it: &Value, e: Value) -> JsResult<T> {
    if !vm.terminated {
        if let Ok(Some(f)) = vm.get_method(it, &PropKey::lit("return")) {
            let _ = vm.call(&f, it.clone(), Vec::new());
        }
    }
    Err(e)
}

/// GetIteratorDirect (§7.4.2).
fn direct(vm: &mut Vm, o: &Value) -> JsResult<(Value, Value)> {
    let next = vm.get_value_key(o, &PropKey::lit("next"))?;
    Ok((o.clone(), next))
}

/// GetIteratorFlattenable (§7.4.13).
fn flattenable(vm: &mut Vm, v: &Value, iterate_strings: bool) -> JsResult<(Value, Value)> {
    if !matches!(v, Value::Object(_)) && !(iterate_strings && matches!(v, Value::String(_))) {
        return vm.type_error("value is not an object");
    }
    let ik = PropKey::Sym(vm.realm.sym_iterator.clone());
    let it = match vm.get_method(v, &ik)? {
        None => v.clone(),
        Some(m) => vm.call(&m, v.clone(), Vec::new())?,
    };
    if !matches!(it, Value::Object(_)) {
        return vm.type_error("iterator is not an object");
    }
    direct(vm, &it)
}

fn this_object(vm: &mut Vm, c: &NativeCall) -> JsResult<Value> {
    if !matches!(c.this, Value::Object(_)) {
        return vm.type_error("Iterator.prototype method called on a non-object");
    }
    Ok(c.this.clone())
}

fn new_helper(vm: &mut Vm, iterator: Value, next: Value, kind: HelperKind) -> Value {
    let p = vm.realm.iterator_helper_proto;
    Value::Object(vm.heap.alloc(Object::new(
        Some(p),
        Kind::IterHelper(Box::new(Helper { iterator, next, kind, counter: 0.0, state: HelperState::Start })),
    )))
}

fn callable_or_close(vm: &mut Vm, o: &Value, f: &Value) -> JsResult<()> {
    if !vm.is_callable(f) {
        let e = vm.error(super::super::ErrorKind::Type, "argument is not a function");
        return close_throw(vm, o, e);
    }
    Ok(())
}

fn limit_or_close(vm: &mut Vm, o: &Value, v: &Value) -> JsResult<f64> {
    let n = match vm.to_number(v) {
        Ok(n) => n,
        Err(e) => return close_throw(vm, o, e),
    };
    if n.is_nan() {
        let e = vm.error(super::super::ErrorKind::Range, "limit must be a number");
        return close_throw(vm, o, e);
    }
    if n.is_finite() && n > 9007199254740991.0 {
        let e = vm.error(super::super::ErrorKind::Range, "limit is too large");
        return close_throw(vm, o, e);
    }
    let i = vm.to_integer_or_infinity(&Value::Number(n))?;
    if i < 0.0 {
        let e = vm.error(super::super::ErrorKind::Range, "limit must be non-negative");
        return close_throw(vm, o, e);
    }
    Ok(i)
}

/// The chunk or window size: an integral Number in [1, 2^32 − 1], else close and throw.
fn size_or_close(vm: &mut Vm, o: &Value, v: &Value) -> JsResult<usize> {
    let Value::Number(n) = *v else {
        let e = vm.error(ErrorKind::Type, "the size must be a number");
        return close_throw(vm, o, e);
    };
    if !n.is_finite() || n.fract() != 0.0 {
        let e = vm.error(ErrorKind::Type, "the size must be an integral number");
        return close_throw(vm, o, e);
    }
    if !(1.0..=4294967295.0).contains(&n) {
        let e = vm.error(ErrorKind::Range, "the size must be between 1 and 2^32 - 1");
        return close_throw(vm, o, e);
    }
    Ok(n as usize)
}

fn helper_parts(vm: &mut Vm, this: &Value) -> JsResult<ObjRef> {
    match this {
        Value::Object(o) if matches!(vm.heap.obj(*o).kind, Kind::IterHelper(_)) => Ok(*o),
        _ => vm.type_error("not an Iterator Helper"),
    }
}

fn helper(vm: &mut Vm, o: ObjRef) -> &mut Helper {
    match &mut vm.heap.obj_mut(o).kind {
        Kind::IterHelper(h) => h,
        _ => unreachable!("iterator helper"),
    }
}

/// %IteratorHelperPrototype%.next (§27.1.2.1.1).
fn helper_next(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let o = helper_parts(vm, &c.this)?;
    match helper(vm, o).state {
        HelperState::Running => return vm.type_error("Iterator Helper is already running"),
        HelperState::Done => return Ok(vm.iter_result(Value::Undefined, true)),
        _ => {}
    }
    helper(vm, o).state = HelperState::Running;
    let r = helper_step(vm, o);
    let h = helper(vm, o);
    match &r {
        Ok(Some(_)) => h.state = HelperState::Suspended,
        _ => h.state = HelperState::Done,
    }
    match r? {
        Some(v) => Ok(vm.iter_result(v, false)),
        None => Ok(vm.iter_result(Value::Undefined, true)),
    }
}

fn helper_step(vm: &mut Vm, o: ObjRef) -> JsResult<Option<Value>> {
    let (it, next) = {
        let h = helper(vm, o);
        (h.iterator.clone(), h.next.clone())
    };
    loop {
        let kind_tag = match &helper(vm, o).kind {
            HelperKind::Map(_) => 0,
            HelperKind::Filter(_) => 1,
            HelperKind::Take(_) => 2,
            HelperKind::Drop(_) => 3,
            HelperKind::FlatMap(..) => 4,
            HelperKind::Concat(..) => 5,
            HelperKind::Chunks(..) => 6,
            HelperKind::Windows(..) => 7,
            HelperKind::Zip(_) => return zip_step(vm, o),
        };
        match kind_tag {
            6 => {
                let HelperKind::Chunks(size, done) = helper(vm, o).kind else { unreachable!("chunks") };
                if done {
                    return Ok(None);
                }
                let mut buf = Vec::new();
                while buf.len() < size {
                    match step(vm, &it, &next)? {
                        Some(v) => buf.push(v),
                        None => {
                            helper(vm, o).kind = HelperKind::Chunks(size, true);
                            if buf.is_empty() {
                                return Ok(None);
                            }
                            break;
                        }
                    }
                }
                return Ok(Some(Value::Object(vm.new_array(buf))));
            }
            7 => {
                loop {
                    let (size, partial, done) = match &helper(vm, o).kind {
                        HelperKind::Windows(s, p, _, d) => (*s, *p, *d),
                        _ => unreachable!("windows"),
                    };
                    if done {
                        return Ok(None);
                    }
                    let Some(v) = step(vm, &it, &next)? else {
                        let HelperKind::Windows(_, _, buf, d) = &mut helper(vm, o).kind else {
                            unreachable!("windows")
                        };
                        *d = true;
                        // A window that never filled up (step 4.b.ii.1).
                        if partial && !buf.is_empty() && buf.len() < size {
                            let b = std::mem::take(buf);
                            return Ok(Some(Value::Object(vm.new_array(b))));
                        }
                        return Ok(None);
                    };
                    let HelperKind::Windows(_, _, buf, _) = &mut helper(vm, o).kind else {
                        unreachable!("windows")
                    };
                    if buf.len() == size {
                        buf.remove(0);
                    }
                    buf.push(v);
                    if buf.len() == size {
                        let b = buf.clone();
                        return Ok(Some(Value::Object(vm.new_array(b))));
                    }
                }
            }
            0 | 1 => {
                let Some(v) = step(vm, &it, &next)? else { return Ok(None) };
                let counter = helper(vm, o).counter;
                helper(vm, o).counter += 1.0;
                let f = match &helper(vm, o).kind {
                    HelperKind::Map(f) | HelperKind::Filter(f) => f.clone(),
                    _ => unreachable!("map or filter"),
                };
                let r = match vm.call(&f, Value::Undefined, vec![v.clone(), Value::Number(counter)]) {
                    Ok(r) => r,
                    Err(e) => return close_throw(vm, &it, e),
                };
                if kind_tag == 0 {
                    return Ok(Some(r));
                }
                if vm.to_boolean(&r) {
                    return Ok(Some(v));
                }
            }
            2 => {
                let HelperKind::Take(rem) = helper(vm, o).kind else { unreachable!("take") };
                if rem == 0.0 {
                    close(vm, &it)?;
                    return Ok(None);
                }
                if rem != f64::INFINITY {
                    helper(vm, o).kind = HelperKind::Take(rem - 1.0);
                }
                return step(vm, &it, &next);
            }
            3 => {
                let HelperKind::Drop(mut rem) = helper(vm, o).kind else { unreachable!("drop") };
                while rem > 0.0 {
                    if rem != f64::INFINITY {
                        rem -= 1.0;
                    }
                    helper(vm, o).kind = HelperKind::Drop(rem);
                    if step(vm, &it, &next)?.is_none() {
                        return Ok(None);
                    }
                }
                return step(vm, &it, &next);
            }
            4 => {
                let inner = match &helper(vm, o).kind {
                    HelperKind::FlatMap(_, inner) => inner.clone(),
                    _ => unreachable!("flatMap"),
                };
                if let Some((iit, inext)) = inner {
                    match step(vm, &iit, &inext) {
                        Ok(Some(v)) => return Ok(Some(v)),
                        Ok(None) => {
                            if let HelperKind::FlatMap(_, inner) = &mut helper(vm, o).kind {
                                *inner = None;
                            }
                            continue;
                        }
                        Err(e) => return close_throw(vm, &it, e),
                    }
                }
                let Some(v) = step(vm, &it, &next)? else { return Ok(None) };
                let counter = helper(vm, o).counter;
                helper(vm, o).counter += 1.0;
                let f = match &helper(vm, o).kind {
                    HelperKind::FlatMap(f, _) => f.clone(),
                    _ => unreachable!("flatMap"),
                };
                let mapped = match vm.call(&f, Value::Undefined, vec![v, Value::Number(counter)]) {
                    Ok(m) => m,
                    Err(e) => return close_throw(vm, &it, e),
                };
                let inner = match flattenable(vm, &mapped, false) {
                    Ok(i) => i,
                    Err(e) => return close_throw(vm, &it, e),
                };
                if let HelperKind::FlatMap(_, slot) = &mut helper(vm, o).kind {
                    *slot = Some(inner);
                }
            }
            _ => {
                let inner = match &helper(vm, o).kind {
                    HelperKind::Concat(_, inner) => inner.clone(),
                    _ => unreachable!("concat"),
                };
                if let Some((iit, inext)) = inner {
                    match step(vm, &iit, &inext)? {
                        Some(v) => return Ok(Some(v)),
                        None => {
                            if let HelperKind::Concat(_, inner) = &mut helper(vm, o).kind {
                                *inner = None;
                            }
                            continue;
                        }
                    }
                }
                let next_item = match &mut helper(vm, o).kind {
                    HelperKind::Concat(rest, _) => {
                        if rest.is_empty() {
                            None
                        } else {
                            Some(rest.remove(0))
                        }
                    }
                    _ => None,
                };
                let Some((iterable, m)) = next_item else { return Ok(None) };
                let iter = vm.call(&m, iterable, Vec::new())?;
                if !matches!(iter, Value::Object(_)) {
                    return vm.type_error("iterator is not an object");
                }
                let inner = direct(vm, &iter)?;
                if let HelperKind::Concat(_, slot) = &mut helper(vm, o).kind {
                    *slot = Some(inner);
                }
            }
        }
    }
}

/// %IteratorHelperPrototype%.return (§27.1.2.1.2).
fn helper_return(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let o = helper_parts(vm, &c.this)?;
    let state = helper(vm, o).state;
    if state == HelperState::Running {
        return vm.type_error("Iterator Helper is already running");
    }
    if state == HelperState::Done {
        return Ok(vm.iter_result(Value::Undefined, true));
    }
    // Suspended-start completes at once; suspended-yield resumes (executing) to close.
    helper(vm, o).state = if state == HelperState::Start { HelperState::Done } else { HelperState::Running };
    let r = helper_close(vm, o);
    helper(vm, o).state = HelperState::Done;
    r
}

fn helper_close(vm: &mut Vm, o: ObjRef) -> JsResult<Value> {
    let it = helper(vm, o).iterator.clone();
    let inner = match &helper(vm, o).kind {
        HelperKind::FlatMap(_, inner) | HelperKind::Concat(_, inner) => inner.clone(),
        _ => None,
    };
    if let HelperKind::Zip(z) = &helper(vm, o).kind {
        let open = open_iters(z);
        close_all(vm, &open, Ok(()))?;
        return Ok(vm.iter_result(Value::Undefined, true));
    }
    if let HelperKind::Concat(..) = helper(vm, o).kind {
        if let Some((iit, _)) = inner {
            close(vm, &iit)?;
        }
        return Ok(vm.iter_result(Value::Undefined, true));
    }
    if let Some((iit, _)) = inner {
        if let Err(e) = close(vm, &iit) {
            return close_throw(vm, &it, e);
        }
    }
    close(vm, &it)?;
    Ok(vm.iter_result(Value::Undefined, true))
}

/// Run a reducer loop: `f(value, counter)` returns Some(result) to stop early.
fn each(
    vm: &mut Vm,
    c: &NativeCall,
    mut f: impl FnMut(&mut Vm, Value, f64) -> JsResult<Option<Value>>,
) -> JsResult<Option<Value>> {
    let o = this_object(vm, c)?;
    callable_or_close(vm, &o, &c.arg(0))?;
    let (it, next) = direct(vm, &o)?;
    let mut counter = 0.0;
    while let Some(v) = step(vm, &it, &next)? {
        match f(vm, v, counter) {
            Ok(Some(r)) => {
                close(vm, &it)?;
                return Ok(Some(r));
            }
            Ok(None) => {}
            Err(e) => return close_throw(vm, &it, e),
        }
        counter += 1.0;
    }
    Ok(None)
}

/// SetterThatIgnoresPrototypeProperties (§27.1.3.2.1.1 and .13.1).
fn ignore_proto_setter(vm: &mut Vm, c: &NativeCall, key: PropKey) -> JsResult<Value> {
    let Value::Object(o) = c.this else { return vm.type_error("setter called on a non-object") };
    if o == vm.realm.iterator_proto {
        return vm.type_error("Cannot assign to this property of Iterator.prototype");
    }
    if vm.get_own_property(o, &key)?.is_none() {
        vm.create_data_property_or_throw(o, key, c.arg(0))?;
    } else {
        vm.set_or_throw(o, key, c.arg(0))?;
    }
    Ok(Value::Undefined)
}

pub(super) fn install(vm: &mut Vm) {
    let ip = vm.realm.iterator_proto;
    let ic = realm::constructor(
        vm,
        "Iterator",
        0,
        |vm, c| match c.new_target {
            None => vm.type_error("Constructor Iterator requires 'new'"),
            Some(nt) if nt == c.callee => vm.type_error("Abstract class Iterator not directly constructable"),
            Some(nt) => {
                let p = vm.prototype_from_ctor(nt, |r| r.iterator_proto)?;
                Ok(Value::Object(vm.new_object_with_proto(Some(p))))
            }
        },
        ip,
    );
    vm.realm.iterator_ctor = ic;
    // `constructor` and @@toStringTag are accessors (§27.1.3.2.1, .13).
    vm.heap.obj_mut(ip).props.remove(&PropKey::lit("constructor"));
    accessor(
        vm,
        ip,
        PropKey::lit("constructor"),
        Some(|vm, _| Ok(Value::Object(vm.realm.iterator_ctor))),
        Some(|vm, c| ignore_proto_setter(vm, &c, PropKey::lit("constructor"))),
    );
    let tag = PropKey::Sym(vm.realm.sym_to_string_tag.clone());
    vm.heap.obj_mut(ip).props.remove(&tag);
    accessor(
        vm,
        ip,
        tag,
        Some(|_, _| Ok(Value::str("Iterator"))),
        Some(|vm, c| {
            let k = PropKey::Sym(vm.realm.sym_to_string_tag.clone());
            ignore_proto_setter(vm, &c, k)
        }),
    );

    let hp = vm.new_object_with_proto(Some(ip));
    vm.realm.iterator_helper_proto = hp;
    method(vm, hp, "next", 0, helper_next);
    method(vm, hp, "return", 0, helper_return);
    realm::to_string_tag(vm, hp, "Iterator Helper");

    let wp = vm.new_object_with_proto(Some(ip));
    vm.realm.wrap_iterator_proto = wp;
    method(vm, wp, "next", 0, |vm, c| {
        let (it, next) = wrapped(vm, &c.this)?;
        vm.call(&next, it, Vec::new())
    });
    method(vm, wp, "return", 0, |vm, c| {
        let (it, _) = wrapped(vm, &c.this)?;
        match vm.get_method(&it, &PropKey::lit("return"))? {
            None => Ok(vm.iter_result(Value::Undefined, true)),
            Some(f) => vm.call(&f, it, Vec::new()),
        }
    });
    fn wrapped(vm: &mut Vm, this: &Value) -> JsResult<(Value, Value)> {
        match this {
            Value::Object(o) => match &vm.heap.obj(*o).kind {
                Kind::IterRecord(r) => Ok((r.iterator.clone(), r.next.clone())),
                _ => vm.type_error("not a wrapped iterator"),
            },
            _ => vm.type_error("not a wrapped iterator"),
        }
    }

    method(vm, ic, "from", 1, |vm, c| {
        let (it, next) = flattenable(vm, &c.arg(0), true)?;
        let ctor = Value::Object(vm.realm.iterator_ctor);
        if vm.ordinary_has_instance(&ctor, &it)? {
            return Ok(it);
        }
        let wp = vm.realm.wrap_iterator_proto;
        Ok(Value::Object(vm.heap.alloc(Object::new(
            Some(wp),
            Kind::IterRecord(Box::new(IterRecord { iterator: it, next, done: false })),
        ))))
    });
    method(vm, ic, "zip", 1, |vm, c| zip_ctor(vm, c, false));
    method(vm, ic, "zipKeyed", 1, |vm, c| zip_ctor(vm, c, true));
    method(vm, ic, "concat", 0, |vm, c| {
        let ik = PropKey::Sym(vm.realm.sym_iterator.clone());
        let mut items = Vec::new();
        for a in &c.args {
            if !matches!(a, Value::Object(_)) {
                return vm.type_error("Iterator.concat: argument is not an object");
            }
            let Some(m) = vm.get_method(a, &ik)? else {
                return vm.type_error("Iterator.concat: argument is not iterable");
            };
            items.push((a.clone(), m));
        }
        Ok(new_helper(vm, Value::Undefined, Value::Undefined, HelperKind::Concat(items, None)))
    });

    method(vm, ip, "map", 1, |vm, c| {
        let o = this_object(vm, &c)?;
        callable_or_close(vm, &o, &c.arg(0))?;
        let (it, next) = direct(vm, &o)?;
        Ok(new_helper(vm, it, next, HelperKind::Map(c.arg(0))))
    });
    method(vm, ip, "filter", 1, |vm, c| {
        let o = this_object(vm, &c)?;
        callable_or_close(vm, &o, &c.arg(0))?;
        let (it, next) = direct(vm, &o)?;
        Ok(new_helper(vm, it, next, HelperKind::Filter(c.arg(0))))
    });
    method(vm, ip, "take", 1, |vm, c| {
        let o = this_object(vm, &c)?;
        let n = limit_or_close(vm, &o, &c.arg(0))?;
        let (it, next) = direct(vm, &o)?;
        Ok(new_helper(vm, it, next, HelperKind::Take(n)))
    });
    method(vm, ip, "chunks", 1, |vm, c| {
        let o = this_object(vm, &c)?;
        let n = size_or_close(vm, &o, &c.arg(0))?;
        let (it, next) = direct(vm, &o)?;
        Ok(new_helper(vm, it, next, HelperKind::Chunks(n, false)))
    });
    method(vm, ip, "windows", 1, |vm, c| {
        let o = this_object(vm, &c)?;
        let n = size_or_close(vm, &o, &c.arg(0))?;
        let u = c.arg(1);
        let is = |s: &str| matches!(&u, Value::String(x) if x.iter().copied().eq(s.encode_utf16()));
        let partial = if u.is_undefined() || is("only-full") {
            false
        } else if is("allow-partial") {
            true
        } else {
            let e = vm.error(ErrorKind::Type, "undersized must be \"only-full\" or \"allow-partial\"");
            return close_throw(vm, &o, e);
        };
        let (it, next) = direct(vm, &o)?;
        Ok(new_helper(vm, it, next, HelperKind::Windows(n, partial, Vec::new(), false)))
    });
    // Iterator.prototype.includes (iterator-includes).
    method(vm, ip, "includes", 1, |vm, c| {
        let o = this_object(vm, &c)?;
        let skip = c.arg(1);
        let to_skip = match skip {
            Value::Undefined => 0.0,
            Value::Number(n) if n.is_infinite() || (n.is_finite() && n.fract() == 0.0) => n,
            _ => {
                let e = vm.error(ErrorKind::Type, "skippedElements must be an integral number");
                return close_throw(vm, &o, e);
            }
        };
        if to_skip < 0.0 || (to_skip.is_finite() && to_skip > 9007199254740991.0) {
            let e = vm.error(ErrorKind::Range, "skippedElements is out of range");
            return close_throw(vm, &o, e);
        }
        let search = c.arg(0);
        let (it, next) = direct(vm, &o)?;
        let mut skipped = 0.0;
        while let Some(v) = step(vm, &it, &next)? {
            if skipped < to_skip {
                skipped += 1.0;
            } else if super::super::value::same_value_zero(&v, &search) {
                close(vm, &it)?;
                return Ok(Value::Bool(true));
            }
        }
        Ok(Value::Bool(false))
    });
    // Iterator.prototype.join (Iterator.prototype.join proposal).
    method(vm, ip, "join", 1, |vm, c| {
        let o = this_object(vm, &c)?;
        let sep = match c.arg(0) {
            Value::Undefined => super::super::value::js_str(","),
            s => match vm.to_string(&s) {
                Ok(s) => s,
                Err(e) => return close_throw(vm, &o, e),
            },
        };
        let (it, next) = direct(vm, &o)?;
        let mut out: Vec<u16> = Vec::new();
        let mut first = true;
        while let Some(v) = step(vm, &it, &next)? {
            if !first {
                out.extend_from_slice(&sep);
            }
            first = false;
            if !v.is_nullish() {
                match vm.to_string(&v) {
                    Ok(s) => out.extend_from_slice(&s),
                    Err(e) => return close_throw(vm, &it, e),
                }
            }
        }
        Ok(Value::String(out.into()))
    });
    method(vm, ip, "drop", 1, |vm, c| {
        let o = this_object(vm, &c)?;
        let n = limit_or_close(vm, &o, &c.arg(0))?;
        let (it, next) = direct(vm, &o)?;
        Ok(new_helper(vm, it, next, HelperKind::Drop(n)))
    });
    method(vm, ip, "flatMap", 1, |vm, c| {
        let o = this_object(vm, &c)?;
        callable_or_close(vm, &o, &c.arg(0))?;
        let (it, next) = direct(vm, &o)?;
        Ok(new_helper(vm, it, next, HelperKind::FlatMap(c.arg(0), None)))
    });
    method(vm, ip, "toArray", 0, |vm, c| {
        let o = this_object(vm, &c)?;
        let (it, next) = direct(vm, &o)?;
        let mut out = Vec::new();
        while let Some(v) = step(vm, &it, &next)? {
            out.push(v);
        }
        Ok(Value::Object(vm.new_array(out)))
    });
    method(vm, ip, "forEach", 1, |vm, c| {
        let f = c.arg(0);
        each(vm, &c, |vm, v, k| {
            vm.call(&f, Value::Undefined, vec![v, Value::Number(k)])?;
            Ok(None)
        })?;
        Ok(Value::Undefined)
    });
    method(vm, ip, "some", 1, |vm, c| {
        let f = c.arg(0);
        let r = each(vm, &c, |vm, v, k| {
            let b = vm.call(&f, Value::Undefined, vec![v, Value::Number(k)])?;
            Ok(vm.to_boolean(&b).then_some(Value::Bool(true)))
        })?;
        Ok(r.unwrap_or(Value::Bool(false)))
    });
    method(vm, ip, "every", 1, |vm, c| {
        let f = c.arg(0);
        let r = each(vm, &c, |vm, v, k| {
            Ok((!{
                let b = vm.call(&f, Value::Undefined, vec![v, Value::Number(k)])?;
                vm.to_boolean(&b)
            })
            .then_some(Value::Bool(false)))
        })?;
        Ok(r.unwrap_or(Value::Bool(true)))
    });
    method(vm, ip, "find", 1, |vm, c| {
        let f = c.arg(0);
        let r = each(vm, &c, |vm, v, k| {
            Ok({
                let b = vm.call(&f, Value::Undefined, vec![v.clone(), Value::Number(k)])?;
                vm.to_boolean(&b)
            }
            .then_some(v))
        })?;
        Ok(r.unwrap_or_default())
    });
    method(vm, ip, "reduce", 1, |vm, c| {
        let o = this_object(vm, &c)?;
        callable_or_close(vm, &o, &c.arg(0))?;
        let (it, next) = direct(vm, &o)?;
        let f = c.arg(0);
        let (mut acc, mut counter) = if c.args.len() >= 2 {
            (c.arg(1), 0.0)
        } else {
            match step(vm, &it, &next)? {
                Some(v) => (v, 1.0),
                None => return vm.type_error("Reduce of empty iterator with no initial value"),
            }
        };
        while let Some(v) = step(vm, &it, &next)? {
            acc = match vm.call(&f, Value::Undefined, vec![acc, v, Value::Number(counter)]) {
                Ok(a) => a,
                Err(e) => return close_throw(vm, &it, e),
            };
            counter += 1.0;
        }
        Ok(acc)
    });
    let _ = native_with;
}

// ---- Iterator.zip / Iterator.zipKeyed (joint-iteration) ----

fn open_iters(z: &Zip) -> Vec<Value> {
    z.iters.iter().zip(&z.open).filter(|(_, o)| **o).map(|((it, _), _)| it.clone()).collect()
}

/// IteratorCloseAll: close `iters` in reverse order, threading the completion.
fn close_all(vm: &mut Vm, iters: &[Value], completion: JsResult<()>) -> JsResult<()> {
    let mut comp = completion;
    for it in iters.iter().rev() {
        comp = match comp {
            Ok(()) => close(vm, it),
            Err(e) => close_throw(vm, it, e),
        };
    }
    comp
}

/// IteratorStep without reading `value`: true when done.
fn step_done(vm: &mut Vm, it: &Value, next: &Value) -> JsResult<bool> {
    let r = vm.call(next, it.clone(), Vec::new())?;
    let Value::Object(ro) = r else { return vm.type_error("Iterator result is not an object") };
    Ok({
        let b = vm.get(ro, &PropKey::lit("done"))?;
        vm.to_boolean(&b)
    })
}

/// IfAbruptCloseIterators.
fn or_close_all<T>(vm: &mut Vm, r: JsResult<T>, iters: &[(Value, Value)]) -> JsResult<T> {
    match r {
        Ok(v) => Ok(v),
        Err(e) => {
            let its: Vec<Value> = iters.iter().map(|(i, _)| i.clone()).collect();
            // A throw completion stays one.
            Err(close_all(vm, &its, Err(e)).err().unwrap_or_default())
        }
    }
}

fn record_parts(vm: &Vm, r: ObjRef) -> (Value, Value) {
    match &vm.heap.obj(r).kind {
        Kind::IterRecord(x) => (x.iterator.clone(), x.next.clone()),
        _ => (Value::Undefined, Value::Undefined),
    }
}

/// Iterator.zip ( iterables [ , options ] ) / Iterator.zipKeyed ( iterables [ , options ] ).
fn zip_ctor(vm: &mut Vm, c: NativeCall, keyed: bool) -> JsResult<Value> {
    let iterables = c.arg(0);
    let Value::Object(io) = iterables else { return vm.type_error("iterables must be an object") };
    let options = match c.arg(1) {
        Value::Undefined => None,
        Value::Object(o) => Some(o),
        _ => return vm.type_error("options must be an object"),
    };
    let mode_v = match options {
        Some(o) => vm.get(o, &PropKey::lit("mode"))?,
        None => Value::Undefined,
    };
    let is = |v: &Value, s: &str| matches!(v, Value::String(x) if x.iter().copied().eq(s.encode_utf16()));
    let mode = if mode_v.is_undefined() || is(&mode_v, "shortest") {
        ZipMode::Shortest
    } else if is(&mode_v, "longest") {
        ZipMode::Longest
    } else if is(&mode_v, "strict") {
        ZipMode::Strict
    } else {
        return vm.type_error("mode must be \"shortest\", \"longest\" or \"strict\"");
    };
    let mut padding_opt = Value::Undefined;
    if mode == ZipMode::Longest {
        if let Some(o) = options {
            padding_opt = vm.get(o, &PropKey::lit("padding"))?;
            if !matches!(padding_opt, Value::Undefined | Value::Object(_)) {
                return vm.type_error("padding must be an object");
            }
        }
    }
    let mut iters: Vec<(Value, Value)> = Vec::new();
    let mut keys = Vec::new();
    if keyed {
        let all = vm.own_property_keys(io)?;
        for k in all {
            let d = vm.get_own_property(io, &k);
            let d = or_close_all(vm, d, &iters)?;
            if !d.is_some_and(|d| d.enumerable == Some(true)) {
                continue;
            }
            let v = vm.get(io, &k);
            let v = or_close_all(vm, v, &iters)?;
            if v.is_undefined() {
                continue;
            }
            keys.push(k);
            let it = flattenable(vm, &v, false);
            let it = or_close_all(vm, it, &iters)?;
            iters.push(it);
        }
    } else {
        let input = vm.get_iterator(&iterables)?;
        loop {
            let n = vm.iter_step(input);
            let Some(v) = or_close_all(vm, n, &iters)? else { break };
            let it = flattenable(vm, &v, false);
            let mut with_input = vec![record_parts(vm, input)];
            with_input.extend(iters.iter().cloned());
            let it = or_close_all(vm, it, &with_input)?;
            iters.push(it);
        }
    }
    let n = iters.len();
    let mut padding = Vec::new();
    if mode == ZipMode::Longest {
        if padding_opt.is_undefined() {
            padding = vec![Value::Undefined; n];
        } else if keyed {
            let Value::Object(po) = padding_opt else { unreachable!("checked") };
            for k in &keys {
                let v = vm.get(po, k);
                padding.push(or_close_all(vm, v, &iters)?);
            }
        } else {
            let pi = vm.get_iterator(&padding_opt);
            let pi = or_close_all(vm, pi, &iters)?;
            let mut using = true;
            for _ in 0..n {
                if using {
                    let s = vm.iter_step(pi);
                    match or_close_all(vm, s, &iters)? {
                        Some(v) => padding.push(v),
                        None => {
                            using = false;
                            padding.push(Value::Undefined);
                        }
                    }
                } else {
                    padding.push(Value::Undefined);
                }
            }
            if using {
                let r = vm.iterator_close(pi);
                or_close_all(vm, r, &iters)?;
            }
        }
    }
    let z = Zip {
        open: vec![true; n],
        finished: vec![false; n],
        iters,
        mode,
        padding,
        keys: keyed.then_some(keys),
    };
    Ok(new_helper(vm, Value::Undefined, Value::Undefined, HelperKind::Zip(Box::new(z))))
}

fn zip_mut(vm: &mut Vm, o: ObjRef) -> &mut Zip {
    match &mut helper(vm, o).kind {
        HelperKind::Zip(z) => z,
        _ => unreachable!("zip helper"),
    }
}

/// One round of IteratorZip's closure.
fn zip_step(vm: &mut Vm, o: ObjRef) -> JsResult<Option<Value>> {
    let n = zip_mut(vm, o).iters.len();
    if n == 0 {
        return Ok(None);
    }
    let mode = zip_mut(vm, o).mode;
    let mut results = Vec::with_capacity(n);
    for i in 0..n {
        let z = zip_mut(vm, o);
        if z.finished[i] {
            results.push(z.padding[i].clone());
            continue;
        }
        let (it, next) = z.iters[i].clone();
        let r = step(vm, &it, &next);
        let r = match r {
            Ok(r) => r,
            Err(e) => {
                let z = zip_mut(vm, o);
                z.open[i] = false;
                let open = open_iters(z);
                close_all(vm, &open, Err(e))?;
                return Ok(None);
            }
        };
        let Some(v) = r else {
            let z = zip_mut(vm, o);
            z.open[i] = false;
            match mode {
                ZipMode::Shortest => {
                    let open = open_iters(z);
                    close_all(vm, &open, Ok(()))?;
                    return Ok(None);
                }
                ZipMode::Strict => {
                    if i != 0 {
                        let open = open_iters(z);
                        let e =
                            vm.error(ErrorKind::Type, "Iterator.zip: the iterators have different lengths");
                        close_all(vm, &open, Err(e))?;
                        return Ok(None);
                    }
                    for k in 1..n {
                        let (it, next) = zip_mut(vm, o).iters[k].clone();
                        match step_done(vm, &it, &next) {
                            Err(e) => {
                                let z = zip_mut(vm, o);
                                z.open[k] = false;
                                let open = open_iters(z);
                                close_all(vm, &open, Err(e))?;
                                return Ok(None);
                            }
                            Ok(true) => zip_mut(vm, o).open[k] = false,
                            Ok(false) => {
                                let open = open_iters(zip_mut(vm, o));
                                let e = vm.error(
                                    ErrorKind::Type,
                                    "Iterator.zip: the iterators have different lengths",
                                );
                                close_all(vm, &open, Err(e))?;
                                return Ok(None);
                            }
                        }
                    }
                    return Ok(None);
                }
                ZipMode::Longest => {
                    if !z.open.iter().any(|&x| x) {
                        return Ok(None);
                    }
                    z.finished[i] = true;
                    results.push(z.padding[i].clone());
                    continue;
                }
            }
        };
        results.push(v);
    }
    let keys = zip_mut(vm, o).keys.clone();
    Ok(Some(match keys {
        None => Value::Object(vm.new_array(results)),
        Some(keys) => {
            let r = vm.heap.alloc(Object::new(None, Kind::Ordinary));
            for (k, v) in keys.into_iter().zip(results) {
                vm.create_data_property_or_throw(r, k, v)?;
            }
            Value::Object(r)
        }
    }))
}

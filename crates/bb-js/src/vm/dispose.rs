//! Explicit resource management (ES2026 §9.5 DisposeCapability, §27.3 DisposableStack,
//! §27.4 AsyncDisposableStack, §20.5.8 SuppressedError).
//!
//! A `using` scope keeps its capability as an object on the operand stack; the compiler
//! emits DisposeResources inline on every way out of the scope, as it does for `finally`.

use super::heap::*;
use super::realm::{self, accessor, method, method_sym, native_with, to_string_tag, value};
use super::value::{PropKey, Value};
use super::{ErrorKind, JsResult, Vm};

/// DisposableResource Record (§9.5.1).
pub struct Resource {
    pub value: Value,
    /// `None`: the `null`/`undefined` of an `await using` (it still makes disposal await).
    pub method: Option<Value>,
    pub is_async: bool,
}

/// DisposeCapability Record (§9.5.2), plus the state of a DisposeResources in progress.
#[derive(Default)]
pub struct DisposeCap {
    pub resources: Vec<Resource>,
    /// The completion so far when it is a throw completion.
    pub error: Option<Value>,
    pub needs_await: bool,
    pub has_awaited: bool,
}

impl DisposeCap {
    pub fn trace(&self, os: &mut Vec<ObjRef>) {
        let mut push = |v: &Value| {
            if let Value::Object(o) = v {
                os.push(*o)
            }
        };
        for r in &self.resources {
            push(&r.value);
            if let Some(m) = &r.method {
                push(m);
            }
        }
        if let Some(e) = &self.error {
            push(e);
        }
    }
}

/// [[DisposableState]] / [[AsyncDisposableState]] and the stack's capability.
pub struct StackData {
    pub is_async: bool,
    pub disposed: bool,
    pub cap: DisposeCap,
}

impl Vm {
    fn cap_mut(&mut self, o: ObjRef) -> Option<&mut DisposeCap> {
        match &mut self.heap.obj_mut(o).kind {
            Kind::DisposeCap(c) => Some(c),
            Kind::DisposableStack(s) => Some(&mut s.cap),
            _ => None,
        }
    }

    /// `[] → [cap]` for a new `using` scope (NewDisposeCapability, §9.5.3).
    pub(super) fn new_dispose_cap(&mut self) -> Value {
        Value::Object(self.heap.alloc(Object::new(None, Kind::DisposeCap(Box::default()))))
    }

    /// GetDisposeMethod (§9.5.6).
    fn dispose_method(&mut self, v: &Value, is_async: bool) -> JsResult<Option<Value>> {
        if is_async {
            let ak = PropKey::Sym(self.realm.sym_async_dispose.clone());
            if let Some(m) = self.get_method(v, &ak)? {
                return Ok(Some(m));
            }
            let sk = PropKey::Sym(self.realm.sym_dispose.clone());
            let Some(m) = self.get_method(v, &sk)? else { return Ok(None) };
            let f = native_with(self, "", 0, async_from_sync_dispose, false, vec![m]);
            return Ok(Some(Value::Object(f)));
        }
        let sk = PropKey::Sym(self.realm.sym_dispose.clone());
        self.get_method(v, &sk)
    }

    /// AddDisposableResource (§9.5.4) without a method: CreateDisposableResource (§9.5.5).
    pub(super) fn add_disposable(&mut self, cap: ObjRef, v: Value, is_async: bool) -> JsResult<()> {
        let r = if v.is_nullish() {
            if !is_async {
                return Ok(());
            }
            Resource { value: Value::Undefined, method: None, is_async }
        } else {
            if !matches!(v, Value::Object(_)) {
                return self.type_error("a using declaration needs an object, null or undefined");
            }
            let Some(m) = self.dispose_method(&v, is_async)? else {
                let what = if is_async { "Symbol.asyncDispose or Symbol.dispose" } else { "Symbol.dispose" };
                return self.type_error(&format!("the value has no {what} method"));
            };
            Resource { value: v, method: Some(m), is_async }
        };
        if let Some(c) = self.cap_mut(cap) {
            c.resources.push(r);
        }
        Ok(())
    }

    /// A newly created SuppressedError (DisposeResources step 3.e.iii.1).
    pub(super) fn suppressed_error(&mut self, error: Value, suppressed: Value) -> Value {
        let proto = self.realm.suppressed_error_proto;
        let o = self.heap.alloc(Object::new(Some(proto), Kind::Error(Default::default())));
        let props = &mut self.heap.obj_mut(o).props;
        props.insert(PropKey::lit("error"), Prop::method(error));
        props.insert(PropKey::lit("suppressed"), Prop::method(suppressed));
        self.capture_stack(o, "SuppressedError");
        Value::Object(o)
    }

    /// Fold a throw completion from a dispose method (or the scope) into the capability's.
    pub(super) fn dispose_fold_error(&mut self, cap: ObjRef, e: Value) {
        let prev = self.cap_mut(cap).and_then(|c| c.error.take());
        let e = match prev {
            Some(p) => self.suppressed_error(e, p),
            None => e,
        };
        if let Some(c) = self.cap_mut(cap) {
            c.error = Some(e);
        }
    }

    /// One step of DisposeResources (§9.5.7): run dispose methods until one must be awaited.
    /// `Some(v)`: await `v` next (fold a rejection in with `dispose_fold_error`); `None`: done.
    pub(super) fn dispose_step(&mut self, cap: ObjRef) -> JsResult<Option<Value>> {
        loop {
            self.burn()?;
            let Some(c) = self.cap_mut(cap) else { return Ok(None) };
            let Some(r) = c.resources.last() else {
                if c.needs_await && !c.has_awaited {
                    c.needs_await = false;
                    return Ok(Some(Value::Undefined));
                }
                return Ok(None);
            };
            if !r.is_async && c.needs_await && !c.has_awaited {
                c.needs_await = false;
                return Ok(Some(Value::Undefined));
            }
            let Some(r) = c.resources.pop() else { return Ok(None) };
            let Some(m) = r.method else {
                c.needs_await = true;
                continue;
            };
            match self.call(&m, r.value, Vec::new()) {
                Ok(v) if r.is_async => {
                    if let Some(c) = self.cap_mut(cap) {
                        c.has_awaited = true;
                    }
                    return Ok(Some(v));
                }
                Ok(_) => {}
                Err(e) if self.terminated => return Err(e),
                Err(e) => self.dispose_fold_error(cap, e),
            }
        }
    }

    /// DisposeResources with no awaits left: the completion's error, if any, is thrown.
    pub(super) fn dispose_sync(&mut self, cap: ObjRef) -> JsResult<()> {
        while self.dispose_step(cap)?.is_some() {}
        match self.cap_mut(cap).and_then(|c| c.error.take()) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// The closure GetDisposeMethod makes from a @@dispose method for `await using`.
fn async_from_sync_dispose(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let m = match &vm.heap.obj(c.callee).kind {
        Kind::Native(n) => n.slots.first().cloned().unwrap_or(Value::Undefined),
        _ => Value::Undefined,
    };
    let pc = Value::Object(vm.realm.promise_ctor);
    let (p, res, rej) = vm.new_promise_capability(&pc)?;
    match vm.call(&m, c.this, Vec::new()) {
        Ok(_) => vm.call(&res, Value::Undefined, vec![Value::Undefined])?,
        Err(e) if vm.terminated => return Err(e),
        Err(e) => vm.call(&rej, Value::Undefined, vec![e])?,
    };
    Ok(p)
}

// ---- built-ins ----

fn this_stack(vm: &mut Vm, c: &NativeCall, is_async: bool) -> JsResult<ObjRef> {
    if let Value::Object(o) = c.this {
        if let Kind::DisposableStack(s) = &vm.heap.obj(o).kind {
            if s.is_async == is_async {
                return Ok(o);
            }
        }
    }
    let name = if is_async { "AsyncDisposableStack" } else { "DisposableStack" };
    vm.type_error(&format!("the receiver is not a {name}"))
}

fn stack_data(vm: &mut Vm, o: ObjRef) -> &mut StackData {
    match &mut vm.heap.obj_mut(o).kind {
        Kind::DisposableStack(s) => s,
        _ => unreachable!("checked by this_stack"),
    }
}

/// The receiver, checked and not yet disposed (the start of use/adopt/defer/move).
fn pending_stack(vm: &mut Vm, c: &NativeCall, is_async: bool) -> JsResult<ObjRef> {
    let o = this_stack(vm, c, is_async)?;
    if stack_data(vm, o).disposed {
        return vm.reference_error("the stack is already disposed");
    }
    Ok(o)
}

fn new_stack(vm: &mut Vm, proto: ObjRef, is_async: bool, cap: DisposeCap) -> ObjRef {
    let data = StackData { is_async, disposed: false, cap };
    vm.heap.alloc(Object::new(Some(proto), Kind::DisposableStack(Box::new(data))))
}

fn stack_ctor(vm: &mut Vm, c: NativeCall, is_async: bool) -> JsResult<Value> {
    let Some(nt) = c.new_target else {
        return vm.type_error("Constructor requires 'new'");
    };
    let proto = if is_async {
        vm.prototype_from_ctor(nt, |r| r.async_disposable_stack_proto)?
    } else {
        vm.prototype_from_ctor(nt, |r| r.disposable_stack_proto)?
    };
    Ok(Value::Object(new_stack(vm, proto, is_async, DisposeCap::default())))
}

fn stack_use(vm: &mut Vm, c: NativeCall, is_async: bool) -> JsResult<Value> {
    let o = pending_stack(vm, &c, is_async)?;
    let v = c.arg(0);
    vm.add_disposable(o, v.clone(), is_async)?;
    Ok(v)
}

fn stack_adopt(vm: &mut Vm, c: NativeCall, is_async: bool) -> JsResult<Value> {
    let o = pending_stack(vm, &c, is_async)?;
    let on_dispose = c.arg(1);
    if !vm.is_callable(&on_dispose) {
        return vm.type_error("onDispose is not a function");
    }
    let v = c.arg(0);
    let f = native_with(vm, "", 0, adopt_closure, false, vec![on_dispose, v.clone()]);
    stack_data(vm, o).cap.resources.push(Resource {
        value: Value::Undefined,
        method: Some(Value::Object(f)),
        is_async,
    });
    Ok(v)
}

fn adopt_closure(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let (f, v) = match &vm.heap.obj(c.callee).kind {
        Kind::Native(n) => (n.slots[0].clone(), n.slots[1].clone()),
        _ => return Ok(Value::Undefined),
    };
    vm.call(&f, Value::Undefined, vec![v])
}

fn stack_defer(vm: &mut Vm, c: NativeCall, is_async: bool) -> JsResult<Value> {
    let o = pending_stack(vm, &c, is_async)?;
    let on_dispose = c.arg(0);
    if !vm.is_callable(&on_dispose) {
        return vm.type_error("onDispose is not a function");
    }
    stack_data(vm, o).cap.resources.push(Resource {
        value: Value::Undefined,
        method: Some(on_dispose),
        is_async,
    });
    Ok(Value::Undefined)
}

fn stack_move(vm: &mut Vm, c: NativeCall, is_async: bool) -> JsResult<Value> {
    let o = pending_stack(vm, &c, is_async)?;
    let d = stack_data(vm, o);
    let cap = std::mem::take(&mut d.cap);
    d.disposed = true;
    let proto =
        if is_async { vm.realm.async_disposable_stack_proto } else { vm.realm.disposable_stack_proto };
    Ok(Value::Object(new_stack(vm, proto, is_async, cap)))
}

fn stack_disposed(vm: &mut Vm, c: NativeCall, is_async: bool) -> JsResult<Value> {
    let o = this_stack(vm, &c, is_async)?;
    Ok(Value::Bool(stack_data(vm, o).disposed))
}

/// For AsyncDisposableStack.prototype.disposeAsync (selfhost.js): `undefined` when already
/// disposed, else the resources, last first, as [value, method, …] (method may be undefined).
pub(in crate::vm) fn async_stack_take(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let c = NativeCall { this: c.arg(0), ..c };
    let o = this_stack(vm, &c, true)?;
    let d = stack_data(vm, o);
    if d.disposed {
        return Ok(Value::Undefined);
    }
    d.disposed = true;
    let rs = std::mem::take(&mut d.cap.resources);
    let mut out = Vec::with_capacity(rs.len() * 2);
    for r in rs.into_iter().rev() {
        out.push(r.value);
        out.push(r.method.unwrap_or(Value::Undefined));
    }
    Ok(Value::Object(vm.new_array(out)))
}

pub(super) fn install(vm: &mut Vm) {
    // SuppressedError (§20.5.8).
    let ep = vm.realm.error_proto(ErrorKind::Error);
    let sp = vm.new_object_with_proto(Some(ep));
    vm.realm.suppressed_error_proto = sp;
    let se = realm::constructor(
        vm,
        "SuppressedError",
        3,
        |vm, c| {
            let nt = c.new_target.unwrap_or(c.callee);
            let proto = vm.prototype_from_ctor(nt, |r| r.suppressed_error_proto)?;
            let o = vm.heap.alloc(Object::new(Some(proto), Kind::Error(Default::default())));
            let msg = c.arg(2);
            if !msg.is_undefined() {
                let m = vm.to_string(&msg)?;
                vm.heap.obj_mut(o).props.insert(PropKey::lit("message"), Prop::method(Value::String(m)));
            }
            let props = &mut vm.heap.obj_mut(o).props;
            props.insert(PropKey::lit("error"), Prop::method(c.arg(0)));
            props.insert(PropKey::lit("suppressed"), Prop::method(c.arg(1)));
            vm.capture_stack(o, "SuppressedError");
            Ok(Value::Object(o))
        },
        sp,
    );
    let g = vm.realm.global;
    let ec = vm.get(g, &PropKey::lit("Error")).ok().and_then(|v| v.as_object());
    vm.heap.obj_mut(se).proto = ec;
    value(vm, sp, "name", Value::str("SuppressedError"));
    value(vm, sp, "message", Value::str(""));

    // DisposableStack (§27.3).
    let op = vm.realm.object_proto;
    let dp = vm.new_object_with_proto(Some(op));
    vm.realm.disposable_stack_proto = dp;
    realm::constructor(vm, "DisposableStack", 0, |vm, c| stack_ctor(vm, c, false), dp);
    accessor(vm, dp, PropKey::lit("disposed"), Some(|vm, c| stack_disposed(vm, c, false)), None);
    let dispose = method(vm, dp, "dispose", 0, |vm, c| {
        let o = this_stack(vm, &c, false)?;
        let d = stack_data(vm, o);
        if d.disposed {
            return Ok(Value::Undefined);
        }
        d.disposed = true;
        vm.dispose_sync(o)?;
        Ok(Value::Undefined)
    });
    method(vm, dp, "use", 1, |vm, c| stack_use(vm, c, false));
    method(vm, dp, "adopt", 2, |vm, c| stack_adopt(vm, c, false));
    method(vm, dp, "defer", 1, |vm, c| stack_defer(vm, c, false));
    method(vm, dp, "move", 0, |vm, c| stack_move(vm, c, false));
    let dk = PropKey::Sym(vm.realm.sym_dispose.clone());
    vm.heap.obj_mut(dp).props.insert(dk, Prop::method(Value::Object(dispose)));
    to_string_tag(vm, dp, "DisposableStack");

    // AsyncDisposableStack (§27.4); disposeAsync is in selfhost.js.
    let ap = vm.new_object_with_proto(Some(op));
    vm.realm.async_disposable_stack_proto = ap;
    realm::constructor(vm, "AsyncDisposableStack", 0, |vm, c| stack_ctor(vm, c, true), ap);
    accessor(vm, ap, PropKey::lit("disposed"), Some(|vm, c| stack_disposed(vm, c, true)), None);
    method(vm, ap, "use", 1, |vm, c| stack_use(vm, c, true));
    method(vm, ap, "adopt", 2, |vm, c| stack_adopt(vm, c, true));
    method(vm, ap, "defer", 1, |vm, c| stack_defer(vm, c, true));
    method(vm, ap, "move", 0, |vm, c| stack_move(vm, c, true));
    to_string_tag(vm, ap, "AsyncDisposableStack");

    // %Iterator.prototype% [ @@dispose ] (§27.1.4.15).
    let ip = vm.realm.iterator_proto;
    let ds = vm.realm.sym_dispose.clone();
    method_sym(vm, ip, ds, "[Symbol.dispose]", 0, |vm, c| {
        if let Some(r) = vm.get_method(&c.this, &PropKey::lit("return"))? {
            vm.call(&r, c.this, Vec::new())?;
        }
        Ok(Value::Undefined)
    });
}

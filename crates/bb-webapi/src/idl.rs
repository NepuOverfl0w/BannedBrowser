//! Defining WebIDL members (WebIDL §3.7.4–3.7.6): operations, attributes and constants
//! are all enumerable, unlike ECMAScript built-ins.

use bb_js::vm::heap::{NativeFn, ObjRef, Prop, Slot};
use bb_js::vm::{realm, PropKey};
use bb_js::{Value, Vm};

/// A regular operation: writable, enumerable, configurable.
pub fn op(vm: &mut Vm, on: ObjRef, name: &str, len: u32, f: NativeFn) -> ObjRef {
    let m = realm::native(vm, name, len, f);
    vm.heap.obj_mut(on).props.insert(
        PropKey::named(name),
        Prop { slot: Slot::Data(Value::Object(m)), writable: true, enumerable: true, configurable: true },
    );
    m
}

/// An attribute with a getter and an optional setter (read-only without one).
pub fn attr(vm: &mut Vm, on: ObjRef, name: &str, get: NativeFn, set: Option<NativeFn>) {
    let g = realm::native(vm, &format!("get {name}"), 0, get);
    let s = set.map(|f| realm::native(vm, &format!("set {name}"), 1, f));
    vm.heap.obj_mut(on).props.insert(
        PropKey::named(name),
        Prop { slot: Slot::Accessor(Some(g), s), writable: false, enumerable: true, configurable: true },
    );
}

/// An attribute whose getter and setter carry `data` in their slots (reflected attributes).
pub fn attr_with(
    vm: &mut Vm,
    on: ObjRef,
    name: &str,
    get: NativeFn,
    set: Option<NativeFn>,
    data: Vec<Value>,
) {
    let g = realm::native_with(vm, &format!("get {name}"), 0, get, false, data.clone());
    let s = set.map(|f| realm::native_with(vm, &format!("set {name}"), 1, f, false, data));
    vm.heap.obj_mut(on).props.insert(
        PropKey::named(name),
        Prop { slot: Slot::Accessor(Some(g), s), writable: false, enumerable: true, configurable: true },
    );
}

/// A constant: read-only, enumerable, not configurable.
pub fn constant(vm: &mut Vm, on: ObjRef, name: &str, v: Value) {
    vm.heap.obj_mut(on).props.insert(
        PropKey::named(name),
        Prop { slot: Slot::Data(v), writable: false, enumerable: true, configurable: false },
    );
}

/// The data slots of the native function being called.
pub fn callee_slots(vm: &Vm, callee: ObjRef) -> Vec<Value> {
    match &vm.heap.obj(callee).kind {
        bb_js::vm::heap::Kind::Native(n) => n.slots.clone(),
        _ => Vec::new(),
    }
}

//! Built-ins written in JavaScript (`selfhost.js`), mostly the async ones whose spec steps
//! are an AsyncFunctionStart closure (Array.fromAsync, …). The script is compiled once per
//! thread and run in every new realm with an object of native helpers; the functions it
//! defines look like built-ins (`[native code]`, %Function.prototype%, no `prototype`).
//!
//! The JavaScript must not reach anything user code can patch: it calls functions through
//! `I.call`, never uses spread, `for…of` or methods of the built-in prototypes.

use super::super::code::Code;
use super::super::compiler::{Compiler, Unit};
use super::super::heap::*;
use super::super::realm::native;
use super::super::value::{PropKey, Value};
use super::super::{ErrorKind, JsResult, Vm};
use std::cell::OnceCell;
use std::rc::Rc;

/// The referrer of self-hosted code; `Function.prototype.toString` reports it as native.
pub const MARK: &str = "\u{0}builtin";

const SOURCE: &str = include_str!("selfhost.js");

fn code() -> Option<Rc<Code>> {
    thread_local! {
        static CODE: OnceCell<Option<Rc<Code>>> = const { OnceCell::new() };
    }
    CODE.with(|c| {
        c.get_or_init(|| {
            let prog = crate::parse_script(SOURCE).ok()?;
            let source: Rc<[char]> = SOURCE.chars().collect::<Vec<_>>().into();
            Some(Rc::new(Compiler::compile_unit(Unit::Script(&prog), source, Some(MARK.into()))))
        })
        .clone()
    })
}

fn obj_arg(vm: &mut Vm, c: &NativeCall, i: usize) -> JsResult<ObjRef> {
    match c.arg(i) {
        Value::Object(o) => Ok(o),
        _ => vm.type_error("value is not an object"),
    }
}

fn helper(vm: &mut Vm, on: ObjRef, name: &str, f: NativeFn) {
    let m = native(vm, name, 0, f);
    vm.heap.obj_mut(on).props.insert(PropKey::named(name), Prop::data(Value::Object(m), false, false, false));
}

pub(super) fn install(vm: &mut Vm) {
    let Some(code) = code() else { return };
    let i = vm.heap.alloc(Object::new(None, Kind::Ordinary));
    helper(vm, i, "call", |vm, c| {
        let f = c.arg(0);
        let this = c.arg(1);
        vm.call(&f, this, c.args.get(2..).map(<[Value]>::to_vec).unwrap_or_default())
    });
    helper(vm, i, "construct", |vm, c| {
        let f = obj_arg(vm, &c, 0)?;
        vm.construct(f, c.args.get(1..).map(<[Value]>::to_vec).unwrap_or_default(), None)
    });
    helper(vm, i, "isCallable", |vm, c| Ok(Value::Bool(vm.is_callable(&c.arg(0)))));
    helper(vm, i, "isConstructor", |vm, c| Ok(Value::Bool(vm.is_constructor(&c.arg(0)))));
    helper(vm, i, "toObject", |vm, c| vm.to_object(&c.arg(0)).map(Value::Object));
    helper(vm, i, "lengthOfArrayLike", |vm, c| {
        let o = obj_arg(vm, &c, 0)?;
        vm.length_of_array_like(o).map(Value::Number)
    });
    helper(vm, i, "arrayCreate", |vm, c| {
        let n = vm.to_number(&c.arg(0))?;
        super::array::array_create(vm, n, None).map(Value::Object)
    });
    helper(vm, i, "createDataPropertyOrThrow", |vm, c| {
        let o = obj_arg(vm, &c, 0)?;
        let k = vm.to_property_key(&c.arg(1))?;
        vm.create_data_property_or_throw(o, k, c.arg(2))?;
        Ok(Value::Undefined)
    });
    helper(vm, i, "asyncFromSyncIterator", |vm, c| Ok(Value::Object(vm.async_from_sync(c.arg(0), c.arg(1)))));
    // defineMethod(target, key, fn): a writable, configurable, non-enumerable built-in.
    helper(vm, i, "defineMethod", |vm, c| {
        let o = obj_arg(vm, &c, 0)?;
        let k = vm.to_property_key(&c.arg(1))?;
        let f = obj_arg(vm, &c, 2)?;
        let fp = vm.realm.function_proto;
        vm.heap.obj_mut(f).proto = Some(fp);
        vm.heap.obj_mut(o).props.insert(k, Prop::method(Value::Object(f)));
        Ok(Value::Undefined)
    });
    // Intrinsics, read before any user code runs.
    let mut vals: Vec<(&str, Value)> = vec![
        ("Array", Value::Object(vm.realm.array_ctor)),
        ("Promise", Value::Object(vm.realm.promise_ctor)),
    ];
    for (name, k) in [("TypeError", ErrorKind::Type), ("RangeError", ErrorKind::Range)] {
        let p = vm.realm.error_proto(k);
        vals.push((name, vm.get(p, &PropKey::lit("constructor")).unwrap_or(Value::Undefined)));
    }
    for (name, sym) in vm.realm.well_known() {
        vals.push((name, Value::Symbol(sym)));
    }
    for (name, v) in vals {
        vm.heap.obj_mut(i).props.insert(PropKey::named(name), Prop::data(v, false, false, false));
    }
    match vm.run_global_code(code) {
        Ok(f) => {
            let _ = vm.call(&f, Value::Undefined, vec![Value::Object(i)]);
        }
        Err(_) => debug_assert!(false, "selfhost.js failed to run"),
    }
}

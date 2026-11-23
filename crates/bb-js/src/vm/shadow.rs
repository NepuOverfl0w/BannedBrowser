//! ShadowRealm (proposal-shadowrealm): a realm whose code exchanges only primitives and
//! callables with its creator, through wrapped function exotic objects.

use super::heap::*;
use super::realm::{self, method, native_with, to_string_tag};
use super::value::{PropKey, Value};
use super::{JsResult, Vm};

fn this_realm(vm: &mut Vm, v: &Value) -> JsResult<u32> {
    if let Value::Object(o) = v {
        if let Kind::ShadowRealm(id) = vm.heap.obj(*o).kind {
            return Ok(id);
        }
    }
    vm.type_error("the receiver is not a ShadowRealm")
}

impl Vm {
    /// GetWrappedValue(realm, value): primitives pass, callables are wrapped for `realm`.
    fn wrapped_value(&mut self, realm: u32, v: Value) -> JsResult<Value> {
        let Value::Object(o) = v else { return Ok(v) };
        if !self.is_callable(&v) {
            // Thrown in the current realm (the caller's), like every TypeError here.
            return self.type_error("only primitives and callables cross a ShadowRealm");
        }
        self.wrap_function(realm, o)
    }

    /// WrappedFunctionCreate(callerRealm, Target).
    fn wrap_function(&mut self, realm: u32, target: ObjRef) -> JsResult<Value> {
        let w = self
            .in_realm(realm, |vm| native_with(vm, "", 0, wrapped_call, false, vec![Value::Object(target)]));
        // CopyNameAndLength: an abrupt completion becomes a TypeError of callerRealm.
        let r = (|| -> JsResult<()> {
            let mut len = 0.0;
            if self.has_own_property(target, &PropKey::lit("length"))? {
                if let Value::Number(n) = self.get(target, &PropKey::lit("length"))? {
                    len = if n == f64::INFINITY {
                        f64::INFINITY
                    } else if n == f64::NEG_INFINITY {
                        0.0
                    } else {
                        self.to_integer_or_infinity(&Value::Number(n))?.max(0.0)
                    };
                }
            }
            let name = match self.get(target, &PropKey::lit("name"))? {
                Value::String(s) => Value::String(s),
                _ => Value::str(""),
            };
            let props = &mut self.heap.obj_mut(w).props;
            props.insert(PropKey::lit("length"), Prop::data(Value::Number(len), false, false, true));
            props.insert(PropKey::lit("name"), Prop::data(name, false, false, true));
            Ok(())
        })();
        match r {
            Ok(()) => Ok(Value::Object(w)),
            Err(e) if self.terminated => Err(e),
            Err(_) => self.type_error("the function can't be wrapped"),
        }
    }
}

/// A wrapped function's [[Call]]: arguments wrapped into the target's realm, the result
/// back into the caller's; an abrupt completion becomes the caller realm's TypeError.
fn wrapped_call(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let (target, caller) = match &vm.heap.obj(c.callee).kind {
        Kind::Native(n) => (n.slots[0].clone(), n.realm),
        _ => return Ok(Value::Undefined),
    };
    let Value::Object(t) = target else { return Ok(Value::Undefined) };
    let target_realm = vm.function_realm(t)?;
    let mut args = Vec::with_capacity(c.args.len());
    for a in c.args {
        args.push(vm.wrapped_value(target_realm, a)?);
    }
    let this = vm.wrapped_value(target_realm, c.this)?;
    match vm.call(&Value::Object(t), this, args) {
        Ok(v) => vm.wrapped_value(caller, v),
        Err(e) if vm.terminated => Err(e),
        Err(_) => vm.type_error("the wrapped function threw"),
    }
}

/// The fulfillment step of ShadowRealmImportValue: the export, wrapped for the caller.
fn import_value_step(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let (name, realm) = match &vm.heap.obj(c.callee).kind {
        Kind::Native(n) => (n.slots[0].clone(), n.realm),
        _ => return Ok(Value::Undefined),
    };
    let Value::Object(exports) = c.arg(0) else { return vm.type_error("not a module namespace") };
    let k = vm.to_property_key(&name)?;
    if !vm.has_own_property(exports, &k)? {
        return vm.type_error(&format!("the module has no export '{}'", k.display()));
    }
    let v = vm.get(exports, &k)?;
    vm.wrapped_value(realm, v)
}

pub(super) fn install(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let proto = vm.new_object_with_proto(Some(op));
    realm::constructor(
        vm,
        "ShadowRealm",
        0,
        |vm, c| {
            let Some(nt) = c.new_target else {
                return vm.type_error("Constructor ShadowRealm requires 'new'");
            };
            let proto = vm.prototype_from_ctor(nt, |r| r.shadow_realm_proto)?;
            let id = vm.create_realm();
            Ok(Value::Object(vm.heap.alloc(Object::new(Some(proto), Kind::ShadowRealm(id)))))
        },
        proto,
    );
    vm.realm.shadow_realm_proto = proto;
    // ShadowRealm.prototype.evaluate ( sourceText ): PerformShadowRealmEval.
    method(vm, proto, "evaluate", 1, |vm, c| {
        let id = this_realm(vm, &c.this)?;
        let Value::String(s) = c.arg(0) else { return vm.type_error("evaluate needs a string") };
        let src = super::to_rust(&s);
        // A SyntaxError belongs to the caller's realm.
        if let Err(e) = crate::parser::parse_eval(&src, Default::default()) {
            return vm.syntax_error(&e.msg);
        }
        let caller = vm.cur_realm;
        let r = vm.in_realm(id, |vm| vm.indirect_eval(Value::String(s)));
        match r {
            Ok(v) => vm.wrapped_value(caller, v),
            Err(e) if vm.terminated => Err(e),
            Err(_) => vm.type_error("the ShadowRealm's code threw"),
        }
    });
    // ShadowRealm.prototype.importValue ( specifier, exportName ).
    method(vm, proto, "importValue", 2, |vm, c| {
        let id = this_realm(vm, &c.this)?;
        let spec = vm.to_string(&c.arg(0))?;
        let name = c.arg(1);
        if !matches!(name, Value::String(_)) {
            return vm.type_error("the export name must be a string");
        }
        let inner = vm.in_realm(id, |vm| vm.dynamic_import(Value::String(spec), Value::Undefined));
        let step = native_with(vm, "", 1, import_value_step, false, vec![name]);
        let pc = Value::Object(vm.realm.promise_ctor);
        let (p, res, rej) = vm.new_promise_capability(&pc)?;
        let thrower = vm.realm.throw_type_error;
        let Value::Object(io) = inner else { return Ok(p) };
        vm.perform_then(io, Value::Object(step), Value::Object(thrower), Some((p.clone(), res, rej)));
        Ok(p)
    });
    to_string_tag(vm, proto, "ShadowRealm");
}

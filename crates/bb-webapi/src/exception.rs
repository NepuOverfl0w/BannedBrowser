//! DOMException (WebIDL §3.14.1): a platform object whose slots hold its name and message.

use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::{realm, PropKey};
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::host::{host, js, rstr, this_of};
use crate::idl;

/// The legacy codes (WebIDL §3.14.2 table).
const CODES: &[(&str, u16, &str)] = &[
    ("IndexSizeError", 1, "INDEX_SIZE_ERR"),
    ("", 2, "DOMSTRING_SIZE_ERR"),
    ("HierarchyRequestError", 3, "HIERARCHY_REQUEST_ERR"),
    ("WrongDocumentError", 4, "WRONG_DOCUMENT_ERR"),
    ("InvalidCharacterError", 5, "INVALID_CHARACTER_ERR"),
    ("", 6, "NO_DATA_ALLOWED_ERR"),
    ("NoModificationAllowedError", 7, "NO_MODIFICATION_ALLOWED_ERR"),
    ("NotFoundError", 8, "NOT_FOUND_ERR"),
    ("NotSupportedError", 9, "NOT_SUPPORTED_ERR"),
    ("InUseAttributeError", 10, "INUSE_ATTRIBUTE_ERR"),
    ("InvalidStateError", 11, "INVALID_STATE_ERR"),
    ("SyntaxError", 12, "SYNTAX_ERR"),
    ("InvalidModificationError", 13, "INVALID_MODIFICATION_ERR"),
    ("NamespaceError", 14, "NAMESPACE_ERR"),
    ("InvalidAccessError", 15, "INVALID_ACCESS_ERR"),
    ("", 16, "VALIDATION_ERR"),
    ("TypeMismatchError", 17, "TYPE_MISMATCH_ERR"),
    ("SecurityError", 18, "SECURITY_ERR"),
    ("NetworkError", 19, "NETWORK_ERR"),
    ("AbortError", 20, "ABORT_ERR"),
    ("URLMismatchError", 21, "URL_MISMATCH_ERR"),
    ("QuotaExceededError", 22, "QUOTA_EXCEEDED_ERR"),
    ("TimeoutError", 23, "TIMEOUT_ERR"),
    ("InvalidNodeTypeError", 24, "INVALID_NODE_TYPE_ERR"),
    ("DataCloneError", 25, "DATA_CLONE_ERR"),
];

/// A new DOMException.
pub fn make(vm: &mut Vm, name: &str, msg: &str) -> Value {
    let proto = match host(vm) {
        Ok(h) => h.proto(C::DOMException),
        Err(_) => return Value::str(msg),
    };
    let o = vm.new_host_object(Some(proto), C::DOMException as u32, 0, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = vec![js(name), js(msg)];
    }
    capture_stack(vm, o);
    Value::Object(o)
}

/// DOMExceptions carry a `stack` like errors (an own data property, as engines do).
fn capture_stack(vm: &mut Vm, o: ObjRef) {
    let e = vm.error(bb_js::vm::ErrorKind::Error, "");
    if let Value::Object(eo) = e {
        if let Ok(s) = vm.get(eo, &PropKey::lit("stack")) {
            realm::value(vm, o, "stack", s);
        }
    }
}

/// Throw a DOMException named `name`.
pub fn throw<T>(vm: &mut Vm, name: &str, msg: &str) -> JsResult<T> {
    Err(make(vm, name, msg))
}

pub fn ctor(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let Some(nt) = c.new_target else { return vm.type_error("DOMException requires 'new'") };
    let msg = match c.arg(0) {
        Value::Undefined => String::new(),
        v => rstr(vm, &v)?,
    };
    let name = match c.arg(1) {
        Value::Undefined => "Error".to_string(),
        v => rstr(vm, &v)?,
    };
    let h = host(vm)?;
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => p,
        _ => h.proto(C::DOMException),
    };
    let o = vm.new_host_object(Some(proto), C::DOMException as u32, 0, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = vec![js(&name), js(&msg)];
    }
    capture_stack(vm, o);
    Ok(Value::Object(o))
}

fn slot(vm: &mut Vm, this: &Value, i: usize) -> JsResult<Value> {
    this_of(vm, this, C::DOMException)?;
    let Value::Object(o) = this else { return vm.type_error("not a DOMException") };
    Ok(vm.host_obj(*o).and_then(|h| h.slots.get(i).cloned()).unwrap_or_default())
}

pub fn setup(vm: &mut Vm, proto: ObjRef, ctor: ObjRef) {
    idl::attr(vm, proto, "name", |vm, c| slot(vm, &c.this, 0), None);
    idl::attr(vm, proto, "message", |vm, c| slot(vm, &c.this, 1), None);
    idl::attr(
        vm,
        proto,
        "code",
        |vm, c| {
            let n = slot(vm, &c.this, 0)?;
            let n = rstr(vm, &n)?;
            let code = CODES.iter().find(|x| !x.0.is_empty() && x.0 == n).map_or(0, |x| x.1);
            Ok(Value::Number(code as f64))
        },
        None,
    );
    for &(_, code, cname) in CODES {
        idl::constant(vm, ctor, cname, Value::Number(code as f64));
        idl::constant(vm, proto, cname, Value::Number(code as f64));
    }
}

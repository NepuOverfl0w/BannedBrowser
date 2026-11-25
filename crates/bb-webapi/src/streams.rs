//! The Streams Standard: ReadableStream, WritableStream, TransformStream, their readers,
//! writers and controllers, the queuing strategies, and TextEncoderStream/TextDecoderStream.
//! They are written in JavaScript (`streams.js`, after the standard's own abstract
//! operations) and installed in a global's realm the first time a script touches one of
//! their names, so pages that never use streams don't pay for them. The host uses a few of
//! the script's helpers: streams over byte chunks (bodies, `Blob.stream()`) and reading a
//! whole stream (a body that streamed).

use bb_js::vm::heap::{NativeCall, ObjRef, Prop, Slot};
use bb_js::vm::{realm, PropKey};
use bb_js::{JsResult, Value, Vm};

use crate::host::*;

type R = JsResult<Value>;

const SOURCE: &str = include_str!("streams.js");

/// The global names the script defines, in its `classes` object.
pub const NAMES: [&str; 15] = [
    "ReadableStream",
    "ReadableStreamDefaultReader",
    "ReadableStreamDefaultController",
    "ReadableStreamBYOBReader",
    "ReadableByteStreamController",
    "ReadableStreamBYOBRequest",
    "WritableStream",
    "WritableStreamDefaultWriter",
    "WritableStreamDefaultController",
    "TransformStream",
    "TransformStreamDefaultController",
    "ByteLengthQueuingStrategy",
    "CountQueuingStrategy",
    "TextEncoderStream",
    "TextDecoderStream",
];

/// Put lazy accessors for the stream interfaces on global `g`: the first read installs the
/// script and turns them into the ordinary interface-object properties.
pub fn setup(vm: &mut Vm, g: ObjRef) {
    for (i, name) in NAMES.iter().enumerate() {
        let get = realm::native_with(vm, name, 0, lazy_get, false, vec![Value::Number(i as f64)]);
        let set = realm::native_with(vm, name, 1, lazy_set, false, vec![Value::Number(i as f64)]);
        vm.heap.obj_mut(g).props.insert(
            PropKey::named(name),
            Prop {
                slot: Slot::Accessor(Some(get), Some(set)),
                writable: false,
                enumerable: false,
                configurable: true,
            },
        );
    }
}

fn index(vm: &mut Vm, c: &NativeCall) -> usize {
    match crate::idl::callee_slots(vm, c.callee).first() {
        Some(Value::Number(n)) => *n as usize,
        _ => 0,
    }
}

fn lazy_get(vm: &mut Vm, c: NativeCall) -> R {
    let i = index(vm, &c);
    let exports = install(vm)?;
    let classes = vm.get(exports, &PropKey::lit("classes"))?;
    let Value::Object(classes) = classes else { return vm.type_error("streams didn't install") };
    vm.get(classes, &PropKey::named(NAMES[i]))
}

fn lazy_set(vm: &mut Vm, c: NativeCall) -> R {
    let i = index(vm, &c);
    let g = vm.realm.global;
    vm.heap.obj_mut(g).props.insert(PropKey::named(NAMES[i]), Prop::data(c.arg(0), true, false, true));
    Ok(Value::Undefined)
}

/// Run the script in the current realm (once), define the interface objects on the global,
/// and return the script's exports.
pub fn install(vm: &mut Vm) -> JsResult<ObjRef> {
    let h = host(vm)?;
    if let Some(o) = h.misc.borrow().get("streams") {
        return Ok(*o);
    }
    let f = vm.run_script_at(SOURCE, Some("bb:streams.js"))?;
    let g = vm.realm.global;
    let exports = vm.call(&f, Value::Undefined, vec![Value::Object(g)])?;
    let Value::Object(exports) = exports else { return vm.type_error("streams didn't install") };
    h.misc.borrow_mut().insert("streams", exports);
    let classes = vm.get(exports, &PropKey::lit("classes"))?;
    if let Value::Object(classes) = classes {
        for name in NAMES {
            // A script may have replaced the property already; keep its value then.
            let lazy = matches!(
                vm.heap.obj(g).props.get(&PropKey::named(name)).map(|p| &p.slot),
                Some(Slot::Accessor(..))
            );
            if lazy {
                let v = vm.get(classes, &PropKey::named(name))?;
                vm.heap.obj_mut(g).props.insert(PropKey::named(name), Prop::data(v, true, false, true));
            }
        }
    }
    Ok(exports)
}

/// One of the script's exported helpers.
pub fn helper(vm: &mut Vm, name: &'static str) -> R {
    let exports = install(vm)?;
    vm.get(exports, &PropKey::lit(name))
}

/// A ReadableStream of the given byte chunks (Uint8Arrays).
pub fn from_chunks(vm: &mut Vm, chunks: Vec<Vec<u8>>) -> R {
    let f = helper(vm, "fromChunks")?;
    let mut arrs = Vec::with_capacity(chunks.len());
    for c in chunks {
        arrs.push(crate::net::uint8_array(vm, c)?);
    }
    let list = vm.new_array(arrs);
    vm.call(&f, Value::Undefined, vec![Value::Object(list)])
}

/// Whether `v` is a ReadableStream of this realm (without installing streams).
pub fn is_stream(vm: &mut Vm, v: &Value) -> JsResult<bool> {
    let h = host(vm)?;
    let installed = h.misc.borrow().get("streams").copied();
    let (Some(exports), Value::Object(o)) = (installed, v) else { return Ok(false) };
    let classes = vm.get(exports, &PropKey::lit("classes"))?;
    let Value::Object(classes) = classes else { return Ok(false) };
    let ctor = vm.get(classes, &PropKey::lit("ReadableStream"))?;
    let Value::Object(ctor) = ctor else { return Ok(false) };
    let proto = vm.get(ctor, &PropKey::lit("prototype"))?;
    let mut p = vm.heap.obj(*o).proto;
    while let Some(x) = p {
        if bb_js::vm::value::same_value(&Value::Object(x), &proto) {
            return Ok(true);
        }
        p = vm.heap.obj(x).proto;
    }
    Ok(false)
}

/// Whether a stream is locked or disturbed (a body that can no longer be read).
pub fn unusable(vm: &mut Vm, stream: &Value) -> JsResult<bool> {
    let f = helper(vm, "unusable")?;
    let r = vm.call(&f, Value::Undefined, vec![stream.clone()])?;
    Ok(vm.to_boolean(&r))
}

/// Whether a stream was read from (or canceled).
pub fn disturbed(vm: &mut Vm, stream: &Value) -> JsResult<bool> {
    let f = helper(vm, "disturbed")?;
    let r = vm.call(&f, Value::Undefined, vec![stream.clone()])?;
    Ok(vm.to_boolean(&r))
}

/// Fetch §5.1 "clone a body": the stream teed, as (this body's new stream, the clone's).
pub fn tee_body(vm: &mut Vm, stream: &Value) -> JsResult<(Value, Value)> {
    let f = helper(vm, "teeBody")?;
    let pair = vm.call(&f, Value::Undefined, vec![stream.clone()])?;
    let Value::Object(pair) = pair else { return vm.type_error("tee failed") };
    Ok((vm.get(pair, &PropKey::Index(0))?, vm.get(pair, &PropKey::Index(1))?))
}

/// A promise of the whole stream as one Uint8Array.
pub fn read_all(vm: &mut Vm, stream: &Value) -> R {
    let f = helper(vm, "readAll")?;
    vm.call(&f, Value::Undefined, vec![stream.clone()])
}

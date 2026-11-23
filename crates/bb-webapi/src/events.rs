//! Events (DOM §2): Event and its subclasses, EventTarget, dispatch, and event handlers
//! (HTML §8.1.8). An Event's state lives in its slots (traced, freed with the object):
//! 0 type, 1 flags, 2 target, 3 currentTarget, 4 eventPhase, 5 timeStamp, 6 the subclass
//! fields (an object), 7 the event path while dispatching.

use std::cell::Cell;
use std::rc::Rc;

use bb_js::vm::heap::{NativeCall, ObjRef, Prop, Slot};
use bb_js::vm::{realm, PropKey};
use bb_js::{JsResult, Value, Vm};

use crate::class::{self, C};
use crate::exception::throw;
use crate::host::*;
use crate::idl::{attr, attr_with, callee_slots, constant, op};

type R = JsResult<Value>;

const BUBBLES: u32 = 1;
const CANCELABLE: u32 = 2;
const COMPOSED: u32 = 4;
const STOP: u32 = 8;
const STOP_IMMEDIATE: u32 = 16;
const CANCELED: u32 = 32;
const INITIALIZED: u32 = 64;
const DISPATCH: u32 = 128;
const TRUSTED: u32 = 256;
const IN_PASSIVE: u32 = 512;

const S_TYPE: usize = 0;
const S_FLAGS: usize = 1;
const S_TARGET: usize = 2;
const S_CURRENT: usize = 3;
const S_PHASE: usize = 4;
const S_TIME: usize = 5;
const S_EXTRA: usize = 6;
const S_PATH: usize = 7;

fn event_obj(vm: &mut Vm, v: &Value) -> JsResult<ObjRef> {
    match (v, vm.host_data(v)) {
        (Value::Object(o), Some((c, _))) if class::implements(c, C::Event) => Ok(*o),
        _ => vm.type_error("Illegal invocation: not an Event"),
    }
}

fn slot(vm: &Vm, o: ObjRef, i: usize) -> Value {
    vm.host_obj(o).and_then(|h| h.slots.get(i).cloned()).unwrap_or_default()
}

fn set_slot(vm: &mut Vm, o: ObjRef, i: usize, v: Value) {
    if let Some(h) = vm.host_obj_mut(o) {
        if h.slots.len() <= i {
            h.slots.resize(i + 1, Value::Undefined);
        }
        h.slots[i] = v;
    }
}

fn flags(vm: &Vm, o: ObjRef) -> u32 {
    match slot(vm, o, S_FLAGS) {
        Value::Number(n) => n as u32,
        _ => 0,
    }
}

fn set_flags(vm: &mut Vm, o: ObjRef, f: u32) {
    set_slot(vm, o, S_FLAGS, Value::Number(f as f64));
}

fn flag_on(vm: &mut Vm, o: ObjRef, f: u32, on: bool) {
    let cur = flags(vm, o);
    set_flags(vm, o, if on { cur | f } else { cur & !f });
}

/// A new event of interface `class`.
pub fn new_event(vm: &mut Vm, h: &Host, class: C, proto: Option<ObjRef>, ty: &str, f: u32) -> ObjRef {
    let proto = proto.unwrap_or_else(|| h.proto(class));
    let o = vm.new_host_object(Some(proto), class as u32, 0, false);
    let extra = vm.new_object_with_proto(None);
    let now = h.embedder.now_ms();
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = vec![
            js(ty),
            Value::Number(f as f64),
            Value::Null,
            Value::Null,
            Value::Number(0.0),
            Value::Number(now),
            Value::Object(extra),
            Value::Undefined,
        ];
    }
    // [LegacyUnforgeable] isTrusted: an own accessor on every event (WebIDL §3.7.7).
    if let Some(g) = h.misc.borrow().get("isTrusted").copied() {
        vm.heap.obj_mut(o).props.insert(
            PropKey::lit("isTrusted"),
            Prop {
                slot: Slot::Accessor(Some(g), None),
                writable: false,
                enumerable: true,
                configurable: false,
            },
        );
    }
    o
}

/// Set a subclass field of an event.
pub fn set_field(vm: &mut Vm, o: ObjRef, name: &str, v: Value) {
    if let Value::Object(x) = slot(vm, o, S_EXTRA) {
        vm.heap.obj_mut(x).props.insert(PropKey::named(name), Prop::data(v, true, true, true));
    }
}

fn get_field(vm: &Vm, o: ObjRef, name: &str) -> Option<Value> {
    match slot(vm, o, S_EXTRA) {
        Value::Object(x) => vm.heap.obj(x).props.get(&PropKey::named(name)).and_then(|p| match &p.slot {
            Slot::Data(v) => Some(v.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// The fields each interface's init dictionary adds, with their kinds (b = boolean, l =
/// long, s = DOMString, a = any, n = Node/Window/EventTarget or null), in the order WebIDL
/// reads them (inherited dictionaries first, then lexicographic).
fn init_fields(c: C) -> Vec<(&'static str, char, Value)> {
    let ui = vec![("detail", 'l', Value::Number(0.0)), ("view", 'n', Value::Null)];
    let modifiers = vec![
        ("altKey", 'b', Value::Bool(false)),
        ("ctrlKey", 'b', Value::Bool(false)),
        ("metaKey", 'b', Value::Bool(false)),
        ("shiftKey", 'b', Value::Bool(false)),
    ];
    match c {
        C::CustomEvent => vec![("detail", 'a', Value::Null)],
        C::UIEvent => ui,
        C::FocusEvent => [ui, vec![("relatedTarget", 'n', Value::Null)]].concat(),
        C::MouseEvent => [
            ui,
            modifiers,
            vec![
                ("button", 'l', Value::Number(0.0)),
                ("buttons", 'l', Value::Number(0.0)),
                ("clientX", 'l', Value::Number(0.0)),
                ("clientY", 'l', Value::Number(0.0)),
                ("relatedTarget", 'n', Value::Null),
                ("screenX", 'l', Value::Number(0.0)),
                ("screenY", 'l', Value::Number(0.0)),
            ],
        ]
        .concat(),
        C::KeyboardEvent => [
            ui,
            modifiers,
            vec![
                ("charCode", 'l', Value::Number(0.0)),
                ("code", 's', js("")),
                ("isComposing", 'b', Value::Bool(false)),
                ("key", 's', js("")),
                ("keyCode", 'l', Value::Number(0.0)),
                ("location", 'l', Value::Number(0.0)),
                ("repeat", 'b', Value::Bool(false)),
            ],
        ]
        .concat(),
        C::ErrorEvent => vec![
            ("colno", 'l', Value::Number(0.0)),
            ("error", 'a', Value::Undefined),
            ("filename", 's', js("")),
            ("lineno", 'l', Value::Number(0.0)),
            ("message", 's', js("")),
        ],
        _ => Vec::new(),
    }
}

/// Event and subclass constructors (DOM §2.5 "inner event creation steps").
pub fn event_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("Failed to construct: use 'new'") };
    if c.args.is_empty() {
        return vm.type_error("Failed to construct: 1 argument required");
    }
    let h = host(vm)?;
    let class = class::ALL.iter().copied().find(|&k| h.ctor(k) == c.callee).unwrap_or(C::Event);
    let ty = rstr(vm, &c.arg(0))?;
    let dict = c.arg(1);
    let mut f = INITIALIZED;
    for (name, bit) in [("bubbles", BUBBLES), ("cancelable", CANCELABLE), ("composed", COMPOSED)] {
        let v = dict_get(vm, &dict, name)?;
        if vm.to_boolean(&v) {
            f |= bit;
        }
    }
    let mut fields = Vec::new();
    for (name, kind, def) in init_fields(class) {
        let v = dict_get(vm, &dict, name)?;
        let v = match (kind, &v) {
            (_, Value::Undefined) => def,
            ('b', _) => Value::Bool(vm.to_boolean(&v)),
            ('l', _) => Value::Number(vm.to_int32(&v)? as f64),
            ('s', _) => js(&rstr(vm, &v)?),
            ('n', Value::Null) => Value::Null,
            ('n', _) => {
                if vm.host_data(&v).is_none() && !matches!(&v, Value::Object(o) if Some(*o) == h.window.get())
                {
                    return vm.type_error(&format!("{name} is not of the expected type"));
                }
                v
            }
            _ => v,
        };
        fields.push((name, v));
    }
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => Some(p),
        _ => None,
    };
    let o = new_event(vm, &h, class, proto, &ty, f);
    for (name, v) in fields {
        set_field(vm, o, name, v);
    }
    Ok(Value::Object(o))
}

pub fn custom_event_ctor(vm: &mut Vm, c: NativeCall) -> R {
    event_ctor(vm, c)
}

/// document.createEvent (DOM §4.5).
pub fn create_event(vm: &mut Vm, c: NativeCall) -> R {
    let name = rstr(vm, &c.arg(0))?.to_ascii_lowercase();
    this_of(vm, &c.this, C::Document)?;
    let class = match name.as_str() {
        "event" | "events" | "htmlevents" | "svgevents" => C::Event,
        "customevent" => C::CustomEvent,
        "uievent" | "uievents" => C::UIEvent,
        "mouseevent" | "mouseevents" => C::MouseEvent,
        "keyboardevent" => C::KeyboardEvent,
        "focusevent" => C::FocusEvent,
        _ => return throw(vm, "NotSupportedError", &format!("unknown event interface '{name}'")),
    };
    let h = host(vm)?;
    let o = new_event(vm, &h, class, None, "", 0);
    for (n, _, d) in init_fields(class) {
        set_field(vm, o, n, d);
    }
    Ok(Value::Object(o))
}

pub fn setup_event(vm: &mut Vm, proto: ObjRef, ctor: ObjRef) {
    for (n, v) in [("NONE", 0), ("CAPTURING_PHASE", 1), ("AT_TARGET", 2), ("BUBBLING_PHASE", 3)] {
        constant(vm, ctor, n, Value::Number(v as f64));
        constant(vm, proto, n, Value::Number(v as f64));
    }
    if let Ok(h) = host(vm) {
        let g = realm::native(vm, "get isTrusted", 0, |vm, c| {
            let o = event_obj(vm, &c.this)?;
            Ok(Value::Bool(flags(vm, o) & TRUSTED != 0))
        });
        h.misc.borrow_mut().insert("isTrusted", g);
    }
    attr(
        vm,
        proto,
        "type",
        |vm, c| {
            let o = event_obj(vm, &c.this)?;
            Ok(slot(vm, o, S_TYPE))
        },
        None,
    );
    attr(
        vm,
        proto,
        "target",
        |vm, c| {
            let o = event_obj(vm, &c.this)?;
            Ok(slot(vm, o, S_TARGET))
        },
        None,
    );
    attr(
        vm,
        proto,
        "srcElement",
        |vm, c| {
            let o = event_obj(vm, &c.this)?;
            Ok(slot(vm, o, S_TARGET))
        },
        None,
    );
    attr(
        vm,
        proto,
        "currentTarget",
        |vm, c| {
            let o = event_obj(vm, &c.this)?;
            Ok(slot(vm, o, S_CURRENT))
        },
        None,
    );
    attr(
        vm,
        proto,
        "eventPhase",
        |vm, c| {
            let o = event_obj(vm, &c.this)?;
            Ok(slot(vm, o, S_PHASE))
        },
        None,
    );
    attr(
        vm,
        proto,
        "timeStamp",
        |vm, c| {
            let o = event_obj(vm, &c.this)?;
            Ok(slot(vm, o, S_TIME))
        },
        None,
    );
    for (name, bit) in [
        ("bubbles", BUBBLES),
        ("cancelable", CANCELABLE),
        ("composed", COMPOSED),
        ("defaultPrevented", CANCELED),
    ] {
        attr_with(
            vm,
            proto,
            name,
            |vm, c| {
                let o = event_obj(vm, &c.this)?;
                let bit = match callee_slots(vm, c.callee).first() {
                    Some(Value::Number(n)) => *n as u32,
                    _ => 0,
                };
                Ok(Value::Bool(flags(vm, o) & bit != 0))
            },
            None,
            vec![Value::Number(bit as f64)],
        );
    }
    attr(
        vm,
        proto,
        "returnValue",
        |vm, c| {
            let o = event_obj(vm, &c.this)?;
            Ok(Value::Bool(flags(vm, o) & CANCELED == 0))
        },
        Some(|vm, c| {
            let o = event_obj(vm, &c.this)?;
            if !vm.to_boolean(&c.arg(0)) {
                cancel(vm, o);
            }
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "cancelBubble",
        |vm, c| {
            let o = event_obj(vm, &c.this)?;
            Ok(Value::Bool(flags(vm, o) & STOP != 0))
        },
        Some(|vm, c| {
            let o = event_obj(vm, &c.this)?;
            if vm.to_boolean(&c.arg(0)) {
                flag_on(vm, o, STOP, true);
            }
            Ok(Value::Undefined)
        }),
    );
    op(vm, proto, "composedPath", 0, |vm, c| {
        let o = event_obj(vm, &c.this)?;
        let items = match slot(vm, o, S_PATH) {
            Value::Object(a) if flags(vm, o) & DISPATCH != 0 => vm.list_from_array_like(&Value::Object(a))?,
            _ => Vec::new(),
        };
        Ok(Value::Object(vm.new_array(items)))
    });
    op(vm, proto, "stopPropagation", 0, |vm, c| {
        let o = event_obj(vm, &c.this)?;
        flag_on(vm, o, STOP, true);
        Ok(Value::Undefined)
    });
    op(vm, proto, "stopImmediatePropagation", 0, |vm, c| {
        let o = event_obj(vm, &c.this)?;
        flag_on(vm, o, STOP | STOP_IMMEDIATE, true);
        Ok(Value::Undefined)
    });
    op(vm, proto, "preventDefault", 0, |vm, c| {
        let o = event_obj(vm, &c.this)?;
        cancel(vm, o);
        Ok(Value::Undefined)
    });
    op(vm, proto, "initEvent", 1, |vm, c| {
        let ty = rstr(vm, &c.arg(0))?;
        let (b, ca) = (vm.to_boolean(&c.arg(1)), vm.to_boolean(&c.arg(2)));
        let o = event_obj(vm, &c.this)?;
        init_event(vm, o, &ty, b, ca);
        Ok(Value::Undefined)
    });
}

/// DOM §2.5 "initialize" (unless dispatching).
fn init_event(vm: &mut Vm, o: ObjRef, ty: &str, bubbles: bool, cancelable: bool) {
    let f = flags(vm, o);
    if f & DISPATCH != 0 {
        return;
    }
    let mut nf = (f & (TRUSTED | COMPOSED)) | INITIALIZED;
    if bubbles {
        nf |= BUBBLES;
    }
    if cancelable {
        nf |= CANCELABLE;
    }
    nf &= !TRUSTED;
    set_flags(vm, o, nf);
    set_slot(vm, o, S_TYPE, js(ty));
    set_slot(vm, o, S_TARGET, Value::Null);
}

/// DOM §2.5 "set the canceled flag".
fn cancel(vm: &mut Vm, o: ObjRef) {
    let f = flags(vm, o);
    if f & CANCELABLE != 0 && f & IN_PASSIVE == 0 {
        set_flags(vm, o, f | CANCELED);
    }
}

/// A subclass field getter (its name and default in the callee's slots).
fn field_getter(vm: &mut Vm, c: NativeCall) -> R {
    let o = event_obj(vm, &c.this)?;
    let s = callee_slots(vm, c.callee);
    let name = match s.first() {
        Some(v) => rstr(vm, v)?,
        None => return Ok(Value::Undefined),
    };
    Ok(get_field(vm, o, &name).unwrap_or_else(|| s.get(1).cloned().unwrap_or_default()))
}

fn define_fields(vm: &mut Vm, proto: ObjRef, c: C, skip: &[&str]) {
    for (name, _, def) in init_fields(c) {
        if !skip.contains(&name) {
            attr_with(vm, proto, name, field_getter, None, vec![js(name), def]);
        }
    }
}

pub fn setup_custom_event(vm: &mut Vm, proto: ObjRef) {
    define_fields(vm, proto, C::CustomEvent, &[]);
    op(vm, proto, "initCustomEvent", 1, |vm, c| {
        let ty = rstr(vm, &c.arg(0))?;
        let (b, ca) = (vm.to_boolean(&c.arg(1)), vm.to_boolean(&c.arg(2)));
        let o = event_obj(vm, &c.this)?;
        if flags(vm, o) & DISPATCH == 0 {
            init_event(vm, o, &ty, b, ca);
            let d = match c.arg(3) {
                Value::Undefined => Value::Null,
                v => v,
            };
            set_field(vm, o, "detail", d);
        }
        Ok(Value::Undefined)
    });
}

pub fn setup_ui_event(vm: &mut Vm, proto: ObjRef) {
    define_fields(vm, proto, C::UIEvent, &[]);
    attr(
        vm,
        proto,
        "which",
        |vm, c| {
            let o = event_obj(vm, &c.this)?;
            Ok(get_field(vm, o, "keyCode")
                .or_else(|| {
                    get_field(vm, o, "button").map(|b| match b {
                        Value::Number(n) => Value::Number(n + 1.0),
                        v => v,
                    })
                })
                .unwrap_or(Value::Number(0.0)))
        },
        None,
    );
}

pub fn setup_mouse_event(vm: &mut Vm, proto: ObjRef) {
    define_fields(vm, proto, C::MouseEvent, &["detail", "view"]);
    for (alias, field) in [
        ("x", "clientX"),
        ("y", "clientY"),
        ("pageX", "clientX"),
        ("pageY", "clientY"),
        ("offsetX", "clientX"),
        ("offsetY", "clientY"),
    ] {
        attr_with(vm, proto, alias, field_getter, None, vec![js(field), Value::Number(0.0)]);
    }
    op(vm, proto, "getModifierState", 1, |vm, c| {
        let k = rstr(vm, &c.arg(0))?;
        let o = event_obj(vm, &c.this)?;
        let f = match k.as_str() {
            "Alt" => "altKey",
            "Control" => "ctrlKey",
            "Meta" => "metaKey",
            "Shift" => "shiftKey",
            _ => return Ok(Value::Bool(false)),
        };
        Ok(get_field(vm, o, f).unwrap_or(Value::Bool(false)))
    });
}

pub fn setup_keyboard_event(vm: &mut Vm, proto: ObjRef) {
    define_fields(vm, proto, C::KeyboardEvent, &["detail", "view"]);
    for (n, v) in [
        ("DOM_KEY_LOCATION_STANDARD", 0),
        ("DOM_KEY_LOCATION_LEFT", 1),
        ("DOM_KEY_LOCATION_RIGHT", 2),
        ("DOM_KEY_LOCATION_NUMPAD", 3),
    ] {
        constant(vm, proto, n, Value::Number(v as f64));
    }
}

pub fn setup_error_event(vm: &mut Vm, proto: ObjRef) {
    define_fields(vm, proto, C::ErrorEvent, &[]);
}

// ---- EventTarget ----

pub fn event_target_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("EventTarget requires 'new'") };
    let h = host(vm)?;
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => p,
        _ => h.proto(C::EventTarget),
    };
    Ok(Value::Object(vm.new_host_object(Some(proto), C::EventTarget as u32, 0, false)))
}

/// The object an EventTarget method acts on: a platform EventTarget, or the window for
/// the global object (and an undefined `this`, as in a bare call in a page script).
fn target_of(vm: &mut Vm, h: &Host, this: &Value) -> JsResult<ObjRef> {
    match this {
        Value::Undefined | Value::Null => match h.window.get() {
            Some(w) => Ok(w),
            None => vm.type_error("no window"),
        },
        Value::Object(o) if Some(*o) == h.window.get() => Ok(*o),
        Value::Object(o) => match vm.host_data(this) {
            Some((c, _)) if class::implements(c, C::EventTarget) => Ok(*o),
            _ => vm.type_error("Illegal invocation: not an EventTarget"),
        },
        _ => vm.type_error("Illegal invocation: not an EventTarget"),
    }
}

/// DOM §2.7 "flatten more" of the options argument: (capture, once, passive).
fn listener_options(vm: &mut Vm, v: &Value, full: bool) -> JsResult<(bool, bool, bool)> {
    match v {
        Value::Object(_) => {
            let cap = dict_get(vm, v, "capture")?;
            let cap = vm.to_boolean(&cap);
            if !full {
                return Ok((cap, false, false));
            }
            let once = dict_get(vm, v, "once")?;
            let passive = dict_get(vm, v, "passive")?;
            let _signal = dict_get(vm, v, "signal")?;
            Ok((cap, vm.to_boolean(&once), vm.to_boolean(&passive)))
        }
        v => Ok((vm.to_boolean(v), false, false)),
    }
}

fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => x == y,
        _ => false,
    }
}

pub fn setup_event_target(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "addEventListener", 2, |vm, c| {
        let h = host(vm)?;
        let t = target_of(vm, &h, &c.this)?;
        let ty = rstr(vm, &c.arg(0))?;
        let cb = c.arg(1);
        let (capture, once, passive) = listener_options(vm, &c.arg(2), true)?;
        if !matches!(cb, Value::Object(_)) {
            if cb.is_nullish() {
                return Ok(Value::Undefined);
            }
            return vm.type_error("the listener must be an object");
        }
        let mut ls = h.listeners.borrow_mut();
        let list = ls.entry(t).or_default();
        if list
            .iter()
            .any(|l| !l.handler && l.ty == ty && l.capture == capture && same_value(&l.callback, &cb))
        {
            return Ok(Value::Undefined);
        }
        list.push(Listener {
            ty,
            callback: cb,
            capture,
            once,
            passive,
            removed: Rc::new(Cell::new(false)),
            handler: false,
        });
        Ok(Value::Undefined)
    });
    op(vm, proto, "removeEventListener", 2, |vm, c| {
        let h = host(vm)?;
        let t = target_of(vm, &h, &c.this)?;
        let ty = rstr(vm, &c.arg(0))?;
        let cb = c.arg(1);
        let (capture, _, _) = listener_options(vm, &c.arg(2), false)?;
        if let Some(list) = h.listeners.borrow_mut().get_mut(&t) {
            if let Some(i) = list.iter().position(|l| {
                !l.handler && l.ty == ty && l.capture == capture && same_value(&l.callback, &cb)
            }) {
                list[i].removed.set(true);
                list.remove(i);
            }
        }
        Ok(Value::Undefined)
    });
    op(vm, proto, "dispatchEvent", 1, |vm, c| {
        let h = host(vm)?;
        let t = target_of(vm, &h, &c.this)?;
        let e = event_obj(vm, &c.arg(0))?;
        let f = flags(vm, e);
        if f & DISPATCH != 0 || f & INITIALIZED == 0 {
            return throw(vm, "InvalidStateError", "the event is being dispatched or not initialized");
        }
        flag_on(vm, e, TRUSTED, false);
        let r = dispatch(vm, &h, e, Value::Object(t))?;
        Ok(Value::Bool(r))
    });
}

/// The event path (DOM §2.9 "get the parent"): the target, its ancestors, and the window
/// for a node in the window's document (except for `load`).
fn event_path(vm: &mut Vm, h: &Host, target: &Value, ty: &str) -> JsResult<Vec<ObjRef>> {
    let Value::Object(t) = target else { return Ok(Vec::new()) };
    let mut path = vec![*t];
    if let Some(n) = node_of(vm, target) {
        let anc: Vec<bb_dom::NodeId> = with_doc(vm, h, n.doc, |d| d.ancestors(n.id).collect())?;
        let mut top = n.id;
        for a in anc {
            top = a;
            if let Value::Object(o) = wrap(vm, h, n.with(a))? {
                path.push(o);
            }
        }
        if n.doc == 0 && top == bb_dom::Document::ROOT && ty != "load" {
            if let Some(w) = h.window.get() {
                path.push(w);
            }
        }
    }
    Ok(path)
}

/// DOM §2.9 "dispatch" event `e` at `target`; returns false if it was canceled.
pub fn dispatch(vm: &mut Vm, h: &Host, e: ObjRef, target: Value) -> JsResult<bool> {
    let ty = rstr(vm, &slot(vm, e, S_TYPE))?;
    flag_on(vm, e, DISPATCH, true);
    set_slot(vm, e, S_TARGET, target.clone());
    let path = event_path(vm, h, &target, &ty)?;
    let arr = vm.new_array(path.iter().map(|o| Value::Object(*o)).collect());
    set_slot(vm, e, S_PATH, Value::Object(arr));
    let bubbles = flags(vm, e) & BUBBLES != 0;
    // Capturing: from the top down to the target.
    for (i, &t) in path.iter().enumerate().rev() {
        if flags(vm, e) & STOP != 0 {
            break;
        }
        let phase = if i == 0 { 2 } else { 1 };
        invoke(vm, h, e, t, &ty, phase, true)?;
    }
    // Bubbling: the target, then (for bubbling events) back up.
    for (i, &t) in path.iter().enumerate() {
        if flags(vm, e) & STOP != 0 {
            break;
        }
        if i > 0 && !bubbles {
            break;
        }
        let phase = if i == 0 { 2 } else { 3 };
        invoke(vm, h, e, t, &ty, phase, false)?;
    }
    set_slot(vm, e, S_PHASE, Value::Number(0.0));
    set_slot(vm, e, S_CURRENT, Value::Null);
    set_slot(vm, e, S_PATH, Value::Undefined);
    let f = flags(vm, e);
    set_flags(vm, e, f & !(DISPATCH | STOP | STOP_IMMEDIATE));
    Ok(f & CANCELED == 0)
}

/// DOM §2.10 "inner invoke" of `t`'s listeners for the capture or bubble pass.
fn invoke(
    vm: &mut Vm,
    h: &Host,
    e: ObjRef,
    t: ObjRef,
    ty: &str,
    phase: u8,
    capture_pass: bool,
) -> JsResult<()> {
    set_slot(vm, e, S_CURRENT, Value::Object(t));
    set_slot(vm, e, S_PHASE, Value::Number(phase as f64));
    let snapshot: Vec<(Value, bool, bool, bool, Rc<Cell<bool>>)> = match h.listeners.borrow().get(&t) {
        Some(list) => list
            .iter()
            .filter(|l| l.ty == ty && l.capture == capture_pass)
            .map(|l| (l.callback.clone(), l.once, l.passive, l.handler, l.removed.clone()))
            .collect(),
        None => return Ok(()),
    };
    for (cb, once, passive, handler, removed) in snapshot {
        if removed.get() {
            continue;
        }
        if once {
            removed.set(true);
            if let Some(list) = h.listeners.borrow_mut().get_mut(&t) {
                list.retain(|l| !Rc::ptr_eq(&l.removed, &removed));
            }
        }
        if passive {
            flag_on(vm, e, IN_PASSIVE, true);
        }
        let r = if handler { call_handler(vm, h, e, t, ty) } else { call_listener(vm, &cb, t, e) };
        if passive {
            flag_on(vm, e, IN_PASSIVE, false);
        }
        if let Err(err) = r {
            if vm.terminated {
                return Err(err);
            }
            report_exception(vm, err);
        }
        if flags(vm, e) & STOP_IMMEDIATE != 0 {
            break;
        }
    }
    Ok(())
}

/// WebIDL §3.11 "call a user object's operation" handleEvent.
fn call_listener(vm: &mut Vm, cb: &Value, t: ObjRef, e: ObjRef) -> JsResult<()> {
    if vm.is_callable(cb) {
        vm.call(cb, Value::Object(t), vec![Value::Object(e)])?;
        return Ok(());
    }
    let Value::Object(o) = cb else { return Ok(()) };
    let f = vm.get(*o, &PropKey::lit("handleEvent"))?;
    if !vm.is_callable(&f) {
        return vm.type_error("the listener has no handleEvent method");
    }
    vm.call(&f, cb.clone(), vec![Value::Object(e)])?;
    Ok(())
}

// ---- event handlers (HTML §8.1.8) ----

const HANDLERS: &[&str] = &[
    "onabort",
    "onauxclick",
    "onbeforeinput",
    "onblur",
    "oncancel",
    "oncanplay",
    "oncanplaythrough",
    "onchange",
    "onclick",
    "onclose",
    "oncontextmenu",
    "oncopy",
    "oncuechange",
    "oncut",
    "ondblclick",
    "ondrag",
    "ondragend",
    "ondragenter",
    "ondragleave",
    "ondragover",
    "ondragstart",
    "ondrop",
    "ondurationchange",
    "onemptied",
    "onended",
    "onerror",
    "onfocus",
    "onformdata",
    "oninput",
    "oninvalid",
    "onkeydown",
    "onkeypress",
    "onkeyup",
    "onload",
    "onloadeddata",
    "onloadedmetadata",
    "onloadstart",
    "onmousedown",
    "onmouseenter",
    "onmouseleave",
    "onmousemove",
    "onmouseout",
    "onmouseover",
    "onmouseup",
    "onpaste",
    "onpause",
    "onplay",
    "onplaying",
    "onprogress",
    "onratechange",
    "onreset",
    "onresize",
    "onscroll",
    "onscrollend",
    "onseeked",
    "onseeking",
    "onselect",
    "onslotchange",
    "onstalled",
    "onsubmit",
    "onsuspend",
    "ontimeupdate",
    "ontoggle",
    "onvolumechange",
    "onwaiting",
    "onwheel",
    "onpointerdown",
    "onpointerup",
    "onpointermove",
    "onpointerover",
    "onpointerout",
    "onpointerenter",
    "onpointerleave",
    "onpointercancel",
    "ongotpointercapture",
    "onlostpointercapture",
    "onanimationstart",
    "onanimationend",
    "onanimationiteration",
    "ontransitionend",
    "ontransitionstart",
    "ontransitionrun",
    "ontransitioncancel",
];

/// WindowEventHandlers (HTML §8.1.8.2): on the window, and reflected by body/frameset.
const WINDOW_HANDLERS: &[&str] = &[
    "onafterprint",
    "onbeforeprint",
    "onbeforeunload",
    "onhashchange",
    "onlanguagechange",
    "onmessage",
    "onmessageerror",
    "onoffline",
    "ononline",
    "onpagehide",
    "onpageshow",
    "onpopstate",
    "onrejectionhandled",
    "onstorage",
    "onunhandledrejection",
    "onunload",
];

/// Body/frameset handlers that are the window's (HTML §8.1.8.2 "Window-reflecting body
/// element event handler set").
const BODY_TO_WINDOW: &[&str] = &["onblur", "onerror", "onfocus", "onload", "onresize", "onscroll"];

pub fn setup_event_handlers(vm: &mut Vm, proto: ObjRef) {
    for &name in HANDLERS {
        attr_with(vm, proto, name, handler_get, Some(handler_set), vec![js(name)]);
    }
}

pub fn setup_window_handlers(vm: &mut Vm, proto: ObjRef) {
    for &name in HANDLERS.iter().chain(WINDOW_HANDLERS) {
        attr_with(vm, proto, name, handler_get, Some(handler_set), vec![js(name)]);
    }
}

/// The object a handler named `name` on `this` belongs to.
fn handler_target(vm: &mut Vm, h: &Host, this: &Value, name: &str) -> JsResult<ObjRef> {
    let t = target_of(vm, h, this)?;
    if let Some(n) = node_of(vm, this) {
        let body = with_doc(vm, h, n.doc, |d| d.is_html(n.id, "body") || d.is_html(n.id, "frameset"))?;
        if body && n.doc == 0 && (BODY_TO_WINDOW.contains(&name) || WINDOW_HANDLERS.contains(&name)) {
            if let Some(w) = h.window.get() {
                return Ok(w);
            }
        }
    }
    Ok(t)
}

fn handler_name(vm: &mut Vm, c: &NativeCall) -> JsResult<String> {
    match callee_slots(vm, c.callee).first() {
        Some(v) => rstr(vm, v),
        None => Ok(String::new()),
    }
}

fn handler_get(vm: &mut Vm, c: NativeCall) -> R {
    let name = handler_name(vm, &c)?;
    let h = host(vm)?;
    let t = handler_target(vm, &h, &c.this, &name)?;
    current_handler(vm, &h, t, &name)
}

fn handler_set(vm: &mut Vm, c: NativeCall) -> R {
    let name = handler_name(vm, &c)?;
    let h = host(vm)?;
    let t = handler_target(vm, &h, &c.this, &name)?;
    let v = c.arg(0);
    let v = if vm.is_callable(&v) { v } else { Value::Null };
    set_handler(&h, t, &name, v);
    Ok(Value::Undefined)
}

/// Set handler `name` of `t` (a function, source text, or null), registering its listener
/// the first time (HTML §8.1.8.1 "activate an event handler").
fn set_handler(h: &Host, t: ObjRef, name: &str, v: Value) {
    h.handlers.borrow_mut().insert((t, name.to_string()), v);
    let ty = name.trim_start_matches("on").to_string();
    let mut ls = h.listeners.borrow_mut();
    let list = ls.entry(t).or_default();
    if !list.iter().any(|l| l.handler && l.ty == ty) {
        list.push(Listener {
            ty,
            callback: Value::Undefined,
            capture: false,
            once: false,
            passive: false,
            removed: Rc::new(Cell::new(false)),
            handler: true,
        });
    }
}

/// HTML §8.1.8.1 "get the current value of the event handler": source text is compiled
/// (once) into a function with the element, its form and the document in scope.
fn current_handler(vm: &mut Vm, h: &Host, t: ObjRef, name: &str) -> R {
    let v = h.handlers.borrow().get(&(t, name.to_string())).cloned().unwrap_or(Value::Null);
    let Value::String(src) = &v else { return Ok(v) };
    let body = bb_js::vm::to_rust(src);
    let is_window = Some(t) == h.window.get();
    let params = if is_window && name == "onerror" { "event, source, lineno, colno, error" } else { "event" };
    let wrapper = format!(
        "(function(){{ with (arguments[0]) with (arguments[1]) with (arguments[2]) return function {name}({params}) {{\n{body}\n}}; }})"
    );
    let f = match vm.run_script(&wrapper) {
        Ok(maker) => {
            let empty = Value::Object(vm.new_object_with_proto(None));
            let (doc, form, el) = match node_of(vm, &Value::Object(t)) {
                Some(n) => {
                    let doc = wrap(vm, h, n.with(bb_dom::Document::ROOT))?;
                    let form = with_doc(vm, h, n.doc, |d| d.form_owner(n.id))?;
                    let form = match form {
                        Some(f) => wrap(vm, h, n.with(f))?,
                        None => empty.clone(),
                    };
                    (doc, form, Value::Object(t))
                }
                None => (empty.clone(), empty.clone(), empty),
            };
            match vm.call(&maker, Value::Undefined, vec![doc, form, el]) {
                Ok(f) => f,
                Err(e) => {
                    report_exception(vm, e);
                    Value::Null
                }
            }
        }
        Err(e) => {
            report_exception(vm, e);
            Value::Null
        }
    };
    h.handlers.borrow_mut().insert((t, name.to_string()), f.clone());
    Ok(f)
}

/// HTML §8.1.8.1 "the event handler processing algorithm".
fn call_handler(vm: &mut Vm, h: &Host, e: ObjRef, t: ObjRef, ty: &str) -> JsResult<()> {
    let name = format!("on{ty}");
    let f = current_handler(vm, h, t, &name)?;
    if !vm.is_callable(&f) {
        return Ok(());
    }
    let is_error_event =
        ty == "error" && vm.host_data(&Value::Object(e)).is_some_and(|(c, _)| c == C::ErrorEvent as u32);
    let ret = if is_error_event && Some(t) == h.window.get() {
        let args = ["message", "filename", "lineno", "colno", "error"]
            .iter()
            .map(|n| get_field(vm, e, n).unwrap_or_default())
            .collect();
        vm.call(&f, Value::Object(t), args)?
    } else {
        vm.call(&f, Value::Object(t), vec![Value::Object(e)])?
    };
    let cancel_it =
        if is_error_event { matches!(ret, Value::Bool(true)) } else { matches!(ret, Value::Bool(false)) };
    if cancel_it {
        cancel(vm, e);
    }
    Ok(())
}

/// A content attribute changed: `on…` attributes set the handler's source (HTML §8.1.8.1
/// "attribute change steps").
pub fn attribute_changed(vm: &mut Vm, h: &Host, n: NodeRef, name: &str) -> JsResult<()> {
    let name = name.to_ascii_lowercase();
    if !name.starts_with("on")
        || !(HANDLERS.contains(&name.as_str()) || WINDOW_HANDLERS.contains(&name.as_str()))
    {
        return Ok(());
    }
    let src = with_doc(vm, h, n.doc, |d| d.attr(n.id, &name).map(str::to_string))?;
    let el = wrap(vm, h, n)?;
    let t = handler_target(vm, h, &el, &name)?;
    let v = match src {
        Some(s) => js(&s),
        None => Value::Null,
    };
    set_handler(h, t, &name, v);
    Ok(())
}

/// Register the handlers of `on…` attributes an element got from the parser.
pub fn parsed_attributes(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<()> {
    let names: Vec<String> = with_doc(vm, h, n.doc, |d| {
        d.element(n.id)
            .map(|e| e.attrs.iter().filter(|a| a.name.starts_with("on")).map(|a| a.name.clone()).collect())
    })?
    .unwrap_or_default();
    for name in names {
        attribute_changed(vm, h, n, &name)?;
    }
    Ok(())
}

// ---- firing ----

/// Fire an event named `ty` at `target` (HTML §8.1.7.1 "fire an event").
pub fn fire(
    vm: &mut Vm,
    h: &Host,
    target: Value,
    ty: &str,
    bubbles: bool,
    cancelable: bool,
) -> JsResult<bool> {
    let mut f = INITIALIZED | TRUSTED;
    if bubbles {
        f |= BUBBLES;
    }
    if cancelable {
        f |= CANCELABLE;
    }
    let e = new_event(vm, h, C::Event, None, ty, f);
    dispatch(vm, h, e, target)
}

/// A trusted UI event (from real input) of interface `class`, with its init fields set to
/// their defaults and `view` to the window.
pub fn trusted_event(vm: &mut Vm, h: &Host, class: C, ty: &str, bubbles: bool, cancelable: bool) -> ObjRef {
    let mut f = INITIALIZED | TRUSTED | COMPOSED;
    if bubbles {
        f |= BUBBLES;
    }
    if cancelable {
        f |= CANCELABLE;
    }
    let e = new_event(vm, h, class, None, ty, f);
    for (name, _, d) in init_fields(class) {
        set_field(vm, e, name, d);
    }
    if let Some(w) = h.window.get() {
        if class != C::Event {
            set_field(vm, e, "view", Value::Object(w));
        }
    }
    e
}

/// HTMLElement.click (HTML §6.5.3): a synthetic, untrusted click.
pub fn fire_synthetic_click(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<()> {
    let e = new_event(vm, h, C::MouseEvent, None, "click", INITIALIZED | BUBBLES | CANCELABLE | COMPOSED);
    for (name, _, d) in init_fields(C::MouseEvent) {
        set_field(vm, e, name, d);
    }
    if let Some(w) = h.window.get() {
        set_field(vm, e, "view", Value::Object(w));
    }
    let t = wrap(vm, h, n)?;
    dispatch(vm, h, e, t)?;
    Ok(())
}

/// HTML §8.1.4.6 "report an exception": an ErrorEvent at the window; if nothing cancels
/// it, the error is logged.
pub fn report_exception(vm: &mut Vm, err: Value) {
    let msg = vm.describe(&err);
    let Ok(h) = host(vm) else { return };
    if h.reporting.get() {
        h.errors.borrow_mut().push(msg);
        return;
    }
    h.reporting.set(true);
    let mut handled = false;
    if let Some(w) = h.window.get() {
        let e = new_event(vm, &h, C::ErrorEvent, None, "error", INITIALIZED | TRUSTED | CANCELABLE);
        set_field(vm, e, "message", js(&format!("Uncaught {msg}")));
        set_field(vm, e, "filename", js(&h.url.borrow().href()));
        set_field(vm, e, "lineno", Value::Number(0.0));
        set_field(vm, e, "colno", Value::Number(0.0));
        set_field(vm, e, "error", err);
        handled = matches!(dispatch(vm, &h, e, Value::Object(w)), Ok(false));
    }
    h.reporting.set(false);
    if !handled {
        log::info!("uncaught exception: {msg}");
        h.console.borrow_mut().push(format!("Uncaught {msg}"));
        h.errors.borrow_mut().push(msg);
    }
}

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

pub fn get_field(vm: &Vm, o: ObjRef, name: &str) -> Option<Value> {
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
        C::ProgressEvent => vec![
            ("lengthComputable", 'b', Value::Bool(false)),
            ("loaded", 'd', Value::Number(0.0)),
            ("total", 'd', Value::Number(0.0)),
        ],
        C::FormDataEvent => vec![("formData", 'F', Value::Undefined)],
        C::SubmitEvent => vec![("submitter", 'n', Value::Null)],
        C::ToggleEvent => {
            vec![("newState", 's', js("")), ("oldState", 's', js("")), ("source", 'n', Value::Null)]
        }
        C::MessageEvent => vec![
            ("data", 'a', Value::Null),
            ("lastEventId", 's', js("")),
            ("origin", 's', js("")),
            ("ports", 'P', Value::Undefined),
            ("source", 'a', Value::Null),
        ],
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
            ('P', _) => {
                // A frozen array of MessagePorts (none can exist here yet, so only empty).
                let a = vm.new_array(Vec::new());
                Value::Object(a)
            }
            ('F', _) => {
                if !vm.host_data(&v).is_some_and(|(k, _)| k == C::FormData as u32) {
                    return vm.type_error("FormDataEvent needs a FormData formData");
                }
                v
            }
            (_, Value::Undefined) => def,
            ('b', _) => Value::Bool(vm.to_boolean(&v)),
            ('l', _) => Value::Number(vm.to_int32(&v)? as f64),
            ('d', _) => Value::Number(vm.to_number(&v)?),
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
        let items = if flags(vm, o) & DISPATCH != 0 { composed_path(vm, o)? } else { Vec::new() };
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
pub fn field_getter(vm: &mut Vm, c: NativeCall) -> R {
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

/// SubmitEvent (HTML §4.10.21.3): its submitter.
pub fn setup_submit_event(vm: &mut Vm, proto: ObjRef) {
    define_fields(vm, proto, C::SubmitEvent, &[]);
}

/// ToggleEvent (HTML §8.1.8.4): oldState and newState.
pub fn setup_toggle_event(vm: &mut Vm, proto: ObjRef) {
    define_fields(vm, proto, C::ToggleEvent, &[]);
}

/// Fire the submit event at `form` (HTML §4.10.21.3 "submit" step 6.3): a trusted,
/// bubbling, cancelable SubmitEvent. Returns false if a listener canceled it.
pub fn fire_submit(vm: &mut Vm, h: &Host, form: NodeRef, submitter: Option<NodeRef>) -> JsResult<bool> {
    let e = trusted_event(vm, h, C::SubmitEvent, "submit", true, true);
    let s = match submitter {
        Some(s) => wrap(vm, h, s)?,
        None => Value::Null,
    };
    set_field(vm, e, "submitter", s);
    let t = wrap(vm, h, form)?;
    dispatch(vm, h, e, t)
}

/// The legacy initXEvent methods (UI Events §A): type, bubbles, cancelable, then `fields`.
fn legacy_init(vm: &mut Vm, c: &NativeCall, fields: &[(&str, char)]) -> R {
    let ty = rstr(vm, &c.arg(0))?;
    let (b, ca) = (vm.to_boolean(&c.arg(1)), vm.to_boolean(&c.arg(2)));
    let o = event_obj(vm, &c.this)?;
    let mut vals = Vec::with_capacity(fields.len());
    for (i, (_, k)) in fields.iter().enumerate() {
        let a = c.arg(3 + i);
        vals.push(match k {
            'b' => Value::Bool(vm.to_boolean(&a)),
            'l' => Value::Number(vm.to_int32(&a)? as f64),
            's' => js(&rstr(vm, &a)?),
            _ if a.is_nullish() => Value::Null,
            _ if matches!(a, Value::Object(_)) => a,
            _ => return vm.type_error("expected an object or null"),
        });
    }
    if flags(vm, o) & DISPATCH == 0 {
        init_event(vm, o, &ty, b, ca);
        for ((name, _), v) in fields.iter().zip(vals) {
            set_field(vm, o, name, v);
        }
    }
    Ok(Value::Undefined)
}

pub fn setup_ui_event(vm: &mut Vm, proto: ObjRef) {
    define_fields(vm, proto, C::UIEvent, &[]);
    op(vm, proto, "initUIEvent", 1, |vm, c| legacy_init(vm, &c, &[("view", 'n'), ("detail", 'l')]));
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

/// UI Events §5.3 FocusEvent: relatedTarget.
pub fn setup_focus_event(vm: &mut Vm, proto: ObjRef) {
    define_fields(vm, proto, C::FocusEvent, &["detail", "view"]);
}

pub fn setup_mouse_event(vm: &mut Vm, proto: ObjRef) {
    define_fields(vm, proto, C::MouseEvent, &["detail", "view"]);
    op(vm, proto, "initMouseEvent", 1, |vm, c| {
        legacy_init(
            vm,
            &c,
            &[
                ("view", 'n'),
                ("detail", 'l'),
                ("screenX", 'l'),
                ("screenY", 'l'),
                ("clientX", 'l'),
                ("clientY", 'l'),
                ("ctrlKey", 'b'),
                ("altKey", 'b'),
                ("shiftKey", 'b'),
                ("metaKey", 'b'),
                ("button", 'l'),
                ("relatedTarget", 'n'),
            ],
        )
    });
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
    op(vm, proto, "initKeyboardEvent", 1, |vm, c| {
        legacy_init(
            vm,
            &c,
            &[
                ("view", 'n'),
                ("key", 's'),
                ("location", 'l'),
                ("ctrlKey", 'b'),
                ("altKey", 'b'),
                ("shiftKey", 'b'),
                ("metaKey", 'b'),
            ],
        )
    });
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

/// DOM §2.7 "flatten more" of the options argument: (capture, once, passive, signal).
type ListenerOptions = (bool, bool, bool, Option<ObjRef>);

fn listener_options(vm: &mut Vm, v: &Value, full: bool) -> JsResult<ListenerOptions> {
    match v {
        Value::Object(_) => {
            let cap = dict_get(vm, v, "capture")?;
            let cap = vm.to_boolean(&cap);
            if !full {
                return Ok((cap, false, false, None));
            }
            let once = dict_get(vm, v, "once")?;
            let passive = dict_get(vm, v, "passive")?;
            let signal = match dict_get(vm, v, "signal")? {
                Value::Undefined => None,
                s => match (&s, vm.host_data(&s)) {
                    (Value::Object(o), Some((c, _))) if c == C::AbortSignal as u32 => Some(*o),
                    _ => return vm.type_error("signal is not an AbortSignal"),
                },
            };
            Ok((cap, vm.to_boolean(&once), vm.to_boolean(&passive), signal))
        }
        v => Ok((vm.to_boolean(v), false, false, None)),
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
        let (capture, once, passive, signal) = listener_options(vm, &c.arg(2), true)?;
        if signal.is_some_and(|s| crate::net::signal_aborted(vm, s)) {
            return Ok(Value::Undefined);
        }
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
            signal,
        });
        Ok(Value::Undefined)
    });
    op(vm, proto, "removeEventListener", 2, |vm, c| {
        let h = host(vm)?;
        let t = target_of(vm, &h, &c.this)?;
        let ty = rstr(vm, &c.arg(0))?;
        let cb = c.arg(1);
        let (capture, ..) = listener_options(vm, &c.arg(2), false)?;
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

// An event's path (DOM §2.9) lives in slot 7 while it is dispatched, as a flat array of
// PATH_STRIDE values per struct: invocation target, shadow-adjusted target (or null),
// relatedTarget (or null), and flags.
const PATH_STRIDE: usize = 4;
const P_IN_SHADOW: u32 = 1;
const P_ROOT_CLOSED: u32 = 2;
const P_SLOT_CLOSED: u32 = 4;

struct PathItem {
    target: ObjRef,
    adjusted: Option<ObjRef>,
    related: Value,
    flags: u32,
}

fn read_path(vm: &mut Vm, e: ObjRef) -> JsResult<Vec<PathItem>> {
    let Value::Object(a) = slot(vm, e, S_PATH) else { return Ok(Vec::new()) };
    let items = vm.list_from_array_like(&Value::Object(a))?;
    Ok(items
        .chunks(PATH_STRIDE)
        .filter_map(|c| {
            let Value::Object(target) = c[0] else { return None };
            Some(PathItem {
                target,
                adjusted: match c[1] {
                    Value::Object(o) => Some(o),
                    _ => None,
                },
                related: c[2].clone(),
                flags: match c[3] {
                    Value::Number(n) => n as u32,
                    _ => 0,
                },
            })
        })
        .collect())
}

/// DOM §4.2.2.1 "retarget" `a` against `b`: `a`, or the host of the shadow tree it is in,
/// moved out until `b` can see it.
pub fn retarget(vm: &mut Vm, h: &Host, a: &Value, b: &Value) -> JsResult<Value> {
    let Some(mut n) = node_of(vm, a) else { return Ok(a.clone()) };
    let bn = node_of(vm, b);
    let out = with_doc(vm, h, n.doc, |d| loop {
        let root = d.tree_root(n.id);
        let Some(host) = d.shadow_host(root) else { return n };
        if bn.is_some_and(|b| b.doc == n.doc && d.is_shadow_including_inclusive_ancestor(root, b.id)) {
            return n;
        }
        n = n.with(host);
    })?;
    if out == node_of(vm, a).unwrap_or(out) {
        return Ok(a.clone());
    }
    wrap(vm, h, out)
}

/// DOM §2.9 "get the parent" of `t` for an event of type `ty`; `first` is the path's first
/// invocation target.
fn get_the_parent(
    vm: &mut Vm,
    h: &Host,
    t: &Value,
    ty: &str,
    composed: bool,
    first: &Value,
) -> JsResult<Value> {
    let Some(n) = node_of(vm, t) else { return Ok(Value::Null) };
    let first = node_of(vm, first);
    enum P {
        Node(bb_dom::NodeId),
        Window,
        None,
    }
    let p = with_doc(vm, h, n.doc, |d| {
        if n.id == bb_dom::Document::ROOT {
            // A document's parent is its window, except for load events.
            return if n.doc == 0 && ty != "load" { P::Window } else { P::None };
        }
        if let Some(host) = d.shadow_host(n.id) {
            let is_first_root = first.is_some_and(|f| f.doc == n.doc && d.tree_root(f.id) == n.id);
            return if !composed && is_first_root { P::None } else { P::Node(host) };
        }
        match d.find_slot(n.id, false).or_else(|| d.parent(n.id)) {
            Some(p) => P::Node(p),
            None => P::None,
        }
    })?;
    match p {
        P::Node(id) => wrap(vm, h, n.with(id)),
        P::Window => Ok(h.window.get().map_or(Value::Null, Value::Object)),
        P::None => Ok(Value::Null),
    }
}

/// Facts about a path entry: (is in a shadow tree, is a closed shadow root, its root).
fn node_facts(vm: &mut Vm, h: &Host, v: &Value) -> JsResult<(bool, bool, Option<NodeRef>)> {
    let Some(n) = node_of(vm, v) else { return Ok((false, false, None)) };
    with_doc(vm, h, n.doc, |d| {
        let root = d.tree_root(n.id);
        let in_shadow = d.is_shadow_root(root);
        let closed = d.shadow_info(n.id).is_some_and(|i| !i.open);
        (in_shadow, closed, Some(n.with(root)))
    })
}

/// DOM §2.9 "dispatch" event `e` at `target`; returns false if it was canceled.
pub fn dispatch(vm: &mut Vm, h: &Host, e: ObjRef, target: Value) -> JsResult<bool> {
    let ty = rstr(vm, &slot(vm, e, S_TYPE))?;
    // Step 1.
    flag_on(vm, e, DISPATCH, true);
    let f0 = flags(vm, e);
    let bubbles = f0 & BUBBLES != 0;
    let composed = f0 & COMPOSED != 0;
    // Step 3: a script's click event gets the activation behavior of its target (or the
    // nearest inclusive ancestor in its path with one); the browser does that itself for
    // trusted clicks.
    let is_activation = ty == "click"
        && f0 & TRUSTED == 0
        && matches!(vm.host_data(&Value::Object(e)), Some((c, _)) if c == C::MouseEvent as u32);
    let mut activation: Option<NodeRef> = None;
    let has_activation = |vm: &mut Vm, v: &Value| -> JsResult<Option<NodeRef>> {
        let Some(n) = node_of(vm, v) else { return Ok(None) };
        Ok(with_doc(vm, h, n.doc, |d| crate::activation::has_activation(d, n.id))?.then_some(n))
    };
    // Step 4.
    let original_related = get_field(vm, e, "relatedTarget").unwrap_or(Value::Null);
    let related = retarget(vm, h, &original_related, &target)?;
    let mut clear_targets = false;
    let mut flat: Vec<Value> = Vec::new();
    let mut push =
        |vm: &mut Vm, t: &Value, adjusted: Value, related: Value, slot_closed: bool| -> JsResult<()> {
            let (in_shadow, root_closed, _) = node_facts(vm, h, t)?;
            let mut fl = 0;
            if in_shadow {
                fl |= P_IN_SHADOW;
            }
            if root_closed {
                fl |= P_ROOT_CLOSED;
            }
            if slot_closed {
                fl |= P_SLOT_CLOSED;
            }
            flat.extend([t.clone(), adjusted, related, Value::Number(fl as f64)]);
            Ok(())
        };
    // Step 5.
    let same = |a: &Value, b: &Value| matches!((a, b), (Value::Object(x), Value::Object(y)) if x == y);
    if !same(&target, &related) || same(&target, &original_related) {
        push(vm, &target, target.clone(), related.clone(), false)?;
        if is_activation {
            activation = has_activation(vm, &target)?;
        }
        let assigned = |vm: &mut Vm, v: &Value| -> JsResult<bool> {
            let Some(n) = node_of(vm, v) else { return Ok(false) };
            with_doc(vm, h, n.doc, |d| d.is_slottable(n.id) && d.find_slot(n.id, false).is_some())
        };
        let mut slottable = assigned(vm, &target)?.then(|| target.clone());
        let mut slot_in_closed_tree = false;
        let mut cur_target = target.clone();
        let mut parent = get_the_parent(vm, h, &target, &ty, composed, &target)?;
        while !matches!(parent, Value::Null) {
            let (_, _, target_root) = node_facts(vm, h, &cur_target)?;
            if slottable.is_some() {
                slottable = None;
                if let Some(p) = node_of(vm, &parent) {
                    slot_in_closed_tree = with_doc(vm, h, p.doc, |d| {
                        d.containing_shadow_root(p.id).and_then(|r| d.shadow_info(r)).is_some_and(|i| !i.open)
                    })?;
                }
            }
            if assigned(vm, &parent)? {
                slottable = Some(parent.clone());
            }
            let rel = retarget(vm, h, &original_related, &parent)?;
            let is_window = node_of(vm, &parent).is_none();
            let sees = match (node_of(vm, &parent), target_root) {
                (Some(p), Some(r)) if p.doc == r.doc => {
                    with_doc(vm, h, p.doc, |d| d.is_shadow_including_inclusive_ancestor(r.id, p.id))?
                }
                _ => false,
            };
            if is_window || sees {
                if is_activation && bubbles && activation.is_none() {
                    activation = has_activation(vm, &parent)?;
                }
                push(vm, &parent, Value::Null, rel, slot_in_closed_tree)?;
            } else if same(&parent, &rel) {
                parent = Value::Null;
            } else {
                cur_target = parent.clone();
                if is_activation && activation.is_none() {
                    activation = has_activation(vm, &cur_target)?;
                }
                push(vm, &parent, cur_target.clone(), rel, slot_in_closed_tree)?;
            }
            if !matches!(parent, Value::Null) {
                parent = get_the_parent(vm, h, &parent, &ty, composed, &target)?;
            }
            slot_in_closed_tree = false;
        }
        let arr = vm.new_array(flat);
        set_slot(vm, e, S_PATH, Value::Object(arr));
        let path = read_path(vm, e)?;
        // Steps 5.10–5.11.
        if let Some(last) = path.iter().rev().find(|p| p.adjusted.is_some()) {
            for v in [Value::Object(last.adjusted.unwrap_or(last.target)), last.related.clone()] {
                if node_facts(vm, h, &v)?.0 {
                    clear_targets = true;
                }
            }
        }
        // Step 5.12: legacy-pre-activation behavior.
        let saved = match activation {
            Some(t) => Some((t, crate::activation::pre(vm, h, t)?)),
            None => None,
        };
        // Steps 5.13–5.14.
        for i in (0..path.len()).rev() {
            let phase = if path[i].adjusted.is_some() { 2 } else { 1 };
            invoke(vm, h, e, &path, i, &ty, phase, true)?;
        }
        for i in 0..path.len() {
            let phase = if path[i].adjusted.is_some() {
                2
            } else {
                if !bubbles {
                    continue;
                }
                3
            };
            invoke(vm, h, e, &path, i, &ty, phase, false)?;
        }
        finish(vm, h, e, clear_targets);
        // Steps 10–11.
        let f = flags(vm, e);
        if let Some((t, saved)) = saved {
            if f & CANCELED != 0 {
                crate::activation::canceled(vm, h, t, &saved)?;
            } else {
                crate::activation::activate(vm, h, t, &saved)?;
            }
        }
        return Ok(f & CANCELED == 0);
    }
    finish(vm, h, e, clear_targets);
    Ok(flags(vm, e) & CANCELED == 0)
}

/// DOM §2.9 dispatch steps 6–9.
fn finish(vm: &mut Vm, h: &Host, e: ObjRef, clear_targets: bool) {
    set_slot(vm, e, S_PHASE, Value::Number(0.0));
    set_slot(vm, e, S_CURRENT, Value::Null);
    set_slot(vm, e, S_PATH, Value::Undefined);
    let f = flags(vm, e);
    set_flags(vm, e, f & !(DISPATCH | STOP | STOP_IMMEDIATE));
    if clear_targets {
        set_slot(vm, e, S_TARGET, Value::Null);
        if get_field(vm, e, "relatedTarget").is_some() {
            set_field(vm, e, "relatedTarget", Value::Null);
        }
    }
    let _ = h;
}

/// DOM §2.9 "composedPath()".
fn composed_path(vm: &mut Vm, e: ObjRef) -> JsResult<Vec<Value>> {
    let path = read_path(vm, e)?;
    let Value::Object(current) = slot(vm, e, S_CURRENT) else { return Ok(Vec::new()) };
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = std::collections::VecDeque::new();
    out.push_back(Value::Object(current));
    let mut current_index = 0usize;
    let mut level: i64 = 0;
    for (i, p) in path.iter().enumerate().rev() {
        if p.flags & P_ROOT_CLOSED != 0 {
            level += 1;
        }
        if p.target == current {
            current_index = i;
            break;
        }
        if p.flags & P_SLOT_CLOSED != 0 {
            level -= 1;
        }
    }
    let (mut cur, mut max) = (level, level);
    for p in path[..current_index].iter().rev() {
        if p.flags & P_ROOT_CLOSED != 0 {
            cur += 1;
        }
        if cur <= max {
            out.push_front(Value::Object(p.target));
        }
        if p.flags & P_SLOT_CLOSED != 0 {
            cur -= 1;
            max = max.min(cur);
        }
    }
    let (mut cur, mut max) = (level, level);
    for p in &path[current_index + 1..] {
        if p.flags & P_SLOT_CLOSED != 0 {
            cur += 1;
        }
        if cur <= max {
            out.push_back(Value::Object(p.target));
        }
        if p.flags & P_ROOT_CLOSED != 0 {
            cur -= 1;
            max = max.min(cur);
        }
    }
    Ok(out.into())
}

/// DOM §2.9 "invoke" path struct `i` for the capture or bubble pass, then §2.10 "inner
/// invoke" of its listeners.
#[allow(clippy::too_many_arguments)]
fn invoke(
    vm: &mut Vm,
    h: &Host,
    e: ObjRef,
    path: &[PathItem],
    i: usize,
    ty: &str,
    phase: u8,
    capture_pass: bool,
) -> JsResult<()> {
    set_slot(vm, e, S_PHASE, Value::Number(phase as f64));
    // Steps 1–2.
    if let Some(t) = path[..=i].iter().rev().find_map(|p| p.adjusted) {
        set_slot(vm, e, S_TARGET, Value::Object(t));
    }
    if get_field(vm, e, "relatedTarget").is_some() {
        set_field(vm, e, "relatedTarget", path[i].related.clone());
    }
    // Step 4.
    if flags(vm, e) & STOP != 0 {
        return Ok(());
    }
    let t = path[i].target;
    set_slot(vm, e, S_CURRENT, Value::Object(t));
    // HTML §7.2.2 window.event: the event, unless the listener is in a shadow tree.
    let in_shadow = path[i].flags & P_IN_SHADOW != 0;
    let saved = h.current_event.replace(if in_shadow { None } else { Some(e) });
    let r = inner_invoke(vm, h, e, t, ty, capture_pass);
    h.current_event.set(saved);
    r
}

fn inner_invoke(vm: &mut Vm, h: &Host, e: ObjRef, t: ObjRef, ty: &str, capture_pass: bool) -> JsResult<()> {
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
        // HTML §8.1.4.4 "clean up after running script" (WebIDL runs it after each callback):
        // with no script running (the event came from a task, not dispatchEvent), a
        // microtask checkpoint.
        if vm.stack_empty() {
            vm.run_jobs();
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

/// Event handler IDL attributes `names` (e.g. XMLHttpRequest's `onload`).
pub fn setup_named_handlers(vm: &mut Vm, proto: ObjRef, names: &[&str]) {
    for &name in names {
        attr_with(vm, proto, name, handler_get, Some(handler_set), vec![js(name)]);
    }
}

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
    // EventHandler is [LegacyTreatNonObjectAsNull]: any object is kept (and only called if
    // callable), anything else is null.
    let v = c.arg(0);
    let v = if matches!(v, Value::Object(_)) { v } else { Value::Null };
    set_handler(&h, t, &name, v);
    Ok(Value::Undefined)
}

/// The value of event handler `name` of `t` (for interfaces with their own handler setters).
pub fn handler_value(vm: &mut Vm, h: &Host, t: ObjRef, name: &str) -> R {
    current_handler(vm, h, t, name)
}

/// Set event handler `name` of `t` to `v` (an object; anything else is null).
pub fn set_handler_value(_vm: &mut Vm, h: &Host, t: ObjRef, name: &str, v: Value) {
    let v = if matches!(v, Value::Object(_)) { v } else { Value::Null };
    set_handler(h, t, name, v);
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
            signal: None,
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
        // In a worker, an exception its own handlers didn't take goes to the Worker object.
        crate::worker::unhandled_error(&h, &msg);
        h.errors.borrow_mut().push(msg);
    }
}

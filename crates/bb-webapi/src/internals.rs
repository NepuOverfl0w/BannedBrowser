//! ElementInternals (HTML §4.13.7): `attachInternals()`, form-associated custom elements'
//! form value, validity and labels, the internals' shadow root, CustomStateSet (with CSS
//! `:state()`), and the internals' ARIA properties; and the form-associated custom element
//! callbacks (formAssociatedCallback, formResetCallback, formDisabledCallback).
//!
//! Which elements are form-associated custom elements, and their submission values, are
//! bb-dom's (the form owner, `form.elements`, disabled state, labels and the entry list use
//! them); the rest of the internals' state is here, by target element.

use std::collections::HashMap;
use std::rc::Rc;

use bb_dom::form_data::EntryValue;
use bb_dom::{FaceSubmission, NodeId};
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::collections::{collection, CollKind};
use crate::exception::throw;
use crate::host::*;
use crate::idl::{attr, attr_with, callee_slots, op};

type R = JsResult<Value>;

/// ValidityStateFlags members, in the order ValidityState's attributes list them.
const FLAGS: [&str; 10] = [
    "valueMissing",
    "typeMismatch",
    "patternMismatch",
    "tooLong",
    "tooShort",
    "rangeUnderflow",
    "rangeOverflow",
    "stepMismatch",
    "badInput",
    "customError",
];

/// ARIAMixin's attributes (WAI-ARIA §10.1, ARIA in HTML), reflected by ElementInternals.
const ARIA: [&str; 44] = [
    "role",
    "ariaAtomic",
    "ariaAutoComplete",
    "ariaBrailleLabel",
    "ariaBrailleRoleDescription",
    "ariaBusy",
    "ariaChecked",
    "ariaColCount",
    "ariaColIndex",
    "ariaColIndexText",
    "ariaColSpan",
    "ariaCurrent",
    "ariaDescription",
    "ariaDisabled",
    "ariaExpanded",
    "ariaHasPopup",
    "ariaHidden",
    "ariaInvalid",
    "ariaKeyShortcuts",
    "ariaLabel",
    "ariaLevel",
    "ariaLive",
    "ariaModal",
    "ariaMultiLine",
    "ariaMultiSelectable",
    "ariaOrientation",
    "ariaPlaceholder",
    "ariaPosInSet",
    "ariaPressed",
    "ariaReadOnly",
    "ariaRelevant",
    "ariaRequired",
    "ariaRoleDescription",
    "ariaRowCount",
    "ariaRowIndex",
    "ariaRowIndexText",
    "ariaRowSpan",
    "ariaSelected",
    "ariaSetSize",
    "ariaSort",
    "ariaValueMax",
    "ariaValueMin",
    "ariaValueNow",
    "ariaValueText",
];

/// ARIAMixin's element-reference attributes: one element, or a list of them.
const ARIA_ELEMENTS: [(&str, bool); 8] = [
    ("ariaActiveDescendantElement", false),
    ("ariaControlsElements", true),
    ("ariaDescribedByElements", true),
    ("ariaDetailsElements", true),
    ("ariaErrorMessageElements", true),
    ("ariaFlowToElements", true),
    ("ariaLabelledByElements", true),
    ("ariaOwnsElements", true),
];

/// An element's attached internals.
pub struct Internals {
    pub obj: ObjRef,
    flags: [bool; 10],
    message: String,
    anchor: Value,
    aria: HashMap<&'static str, String>,
    /// The element-reference ARIA attributes' values (an element, or a frozen array).
    aria_elements: HashMap<&'static str, Value>,
    states: Vec<String>,
    /// Each state's position in add order (iterators continue after the last one they
    /// returned, as a Set's do).
    state_seqs: Vec<u64>,
    next_seq: u64,
    states_obj: Option<ObjRef>,
    validity_obj: Option<ObjRef>,
}

/// The internals of every element that attached them, and what the form-associated callbacks
/// last saw (form owner, disabled).
#[derive(Default)]
pub struct State {
    pub by_target: HashMap<u64, Internals>,
    seen: HashMap<u64, (Option<NodeId>, bool)>,
}

impl State {
    pub fn trace(&self, out: &mut Vec<ObjRef>) {
        for i in self.by_target.values() {
            out.push(i.obj);
            out.extend(i.states_obj);
            out.extend(i.validity_obj);
            for v in std::iter::once(&i.anchor).chain(i.aria_elements.values()) {
                if let Value::Object(o) = v {
                    out.push(*o);
                }
            }
        }
    }

    /// Elements moved to other ids (adoption): their internals follow.
    pub fn remap(&mut self, vm: &mut Vm, pairs: &[(u64, u64)]) {
        if self.by_target.is_empty() && self.seen.is_empty() {
            return;
        }
        for &(old, new) in pairs {
            if let Some(i) = self.by_target.remove(&old) {
                for o in [Some(i.obj), i.states_obj, i.validity_obj].into_iter().flatten() {
                    if let Some(ho) = vm.host_obj_mut(o) {
                        ho.id = new;
                    }
                }
                self.by_target.insert(new, i);
            }
            if let Some(s) = self.seen.remove(&old) {
                self.seen.insert(new, s);
            }
        }
    }
}

// ---- attachInternals (HTML §4.13.7.1) ----

fn attach_internals(vm: &mut Vm, c: NativeCall) -> R {
    let (h, n) = this_node(vm, &c.this, C::HTMLElement)?;
    // Steps 1–4.
    if crate::ce::is_value(&h, n).is_some() {
        return throw(vm, "NotSupportedError", "customized built-in elements can't attach internals");
    }
    let local = with_doc(vm, &h, n.doc, |d| d.element(n.id).map(|e| e.name.clone()))?.unwrap_or_default();
    let Some(def) = crate::ce::lookup(&h, n.doc, bb_dom::Namespace::Html, &local, None) else {
        return throw(vm, "NotSupportedError", "attachInternals needs an autonomous custom element");
    };
    if h.ce.borrow().defs[def].disable_internals {
        return throw(vm, "NotSupportedError", "this custom element disables internals");
    }
    // Step 5.
    if h.internals.borrow().by_target.contains_key(&n.pack()) {
        return throw(vm, "NotSupportedError", "the element's internals are already attached");
    }
    // Step 6.
    let custom = matches!(crate::ce::state(&h, n), Some(crate::ce::State::Custom));
    if !custom && !crate::ce::is_precustomized(&h, n) {
        return throw(
            vm,
            "NotSupportedError",
            "attachInternals needs a custom element being constructed or defined",
        );
    }
    let obj =
        vm.new_host_object(Some(h.proto(C::ElementInternals)), C::ElementInternals as u32, n.pack(), false);
    h.internals.borrow_mut().by_target.insert(
        n.pack(),
        Internals {
            obj,
            flags: [false; 10],
            message: String::new(),
            anchor: Value::Null,
            aria: HashMap::new(),
            aria_elements: HashMap::new(),
            states: Vec::new(),
            state_seqs: Vec::new(),
            next_seq: 1,
            states_obj: None,
            validity_obj: None,
        },
    );
    Ok(Value::Object(obj))
}

pub fn setup_html_element(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "attachInternals", 0, attach_internals);
}

// ---- ElementInternals (§4.13.7.2–7) ----

/// `this` as ElementInternals: (host, target element).
fn this(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, NodeRef)> {
    let (_, id) = this_of(vm, &c.this, C::ElementInternals)?;
    Ok((host(vm)?, NodeRef::unpack(id)))
}

/// The target, checked to be a form-associated custom element (else NotSupportedError).
fn face_this(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, NodeRef)> {
    let (h, n) = this(vm, c)?;
    if !with_doc(vm, &h, n.doc, |d| d.is_form_associated_custom(n.id))?
        && !crate::ce::form_associated_def(&h, n)
    {
        return throw(vm, "NotSupportedError", "the element isn't a form-associated custom element");
    }
    Ok((h, n))
}

fn with_internals<T>(h: &Host, n: NodeRef, f: impl FnOnce(&mut Internals) -> T) -> Option<T> {
    h.internals.borrow_mut().by_target.get_mut(&n.pack()).map(f)
}

/// §4.10.21.2 "barred from constraint validation" for a form-associated custom element: it
/// is disabled, has a `readonly` attribute, or is in a datalist.
fn will_validate(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<bool> {
    with_doc(vm, h, n.doc, |d| {
        !(d.is_disabled(n.id)
            || d.attr(n.id, "readonly").is_some()
            || d.ancestors(n.id).any(|a| d.is_html(a, "datalist")))
    })
}

fn invalid(h: &Host, n: NodeRef) -> bool {
    with_internals(h, n, |i| i.flags.iter().any(|&f| f)).unwrap_or(false)
}

/// §4.10.21.2 "check validity": an invalid candidate fires a cancelable `invalid` event.
fn check_validity(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<bool> {
    if !will_validate(vm, h, n)? || !invalid(h, n) {
        return Ok(true);
    }
    let t = wrap(vm, h, n)?;
    crate::events::fire(vm, h, t, "invalid", false, true)?;
    Ok(false)
}

/// setFormValue's value: null, a string, a File or a FormData.
fn submission_value(vm: &mut Vm, v: &Value) -> JsResult<FaceSubmission> {
    if v.is_nullish() {
        return Ok(FaceSubmission::None);
    }
    if let Some((c, _)) = vm.host_data(v) {
        if c == C::FormData as u32 {
            let Value::Object(o) = v else { unreachable!("host data implies an object") };
            return Ok(FaceSubmission::Entries(crate::blob::entry_values(vm, *o)));
        }
        if c == C::File as u32 {
            if let Some((bytes, ty, name)) = crate::blob::blob_data(vm, v) {
                return Ok(FaceSubmission::Value(EntryValue::File {
                    name: name.unwrap_or_default(),
                    content_type: ty,
                    bytes,
                }));
            }
        }
    }
    Ok(FaceSubmission::Value(EntryValue::Text(rstr(vm, v)?)))
}

pub fn setup_internals(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "shadowRoot",
        |vm, c| {
            let (h, n) = this(vm, &c)?;
            let r = with_doc(vm, &h, n.doc, |d| {
                d.shadow_root(n.id)
                    .filter(|&r| d.shadow_info(r).is_some_and(|i| i.available_to_element_internals))
            })?;
            wrap_opt(vm, &h, r.map(|r| n.with(r)))
        },
        None,
    );
    op(vm, proto, "setFormValue", 1, |vm, c| {
        let (h, n) = face_this(vm, &c)?;
        let value = submission_value(vm, &c.arg(0))?;
        // The state (for restoring) is converted too; nothing restores yet.
        if c.args.len() > 1 {
            submission_value(vm, &c.arg(1))?;
        }
        with_doc_mut(vm, &h, n.doc, |d| d.set_face_submission(n.id, value))?;
        Ok(Value::Undefined)
    });
    attr(
        vm,
        proto,
        "form",
        |vm, c| {
            let (h, n) = face_this(vm, &c)?;
            let f = with_doc(vm, &h, n.doc, |d| d.form_owner(n.id))?;
            wrap_opt(vm, &h, f.map(|f| n.with(f)))
        },
        None,
    );
    op(vm, proto, "setValidity", 0, |vm, c| {
        let (h, n) = face_this(vm, &c)?;
        // ValidityStateFlags, members in lexicographic order.
        let mut sorted: Vec<(usize, &str)> = FLAGS.iter().copied().enumerate().collect();
        sorted.sort_by_key(|(_, k)| *k);
        let mut flags = [false; 10];
        for (i, k) in sorted {
            let v = match &c.arg(0) {
                Value::Object(o) => vm.get(*o, &PropKey::named(k))?,
                Value::Undefined | Value::Null => Value::Undefined,
                _ => return vm.type_error("the flags must be an object"),
            };
            flags[i] = vm.to_boolean(&v);
        }
        let message = match c.arg(1) {
            Value::Undefined => None,
            v => Some(rstr(vm, &v)?),
        };
        let any = flags.iter().any(|&f| f);
        // Step 2.
        if any && message.as_deref().is_none_or(str::is_empty) {
            return vm.type_error("a validity flag needs a message");
        }
        // Step 6: the anchor must be a shadow-including descendant of the element.
        let anchor = c.arg(2);
        if !anchor.is_undefined() {
            if !matches!(vm.host_data(&anchor), Some((k, _)) if crate::class::implements(k, C::HTMLElement)) {
                return vm.type_error("the anchor must be an HTMLElement");
            }
            let a = arg_node(vm, &anchor, "the anchor")?;
            let ok = a.doc == n.doc
                && with_doc(vm, &h, n.doc, |d| d.is_shadow_including_inclusive_ancestor(n.id, a.id))?;
            if !ok {
                return throw(
                    vm,
                    "NotFoundError",
                    "the anchor isn't a shadow-including descendant of the element",
                );
            }
        }
        let message =
            if any { bb_dom::normalize_newlines(&message.unwrap_or_default()) } else { String::new() };
        // Mirrored into the document as a custom error, for `:valid`/`:invalid` and forms.
        let mirrored = if any { message.clone() } else { String::new() };
        with_doc_mut(vm, &h, n.doc, |d| d.set_custom_validity(n.id, &mirrored))?;
        h.mark_dirty();
        with_internals(&h, n, |i| {
            i.flags = flags;
            i.message = message;
            i.anchor = if anchor.is_undefined() { Value::Null } else { anchor };
        });
        Ok(Value::Undefined)
    });
    attr(
        vm,
        proto,
        "willValidate",
        |vm, c| {
            let (h, n) = face_this(vm, &c)?;
            Ok(Value::Bool(will_validate(vm, &h, n)?))
        },
        None,
    );
    attr(
        vm,
        proto,
        "validity",
        |vm, c| {
            let (h, n) = face_this(vm, &c)?;
            if let Some(Some(o)) = with_internals(&h, n, |i| i.validity_obj) {
                return Ok(Value::Object(o));
            }
            let o =
                vm.new_host_object(Some(h.proto(C::ValidityState)), C::ValidityState as u32, n.pack(), false);
            with_internals(&h, n, |i| i.validity_obj = Some(o));
            Ok(Value::Object(o))
        },
        None,
    );
    attr(
        vm,
        proto,
        "validationMessage",
        |vm, c| {
            let (h, n) = face_this(vm, &c)?;
            if !will_validate(vm, &h, n)? {
                return Ok(js(""));
            }
            Ok(js(&with_internals(&h, n, |i| i.message.clone()).unwrap_or_default()))
        },
        None,
    );
    for name in ["checkValidity", "reportValidity"] {
        // reportValidity also reports the problem to the user, which needs validation UI
        // the browser doesn't have yet: here both fire `invalid` and answer.
        op(vm, proto, name, 0, |vm, c| {
            let (h, n) = face_this(vm, &c)?;
            Ok(Value::Bool(check_validity(vm, &h, n)?))
        });
    }
    attr(
        vm,
        proto,
        "labels",
        |vm, c| {
            let (h, n) = face_this(vm, &c)?;
            let labels: Vec<NodeRef> = with_doc(vm, &h, n.doc, |d| {
                let root = d.tree_root(n.id);
                d.descendants(root)
                    .filter(|&x| d.is_html(x, "label") && d.labeled_control(x) == Some(n.id))
                    .map(|x| n.with(x))
                    .collect()
            })?;
            collection(vm, &h, CollKind::Static(labels), None)
        },
        None,
    );
    attr(
        vm,
        proto,
        "states",
        |vm, c| {
            let (h, n) = this(vm, &c)?;
            if let Some(Some(o)) = with_internals(&h, n, |i| i.states_obj) {
                return Ok(Value::Object(o));
            }
            let o = vm.new_host_object(
                Some(h.proto(C::CustomStateSet)),
                C::CustomStateSet as u32,
                n.pack(),
                false,
            );
            with_internals(&h, n, |i| i.states_obj = Some(o));
            Ok(Value::Object(o))
        },
        None,
    );
    for (i, name) in ARIA.iter().enumerate() {
        attr_with(
            vm,
            proto,
            name,
            |vm, c| {
                let (h, n) = this(vm, &c)?;
                let k = aria_name(vm, &c);
                let v = with_internals(&h, n, |i| i.aria.get(k).cloned()).flatten();
                Ok(v.map_or(Value::Null, |s| js(&s)))
            },
            Some(|vm, c| {
                let (h, n) = this(vm, &c)?;
                let k = aria_name(vm, &c);
                let v = if c.arg(0).is_nullish() { None } else { Some(rstr(vm, &c.arg(0))?) };
                with_internals(&h, n, |i| match v {
                    Some(v) => {
                        i.aria.insert(k, v);
                    }
                    None => {
                        i.aria.remove(k);
                    }
                });
                Ok(Value::Undefined)
            }),
            vec![Value::Number(i as f64)],
        );
    }
    setup_aria_elements(vm, proto);
}

/// The element-reference ARIA attributes (stored as set: an Element, or a frozen array of
/// them; null when unset).
fn setup_aria_elements(vm: &mut Vm, proto: ObjRef) {
    for (i, _) in ARIA_ELEMENTS.iter().enumerate() {
        attr_with(
            vm,
            proto,
            ARIA_ELEMENTS[i].0,
            |vm, c| {
                let (h, n) = this(vm, &c)?;
                let (k, _) = aria_element(vm, &c);
                Ok(with_internals(&h, n, |i| i.aria_elements.get(k).cloned())
                    .flatten()
                    .unwrap_or(Value::Null))
            },
            Some(|vm, c| {
                let (h, n) = this(vm, &c)?;
                let (k, list) = aria_element(vm, &c);
                let v = c.arg(0);
                let is_element = |vm: &Vm, v: &Value| matches!(vm.host_data(v), Some((c, _)) if crate::class::implements(c, C::Element));
                let stored = if v.is_nullish() {
                    None
                } else if list {
                    let mut items = Vec::new();
                    for x in vm.iterate_to_vec(&v)? {
                        if !is_element(vm, &x) {
                            return vm.type_error("the list must hold elements");
                        }
                        items.push(x);
                    }
                    let arr = vm.new_array(items);
                    let freeze = vm.get(vm.realm.object_ctor, &PropKey::lit("freeze"))?;
                    vm.call(&freeze, Value::Undefined, vec![Value::Object(arr)])?;
                    Some(Value::Object(arr))
                } else if is_element(vm, &v) {
                    Some(v)
                } else {
                    return vm.type_error("the value must be an element");
                };
                with_internals(&h, n, |i| match stored {
                    Some(v) => {
                        i.aria_elements.insert(k, v);
                    }
                    None => {
                        i.aria_elements.remove(k);
                    }
                });
                Ok(Value::Undefined)
            }),
            vec![Value::Number(i as f64)],
        );
    }
}

fn aria_element(vm: &Vm, c: &NativeCall) -> (&'static str, bool) {
    match callee_slots(vm, c.callee).first() {
        Some(Value::Number(i)) => ARIA_ELEMENTS.get(*i as usize).copied().unwrap_or(ARIA_ELEMENTS[0]),
        _ => ARIA_ELEMENTS[0],
    }
}

fn aria_name(vm: &Vm, c: &NativeCall) -> &'static str {
    match callee_slots(vm, c.callee).first() {
        Some(Value::Number(i)) => ARIA.get(*i as usize).copied().unwrap_or("role"),
        _ => "role",
    }
}

// ---- ValidityState (§4.10.21.3) ----

pub fn setup_validity_state(vm: &mut Vm, proto: ObjRef) {
    for (i, name) in FLAGS.iter().enumerate() {
        attr_with(
            vm,
            proto,
            name,
            |vm, c| {
                let i = match callee_slots(vm, c.callee).first() {
                    Some(Value::Number(i)) => *i as usize,
                    _ => 0,
                };
                Ok(Value::Bool(flags_of(vm, &c)?[i]))
            },
            None,
            vec![Value::Number(i as f64)],
        );
    }
    attr(vm, proto, "valid", |vm, c| Ok(Value::Bool(!flags_of(vm, &c)?.iter().any(|&f| f))), None);
}

/// A ValidityState's flags: its element's internals for a form-associated custom element,
/// else the built-in control's validity states.
fn flags_of(vm: &mut Vm, c: &NativeCall) -> JsResult<[bool; 10]> {
    let (_, id) = this_of(vm, &c.this, C::ValidityState)?;
    let h = host(vm)?;
    let n = NodeRef::unpack(id);
    if let Some(f) = with_internals(&h, n, |s| s.flags) {
        if with_doc(vm, &h, n.doc, |d| d.is_form_associated_custom(n.id))?
            || crate::ce::form_associated_def(&h, n)
        {
            return Ok(f);
        }
    }
    let v = with_doc(vm, &h, n.doc, |d| d.validity(n.id))?;
    Ok([
        v.value_missing,
        v.type_mismatch,
        v.pattern_mismatch,
        v.too_long,
        v.too_short,
        v.range_underflow,
        v.range_overflow,
        v.step_mismatch,
        v.bad_input,
        v.custom_error,
    ])
}

/// checkValidity for a form-associated custom element (for forms' static validation).
pub fn check_face(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<bool> {
    check_validity(vm, h, n)
}

// ---- CustomStateSet (§4.13.7.7): setlike<DOMString> ----

fn states_this(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, NodeRef)> {
    let (_, id) = this_of(vm, &c.this, C::CustomStateSet)?;
    Ok((host(vm)?, NodeRef::unpack(id)))
}

fn states(h: &Host, n: NodeRef) -> Vec<String> {
    with_internals(h, n, |i| i.states.clone()).unwrap_or_default()
}

fn set_states(vm: &mut Vm, h: &Host, n: NodeRef, list: Vec<String>) -> JsResult<()> {
    with_internals(h, n, |i| {
        let mut seqs = Vec::with_capacity(list.len());
        for s in &list {
            match i.states.iter().position(|x| x == s) {
                Some(k) => seqs.push(i.state_seqs[k]),
                None => {
                    seqs.push(i.next_seq);
                    i.next_seq += 1;
                }
            }
        }
        i.states = list.clone();
        i.state_seqs = seqs;
    });
    with_doc_mut(vm, h, n.doc, |d| d.set_custom_states(n.id, list))?;
    h.mark_dirty();
    Ok(())
}

/// A setlike iterator (WebIDL §3.7.12): it reads the set as it goes, so states added while
/// iterating are visited. Its `next` keeps (set, position, kind) in its own slots.
fn iterate(vm: &mut Vm, c: &NativeCall, entries: bool) -> R {
    states_this(vm, c)?;
    let next = bb_js::vm::realm::native_with(
        vm,
        "next",
        0,
        iter_next,
        false,
        vec![c.this.clone(), Value::Number(0.0), Value::Bool(entries)],
    );
    // %IteratorPrototype%, from an array iterator's prototype chain.
    let arr = vm.new_array(Vec::new());
    let values = vm.get(arr, &PropKey::lit("values"))?;
    let ai = vm.call(&values, Value::Object(arr), Vec::new())?;
    let Value::Object(ai) = ai else { return vm.type_error("no array iterator") };
    let iter_proto = vm.heap.obj(ai).proto.and_then(|p| vm.heap.obj(p).proto);
    let it = vm.new_object_with_proto(iter_proto);
    vm.heap
        .obj_mut(it)
        .props
        .insert(PropKey::lit("next"), bb_js::vm::heap::Prop::method(Value::Object(next)));
    Ok(Value::Object(it))
}

fn iter_next(vm: &mut Vm, c: NativeCall) -> R {
    let slots = callee_slots(vm, c.callee);
    let (set, last, entries) = match slots.as_slice() {
        [set, Value::Number(i), Value::Bool(e)] => (set.clone(), *i, *e),
        _ => return vm.type_error("not a CustomStateSet iterator"),
    };
    let (_, id) = this_of(vm, &set, C::CustomStateSet)?;
    let h = host(vm)?;
    // The first state added after the last one returned.
    let next = with_internals(&h, NodeRef::unpack(id), |i| {
        i.state_seqs
            .iter()
            .zip(&i.states)
            .filter(|(q, _)| **q as f64 > last)
            .min_by_key(|(q, _)| **q)
            .map(|(q, s)| (*q, s.clone()))
    })
    .flatten();
    let result = vm.new_object();
    match next {
        Some((seq, s)) => {
            let s = &s;
            if let bb_js::vm::heap::Kind::Native(n) = &mut vm.heap.obj_mut(c.callee).kind {
                n.slots[1] = Value::Number(seq as f64);
            }
            let v = if entries { Value::Object(vm.new_array(vec![js(s), js(s)])) } else { js(s) };
            vm.heap
                .obj_mut(result)
                .props
                .insert(PropKey::lit("value"), bb_js::vm::heap::Prop::data(v, true, true, true));
            vm.heap.obj_mut(result).props.insert(
                PropKey::lit("done"),
                bb_js::vm::heap::Prop::data(Value::Bool(false), true, true, true),
            );
        }
        None => {
            if let bb_js::vm::heap::Kind::Native(n) = &mut vm.heap.obj_mut(c.callee).kind {
                n.slots[1] = Value::Number(f64::INFINITY);
            }
            vm.heap.obj_mut(result).props.insert(
                PropKey::lit("value"),
                bb_js::vm::heap::Prop::data(Value::Undefined, true, true, true),
            );
            vm.heap.obj_mut(result).props.insert(
                PropKey::lit("done"),
                bb_js::vm::heap::Prop::data(Value::Bool(true), true, true, true),
            );
        }
    }
    Ok(Value::Object(result))
}

pub fn setup_custom_state_set(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "size",
        |vm, c| {
            let (h, n) = states_this(vm, &c)?;
            Ok(Value::Number(states(&h, n).len() as f64))
        },
        None,
    );
    op(vm, proto, "add", 1, |vm, c| {
        let s = rstr(vm, &c.arg(0))?;
        let (h, n) = states_this(vm, &c)?;
        let mut list = states(&h, n);
        if !list.contains(&s) {
            list.push(s);
            set_states(vm, &h, n, list)?;
        }
        Ok(c.this.clone())
    });
    op(vm, proto, "delete", 1, |vm, c| {
        let s = rstr(vm, &c.arg(0))?;
        let (h, n) = states_this(vm, &c)?;
        let mut list = states(&h, n);
        let had = list.contains(&s);
        if had {
            list.retain(|x| *x != s);
            set_states(vm, &h, n, list)?;
        }
        Ok(Value::Bool(had))
    });
    op(vm, proto, "has", 1, |vm, c| {
        let s = rstr(vm, &c.arg(0))?;
        let (h, n) = states_this(vm, &c)?;
        Ok(Value::Bool(states(&h, n).contains(&s)))
    });
    op(vm, proto, "clear", 0, |vm, c| {
        let (h, n) = states_this(vm, &c)?;
        set_states(vm, &h, n, Vec::new())?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "forEach", 1, |vm, c| {
        let (h, n) = states_this(vm, &c)?;
        let cb = c.arg(0);
        if !vm.is_callable(&cb) {
            return vm.type_error("forEach needs a function");
        }
        // Live, as a Set's forEach is: states added while iterating are visited.
        let mut i = 0;
        loop {
            let list = states(&h, n);
            let Some(s) = list.get(i).cloned() else { break };
            vm.call(&cb, c.arg(1), vec![js(&s), js(&s), c.this.clone()])?;
            i += 1;
        }
        Ok(Value::Undefined)
    });
    let values = op(vm, proto, "values", 0, |vm, c| iterate(vm, &c, false));
    vm.heap
        .obj_mut(proto)
        .props
        .insert(PropKey::lit("keys"), bb_js::vm::heap::Prop::method(Value::Object(values)));
    let it = PropKey::Sym(vm.realm.sym_iterator.clone());
    vm.heap.obj_mut(proto).props.insert(it, bb_js::vm::heap::Prop::method(Value::Object(values)));
    op(vm, proto, "entries", 0, |vm, c| iterate(vm, &c, true));
}

// ---- form-associated custom elements: the callbacks (§4.13.7) ----

/// After a change that can move form owners or disabled states (insertion, removal, `form`,
/// `id`, `disabled` attributes): each form-associated custom element of document `doc`
/// whose form owner or disabledness changed gets its callback.
pub fn forms_changed(vm: &mut Vm, h: &Host, doc: u32) {
    if !h.ce.borrow().defs.iter().any(|d| d.form_associated) {
        return;
    }
    let now: Vec<(NodeRef, Option<NodeId>, bool)> = with_doc(vm, h, doc, |d| {
        d.form_associated_customs()
            .into_iter()
            .map(|x| (NodeRef { doc, id: x }, d.form_owner(x), d.is_disabled(x)))
            .collect()
    })
    .unwrap_or_default();
    for (n, owner, disabled) in now {
        let before = h.internals.borrow().seen.get(&n.pack()).copied().unwrap_or((None, false));
        h.internals.borrow_mut().seen.insert(n.pack(), (owner, disabled));
        if before.0 != owner {
            let form = owner.map_or(Ok(Value::Null), |f| wrap(vm, h, n.with(f))).unwrap_or(Value::Null);
            crate::ce::enqueue_form_callback(h, n, "formAssociatedCallback", vec![form]);
        }
        if before.1 != disabled {
            crate::ce::enqueue_form_callback(h, n, "formDisabledCallback", vec![Value::Bool(disabled)]);
        }
    }
}

/// A form was reset: its form-associated custom elements get formResetCallback.
pub fn form_reset(vm: &mut Vm, h: &Host, form: NodeRef) {
    let faces: Vec<NodeId> = with_doc(vm, h, form.doc, |d| {
        d.form_controls(form.id).into_iter().filter(|&x| d.is_form_associated_custom(x)).collect()
    })
    .unwrap_or_default();
    for x in faces {
        crate::ce::enqueue_form_callback(h, form.with(x), "formResetCallback", Vec::new());
    }
}

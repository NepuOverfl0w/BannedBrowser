//! Custom elements (HTML §4.13): the CustomElementRegistry (`customElements`), element
//! definitions, custom element states, reactions (§4.13.6) and upgrades (§4.13.5), and the
//! [HTMLConstructor] steps of HTML element constructors (§3.2.3).
//!
//! Reactions are queued on their elements and run when the outermost of the DOM's mutation
//! algorithms (insert, remove, replace all, adopt, attribute changes) or createElement /
//! define / upgrade returns: the [CEReactions] scope is that algorithm rather than the whole
//! API call. Elements the HTML parser creates are upgraded right after each pump of the
//! parser (between scripts) rather than constructed as each start tag is read.
//!
//! Not done yet: ElementInternals and form-associated custom elements, scoped registries,
//! `:defined`, and the disableShadow feature (there is no Shadow DOM yet).

use std::collections::{HashMap, VecDeque};

use bb_dom::{Namespace, NodeId};
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::exception::throw;
use crate::host::*;
use crate::idl::op;

type R = JsResult<Value>;

/// A custom element definition (§4.13.4).
pub struct Def {
    pub name: String,
    pub local_name: String,
    pub ctor: ObjRef,
    pub proto: ObjRef,
    pub observed: Vec<String>,
    pub connected: Value,
    pub disconnected: Value,
    pub adopted: Value,
    pub attribute_changed: Value,
    /// The construction stack: elements being upgraded; None is the "already constructed"
    /// marker.
    pub construction: Vec<Option<ObjRef>>,
}

/// A custom element state (DOM §4.9).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Undefined,
    Failed,
    Uncustomized,
    Custom,
}

pub enum Reaction {
    Upgrade(usize),
    Callback(Value, Vec<Value>),
}

pub struct Elem {
    pub state: State,
    pub def: Option<usize>,
    pub is: Option<String>,
    pub queue: VecDeque<Reaction>,
}

#[derive(Default)]
pub struct Registry {
    pub defs: Vec<Def>,
    /// Elements' custom element data, by packed node (absent: never looked at).
    pub elems: HashMap<u64, Elem>,
    /// The custom element reactions stack (§4.13.6): an element queue per [CEReactions]
    /// scope entered.
    pub stack: Vec<Vec<u64>>,
    /// The backup element queue, for reactions outside any scope.
    pub backup: Vec<u64>,
    /// The "element definition is running" flag.
    pub defining: bool,
    /// whenDefined promises by name: (promise, resolve function).
    pub when: HashMap<String, (Value, Value)>,
}

impl Registry {
    pub fn trace(&self, out: &mut Vec<ObjRef>) {
        let push = |v: &Value, out: &mut Vec<ObjRef>| {
            if let Value::Object(o) = v {
                out.push(*o);
            }
        };
        for d in &self.defs {
            out.push(d.ctor);
            out.push(d.proto);
            for v in [&d.connected, &d.disconnected, &d.adopted, &d.attribute_changed] {
                push(v, out);
            }
            out.extend(d.construction.iter().flatten());
        }
        for e in self.elems.values() {
            for r in &e.queue {
                if let Reaction::Callback(f, args) = r {
                    push(f, out);
                    for a in args {
                        push(a, out);
                    }
                }
            }
        }
        for (p, r) in self.when.values() {
            push(p, out);
            push(r, out);
        }
    }
}

// ---- names ----

/// HTML §4.13.2 "valid custom element name".
pub fn valid_name(name: &str) -> bool {
    const RESERVED: [&str; 8] = [
        "annotation-xml",
        "color-profile",
        "font-face",
        "font-face-src",
        "font-face-uri",
        "font-face-format",
        "font-face-name",
        "missing-glyph",
    ];
    let mut chars = name.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase())
        || !name.contains('-')
        || RESERVED.contains(&name)
    {
        return false;
    }
    chars.all(|c| {
        matches!(c, '-' | '.' | '0'..='9' | '_' | 'a'..='z' | '\u{B7}'
            | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}'
            | '\u{200C}'..='\u{200D}' | '\u{203F}'..='\u{2040}' | '\u{2070}'..='\u{218F}'
            | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}'
            | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
    })
}

/// HTML §4.13.4 "look up a custom element definition" (only the window's document has a
/// registry).
pub fn lookup(h: &Host, doc: u32, ns: Namespace, local: &str, is: Option<&str>) -> Option<usize> {
    if doc != 0 || ns != Namespace::Html {
        return None;
    }
    let r = h.ce.borrow();
    r.defs.iter().position(|d| d.name == local && d.local_name == local).or_else(|| {
        let is = is?;
        r.defs.iter().position(|d| d.name == is && d.local_name == local)
    })
}

// ---- element data ----

/// The element's custom element data, made on first use (an element whose name is a valid
/// custom element name, or that has an `is` value, starts "undefined").
fn ensure(h: &Host, d: &bb_dom::Document, n: NodeRef) {
    let mut r = h.ce.borrow_mut();
    r.elems.entry(n.pack()).or_insert_with(|| {
        let e = d.element(n.id);
        let is = e.and_then(|e| e.attr("is")).map(str::to_string);
        let custom_name = e.is_some_and(|e| e.ns == Namespace::Html && valid_name(&e.name));
        let state = if custom_name || is.is_some() { State::Undefined } else { State::Uncustomized };
        Elem { state, def: None, is, queue: VecDeque::new() }
    });
}

pub fn state(h: &Host, n: NodeRef) -> Option<State> {
    h.ce.borrow().elems.get(&n.pack()).map(|e| e.state)
}

fn is_custom(h: &Host, n: NodeRef) -> Option<usize> {
    let r = h.ce.borrow();
    let e = r.elems.get(&n.pack())?;
    (e.state == State::Custom).then_some(e.def).flatten()
}

/// §4.13.6 "enqueue an element on the appropriate element queue" with its reaction.
fn enqueue(h: &Host, n: NodeRef, reaction: Reaction) {
    let mut r = h.ce.borrow_mut();
    let k = n.pack();
    let Some(e) = r.elems.get_mut(&k) else { return };
    e.queue.push_back(reaction);
    let q = match r.stack.last_mut() {
        Some(q) => q,
        None => &mut r.backup,
    };
    if !q.contains(&k) {
        q.push(k);
    }
}

/// §4.13.6 "enqueue a custom element callback reaction".
fn enqueue_callback(h: &Host, n: NodeRef, which: &str, args: Vec<Value>) {
    let Some(def) = is_custom(h, n) else { return };
    let f = {
        let r = h.ce.borrow();
        let d = &r.defs[def];
        match which {
            "connectedCallback" => d.connected.clone(),
            "disconnectedCallback" => d.disconnected.clone(),
            "adoptedCallback" => d.adopted.clone(),
            _ => {
                let name = match args.first() {
                    Some(Value::String(s)) => bb_js::vm::to_rust(s),
                    _ => String::new(),
                };
                if !d.observed.contains(&name) {
                    return;
                }
                d.attribute_changed.clone()
            }
        }
    };
    if !f.is_undefined() {
        enqueue(h, n, Reaction::Callback(f, args));
    }
}

// ---- [CEReactions] ----

pub fn enter(h: &Host) {
    h.ce.borrow_mut().stack.push(Vec::new());
}

/// Leave a [CEReactions] scope: pop its element queue and invoke its reactions.
pub fn leave(vm: &mut Vm, h: &Host) {
    let q = h.ce.borrow_mut().stack.pop().unwrap_or_default();
    invoke(vm, h, q);
}

/// Invoke the reactions of the backup element queue (outside any scope).
pub fn flush(vm: &mut Vm, h: &Host) {
    let q = std::mem::take(&mut h.ce.borrow_mut().backup);
    invoke(vm, h, q);
}

/// §4.13.6 "invoke custom element reactions" in an element queue.
fn invoke(vm: &mut Vm, h: &Host, queue: Vec<u64>) {
    for k in queue {
        loop {
            let reaction = h.ce.borrow_mut().elems.get_mut(&k).and_then(|e| e.queue.pop_front());
            let Some(reaction) = reaction else { break };
            let n = NodeRef::unpack(k);
            let r = match reaction {
                Reaction::Upgrade(def) => upgrade(vm, h, n, def),
                Reaction::Callback(f, args) => match wrap(vm, h, n) {
                    Ok(this) => vm.call(&f, this, args).map(|_| ()),
                    Err(e) => Err(e),
                },
            };
            if let Err(e) = r {
                crate::events::report_exception(vm, e);
            }
        }
    }
}

// ---- upgrades (§4.13.5) ----

/// "Try to upgrade an element": enqueue its upgrade if it has a definition now.
fn try_upgrade(h: &Host, d: &bb_dom::Document, n: NodeRef) {
    let Some(e) = d.element(n.id) else { return };
    ensure(h, d, n);
    let st = state(h, n);
    if st != Some(State::Undefined) {
        return;
    }
    let is = h.ce.borrow().elems.get(&n.pack()).and_then(|x| x.is.clone());
    if let Some(def) = lookup(h, n.doc, e.ns, &e.name, is.as_deref()) {
        enqueue(h, n, Reaction::Upgrade(def));
    }
}

/// "Upgrade an element".
fn upgrade(vm: &mut Vm, h: &Host, n: NodeRef, def: usize) -> JsResult<()> {
    // Step 1.
    if !matches!(state(h, n), Some(State::Undefined | State::Uncustomized)) {
        return Ok(());
    }
    // Steps 2–3.
    {
        let mut r = h.ce.borrow_mut();
        if let Some(e) = r.elems.get_mut(&n.pack()) {
            e.def = Some(def);
            e.state = State::Failed;
        }
    }
    // Step 4: attributeChangedCallback for each attribute (state "failed" lets the
    // callbacks be queued once the definition is set, as custom).
    let (attrs, connected) = with_doc(vm, h, n.doc, |d| {
        let attrs: Vec<(String, String, Option<String>)> = d
            .element(n.id)
            .map(|e| {
                e.attrs
                    .iter()
                    .map(|a| {
                        let ns = (a.ns != Namespace::None).then(|| a.ns.url().to_string());
                        (a.name.clone(), a.value.clone(), ns)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let connected = std::iter::once(n.id).chain(d.ancestors(n.id)).last() == Some(bb_dom::Document::ROOT);
        (attrs, connected)
    })?;
    set_state(h, n, State::Custom);
    for (name, value, ns) in attrs {
        let ns = ns.map_or(Value::Null, |u| js(&u));
        enqueue_callback(h, n, "attributeChangedCallback", vec![js(&name), Value::Null, js(&value), ns]);
    }
    // Step 5.
    if connected {
        enqueue_callback(h, n, "connectedCallback", Vec::new());
    }
    set_state(h, n, State::Failed);
    // Steps 6–8: construct, with the element on the construction stack.
    let el = wrap(vm, h, n)?;
    let Value::Object(eo) = el else { return Ok(()) };
    let ctor = {
        let mut r = h.ce.borrow_mut();
        r.defs[def].construction.push(Some(eo));
        r.defs[def].ctor
    };
    let result = vm.construct(ctor, Vec::new(), None);
    h.ce.borrow_mut().defs[def].construction.pop();
    match result {
        Ok(Value::Object(o)) if o == eo => {
            set_state(h, n, State::Custom);
            Ok(())
        }
        Ok(_) => {
            fail(h, n);
            vm.type_error("the custom element constructor didn't return the element being upgraded")
        }
        Err(e) => {
            fail(h, n);
            Err(e)
        }
    }
}

fn set_state(h: &Host, n: NodeRef, s: State) {
    if let Some(e) = h.ce.borrow_mut().elems.get_mut(&n.pack()) {
        e.state = s;
    }
}

/// An upgrade threw: no definition, no reactions (§4.13.5 step 8.1).
fn fail(h: &Host, n: NodeRef) {
    if let Some(e) = h.ce.borrow_mut().elems.get_mut(&n.pack()) {
        e.state = State::Failed;
        e.def = None;
        e.queue.clear();
    }
}

// ---- hooks from the DOM's algorithms ----

/// Nodes were inserted: connected custom elements get connectedCallback, others may upgrade
/// (DOM §4.2.3 "insert" step 7.7).
pub fn inserted(vm: &mut Vm, h: &Host, roots: &[NodeRef]) {
    if h.ce.borrow().defs.is_empty() && !has_custom_names(h, roots) {
        return;
    }
    let _ = with_doc(vm, h, 0, |d| {
        for r in roots {
            if r.doc != 0
                || std::iter::once(r.id).chain(d.ancestors(r.id)).last() != Some(bb_dom::Document::ROOT)
            {
                continue;
            }
            for x in d.descendants(r.id) {
                if d.element(x).is_none() {
                    continue;
                }
                let n = r.with(x);
                ensure(h, d, n);
                if is_custom(h, n).is_some() {
                    enqueue_callback(h, n, "connectedCallback", Vec::new());
                } else {
                    try_upgrade(h, d, n);
                }
            }
        }
    });
}

fn has_custom_names(h: &Host, roots: &[NodeRef]) -> bool {
    !h.ce.borrow().elems.is_empty() && !roots.is_empty()
}

/// A node is about to be removed from document 0 (it is still connected): its custom
/// elements get disconnectedCallback (DOM §4.2.3 "remove" step 15).
pub fn removing(vm: &mut Vm, h: &Host, node: NodeRef) {
    if node.doc != 0 || h.ce.borrow().elems.is_empty() {
        return;
    }
    let _ = with_doc(vm, h, 0, |d| {
        if std::iter::once(node.id).chain(d.ancestors(node.id)).last() != Some(bb_dom::Document::ROOT) {
            return;
        }
        for x in d.descendants(node.id) {
            enqueue_callback(h, node.with(x), "disconnectedCallback", Vec::new());
        }
    });
}

/// An attribute changed (DOM §4.9 "handle attribute changes" step 2).
pub fn attribute_changed(
    h: &Host,
    n: NodeRef,
    ns: Namespace,
    local: &str,
    old: Option<&str>,
    new: Option<&str>,
) {
    if is_custom(h, n).is_none() {
        return;
    }
    let s = |v: Option<&str>| v.map_or(Value::Null, js);
    let ns = if ns == Namespace::None { Value::Null } else { js(ns.url()) };
    enqueue_callback(h, n, "attributeChangedCallback", vec![js(local), s(old), s(new), ns]);
}

/// Nodes moved to another document (DOM §4.5 "adopt" step 3.2): their data follows, and
/// custom elements get adoptedCallback(old document, new document).
pub fn adopted(vm: &mut Vm, h: &Host, pairs: &[(u64, u64)], old_doc: u32, new_doc: u32) {
    let mut moved = Vec::new();
    {
        let mut r = h.ce.borrow_mut();
        for &(o, n) in pairs {
            if let Some(e) = r.elems.remove(&o) {
                if e.state == State::Custom {
                    moved.push(n);
                }
                r.elems.insert(n, e);
            }
            let Registry { stack, backup, .. } = &mut *r;
            for k in stack.iter_mut().flatten().chain(backup.iter_mut()) {
                if *k == o {
                    *k = n;
                }
            }
        }
    }
    if moved.is_empty() {
        return;
    }
    let root = bb_dom::Document::ROOT;
    let (Ok(od), Ok(nd)) =
        (wrap(vm, h, NodeRef { doc: old_doc, id: root }), wrap(vm, h, NodeRef { doc: new_doc, id: root }))
    else {
        return;
    };
    for n in moved {
        enqueue_callback(h, NodeRef::unpack(n), "adoptedCallback", vec![od.clone(), nd.clone()]);
    }
}

/// Copies made by cloneNode get the originals' `is` values; defined ones are upgraded.
pub fn cloned(vm: &mut Vm, h: &Host, pairs: &[(NodeRef, NodeRef)]) {
    if h.ce.borrow().elems.is_empty() {
        return;
    }
    let _ = with_doc(vm, h, pairs.first().map_or(0, |p| p.0.doc), |d| {
        for &(src, copy) in pairs {
            let Some(e) = d.element(copy.id) else { continue };
            let is = h.ce.borrow().elems.get(&src.pack()).and_then(|x| x.is.clone());
            let custom = e.ns == Namespace::Html && valid_name(&e.name);
            if !custom && is.is_none() {
                continue;
            }
            h.ce.borrow_mut().elems.insert(
                copy.pack(),
                Elem { state: State::Undefined, def: None, is: is.clone(), queue: VecDeque::new() },
            );
            if let Some(def) = lookup(h, copy.doc, e.ns, &e.name, is.as_deref()) {
                enqueue(h, copy, Reaction::Upgrade(def));
            }
        }
    });
}

/// Elements the HTML parser created (arena indices from `before`) get their data and are
/// upgraded if defined (HTML §13.2.6.1 "create an element for a token", approximated).
pub fn parsed(vm: &mut Vm, h: &Host, before: usize) {
    if h.ce.borrow().defs.is_empty() {
        // Record `is` values and states lazily later; nothing to upgrade now.
        return;
    }
    enter(h);
    let _ = with_doc(vm, h, 0, |d| {
        for i in before..d.len() {
            let n = NodeRef { doc: 0, id: NodeId(i as u32) };
            if d.element(n.id).is_some()
                && std::iter::once(n.id).chain(d.ancestors(n.id)).last() == Some(bb_dom::Document::ROOT)
            {
                ensure(h, d, n);
                try_upgrade(h, d, n);
            }
        }
    });
    leave(vm, h);
}

// ---- document.createElement (DOM §4.9 "create an element", synchronous custom elements) ----

/// createElement's custom element steps: Some(element) when a definition made it, else None
/// (the caller creates a plain element and records its state with `created`).
pub fn create(vm: &mut Vm, h: &Host, doc: u32, local: &str, is: Option<&str>) -> JsResult<Option<Value>> {
    let Some(def) = lookup(h, doc, Namespace::Html, local, is) else { return Ok(None) };
    let (autonomous, ctor) = {
        let r = h.ce.borrow();
        (r.defs[def].name == r.defs[def].local_name, r.defs[def].ctor)
    };
    if autonomous {
        // Step 6.2: construct it; the result must be a fresh element of this document.
        let v = match vm.construct(ctor, Vec::new(), None) {
            Ok(v) => v,
            Err(e) => {
                // Step 6.2.3: report it, and make an HTMLUnknownElement in the "failed" state.
                crate::events::report_exception(vm, e);
                let id = with_doc_mut(vm, h, doc, |d| d.create_element(Namespace::Html, local, Vec::new()))?;
                let n = NodeRef { doc, id };
                h.ce.borrow_mut().elems.insert(
                    n.pack(),
                    Elem { state: State::Failed, def: None, is: None, queue: VecDeque::new() },
                );
                let w = wrap(vm, h, n)?;
                if let Value::Object(o) = w {
                    vm.heap.obj_mut(o).proto = Some(h.proto(C::HTMLUnknownElement));
                }
                return Ok(Some(w));
            }
        };
        let Some(n) = node_of(vm, &v) else {
            return vm.type_error("the custom element constructor didn't return an element");
        };
        let ok = with_doc(vm, h, n.doc, |d| {
            let e = d.element(n.id);
            n.doc == doc
                && e.is_some_and(|e| e.attrs.is_empty() && e.name == local && e.ns == Namespace::Html)
                && d.first_child(n.id).is_none()
                && d.parent(n.id).is_none()
        })?;
        if !ok {
            return throw(
                vm,
                "NotSupportedError",
                "the custom element constructor returned an unsuitable element",
            );
        }
        return Ok(Some(v));
    }
    // A customized built-in element: created, then upgraded synchronously (step 5).
    let id = with_doc_mut(vm, h, doc, |d| d.create_element(Namespace::Html, local, Vec::new()))?;
    let n = NodeRef { doc, id };
    h.ce.borrow_mut().elems.insert(
        n.pack(),
        Elem { state: State::Undefined, def: None, is: is.map(str::to_string), queue: VecDeque::new() },
    );
    let w = wrap(vm, h, n)?;
    if let Err(e) = upgrade(vm, h, n, def) {
        crate::events::report_exception(vm, e);
    }
    Ok(Some(w))
}

/// Record a created element's state and `is` value (no definition applied).
pub fn created(h: &Host, n: NodeRef, local: &str, is: Option<&str>) {
    let custom = valid_name(local) || is.is_some();
    if !custom && h.ce.borrow().elems.is_empty() {
        return;
    }
    h.ce.borrow_mut().elems.insert(
        n.pack(),
        Elem {
            state: if custom { State::Undefined } else { State::Uncustomized },
            def: None,
            is: is.map(str::to_string),
            queue: VecDeque::new(),
        },
    );
}

// ---- [HTMLConstructor] (HTML §3.2.3) ----

pub fn html_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("Illegal constructor") };
    let h = host(vm)?;
    // Step 2: `new HTMLElement()` itself.
    if nt == c.callee {
        return vm.type_error("Illegal constructor");
    }
    // Step 3.
    let def = h.ce.borrow().defs.iter().position(|d| d.ctor == nt);
    let Some(def) = def else {
        return vm.type_error("Illegal constructor: no custom element is defined with it");
    };
    let (name, local) = {
        let r = h.ce.borrow();
        (r.defs[def].name.clone(), r.defs[def].local_name.clone())
    };
    // Steps 4–5: the interface must match.
    let class = if name == local { C::HTMLElement } else { crate::class::html_class(&local) };
    if c.callee != h.ctor(class) {
        return vm.type_error("Illegal constructor: wrong interface for this custom element");
    }
    // Step 6.
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => p,
        _ => h.proto(class),
    };
    // Step 7: a new element.
    let top = h.ce.borrow().defs[def].construction.last().copied();
    let Some(top) = top else {
        let id = with_doc_mut(vm, &h, 0, |d| d.create_element(Namespace::Html, &local, Vec::new()))?;
        let n = NodeRef { doc: 0, id };
        h.ce.borrow_mut().elems.insert(
            n.pack(),
            Elem {
                state: State::Custom,
                def: Some(def),
                is: (name != local).then(|| name.clone()),
                queue: VecDeque::new(),
            },
        );
        let w = wrap(vm, &h, n)?;
        if let Value::Object(o) = w {
            vm.heap.obj_mut(o).proto = Some(proto);
        }
        return Ok(w);
    };
    // Steps 9–14: the element being upgraded.
    let Some(el) = top else { return vm.type_error("this custom element is already constructed") };
    vm.heap.obj_mut(el).proto = Some(proto);
    if let Some(slot) = h.ce.borrow_mut().defs[def].construction.last_mut() {
        *slot = None;
    }
    Ok(Value::Object(el))
}

// ---- CustomElementRegistry (§4.13.4) ----

fn callback(vm: &mut Vm, proto: ObjRef, name: &str) -> R {
    let v = vm.get(proto, &PropKey::named(name))?;
    if !v.is_undefined() && !vm.is_callable(&v) {
        return vm.type_error(&format!("{name} must be a function"));
    }
    Ok(v)
}

fn define(vm: &mut Vm, c: NativeCall) -> R {
    let h = host(vm)?;
    let name = rstr(vm, &c.arg(0))?;
    let ctor = c.arg(1);
    // Step 1.
    let Value::Object(ctor_o) = ctor else { return vm.type_error("the constructor must be a constructor") };
    if !vm.is_constructor(&ctor) {
        return vm.type_error("the constructor must be a constructor");
    }
    // Steps 2–4.
    if !valid_name(&name) {
        return throw(vm, "SyntaxError", &format!("'{name}' is not a valid custom element name"));
    }
    {
        let r = h.ce.borrow();
        if r.defs.iter().any(|d| d.name == name) {
            return throw(vm, "NotSupportedError", &format!("'{name}' is already defined"));
        }
        if r.defs.iter().any(|d| d.ctor == ctor_o) {
            return throw(vm, "NotSupportedError", "this constructor is already defined");
        }
    }
    // Steps 5–7: extends.
    let mut local = name.clone();
    let ext = dict_get(vm, &c.arg(2), "extends")?;
    if !ext.is_undefined() {
        let e = rstr(vm, &ext)?;
        if valid_name(&e) {
            return throw(vm, "NotSupportedError", "a custom element can't extend another custom element");
        }
        if crate::class::html_class(&e) == C::HTMLUnknownElement {
            return throw(vm, "NotSupportedError", &format!("'{e}' is not an HTML element"));
        }
        local = e;
    }
    // Steps 8–9.
    if h.ce.borrow().defining {
        return throw(vm, "NotSupportedError", "a custom element is already being defined");
    }
    h.ce.borrow_mut().defining = true;
    let r = (|| -> JsResult<(ObjRef, [Value; 4], Vec<String>)> {
        // Step 14.1–14.2.
        let Value::Object(proto) = vm.get(ctor_o, &PropKey::lit("prototype"))? else {
            return vm.type_error("the constructor's prototype must be an object");
        };
        let cbs = [
            callback(vm, proto, "connectedCallback")?,
            callback(vm, proto, "disconnectedCallback")?,
            callback(vm, proto, "adoptedCallback")?,
            callback(vm, proto, "attributeChangedCallback")?,
        ];
        let mut observed = Vec::new();
        if !cbs[3].is_undefined() {
            let v = vm.get(ctor_o, &PropKey::lit("observedAttributes"))?;
            if !v.is_undefined() {
                for x in vm.iterate_to_vec(&v)? {
                    observed.push(rstr(vm, &x)?);
                }
            }
        }
        // disabledFeatures and formAssociated are read (steps 14.6–14.9) for their side effects.
        for k in ["disabledFeatures", "formAssociated"] {
            let v = vm.get(ctor_o, &PropKey::named(k))?;
            if k == "disabledFeatures" && !v.is_undefined() {
                for x in vm.iterate_to_vec(&v)? {
                    rstr(vm, &x)?;
                }
            }
        }
        Ok((proto, cbs, observed))
    })();
    h.ce.borrow_mut().defining = false;
    let (proto, [connected, disconnected, adopted, attribute_changed], observed) = r?;
    // Steps 15–16.
    let def = {
        let mut r = h.ce.borrow_mut();
        r.defs.push(Def {
            name: name.clone(),
            local_name: local.clone(),
            ctor: ctor_o,
            proto,
            observed,
            connected,
            disconnected,
            adopted,
            attribute_changed,
            construction: Vec::new(),
        });
        r.defs.len() - 1
    };
    // Steps 17–19: upgrade candidates, in tree order.
    enter(&h);
    let _ = with_doc(vm, &h, 0, |d| {
        for x in d.descendants(bb_dom::Document::ROOT) {
            let Some(e) = d.element(x) else { continue };
            if e.ns != Namespace::Html || e.name != local {
                continue;
            }
            let n = NodeRef { doc: 0, id: x };
            ensure(&h, d, n);
            let is = h.ce.borrow().elems.get(&n.pack()).and_then(|x| x.is.clone());
            let matches = if name == local { true } else { is.as_deref() == Some(name.as_str()) };
            if matches && state(&h, n) == Some(State::Undefined) {
                enqueue(&h, n, Reaction::Upgrade(def));
            }
        }
    });
    leave(vm, &h);
    // Step 20: resolve whenDefined.
    let waiting = h.ce.borrow_mut().when.remove(&name);
    if let Some((_, resolve)) = waiting {
        vm.call(&resolve, Value::Undefined, vec![ctor])?;
    }
    Ok(Value::Undefined)
}

pub fn setup_registry(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "define", 2, define);
    op(vm, proto, "get", 1, |vm, c| {
        let name = rstr(vm, &c.arg(0))?;
        let h = host(vm)?;
        let r = h.ce.borrow();
        Ok(r.defs.iter().find(|d| d.name == name).map_or(Value::Undefined, |d| Value::Object(d.ctor)))
    });
    op(vm, proto, "getName", 1, |vm, c| {
        let h = host(vm)?;
        let Value::Object(o) = c.arg(0) else {
            return vm.type_error("getName needs a constructor");
        };
        let r = h.ce.borrow();
        Ok(r.defs.iter().find(|d| d.ctor == o).map_or(Value::Null, |d| js(&d.name)))
    });
    op(vm, proto, "whenDefined", 1, |vm, c| {
        let name = rstr(vm, &c.arg(0))?;
        let h = host(vm)?;
        let p = vm.new_promise(None);
        let (res, rej) = vm.create_resolving_functions(p);
        if !valid_name(&name) {
            let e = crate::exception::make(
                vm,
                "SyntaxError",
                &format!("'{name}' is not a valid custom element name"),
            );
            vm.call(&rej, Value::Undefined, vec![e])?;
            return Ok(Value::Object(p));
        }
        let defined = h.ce.borrow().defs.iter().find(|d| d.name == name).map(|d| d.ctor);
        if let Some(ctor) = defined {
            vm.call(&res, Value::Undefined, vec![Value::Object(ctor)])?;
            return Ok(Value::Object(p));
        }
        if let Some((existing, _)) = h.ce.borrow().when.get(&name) {
            return Ok(existing.clone());
        }
        h.ce.borrow_mut().when.insert(name, (Value::Object(p), res));
        Ok(Value::Object(p))
    });
    op(vm, proto, "upgrade", 1, |vm, c| {
        let h = host(vm)?;
        let Some(root) = node_of(vm, &c.arg(0)) else { return vm.type_error("upgrade needs a node") };
        enter(&h);
        let _ = with_doc(vm, &h, root.doc, |d| {
            for x in d.descendants(root.id) {
                if d.element(x).is_some() {
                    try_upgrade(&h, d, root.with(x));
                }
            }
        });
        leave(vm, &h);
        Ok(Value::Undefined)
    });
}

/// window.customElements: the registry object, made once.
pub fn registry_getter(vm: &mut Vm, _c: NativeCall) -> R {
    let h = host(vm)?;
    if let Some(o) = h.misc.borrow().get("customElements") {
        return Ok(Value::Object(*o));
    }
    let o = vm.new_host_object(
        Some(h.proto(C::CustomElementRegistry)),
        C::CustomElementRegistry as u32,
        0,
        false,
    );
    h.misc.borrow_mut().insert("customElements", o);
    Ok(Value::Object(o))
}

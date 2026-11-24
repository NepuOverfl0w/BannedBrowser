//! Mutation observers (DOM §4.3), and the mutation algorithms that queue their records:
//! attribute changes (DOM §4.9 "change", "append", "remove an attribute" and "handle
//! attribute changes") and "replace data" (DOM §4.10). The tree algorithms (tree.rs) queue
//! tree mutation records (DOM §4.3.2) through [`tree_record`].

use std::collections::HashMap;
use std::rc::Rc;

use bb_dom::{Attribute, Namespace, NodeData};
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::{realm, PropKey};
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::exception::throw;
use crate::host::{
    dict_get, host, js, rstr, this_of, utf16_len, with_doc, with_doc_mut, wrap, wrap_opt, Host, NodeRef,
};
use crate::idl::{attr_with, callee_slots, op};

type R = JsResult<Value>;

/// MutationObserverInit (DOM §4.3.1), after `observe` filled in the implied members.
#[derive(Clone, Default, Debug)]
pub struct Options {
    pub child_list: bool,
    pub attributes: bool,
    pub character_data: bool,
    pub subtree: bool,
    pub attribute_old_value: bool,
    pub character_data_old_value: bool,
    pub attribute_filter: Option<Vec<String>>,
}

/// A registered observer (DOM §4.3). A transient one names its source: the node whose
/// registered observer (for the same observer) it was made from.
#[derive(Clone, Debug)]
pub struct Registered {
    pub observer: u32,
    pub options: Options,
    pub source: Option<u64>,
}

/// A MutationObserver's state. Its callback is slot 0 of `obj`.
pub struct Observer {
    pub obj: ObjRef,
    pub records: Vec<Value>,
    /// The nodes it observes ("node list").
    pub nodes: Vec<u64>,
    /// Nodes holding its transient registered observers, so delivery can remove them.
    pub transient: Vec<u64>,
}

/// The mutation observers of the window's agent (DOM §4.3: "mutation observer microtask
/// queued", "pending mutation observers"), and each node's registered observer list.
#[derive(Default)]
pub struct State {
    pub observers: Vec<Observer>,
    pub registered: HashMap<u64, Vec<Registered>>,
    pub pending: Vec<u32>,
    pub microtask_queued: bool,
}

impl State {
    /// Objects to keep alive: observers that observe something or have records waiting.
    pub fn trace(&self, out: &mut Vec<ObjRef>) {
        for o in &self.observers {
            if !o.nodes.is_empty() || !o.transient.is_empty() || !o.records.is_empty() {
                out.push(o.obj);
                for r in &o.records {
                    if let Value::Object(r) = r {
                        out.push(*r);
                    }
                }
            }
        }
    }

    /// Nodes moved to other ids (adopted into another document's arena).
    pub fn remap(&mut self, pairs: &[(u64, u64)]) {
        if self.registered.is_empty() {
            return;
        }
        let map: HashMap<u64, u64> = pairs.iter().copied().collect();
        let fix = |v: &mut u64| {
            if let Some(n) = map.get(v) {
                *v = *n;
            }
        };
        let old = std::mem::take(&mut self.registered);
        for (mut k, mut list) in old {
            fix(&mut k);
            for r in &mut list {
                if let Some(s) = &mut r.source {
                    fix(s);
                }
            }
            self.registered.entry(k).or_default().extend(list);
        }
        for o in &mut self.observers {
            o.nodes.iter_mut().for_each(fix);
            o.transient.iter_mut().for_each(fix);
        }
    }

    /// Whether any node has a registered observer (else nothing needs recording).
    pub fn any(&self) -> bool {
        !self.registered.is_empty()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Attributes,
    CharacterData,
    ChildList,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Attributes => "attributes",
            Kind::CharacterData => "characterData",
            Kind::ChildList => "childList",
        }
    }
}

/// A mutation record's fields (DOM §4.3.5).
pub struct Rec {
    pub kind: Kind,
    pub target: NodeRef,
    pub name: Option<String>,
    pub namespace: Option<String>,
    pub old: Option<String>,
    pub added: Vec<NodeRef>,
    pub removed: Vec<NodeRef>,
    pub prev: Option<NodeRef>,
    pub next: Option<NodeRef>,
}

/// DOM §4.3.2 "queue a mutation record".
pub fn queue_record(vm: &mut Vm, h: &Host, rec: Rec) -> JsResult<()> {
    if !h.mo.borrow().any() {
        return Ok(());
    }
    // Steps 1–3: the interested observers, each with the old value it gets.
    let nodes: Vec<NodeRef> = with_doc(vm, h, rec.target.doc, |d| {
        std::iter::once(rec.target.id).chain(d.ancestors(rec.target.id)).map(|x| rec.target.with(x)).collect()
    })?;
    let mut interested: Vec<(u32, Option<String>)> = Vec::new();
    {
        let st = h.mo.borrow();
        for node in &nodes {
            let Some(list) = st.registered.get(&node.pack()) else { continue };
            for r in list {
                let o = &r.options;
                let skip = (*node != rec.target && !o.subtree)
                    || (rec.kind == Kind::Attributes && !o.attributes)
                    || (rec.kind == Kind::Attributes
                        && o.attribute_filter.as_ref().is_some_and(|f| {
                            rec.namespace.is_some() || !f.iter().any(|x| Some(x) == rec.name.as_ref())
                        }))
                    || (rec.kind == Kind::CharacterData && !o.character_data)
                    || (rec.kind == Kind::ChildList && !o.child_list);
                if skip {
                    continue;
                }
                let i = match interested.iter().position(|(x, _)| *x == r.observer) {
                    Some(i) => i,
                    None => {
                        interested.push((r.observer, None));
                        interested.len() - 1
                    }
                };
                if (rec.kind == Kind::Attributes && o.attribute_old_value)
                    || (rec.kind == Kind::CharacterData && o.character_data_old_value)
                {
                    interested[i].1 = rec.old.clone();
                }
            }
        }
    }
    if interested.is_empty() {
        return Ok(());
    }
    // Step 4.
    for (observer, old) in interested {
        let record = new_record(vm, h, &rec, old)?;
        let mut st = h.mo.borrow_mut();
        if let Some(o) = st.observers.get_mut(observer as usize) {
            o.records.push(record);
        }
        if !st.pending.contains(&observer) {
            st.pending.push(observer);
        }
    }
    queue_microtask(vm, h);
    Ok(())
}

/// DOM §4.3.2 "queue a tree mutation record".
pub fn tree_record(
    vm: &mut Vm,
    h: &Host,
    target: NodeRef,
    added: Vec<NodeRef>,
    removed: Vec<NodeRef>,
    prev: Option<NodeRef>,
    next: Option<NodeRef>,
) -> JsResult<()> {
    if added.is_empty() && removed.is_empty() {
        return Ok(());
    }
    queue_record(
        vm,
        h,
        Rec {
            kind: Kind::ChildList,
            target,
            name: None,
            namespace: None,
            old: None,
            added,
            removed,
            prev,
            next,
        },
    )
}

/// DOM §4.2.3 "remove" step 14: observers of `parent`'s inclusive ancestors with `subtree`
/// keep observing the removed `node` until their next delivery (transient observers).
pub fn add_transient_observers(vm: &mut Vm, h: &Host, parent: NodeRef, node: NodeRef) -> JsResult<()> {
    if !h.mo.borrow().any() {
        return Ok(());
    }
    let ancestors: Vec<u64> = with_doc(vm, h, parent.doc, |d| {
        std::iter::once(parent.id).chain(d.ancestors(parent.id)).map(|x| parent.with(x).pack()).collect()
    })?;
    let mut st = h.mo.borrow_mut();
    let mut new = Vec::new();
    for a in ancestors {
        if let Some(list) = st.registered.get(&a) {
            for r in list.iter().filter(|r| r.options.subtree) {
                // One made from a transient observer keeps its source, so observe() on the
                // original target drops the whole chain.
                let source = Some(r.source.unwrap_or(a));
                new.push(Registered { observer: r.observer, options: r.options.clone(), source });
            }
        }
    }
    if new.is_empty() {
        return Ok(());
    }
    let key = node.pack();
    for r in new {
        if let Some(o) = st.observers.get_mut(r.observer as usize) {
            if !o.transient.contains(&key) {
                o.transient.push(key);
            }
        }
        // The observer is notified (and drops the transient observers) at the next
        // checkpoint even if no record is queued.
        if !st.pending.contains(&r.observer) {
            st.pending.push(r.observer);
        }
        st.registered.entry(key).or_default().push(r);
    }
    drop(st);
    queue_microtask(vm, h);
    Ok(())
}

fn queue_microtask(vm: &mut Vm, h: &Host) {
    if h.mo.borrow().microtask_queued {
        return;
    }
    h.mo.borrow_mut().microtask_queued = true;
    let cached = h.misc.borrow().get("mo_notify").copied();
    let f = match cached {
        Some(f) => f,
        None => {
            let f = realm::native(vm, "notify mutation observers", 0, notify);
            h.misc.borrow_mut().insert("mo_notify", f);
            f
        }
    };
    vm.enqueue_microtask(Value::Object(f));
}

/// DOM §4.3.2 "notify mutation observers" (the mutation observer microtask).
fn notify(vm: &mut Vm, _c: NativeCall) -> R {
    let h = host(vm)?;
    let set = {
        let mut st = h.mo.borrow_mut();
        st.microtask_queued = false;
        let mut set = std::mem::take(&mut st.pending);
        // In the order the observers were created.
        set.sort_unstable();
        set
    };
    // Step 4 (signal slots) has nothing to do: no slot gets assigned nodes without shadow trees.
    for id in set {
        let (obj, records) = {
            let mut st = h.mo.borrow_mut();
            let Some(o) = st.observers.get_mut(id as usize) else { continue };
            let records = std::mem::take(&mut o.records);
            let transient = std::mem::take(&mut o.transient);
            let obj = o.obj;
            for n in transient {
                remove_registered(&mut st, n, |r| r.observer == id && r.source.is_some());
            }
            (obj, records)
        };
        if records.is_empty() {
            continue;
        }
        let cb = vm.host_obj(obj).and_then(|o| o.slots.first().cloned()).unwrap_or(Value::Undefined);
        let list = vm.new_array(records);
        if let Err(e) = vm.call(&cb, Value::Object(obj), vec![Value::Object(list), Value::Object(obj)]) {
            if vm.terminated {
                return Err(e);
            }
            crate::events::report_exception(vm, e);
        }
    }
    Ok(Value::Undefined)
}

fn remove_registered(st: &mut State, node: u64, f: impl Fn(&Registered) -> bool) {
    if let Some(list) = st.registered.get_mut(&node) {
        list.retain(|r| !f(r));
        if list.is_empty() {
            st.registered.remove(&node);
        }
    }
}

// ---- MutationRecord ----

const RECORD_FIELDS: [&str; 9] = [
    "type",
    "target",
    "addedNodes",
    "removedNodes",
    "previousSibling",
    "nextSibling",
    "attributeName",
    "attributeNamespace",
    "oldValue",
];

fn new_record(vm: &mut Vm, h: &Host, rec: &Rec, old: Option<String>) -> R {
    let opt = |s: &Option<String>| s.as_deref().map_or(Value::Null, js);
    let target = wrap(vm, h, rec.target)?;
    let added = node_list(vm, h, &rec.added)?;
    let removed = node_list(vm, h, &rec.removed)?;
    let prev = wrap_opt(vm, h, rec.prev)?;
    let next = wrap_opt(vm, h, rec.next)?;
    let slots = vec![
        js(rec.kind.name()),
        target,
        added,
        removed,
        prev,
        next,
        opt(&rec.name),
        opt(&rec.namespace),
        opt(&old),
    ];
    let o = vm.new_host_object(Some(h.proto(C::MutationRecord)), C::MutationRecord as u32, 0, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = slots;
    }
    Ok(Value::Object(o))
}

fn node_list(vm: &mut Vm, h: &Host, nodes: &[NodeRef]) -> R {
    crate::collections::collection(vm, h, crate::host::CollKind::Static(nodes.to_vec()), None)
}

pub fn setup_record(vm: &mut Vm, proto: ObjRef) {
    for (i, name) in RECORD_FIELDS.iter().enumerate() {
        attr_with(
            vm,
            proto,
            name,
            |vm, c| {
                this_of(vm, &c.this, C::MutationRecord)?;
                let i = match callee_slots(vm, c.callee).first() {
                    Some(Value::Number(i)) => *i as usize,
                    _ => return Ok(Value::Undefined),
                };
                let Value::Object(o) = c.this else { return Ok(Value::Undefined) };
                Ok(vm.host_obj(o).and_then(|ho| ho.slots.get(i).cloned()).unwrap_or(Value::Null))
            },
            None,
            vec![Value::Number(i as f64)],
        );
    }
}

// ---- MutationObserver ----

pub fn observer_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("MutationObserver requires 'new'") };
    let cb = c.arg(0);
    if !vm.is_callable(&cb) {
        return vm.type_error("MutationObserver's callback is not a function");
    }
    let h = host(vm)?;
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => p,
        _ => h.proto(C::MutationObserver),
    };
    let id = h.mo.borrow().observers.len() as u64;
    let o = vm.new_host_object(Some(proto), C::MutationObserver as u32, id, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = vec![cb];
    }
    h.mo.borrow_mut().observers.push(Observer {
        obj: o,
        records: Vec::new(),
        nodes: Vec::new(),
        transient: Vec::new(),
    });
    Ok(Value::Object(o))
}

fn observer_this(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, u32)> {
    let (_, id) = this_of(vm, &c.this, C::MutationObserver)?;
    Ok((host(vm)?, id as u32))
}

/// A dictionary member that "exists" (WebIDL: present and not undefined), as a boolean.
fn member(vm: &mut Vm, d: &Value, name: &'static str) -> JsResult<Option<bool>> {
    let v = dict_get(vm, d, name)?;
    Ok(match v {
        Value::Undefined => None,
        v => Some(vm.to_boolean(&v)),
    })
}

/// MutationObserverInit, its members converted in lexicographic order (WebIDL §3.2.17).
fn options(vm: &mut Vm, d: &Value) -> JsResult<(Options, [Option<bool>; 4], bool)> {
    let filter = match dict_get(vm, d, "attributeFilter")? {
        Value::Undefined => None,
        v => {
            let Value::Object(o) = &v else { return vm.type_error("attributeFilter must be a sequence") };
            let key = PropKey::Sym(vm.realm.sym_iterator.clone());
            let f = vm.get(*o, &key)?;
            let items = crate::url::iter_values(vm, &v, &f)?;
            let mut out = Vec::with_capacity(items.len());
            for x in items {
                out.push(rstr(vm, &x)?);
            }
            Some(out)
        }
    };
    let attribute_old_value = member(vm, d, "attributeOldValue")?;
    let attributes = member(vm, d, "attributes")?;
    let character_data = member(vm, d, "characterData")?;
    let character_data_old_value = member(vm, d, "characterDataOldValue")?;
    let child_list = member(vm, d, "childList")?.unwrap_or(false);
    let subtree = member(vm, d, "subtree")?.unwrap_or(false);
    let has_filter = filter.is_some();
    Ok((
        Options {
            child_list,
            attributes: attributes.unwrap_or(false),
            character_data: character_data.unwrap_or(false),
            subtree,
            attribute_old_value: attribute_old_value.unwrap_or(false),
            character_data_old_value: character_data_old_value.unwrap_or(false),
            attribute_filter: filter,
        },
        [attributes, attribute_old_value, character_data, character_data_old_value],
        has_filter,
    ))
}

pub fn setup_observer(vm: &mut Vm, proto: ObjRef) {
    // DOM §4.3.1 observe(target, options).
    op(vm, proto, "observe", 1, |vm, c| {
        let (h, id) = observer_this(vm, &c)?;
        let target = crate::host::arg_node(vm, &c.arg(0), "the target")?;
        let (mut o, [attributes, attr_old, character_data, cd_old], has_filter) = options(vm, &c.arg(1))?;
        // Steps 1–2.
        if (attr_old.is_some() || has_filter) && attributes.is_none() {
            o.attributes = true;
        }
        if cd_old.is_some() && character_data.is_none() {
            o.character_data = true;
        }
        // Steps 3–6.
        if !(o.child_list || o.attributes || o.character_data) {
            return vm.type_error("observe() needs childList, attributes or characterData");
        }
        if o.attribute_old_value && !o.attributes {
            return vm.type_error("attributeOldValue needs attributes");
        }
        if o.attribute_filter.is_some() && !o.attributes {
            return vm.type_error("attributeFilter needs attributes");
        }
        if o.character_data_old_value && !o.character_data {
            return vm.type_error("characterDataOldValue needs characterData");
        }
        let key = target.pack();
        let mut st = h.mo.borrow_mut();
        let existing =
            st.registered.get(&key).is_some_and(|l| l.iter().any(|r| r.observer == id && r.source.is_none()));
        if existing {
            // Step 7: replace the options, dropping the transient observers made from it.
            let nodes: Vec<u64> =
                st.observers.get(id as usize).map(|o| o.transient.clone()).unwrap_or_default();
            for n in nodes {
                remove_registered(&mut st, n, |r| r.observer == id && r.source == Some(key));
            }
            if let Some(list) = st.registered.get_mut(&key) {
                for r in list.iter_mut().filter(|r| r.observer == id && r.source.is_none()) {
                    r.options = o.clone();
                }
            }
        } else {
            // Step 8.
            st.registered.entry(key).or_default().push(Registered { observer: id, options: o, source: None });
            if let Some(ob) = st.observers.get_mut(id as usize) {
                ob.nodes.push(key);
            }
        }
        Ok(Value::Undefined)
    });
    // DOM §4.3.1 disconnect().
    op(vm, proto, "disconnect", 0, |vm, c| {
        let (h, id) = observer_this(vm, &c)?;
        let mut st = h.mo.borrow_mut();
        let (nodes, transient) = match st.observers.get_mut(id as usize) {
            Some(o) => {
                o.records.clear();
                (std::mem::take(&mut o.nodes), std::mem::take(&mut o.transient))
            }
            None => return Ok(Value::Undefined),
        };
        for n in nodes.into_iter().chain(transient) {
            remove_registered(&mut st, n, |r| r.observer == id);
        }
        Ok(Value::Undefined)
    });
    // DOM §4.3.1 takeRecords().
    op(vm, proto, "takeRecords", 0, |vm, c| {
        let (h, id) = observer_this(vm, &c)?;
        let records =
            h.mo.borrow_mut()
                .observers
                .get_mut(id as usize)
                .map(|o| std::mem::take(&mut o.records))
                .unwrap_or_default();
        Ok(Value::Object(vm.new_array(records)))
    });
}

// ---- attribute changes (DOM §4.9) ----

fn ns_string(ns: Namespace) -> Option<String> {
    (ns != Namespace::None).then(|| ns.url().to_string())
}

/// DOM §4.9 "handle attribute changes": queue the record, then the attribute change steps
/// (event handler attributes).
fn handle_change(
    vm: &mut Vm,
    h: &Host,
    n: NodeRef,
    ns: Namespace,
    local: &str,
    old: Option<String>,
) -> JsResult<()> {
    queue_record(
        vm,
        h,
        Rec {
            kind: Kind::Attributes,
            target: n,
            name: Some(local.to_string()),
            namespace: ns_string(ns),
            old,
            added: Vec::new(),
            removed: Vec::new(),
            prev: None,
            next: None,
        },
    )?;
    if ns == Namespace::None {
        crate::events::attribute_changed(vm, h, n, local)?;
        // HTML §4.12.5: a canvas's width or height attribute set resets its bitmap.
        if matches!(local, "width" | "height") && with_doc(vm, h, n.doc, |d| d.is_html(n.id, "canvas"))? {
            crate::canvas::size_changed(vm, h, n)?;
        }
        // HTML §4.12.1: a src attribute set on a connected script element that hasn't
        // started prepares it.
        if local == "src" && n.doc == 0 && !h.started_scripts.borrow().contains(&n.pack()) {
            let set = with_doc(vm, h, n.doc, |d| d.is_html(n.id, "script") && d.attr(n.id, "src").is_some())?;
            if set {
                crate::loader::script_inserted(vm, h, n);
            }
        }
        if matches!(local, "src" | "srcdoc") && h.frames.borrow().nested {
            let hr = host(vm)?;
            if std::ptr::eq(&*hr, h) {
                crate::frames::attribute_changed(vm, &hr, n, local)?;
            }
        }
    }
    Ok(())
}

/// Set attribute (`ns`, `local`) of element `n` to `value`: DOM §4.9 "change an attribute"
/// when it exists, else "append an attribute" (with `prefix`).
pub fn set_attr(
    vm: &mut Vm,
    h: &Host,
    n: NodeRef,
    ns: Namespace,
    prefix: Option<String>,
    local: &str,
    value: &str,
) -> JsResult<()> {
    let old = with_doc_mut(vm, h, n.doc, |d| {
        let old = d
            .element(n.id)
            .and_then(|e| e.attrs.iter().find(|a| a.ns == ns && a.name == local).map(|a| a.value.clone()));
        crate::dom::set_attr_raw(d, n.id, ns, prefix, local, value);
        old
    })?;
    handle_change(vm, h, n, ns, local, old)
}

/// DOM §4.9 "remove an attribute": the attribute at `index` of element `n`.
pub fn remove_attr_at(vm: &mut Vm, h: &Host, n: NodeRef, index: usize) -> JsResult<Option<Attribute>> {
    let removed = with_doc_mut(vm, h, n.doc, |d| {
        let a = d.element(n.id)?.attrs.get(index)?.clone();
        if a.ns == Namespace::None {
            d.remove_attr(n.id, &a.name);
        } else if let Some(e) = d.element_mut(n.id) {
            e.attrs.remove(index);
            d.mark_style_dirty(n.id);
        }
        Some(a)
    })?;
    if let Some(a) = &removed {
        crate::attr::detach_attr(h, n, a);
        handle_change(vm, h, n, a.ns, &a.name, Some(a.value.clone()))?;
    }
    Ok(removed)
}

/// Remove attribute (`ns`, `local`) of element `n`, if it has one.
pub fn remove_attr(
    vm: &mut Vm,
    h: &Host,
    n: NodeRef,
    ns: Namespace,
    local: &str,
) -> JsResult<Option<Attribute>> {
    let i = with_doc(vm, h, n.doc, |d| {
        d.element(n.id).and_then(|e| e.attrs.iter().position(|a| a.ns == ns && a.name == local))
    })?;
    match i {
        Some(i) => remove_attr_at(vm, h, n, i),
        None => Ok(None),
    }
}

/// Set (Some) or remove (None) a plain attribute of element `n`.
pub fn set_or_remove(vm: &mut Vm, h: &Host, n: NodeRef, local: &str, value: Option<&str>) -> JsResult<()> {
    match value {
        Some(v) => set_attr(vm, h, n, Namespace::None, None, local, v),
        None => remove_attr(vm, h, n, Namespace::None, local).map(|_| ()),
    }
}

/// An attribute's value changed in place (an Attr node's `value`), as "change an attribute".
pub fn attr_value_changed(
    vm: &mut Vm,
    h: &Host,
    n: NodeRef,
    ns: Namespace,
    local: &str,
    old: String,
) -> JsResult<()> {
    handle_change(vm, h, n, ns, local, Some(old))
}

// ---- character data (DOM §4.10) ----

/// A character data node's data, as UTF-16.
pub fn data16(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<Vec<u16>> {
    let s = with_doc(vm, h, n.doc, |d| crate::dom::char_data(d, n.id).unwrap_or_default())?;
    Ok(s.encode_utf16().collect())
}

/// DOM §4.10 "replace data" of node `n`: `count` code units at `offset` become `data`.
pub fn replace_data(vm: &mut Vm, h: &Host, n: NodeRef, offset: u32, count: u32, data: &str) -> JsResult<()> {
    let cur = data16(vm, h, n)?;
    // Steps 1–3.
    let len = cur.len() as u32;
    if offset > len {
        return throw(vm, "IndexSizeError", "the offset is past the end of the data");
    }
    let count = count.min(len - offset);
    // Step 4.
    let old = String::from_utf16_lossy(&cur);
    queue_record(
        vm,
        h,
        Rec {
            kind: Kind::CharacterData,
            target: n,
            name: None,
            namespace: None,
            old: Some(old),
            added: Vec::new(),
            removed: Vec::new(),
            prev: None,
            next: None,
        },
    )?;
    // Steps 5–7.
    let mut out: Vec<u16> = cur[..offset as usize].to_vec();
    out.extend(data.encode_utf16());
    out.extend_from_slice(&cur[(offset + count) as usize..]);
    let s = String::from_utf16_lossy(&out);
    with_doc_mut(vm, h, n.doc, |d| {
        if matches!(
            d.node(n.id).data,
            NodeData::Text(_) | NodeData::Comment(_) | NodeData::ProcessingInstruction { .. }
        ) {
            d.set_text(n.id, &s);
        }
    })?;
    // Steps 8–11: live ranges.
    crate::range::replaced_data(h, n, offset, count, utf16_len(data) as u32);
    Ok(())
}

/// Set a character data node's whole data (the `data` and `nodeValue` setters, …).
pub fn set_data(vm: &mut Vm, h: &Host, n: NodeRef, data: &str) -> JsResult<()> {
    let len = data16(vm, h, n)?.len() as u32;
    replace_data(vm, h, n, 0, len, data)
}

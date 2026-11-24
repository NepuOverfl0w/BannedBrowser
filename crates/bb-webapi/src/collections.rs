//! Collections (DOM §4.2.10 NodeList, §4.2.10.2 HTMLCollection, §7.1 DOMTokenList) and the
//! legacy platform object hooks (WebIDL §3.9) that give them, NamedNodeMap and DOMStringMap
//! their indexed and named properties. Live collections are recomputed on each access.

use bb_dom::{Document, Namespace, NodeData, NodeId};
use bb_js::vm::heap::{NativeCall, ObjRef, Prop, Slot};
use bb_js::vm::host::HostExotic;
use bb_js::vm::object::PropDesc;
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};

use crate::class::{self, C};
use crate::exception::throw;
pub use crate::host::CollKind;
use crate::host::*;
use crate::idl::{attr, op};

/// The object for collection `kind`; `cache` = (tag, node) for [SameObject] attributes.
pub fn collection(vm: &mut Vm, h: &Host, kind: CollKind, cache: Option<(u8, u64)>) -> JsResult<Value> {
    if let Some(k) = cache {
        if let Some(o) = h.coll_cache.borrow().get(&k) {
            return Ok(Value::Object(*o));
        }
    }
    let class = match &kind {
        CollKind::ChildNodes(_) | CollKind::Static(_) => C::NodeList,
        CollKind::Attributes(_) => C::NamedNodeMap,
        CollKind::ClassList(_) => C::DOMTokenList,
        CollKind::Dataset(_) => C::DOMStringMap,
        CollKind::InlineStyle(_) | CollKind::Computed(..) => C::CSSStyleDeclaration,
        _ => C::HTMLCollection,
    };
    let id = {
        let mut cs = h.collections.borrow_mut();
        cs.push(kind);
        cs.len() as u64 - 1
    };
    let o = vm.new_host_object(Some(h.proto(class)), class as u32, id, true);
    if let Some(k) = cache {
        h.coll_cache.borrow_mut().insert(k, o);
    }
    Ok(Value::Object(o))
}

fn kind_of(h: &Host, id: u64) -> Option<CollKind> {
    h.collections.borrow().get(id as usize).cloned()
}

/// The nodes a node-collection lists, in tree order.
pub fn items(h: &Host, kind: &CollKind) -> Vec<NodeRef> {
    let docs = h.docs.borrow();
    let doc_of = |n: &NodeRef| docs.get(n.doc as usize);
    match kind {
        CollKind::ChildNodes(n) => {
            doc_of(n).map(|d| d.children(n.id).map(|k| n.with(k)).collect()).unwrap_or_default()
        }
        CollKind::WindowNamed(name) => {
            drop(docs);
            crate::window::named_objects(h, name).into_iter().map(|id| NodeRef { doc: 0, id }).collect()
        }
        CollKind::Children(n) => {
            doc_of(n).map(|d| d.element_children(n.id).map(|k| n.with(k)).collect()).unwrap_or_default()
        }
        CollKind::TagName(n, q) => {
            let Some(d) = doc_of(n) else { return Vec::new() };
            let html_doc = h.doc_types.borrow().get(n.doc as usize).map(String::as_str) == Some("text/html");
            let lower = q.to_ascii_lowercase();
            descendant_elements(d, n.id)
                .filter(|&x| {
                    if q == "*" {
                        return true;
                    }
                    let Some(e) = d.element(x) else { return false };
                    let qn = match &e.prefix {
                        Some(p) => format!("{p}:{}", e.name),
                        None => e.name.clone(),
                    };
                    if html_doc && e.ns == Namespace::Html {
                        qn == lower
                    } else {
                        qn == *q
                    }
                })
                .map(|x| n.with(x))
                .collect()
        }
        CollKind::TagNameNs(n, ns, local) => {
            let Some(d) = doc_of(n) else { return Vec::new() };
            descendant_elements(d, n.id)
                .filter(|&x| {
                    let Some(e) = d.element(x) else { return false };
                    let ns_ok = match ns {
                        None => true,
                        Some(None) => e.ns == Namespace::None,
                        Some(Some(u)) => e.ns.url() == u,
                    };
                    ns_ok && (local == "*" || e.name == *local)
                })
                .map(|x| n.with(x))
                .collect()
        }
        CollKind::ClassName(n, classes) => {
            let Some(d) = doc_of(n) else { return Vec::new() };
            if classes.is_empty() {
                return Vec::new();
            }
            let quirks = d.quirks_mode == bb_dom::QuirksMode::Quirks;
            descendant_elements(d, n.id)
                .filter(|&x| {
                    let Some(e) = d.element(x) else { return false };
                    classes.iter().all(|c| {
                        e.classes().any(|ec| if quirks { ec.eq_ignore_ascii_case(c) } else { ec == c })
                    })
                })
                .map(|x| n.with(x))
                .collect()
        }
        CollKind::Static(v) => v.clone(),
        CollKind::Options(n) => doc_of(n)
            .map(|d| d.list_of_options(n.id).into_iter().map(|k| n.with(k)).collect())
            .unwrap_or_default(),
        CollKind::FormElements(n) => doc_of(n)
            .map(|d| d.form_controls(n.id).into_iter().map(|k| n.with(k)).collect())
            .unwrap_or_default(),
        CollKind::Attributes(_)
        | CollKind::ClassList(_)
        | CollKind::Dataset(_)
        | CollKind::InlineStyle(_)
        | CollKind::Computed(..) => Vec::new(),
    }
}

fn descendant_elements(d: &Document, root: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    d.descendants(root).filter(move |&x| x != root && d.node(x).is_element())
}

/// The ordered set of `class` tokens (DOM §7.1 DOMTokenList, "ordered set parser").
pub fn tokens(h: &Host, n: NodeRef) -> Vec<String> {
    let docs = h.docs.borrow();
    let v = docs.get(n.doc as usize).and_then(|d| d.attr(n.id, "class")).unwrap_or("").to_string();
    let mut out: Vec<String> = Vec::new();
    for t in v.split_ascii_whitespace() {
        if !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// HTMLCollection's supported property names (DOM §4.2.10.2).
fn collection_names(h: &Host, nodes: &[NodeRef]) -> Vec<(String, NodeRef)> {
    let docs = h.docs.borrow();
    let mut out: Vec<(String, NodeRef)> = Vec::new();
    for n in nodes {
        let Some(e) = docs.get(n.doc as usize).and_then(|d| d.element(n.id)) else { continue };
        if let Some(id) = e.id().filter(|s| !s.is_empty()) {
            if !out.iter().any(|x| x.0 == id) {
                out.push((id.to_string(), *n));
            }
        }
        if e.ns == Namespace::Html {
            if let Some(nm) = e.attr("name").filter(|s| !s.is_empty()) {
                if !out.iter().any(|x| x.0 == nm) {
                    out.push((nm.to_string(), *n));
                }
            }
        }
    }
    out
}

fn this_coll(vm: &mut Vm, c: &NativeCall, iface: C) -> JsResult<(std::rc::Rc<Host>, CollKind)> {
    let (_, id) = this_of(vm, &c.this, iface)?;
    let h = host(vm)?;
    match kind_of(&h, id) {
        Some(k) => Ok((h, k)),
        None => vm.type_error("Illegal invocation"),
    }
}

/// Make @@iterator, forEach, entries, keys and values Array.prototype's (WebIDL §3.7.9.1).
fn array_iterable(vm: &mut Vm, proto: ObjRef, with_methods: bool) {
    let ap = vm.realm.array_proto;
    let values = vm.heap.obj(ap).props.get(&PropKey::lit("values")).and_then(|p| match &p.slot {
        Slot::Data(v) => Some(v.clone()),
        _ => None,
    });
    if let Some(v) = values {
        let k = PropKey::Sym(vm.realm.sym_iterator.clone());
        vm.heap.obj_mut(proto).props.insert(k, Prop::method(v));
    }
    if with_methods {
        for m in ["forEach", "entries", "keys", "values"] {
            let f = vm.heap.obj(ap).props.get(&PropKey::named(m)).and_then(|p| match &p.slot {
                Slot::Data(v) => Some(v.clone()),
                _ => None,
            });
            if let Some(f) = f {
                vm.heap.obj_mut(proto).props.insert(
                    PropKey::named(m),
                    Prop { slot: Slot::Data(f), writable: true, enumerable: true, configurable: true },
                );
            }
        }
    }
}

pub fn setup_node_list(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "length",
        |vm, c| {
            let (h, k) = this_coll(vm, &c, C::NodeList)?;
            Ok(Value::Number(items(&h, &k).len() as f64))
        },
        None,
    );
    op(vm, proto, "item", 1, |vm, c| {
        let i = to_ulong(vm, &c.arg(0))?;
        let (h, k) = this_coll(vm, &c, C::NodeList)?;
        let n = items(&h, &k).get(i as usize).copied();
        wrap_opt(vm, &h, n)
    });
    array_iterable(vm, proto, true);
}

pub fn setup_html_collection(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "length",
        |vm, c| {
            let (h, k) = this_coll(vm, &c, C::HTMLCollection)?;
            Ok(Value::Number(items(&h, &k).len() as f64))
        },
        None,
    );
    op(vm, proto, "item", 1, |vm, c| {
        let i = to_ulong(vm, &c.arg(0))?;
        let (h, k) = this_coll(vm, &c, C::HTMLCollection)?;
        let n = items(&h, &k).get(i as usize).copied();
        wrap_opt(vm, &h, n)
    });
    op(vm, proto, "namedItem", 1, |vm, c| {
        let name = rstr(vm, &c.arg(0))?;
        let (h, k) = this_coll(vm, &c, C::HTMLCollection)?;
        if name.is_empty() {
            return Ok(Value::Null);
        }
        let list = items(&h, &k);
        let n = collection_names(&h, &list).into_iter().find(|x| x.0 == name).map(|x| x.1);
        wrap_opt(vm, &h, n)
    });
    array_iterable(vm, proto, false);
}

// ---- DOMTokenList ----

fn token_this(vm: &mut Vm, c: &NativeCall) -> JsResult<(std::rc::Rc<Host>, NodeRef)> {
    match this_coll(vm, c, C::DOMTokenList)? {
        (h, CollKind::ClassList(n)) => Ok((h, n)),
        _ => vm.type_error("Illegal invocation"),
    }
}

fn set_tokens(vm: &mut Vm, h: &Host, n: NodeRef, toks: &[String]) -> JsResult<()> {
    let v = toks.join(" ");
    crate::mutation::set_or_remove(vm, h, n, "class", Some(&v))
}

/// DOM §7.1 "validate a token".
fn check_token(vm: &mut Vm, t: &str) -> JsResult<()> {
    if t.is_empty() {
        return throw(vm, "SyntaxError", "the token is empty");
    }
    if t.chars().any(|c| c.is_ascii_whitespace()) {
        return throw(vm, "InvalidCharacterError", "the token contains whitespace");
    }
    Ok(())
}

fn args_tokens(vm: &mut Vm, c: &NativeCall) -> JsResult<Vec<String>> {
    let mut out = Vec::new();
    for a in &c.args {
        out.push(rstr(vm, a)?);
    }
    Ok(out)
}

pub fn setup_token_list(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "length",
        |vm, c| {
            let (h, n) = token_this(vm, &c)?;
            Ok(Value::Number(tokens(&h, n).len() as f64))
        },
        None,
    );
    op(vm, proto, "item", 1, |vm, c| {
        let i = to_ulong(vm, &c.arg(0))?;
        let (h, n) = token_this(vm, &c)?;
        Ok(tokens(&h, n).get(i as usize).map_or(Value::Null, |s| js(s)))
    });
    op(vm, proto, "contains", 1, |vm, c| {
        let t = rstr(vm, &c.arg(0))?;
        let (h, n) = token_this(vm, &c)?;
        Ok(Value::Bool(tokens(&h, n).contains(&t)))
    });
    op(vm, proto, "add", 0, |vm, c| {
        let ts = args_tokens(vm, &c)?;
        let (h, n) = token_this(vm, &c)?;
        for t in &ts {
            check_token(vm, t)?;
        }
        let mut cur = tokens(&h, n);
        for t in ts {
            if !cur.contains(&t) {
                cur.push(t);
            }
        }
        set_tokens(vm, &h, n, &cur)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "remove", 0, |vm, c| {
        let ts = args_tokens(vm, &c)?;
        let (h, n) = token_this(vm, &c)?;
        for t in &ts {
            check_token(vm, t)?;
        }
        let mut cur = tokens(&h, n);
        cur.retain(|x| !ts.contains(x));
        let has = with_doc(vm, &h, n.doc, |d| d.attr(n.id, "class").is_some())?;
        if has {
            set_tokens(vm, &h, n, &cur)?;
        }
        Ok(Value::Undefined)
    });
    op(vm, proto, "toggle", 1, |vm, c| {
        let t = rstr(vm, &c.arg(0))?;
        let force = match c.arg(1) {
            Value::Undefined => None,
            v => Some(vm.to_boolean(&v)),
        };
        let (h, n) = token_this(vm, &c)?;
        check_token(vm, &t)?;
        let mut cur = tokens(&h, n);
        if cur.contains(&t) {
            if force != Some(true) {
                cur.retain(|x| *x != t);
                set_tokens(vm, &h, n, &cur)?;
                return Ok(Value::Bool(false));
            }
            return Ok(Value::Bool(true));
        }
        if force == Some(false) {
            return Ok(Value::Bool(false));
        }
        cur.push(t);
        set_tokens(vm, &h, n, &cur)?;
        Ok(Value::Bool(true))
    });
    op(vm, proto, "replace", 2, |vm, c| {
        let t = rstr(vm, &c.arg(0))?;
        let nt = rstr(vm, &c.arg(1))?;
        let (h, n) = token_this(vm, &c)?;
        // DOM §7.1 replace: both empty checks come before the whitespace checks.
        if t.is_empty() || nt.is_empty() {
            return throw(vm, "SyntaxError", "the token is empty");
        }
        check_token(vm, &t)?;
        check_token(vm, &nt)?;
        let cur = tokens(&h, n);
        if !cur.contains(&t) {
            return Ok(Value::Bool(false));
        }
        let mut out: Vec<String> = Vec::new();
        for x in cur {
            let v = if x == t || x == nt { nt.clone() } else { x };
            if !out.contains(&v) {
                out.push(v);
            }
        }
        set_tokens(vm, &h, n, &out)?;
        Ok(Value::Bool(true))
    });
    op(vm, proto, "supports", 1, |vm, c| {
        token_this(vm, &c)?;
        vm.type_error("classList has no supported tokens")
    });
    attr(
        vm,
        proto,
        "value",
        |vm, c| {
            let (h, n) = token_this(vm, &c)?;
            let v = with_doc(vm, &h, n.doc, |d| d.attr(n.id, "class").unwrap_or("").to_string())?;
            Ok(js(&v))
        },
        Some(|vm, c| {
            let v = rstr(vm, &c.arg(0))?;
            let (h, n) = token_this(vm, &c)?;
            crate::mutation::set_or_remove(vm, &h, n, "class", Some(&v))?;
            Ok(Value::Undefined)
        }),
    );
    // stringifier
    op(vm, proto, "toString", 0, |vm, c| {
        let (h, n) = token_this(vm, &c)?;
        let v = with_doc(vm, &h, n.doc, |d| d.attr(n.id, "class").unwrap_or("").to_string())?;
        Ok(js(&v))
    });
    array_iterable(vm, proto, true);
}

// ---- the legacy platform object hooks ----

pub struct Exotic;

/// Whether named property `name` is visible on `o` (WebIDL §3.9 "named property visibility
/// algorithm", without [LegacyOverrideBuiltIns]).
fn named_visible(vm: &mut Vm, o: ObjRef, k: &PropKey) -> JsResult<bool> {
    if vm.heap.obj(o).props.get(k).is_some() {
        return Ok(false);
    }
    match vm.heap.obj(o).proto {
        Some(p) => Ok(!vm.has_property(p, k)?),
        None => Ok(true),
    }
}

fn index_desc(v: Value) -> PropDesc {
    PropDesc::data(v, false, true, true)
}

impl Exotic {
    /// (indexed items, named items, named enumerable) for object `o` of `class`.
    fn indexed_len(&self, h: &Host, class: C, kind: &CollKind) -> usize {
        match class {
            C::NodeList | C::HTMLCollection => items(h, kind).len(),
            C::DOMTokenList => match kind {
                CollKind::ClassList(n) => tokens(h, *n).len(),
                _ => 0,
            },
            C::NamedNodeMap => crate::attr::map_len(h, kind),
            C::CSSStyleDeclaration => crate::css::decl_names(h, kind).len(),
            _ => 0,
        }
    }

    fn indexed_get(
        &self,
        vm: &mut Vm,
        h: &Host,
        class: C,
        kind: &CollKind,
        i: usize,
    ) -> JsResult<Option<Value>> {
        Ok(match class {
            C::NodeList | C::HTMLCollection => match items(h, kind).get(i) {
                Some(n) => Some(wrap(vm, h, *n)?),
                None => None,
            },
            C::DOMTokenList => match kind {
                CollKind::ClassList(n) => tokens(h, *n).get(i).map(|s| js(s)),
                _ => None,
            },
            C::NamedNodeMap => crate::attr::map_item(vm, h, kind, i)?,
            C::CSSStyleDeclaration => crate::css::decl_names(h, kind).get(i).map(|s| js(s)),
            _ => None,
        })
    }

    /// The supported property names and their values.
    fn names(&self, vm: &mut Vm, h: &Host, class: C, kind: &CollKind) -> JsResult<Vec<(String, Value)>> {
        Ok(match class {
            C::HTMLCollection => {
                let list = items(h, kind);
                let mut out = Vec::new();
                for (name, n) in collection_names(h, &list) {
                    let v = wrap(vm, h, n)?;
                    out.push((name, v));
                }
                out
            }
            C::NamedNodeMap => crate::attr::map_names(vm, h, kind)?,
            C::DOMStringMap => {
                crate::html::dataset_names(h, kind).into_iter().map(|(k, v)| (k, js(&v))).collect()
            }
            _ => Vec::new(),
        })
    }
}

fn setup(vm: &mut Vm, class: u32, id: u64) -> JsResult<Option<(std::rc::Rc<Host>, C, CollKind)>> {
    let Some(c) = class::from_u32(class) else { return Ok(None) };
    let h = host(vm)?;
    if c == C::Storage {
        // Storage's payload is its area (0 local, 1 session), not a collection.
        return Ok(Some((h, c, CollKind::Static(Vec::new()))));
    }
    let Some(k) = kind_of(&h, id) else { return Ok(None) };
    Ok(Some((h, c, k)))
}

impl HostExotic for Exotic {
    fn get_own(
        &self,
        vm: &mut Vm,
        o: ObjRef,
        class: u32,
        id: u64,
        k: &PropKey,
    ) -> JsResult<Option<PropDesc>> {
        if class == crate::window::WINDOW_PROPERTIES {
            return crate::window::properties_get_own(vm, k);
        }
        if class == C::DOMRectList as u32 {
            let PropKey::Index(i) = k else { return Ok(None) };
            return Ok(crate::css::rect_list_get(vm, o, *i).map(index_desc));
        }
        if class == crate::frames::CROSS_ORIGIN_WINDOW || class == crate::frames::CROSS_ORIGIN_LOCATION {
            return crate::frames::exotic_get_own(vm, o, class, k);
        }
        let Some((h, c, kind)) = setup(vm, class, id)? else { return Ok(None) };
        if c == C::Storage {
            if matches!(k, PropKey::Sym(_)) {
                return Ok(None);
            }
            let name = k.display();
            let Some((_, v)) = crate::storage::names(&h, id).into_iter().find(|x| x.0 == name) else {
                return Ok(None);
            };
            if !named_visible(vm, o, k)? {
                return Ok(None);
            }
            return Ok(Some(PropDesc::data(js(&v), true, true, true)));
        }
        if let PropKey::Index(i) = k {
            if c != C::DOMStringMap {
                return Ok(self.indexed_get(vm, &h, c, &kind, *i as usize)?.map(index_desc));
            }
        }
        if matches!(k, PropKey::Sym(_)) {
            return Ok(None);
        }
        let name = k.display();
        let names = self.names(vm, &h, c, &kind)?;
        let Some((_, v)) = names.into_iter().find(|x| x.0 == name) else { return Ok(None) };
        // DOMStringMap has [LegacyOverrideBuiltIns]: its names win over the prototype's.
        if c != C::DOMStringMap && !named_visible(vm, o, k)? {
            return Ok(None);
        }
        let (writable, enumerable) = match c {
            C::DOMStringMap => (true, true),
            _ => (false, false),
        };
        Ok(Some(PropDesc::data(v, writable, enumerable, true)))
    }

    fn own_keys(&self, vm: &mut Vm, o: ObjRef, class: u32, id: u64) -> JsResult<Vec<PropKey>> {
        if class == crate::window::WINDOW_PROPERTIES {
            return crate::window::properties_keys(vm);
        }
        if class == C::DOMRectList as u32 {
            return Ok((0..crate::css::rect_list_len(vm, o) as u32).map(PropKey::Index).collect());
        }
        if class == crate::frames::CROSS_ORIGIN_WINDOW || class == crate::frames::CROSS_ORIGIN_LOCATION {
            return crate::frames::exotic_keys(vm, o, class);
        }
        let Some((h, c, kind)) = setup(vm, class, id)? else { return Ok(Vec::new()) };
        if c == C::Storage {
            let mut keys = Vec::new();
            for (name, _) in crate::storage::names(&h, id) {
                let k = PropKey::named(&name);
                if named_visible(vm, o, &k)? {
                    keys.push(k);
                }
            }
            return Ok(keys);
        }
        let mut keys: Vec<PropKey> = (0..self.indexed_len(&h, c, &kind) as u32).map(PropKey::Index).collect();
        for (name, _) in self.names(vm, &h, c, &kind)? {
            let k = PropKey::named(&name);
            if c == C::DOMStringMap || named_visible(vm, o, &k)? {
                keys.push(k);
            }
        }
        Ok(keys)
    }

    fn define(
        &self,
        vm: &mut Vm,
        o: ObjRef,
        class: u32,
        id: u64,
        k: &PropKey,
        d: &PropDesc,
    ) -> JsResult<Option<bool>> {
        // WebIDL §3.7.4.2: the named properties object refuses definitions.
        if class == crate::window::WINDOW_PROPERTIES {
            return Ok(Some(false));
        }
        if class == crate::frames::CROSS_ORIGIN_WINDOW || class == crate::frames::CROSS_ORIGIN_LOCATION {
            return crate::frames::exotic_refuse(vm);
        }
        if class == C::DOMRectList as u32 {
            return Ok(if matches!(k, PropKey::Index(_)) { Some(false) } else { None });
        }
        let Some((h, c, kind)) = setup(vm, class, id)? else { return Ok(None) };
        if c == C::Storage {
            return crate::storage::define(vm, &h, id, k, d);
        }
        if c == C::DOMStringMap {
            return crate::html::dataset_define(vm, &h, &kind, k, d);
        }
        if matches!(k, PropKey::Index(_)) {
            // No indexed setters: indices can't be defined (WebIDL §3.9.3 step 1).
            return Ok(Some(false));
        }
        if matches!(k, PropKey::Sym(_)) {
            return Ok(None);
        }
        let name = k.display();
        let supported = self.names(vm, &h, c, &kind)?.iter().any(|x| x.0 == name);
        if supported && vm.heap.obj(o).props.get(k).is_none() {
            return Ok(Some(false));
        }
        Ok(None)
    }

    fn delete(&self, vm: &mut Vm, o: ObjRef, class: u32, id: u64, k: &PropKey) -> JsResult<Option<bool>> {
        // WebIDL §3.7.4.3.
        if class == crate::window::WINDOW_PROPERTIES {
            return Ok(Some(false));
        }
        if class == crate::frames::CROSS_ORIGIN_WINDOW || class == crate::frames::CROSS_ORIGIN_LOCATION {
            return crate::frames::exotic_refuse(vm);
        }
        if class == C::DOMRectList as u32 {
            return Ok(match k {
                PropKey::Index(i) => Some(*i as usize >= crate::css::rect_list_len(vm, o)),
                _ => None,
            });
        }
        let Some((h, c, kind)) = setup(vm, class, id)? else { return Ok(None) };
        if c == C::Storage {
            return crate::storage::delete(vm, &h, id, k);
        }
        if c == C::DOMStringMap {
            return crate::html::dataset_delete(vm, &h, &kind, k);
        }
        if let PropKey::Index(i) = k {
            return Ok(Some((*i as usize) >= self.indexed_len(&h, c, &kind)));
        }
        if matches!(k, PropKey::Sym(_)) {
            return Ok(None);
        }
        let name = k.display();
        if self.names(vm, &h, c, &kind)?.iter().any(|x| x.0 == name) && named_visible(vm, o, k)? {
            return Ok(Some(false));
        }
        Ok(None)
    }

    fn set(
        &self,
        vm: &mut Vm,
        o: ObjRef,
        class: u32,
        id: u64,
        k: &PropKey,
        v: &Value,
    ) -> JsResult<Option<bool>> {
        if class == crate::frames::CROSS_ORIGIN_WINDOW || class == crate::frames::CROSS_ORIGIN_LOCATION {
            return crate::frames::exotic_set(vm, class, k);
        }
        if class == C::DOMRectList as u32 {
            // No indexed setter: indices can't be set (WebIDL §3.9.2).
            return Ok(if matches!(k, PropKey::Index(_)) { Some(false) } else { None });
        }
        let Some((h, c, kind)) = setup(vm, class, id)? else { return Ok(None) };
        if c == C::Storage {
            if matches!(k, PropKey::Sym(_)) {
                return Ok(None);
            }
            // The named setter, unless the object or its prototype chain has the property
            // (WebIDL §3.9.2 via LegacyPlatformObjectGetOwnProperty without named props).
            if vm.heap.obj(o).props.get(k).is_none() {
                let proto_has = match vm.heap.obj(o).proto {
                    Some(p) => vm.has_property(p, k)?,
                    None => false,
                };
                if !proto_has {
                    let s = rstr(vm, v)?;
                    crate::storage::set_item(vm, &h, id == 0, &k.display(), &s)?;
                    return Ok(Some(true));
                }
            }
            return Ok(None);
        }
        if c == C::DOMStringMap {
            if let PropKey::Str(_) = k {
                let s = rstr(vm, v)?;
                return crate::html::dataset_set(vm, &h, &kind, &k.display(), &s).map(Some);
            }
        }
        Ok(None)
    }
}

/// Whether node `id` is an element of `d` (helper for other modules).
pub fn is_element(d: &Document, id: NodeId) -> bool {
    matches!(d.node(id).data, NodeData::Element(_))
}

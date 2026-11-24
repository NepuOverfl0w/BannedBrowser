//! URL and URLSearchParams (URL Standard §6), over bb-url.
//!
//! A URL object keeps its serialization in slot 0 and its URLSearchParams in slot 1; a
//! URLSearchParams keeps its list, serialized as application/x-www-form-urlencoded (which
//! round-trips every name and value), in slot 0 and its URL object (or undefined) in slot 1.

use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::{realm, PropKey};
use bb_js::{JsResult, Value, Vm};
use bb_url::Url;

use crate::class::C;
use crate::host::*;
use crate::idl::{attr, attr_with, callee_slots, op};

type R = JsResult<Value>;

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

fn obj_of(vm: &mut Vm, v: &Value, iface: C) -> JsResult<ObjRef> {
    this_of(vm, v, iface)?;
    match v {
        Value::Object(o) => Ok(*o),
        _ => vm.type_error("Illegal invocation"),
    }
}

fn url_of(vm: &mut Vm, o: ObjRef) -> JsResult<Url> {
    let href = rstr(vm, &slot(vm, o, 0))?;
    match Url::parse(&href) {
        Ok(u) => Ok(u),
        Err(_) => vm.type_error("invalid URL state"),
    }
}

/// Store a URL object's new URL; its searchParams' list follows the query (URL §6.1
/// "update the URLSearchParams").
fn store_url(vm: &mut Vm, o: ObjRef, u: &Url) {
    set_slot(vm, o, 0, js(&u.href()));
    if let Value::Object(p) = slot(vm, o, 1) {
        let q = u.query().unwrap_or("").to_string();
        let list = bb_url::form_urldecode(&q);
        set_slot(vm, p, 0, js(&bb_url::form_urlencode(&list)));
    }
}

/// The URL Standard's API URL parser: (url, base) → URL, or None.
fn api_parse(vm: &mut Vm, url: &Value, base: &Value) -> JsResult<Option<Url>> {
    let u = rstr(vm, url)?;
    let base = match base {
        Value::Undefined => None,
        b => {
            let b = rstr(vm, b)?;
            match Url::parse(&b) {
                Ok(b) => Some(b),
                Err(_) => return Ok(None),
            }
        }
    };
    Ok(Url::parse_with_base(&u, base.as_ref()).ok())
}

fn new_url(vm: &mut Vm, h: &Host, proto: Option<ObjRef>, u: &Url) -> ObjRef {
    let o = vm.new_host_object(Some(proto.unwrap_or_else(|| h.proto(C::URL))), C::URL as u32, 0, false);
    let params = vm.new_host_object(Some(h.proto(C::URLSearchParams)), C::URLSearchParams as u32, 0, false);
    let q = bb_url::form_urldecode(u.query().unwrap_or(""));
    if let Some(ho) = vm.host_obj_mut(params) {
        ho.slots = vec![js(&bb_url::form_urlencode(&q)), Value::Object(o)];
    }
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = vec![js(&u.href()), Value::Object(params)];
    }
    o
}

pub fn url_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("URL requires 'new'") };
    let Some(u) = api_parse(vm, &c.arg(0), &c.arg(1))? else {
        let s = rstr(vm, &c.arg(0))?;
        return vm.type_error(&format!("'{s}' is not a valid URL"));
    };
    let h = host(vm)?;
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => Some(p),
        _ => None,
    };
    Ok(Value::Object(new_url(vm, &h, proto, &u)))
}

fn url_part(vm: &mut Vm, c: NativeCall) -> R {
    let o = obj_of(vm, &c.this, C::URL)?;
    let part = match callee_slots(vm, c.callee).first() {
        Some(v) => rstr(vm, v)?,
        None => String::new(),
    };
    let u = url_of(vm, o)?;
    let s = match part.as_str() {
        "href" => u.href(),
        "origin" => u.origin().serialize(),
        "protocol" => u.protocol(),
        "username" => u.username().to_string(),
        "password" => u.password().to_string(),
        "host" => u.host_with_port(),
        "hostname" => u.hostname(),
        "port" => u.port().map(|p| p.to_string()).unwrap_or_default(),
        "pathname" => u.pathname(),
        "search" => u.search(),
        "hash" => u.hash(),
        _ => String::new(),
    };
    Ok(js(&s))
}

fn url_set(vm: &mut Vm, c: NativeCall) -> R {
    let o = obj_of(vm, &c.this, C::URL)?;
    let part = match callee_slots(vm, c.callee).first() {
        Some(v) => rstr(vm, v)?,
        None => String::new(),
    };
    let v = rstr(vm, &c.arg(0))?;
    let mut u = url_of(vm, o)?;
    match part.as_str() {
        "href" => {
            if u.set_href(&v).is_err() {
                return vm.type_error(&format!("'{v}' is not a valid URL"));
            }
        }
        "protocol" => u.set_protocol(&v),
        "username" => u.set_username(&v),
        "password" => u.set_password(&v),
        "host" => u.set_host(&v),
        "hostname" => u.set_hostname(&v),
        "port" => u.set_port(&v),
        "pathname" => u.set_pathname(&v),
        "search" => u.set_search(&v),
        "hash" => u.set_hash(&v),
        _ => {}
    }
    store_url(vm, o, &u);
    Ok(Value::Undefined)
}

pub fn setup_url(vm: &mut Vm, proto: ObjRef, ctor: ObjRef) {
    for part in
        ["href", "protocol", "username", "password", "host", "hostname", "port", "pathname", "search", "hash"]
    {
        attr_with(vm, proto, part, url_part, Some(url_set), vec![js(part)]);
    }
    attr_with(vm, proto, "origin", url_part, None, vec![js("origin")]);
    attr(
        vm,
        proto,
        "searchParams",
        |vm, c| {
            let o = obj_of(vm, &c.this, C::URL)?;
            Ok(slot(vm, o, 1))
        },
        None,
    );
    for name in ["toString", "toJSON"] {
        let f = realm::native_with(vm, name, 0, url_part, false, vec![js("href")]);
        vm.heap.obj_mut(proto).props.insert(
            PropKey::named(name),
            bb_js::vm::heap::Prop {
                slot: bb_js::vm::heap::Slot::Data(Value::Object(f)),
                writable: true,
                enumerable: true,
                configurable: true,
            },
        );
    }
    op(vm, ctor, "canParse", 1, |vm, c| Ok(Value::Bool(api_parse(vm, &c.arg(0), &c.arg(1))?.is_some())));
    op(vm, ctor, "parse", 1, |vm, c| {
        let Some(u) = api_parse(vm, &c.arg(0), &c.arg(1))? else { return Ok(Value::Null) };
        let h = host(vm)?;
        Ok(Value::Object(new_url(vm, &h, None, &u)))
    });
}

// ---- URLSearchParams ----

fn list(vm: &mut Vm, o: ObjRef) -> JsResult<Vec<(String, String)>> {
    let s = rstr(vm, &slot(vm, o, 0))?;
    Ok(bb_url::form_urldecode(&s))
}

/// Store the list; the associated URL's query follows (URL §6.2 "update").
fn store_list(vm: &mut Vm, o: ObjRef, l: &[(String, String)]) -> JsResult<()> {
    let s = bb_url::form_urlencode(l);
    set_slot(vm, o, 0, js(&s));
    if let Value::Object(u) = slot(vm, o, 1) {
        let mut url = url_of(vm, u)?;
        url.set_query_encoded(if s.is_empty() { None } else { Some(&s) });
        set_slot(vm, u, 0, js(&url.href()));
    }
    Ok(())
}

fn params(vm: &mut Vm, c: &NativeCall) -> JsResult<(ObjRef, Vec<(String, String)>)> {
    let o = obj_of(vm, &c.this, C::URLSearchParams)?;
    let l = list(vm, o)?;
    Ok((o, l))
}

pub fn params_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("URLSearchParams requires 'new'") };
    let init = c.arg(0);
    let mut l: Vec<(String, String)> = Vec::new();
    match &init {
        Value::Undefined | Value::Null if matches!(init, Value::Undefined) => {}
        Value::Object(o) => {
            let it_key = PropKey::Sym(vm.realm.sym_iterator.clone());
            let iter = vm.get(*o, &it_key)?;
            if !iter.is_nullish() {
                // sequence<sequence<USVString>>
                let outer = iter_values(vm, &init, &iter)?;
                for pair in outer {
                    let Value::Object(po) = &pair else {
                        return vm.type_error("each pair must be a sequence");
                    };
                    let it = vm.get(*po, &it_key)?;
                    let inner = iter_values(vm, &pair, &it)?;
                    if inner.len() != 2 {
                        return vm.type_error("each pair must have exactly two items");
                    }
                    let (k, v) = (rstr(vm, &inner[0])?, rstr(vm, &inner[1])?);
                    l.push((k, v));
                }
            } else {
                // record<USVString, USVString>
                for k in vm.own_property_keys(*o)? {
                    if matches!(k, PropKey::Sym(_)) {
                        continue;
                    }
                    let Some(d) = vm.get_own_property(*o, &k)? else { continue };
                    if d.enumerable != Some(true) {
                        continue;
                    }
                    let v = vm.get(*o, &k)?;
                    let v = rstr(vm, &v)?;
                    l.push((k.display(), v));
                }
            }
        }
        v => {
            let s = rstr(vm, v)?;
            l = bb_url::form_urldecode(s.strip_prefix('?').unwrap_or(&s));
        }
    }
    let h = host(vm)?;
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => p,
        _ => h.proto(C::URLSearchParams),
    };
    let o = vm.new_host_object(Some(proto), C::URLSearchParams as u32, 0, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = vec![js(&bb_url::form_urlencode(&l)), Value::Undefined];
    }
    Ok(Value::Object(o))
}

/// Iterate `v` with its @@iterator `f` (WebIDL sequence conversion).
pub(crate) fn iter_values(vm: &mut Vm, v: &Value, f: &Value) -> JsResult<Vec<Value>> {
    if !vm.is_callable(f) {
        return vm.type_error("not iterable");
    }
    let it = vm.call(f, v.clone(), Vec::new())?;
    let Value::Object(it) = it else { return vm.type_error("the iterator is not an object") };
    let next = vm.get(it, &PropKey::lit("next"))?;
    let mut out = Vec::new();
    loop {
        let r = vm.call(&next, Value::Object(it), Vec::new())?;
        let Value::Object(r) = r else { return vm.type_error("the iterator result is not an object") };
        let done = vm.get(r, &PropKey::lit("done"))?;
        if vm.to_boolean(&done) {
            break;
        }
        out.push(vm.get(r, &PropKey::lit("value"))?);
    }
    Ok(out)
}

fn kv_arg(vm: &mut Vm, c: &NativeCall) -> JsResult<(String, Option<String>)> {
    let k = rstr(vm, &c.arg(0))?;
    let v = match c.arg(1) {
        Value::Undefined => None,
        v => Some(rstr(vm, &v)?),
    };
    Ok((k, v))
}

pub fn setup_params(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "size",
        |vm, c| {
            let (_, l) = params(vm, &c)?;
            Ok(Value::Number(l.len() as f64))
        },
        None,
    );
    op(vm, proto, "append", 2, |vm, c| {
        let k = rstr(vm, &c.arg(0))?;
        let v = rstr(vm, &c.arg(1))?;
        let (o, mut l) = params(vm, &c)?;
        l.push((k, v));
        store_list(vm, o, &l)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "delete", 1, |vm, c| {
        let (k, v) = kv_arg(vm, &c)?;
        let (o, mut l) = params(vm, &c)?;
        l.retain(|(n, x)| !(n == &k && v.as_ref().is_none_or(|v| v == x)));
        store_list(vm, o, &l)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "get", 1, |vm, c| {
        let k = rstr(vm, &c.arg(0))?;
        let (_, l) = params(vm, &c)?;
        Ok(l.into_iter().find(|(n, _)| *n == k).map_or(Value::Null, |(_, v)| js(&v)))
    });
    op(vm, proto, "getAll", 1, |vm, c| {
        let k = rstr(vm, &c.arg(0))?;
        let (_, l) = params(vm, &c)?;
        let vals = l.into_iter().filter(|(n, _)| *n == k).map(|(_, v)| js(&v)).collect();
        Ok(Value::Object(vm.new_array(vals)))
    });
    op(vm, proto, "has", 1, |vm, c| {
        let (k, v) = kv_arg(vm, &c)?;
        let (_, l) = params(vm, &c)?;
        Ok(Value::Bool(l.iter().any(|(n, x)| n == &k && v.as_ref().is_none_or(|v| v == x))))
    });
    op(vm, proto, "set", 2, |vm, c| {
        let k = rstr(vm, &c.arg(0))?;
        let v = rstr(vm, &c.arg(1))?;
        let (o, l) = params(vm, &c)?;
        let mut out: Vec<(String, String)> = Vec::new();
        let mut done = false;
        for (n, x) in l {
            if n == k {
                if !done {
                    out.push((n, v.clone()));
                    done = true;
                }
            } else {
                out.push((n, x));
            }
        }
        if !done {
            out.push((k, v));
        }
        store_list(vm, o, &out)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "sort", 0, |vm, c| {
        let (o, mut l) = params(vm, &c)?;
        // A stable sort by name in UTF-16 code unit order (URL §6.2).
        l.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
        store_list(vm, o, &l)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "toString", 0, |vm, c| {
        let o = obj_of(vm, &c.this, C::URLSearchParams)?;
        Ok(slot(vm, o, 0))
    });
    // iterable<USVString, USVString>: iterators over a snapshot of the list.
    for (name, kind) in [("entries", 0.0), ("keys", 1.0), ("values", 2.0)] {
        let f = realm::native_with(vm, name, 0, params_iter, false, vec![Value::Number(kind)]);
        vm.heap.obj_mut(proto).props.insert(
            PropKey::named(name),
            bb_js::vm::heap::Prop {
                slot: bb_js::vm::heap::Slot::Data(Value::Object(f)),
                writable: true,
                enumerable: true,
                configurable: true,
            },
        );
        if name == "entries" {
            let k = PropKey::Sym(vm.realm.sym_iterator.clone());
            vm.heap.obj_mut(proto).props.insert(k, bb_js::vm::heap::Prop::method(Value::Object(f)));
        }
    }
    op(vm, proto, "forEach", 1, |vm, c| {
        let f = c.arg(0);
        if !vm.is_callable(&f) {
            return vm.type_error("forEach needs a function");
        }
        let (_, l) = params(vm, &c)?;
        for (k, v) in l {
            vm.call(&f, c.arg(1), vec![js(&v), js(&k), c.this.clone()])?;
        }
        Ok(Value::Undefined)
    });
}

fn params_iter(vm: &mut Vm, c: NativeCall) -> R {
    let kind = match callee_slots(vm, c.callee).first() {
        Some(Value::Number(n)) => *n as u8,
        _ => 0,
    };
    let (_, l) = params(vm, &c)?;
    let mut items = Vec::with_capacity(l.len());
    for (k, v) in l {
        items.push(match kind {
            1 => js(&k),
            2 => js(&v),
            _ => Value::Object(vm.new_array(vec![js(&k), js(&v)])),
        });
    }
    let a = vm.new_array(items);
    let ap = vm.realm.array_proto;
    let values = vm.get(ap, &PropKey::lit("values"))?;
    vm.call(&values, Value::Object(a), Vec::new())
}

//! Web storage (HTML §12.2: Storage, localStorage, sessionStorage) and `document.cookie`
//! (HTML §3.1.5). localStorage areas belong to the embedder (shared by the client's pages,
//! saved with the profile); sessionStorage lives with the window. Storage objects are legacy
//! platform objects: their keys are named properties (the hooks in collections.rs).

use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::object::PropDesc;
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::exception::throw;
use crate::host::*;
use crate::idl::{attr, op};

/// HTML §12.2.1: an area holds at most this many UTF-16 code units of keys and values.
const QUOTA: usize = 5_000_000;

fn is_local(id: u64) -> bool {
    id == 0
}

pub fn area(h: &Host, local: bool) -> Vec<(String, String)> {
    if local && h.embedder.has_storage() {
        h.embedder.storage_area()
    } else if local {
        h.local_mem.borrow().clone()
    } else {
        h.session.borrow().clone()
    }
}

fn store(vm: &mut Vm, h: &Host, local: bool, a: Vec<(String, String)>) -> JsResult<()> {
    let size: usize = a.iter().map(|(k, v)| utf16_len(k) + utf16_len(v)).sum();
    if size > QUOTA {
        return throw(vm, "QuotaExceededError", "the storage quota is exceeded");
    }
    if local && h.embedder.has_storage() {
        h.embedder.set_storage_area(a);
    } else if local {
        *h.local_mem.borrow_mut() = a;
    } else {
        *h.session.borrow_mut() = a;
    }
    Ok(())
}

/// The Storage object (`id`: 0 local, 1 session), made once.
fn storage_object(vm: &mut Vm, local: bool) -> JsResult<Value> {
    let h = host(vm)?;
    // Opaque origins (about:blank, data:, file: in some browsers) can't have storage; we
    // allow file: pages theirs, like the major engines.
    let key = if local { "localStorage" } else { "sessionStorage" };
    if let Some(o) = h.misc.borrow().get(key) {
        return Ok(Value::Object(*o));
    }
    let o = vm.new_host_object(Some(h.proto(C::Storage)), C::Storage as u32, if local { 0 } else { 1 }, true);
    h.misc.borrow_mut().insert(key, o);
    Ok(Value::Object(o))
}

fn this_area(vm: &mut Vm, c: &NativeCall) -> JsResult<(std::rc::Rc<Host>, bool)> {
    let (_, id) = this_of(vm, &c.this, C::Storage)?;
    Ok((host(vm)?, is_local(id)))
}

pub fn set_item(vm: &mut Vm, h: &Host, local: bool, k: &str, v: &str) -> JsResult<()> {
    let mut a = area(h, local);
    match a.iter_mut().find(|x| x.0 == k) {
        Some(x) if x.1 == v => return Ok(()),
        Some(x) => x.1 = v.to_string(),
        None => a.push((k.to_string(), v.to_string())),
    }
    store(vm, h, local, a)
}

pub fn remove_item(vm: &mut Vm, h: &Host, local: bool, k: &str) -> JsResult<()> {
    let mut a = area(h, local);
    let n = a.len();
    a.retain(|x| x.0 != k);
    if a.len() != n {
        store(vm, h, local, a)?;
    }
    Ok(())
}

pub fn setup_storage(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "length",
        |vm, c| {
            let (h, l) = this_area(vm, &c)?;
            Ok(Value::Number(area(&h, l).len() as f64))
        },
        None,
    );
    op(vm, proto, "key", 1, |vm, c| {
        let i = to_ulong(vm, &c.arg(0))?;
        let (h, l) = this_area(vm, &c)?;
        Ok(area(&h, l).get(i as usize).map_or(Value::Null, |x| js(&x.0)))
    });
    op(vm, proto, "getItem", 1, |vm, c| {
        let k = rstr(vm, &c.arg(0))?;
        let (h, l) = this_area(vm, &c)?;
        Ok(area(&h, l).into_iter().find(|x| x.0 == k).map_or(Value::Null, |x| js(&x.1)))
    });
    op(vm, proto, "setItem", 2, |vm, c| {
        let k = rstr(vm, &c.arg(0))?;
        let v = rstr(vm, &c.arg(1))?;
        let (h, l) = this_area(vm, &c)?;
        set_item(vm, &h, l, &k, &v)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "removeItem", 1, |vm, c| {
        let k = rstr(vm, &c.arg(0))?;
        let (h, l) = this_area(vm, &c)?;
        remove_item(vm, &h, l, &k)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "clear", 0, |vm, c| {
        let (h, l) = this_area(vm, &c)?;
        store(vm, &h, l, Vec::new())?;
        Ok(Value::Undefined)
    });
}

/// The named properties of a Storage object, for the exotic hooks.
pub fn names(h: &Host, id: u64) -> Vec<(String, String)> {
    area(h, is_local(id))
}

pub fn define(vm: &mut Vm, h: &Host, id: u64, k: &PropKey, d: &PropDesc) -> JsResult<Option<bool>> {
    if matches!(k, PropKey::Sym(_)) {
        return Ok(None);
    }
    if d.get.is_some() || d.set.is_some() {
        return Ok(Some(false));
    }
    let v = match &d.value {
        Some(v) => rstr(vm, v)?,
        None => return Ok(Some(false)),
    };
    set_item(vm, h, is_local(id), &k.display(), &v)?;
    Ok(Some(true))
}

pub fn delete(vm: &mut Vm, h: &Host, id: u64, k: &PropKey) -> JsResult<Option<bool>> {
    if matches!(k, PropKey::Sym(_)) {
        return Ok(None);
    }
    let name = k.display();
    if !names(h, id).iter().any(|x| x.0 == name) {
        return Ok(None);
    }
    remove_item(vm, h, is_local(id), &name)?;
    Ok(Some(true))
}

pub fn install(vm: &mut Vm, g: ObjRef) {
    attr(vm, g, "localStorage", |vm, _| storage_object(vm, true), None);
    attr(vm, g, "sessionStorage", |vm, _| storage_object(vm, false), None);
}

/// document.cookie (HTML §3.1.5): the embedder's cookie jar, as a non-HTTP API.
pub fn setup_document_cookie(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "cookie",
        |vm, c| {
            let (h, n) = this_node(vm, &c.this, C::Document)?;
            Ok(js(&if n.doc == 0 { h.embedder.cookie() } else { String::new() }))
        },
        Some(|vm, c| {
            let v = rstr(vm, &c.arg(0))?;
            let (h, n) = this_node(vm, &c.this, C::Document)?;
            if n.doc == 0 {
                h.embedder.set_cookie(&v);
            }
            Ok(Value::Undefined)
        }),
    );
}

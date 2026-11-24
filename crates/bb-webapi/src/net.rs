//! Networking from scripts: Fetch (Fetch Standard §5: Headers, Request, Response, fetch()),
//! XMLHttpRequest (XHR Standard), AbortController/AbortSignal (DOM §3), ProgressEvent, and
//! TextEncoder/TextDecoder (Encoding Standard §7).
//!
//! Requests go to the embedder: asynchronously when it supports that (`start_fetch`, then
//! `poll_fetches` from the event loop), otherwise through `fetch_sync` in a queued task.
//! Completions settle promises or advance XMLHttpRequests as tasks.

use std::rc::Rc;

use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::typed::TaKind;
use bb_js::vm::{realm, PropKey};
use bb_js::{JsResult, Value, Vm};
use bb_url::Url;

use crate::class::C;
use crate::exception::{make as dom_exception, throw};
use crate::host::*;
use crate::idl::{attr, attr_with, callee_slots, constant, op};

type R = JsResult<Value>;

/// An HTTP request scripts make.
#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// Its response (after redirects).
#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub url: Url,
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A request in flight, and what its completion settles.
pub enum Pending {
    /// fetch(): its promise's resolve and reject functions, and its abort signal.
    Fetch { resolve: Value, reject: Value, signal: Value },
    /// An XMLHttpRequest object.
    Xhr(ObjRef),
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

fn obj_of(vm: &mut Vm, v: &Value, iface: C) -> JsResult<ObjRef> {
    this_of(vm, v, iface)?;
    match v {
        Value::Object(o) => Ok(*o),
        _ => vm.type_error("Illegal invocation"),
    }
}

fn new_obj(vm: &mut Vm, h: &Host, c: C, proto: Option<ObjRef>, slots: Vec<Value>) -> ObjRef {
    let o = vm.new_host_object(Some(proto.unwrap_or_else(|| h.proto(c))), c as u32, 0, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = slots;
    }
    o
}

fn proto_from(vm: &mut Vm, nt: ObjRef) -> JsResult<Option<ObjRef>> {
    Ok(match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => Some(p),
        _ => None,
    })
}

/// A Uint8Array over a copy of `bytes`.
pub fn uint8_array(vm: &mut Vm, bytes: Vec<u8>) -> R {
    let buf = vm.new_array_buffer(bytes);
    let ctor = vm.realm.ta_ctors[TaKind::Uint8.index()];
    vm.construct(ctor, vec![Value::Object(buf)], None)
}

// ---- Headers (Fetch §5.2) ----

/// Header names (Fetch §2.2.2 "header name": a token).
fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Fetch §2.2.2 "normalize" a header value: strip leading and trailing HTTP whitespace.
fn normalize(v: &str) -> String {
    v.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r')).to_string()
}

fn valid_value(v: &str) -> bool {
    !v.contains(['\0', '\n', '\r'])
}

/// Fetch §2.2.2 forbidden request-header names (their changes are ignored).
fn forbidden(n: &str) -> bool {
    matches!(
        n,
        "accept-charset"
            | "accept-encoding"
            | "access-control-request-headers"
            | "access-control-request-method"
            | "connection"
            | "content-length"
            | "cookie"
            | "cookie2"
            | "date"
            | "dnt"
            | "expect"
            | "host"
            | "keep-alive"
            | "origin"
            | "referer"
            | "set-cookie"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "via"
    ) || n.starts_with("proxy-")
        || n.starts_with("sec-")
}

/// A Headers object's list: (lowercased name, value) in insertion order.
fn header_list(vm: &mut Vm, o: ObjRef) -> JsResult<Vec<(String, String)>> {
    let Value::Object(a) = slot(vm, o, 0) else { return Ok(Vec::new()) };
    let items = vm.list_from_array_like(&Value::Object(a))?;
    let mut out = Vec::with_capacity(items.len() / 2);
    for pair in items.chunks(2) {
        if let [k, v] = pair {
            out.push((rstr(vm, k)?, rstr(vm, v)?));
        }
    }
    Ok(out)
}

fn store_headers(vm: &mut Vm, o: ObjRef, l: &[(String, String)]) {
    let flat: Vec<Value> = l.iter().flat_map(|(k, v)| [js(k), js(v)]).collect();
    let a = vm.new_array(flat);
    set_slot(vm, o, 0, Value::Object(a));
}

/// "sort and combine" (Fetch §2.2.2): sorted names, values of equal names joined by ", "
/// (set-cookie stays separate).
fn sort_and_combine(l: &[(String, String)]) -> Vec<(String, String)> {
    let mut names: Vec<&String> = l.iter().map(|x| &x.0).collect();
    names.sort();
    names.dedup();
    let mut out = Vec::new();
    for n in names {
        if n == "set-cookie" {
            for (_, v) in l.iter().filter(|x| &x.0 == n) {
                out.push((n.clone(), v.clone()));
            }
        } else {
            let vals: Vec<&str> = l.iter().filter(|x| &x.0 == n).map(|x| x.1.as_str()).collect();
            out.push((n.clone(), vals.join(", ")));
        }
    }
    out
}

/// New Headers with `guard` ("none", "request", "response", "immutable").
pub fn new_headers(vm: &mut Vm, h: &Host, l: &[(String, String)], guard: &str) -> ObjRef {
    let o = new_obj(vm, h, C::Headers, None, vec![Value::Undefined, js(guard)]);
    store_headers(vm, o, l);
    o
}

fn guard_of(vm: &mut Vm, o: ObjRef) -> JsResult<String> {
    let g = slot(vm, o, 1);
    rstr(vm, &g)
}

/// Fetch §5.2 "append" a header to a Headers object.
fn headers_append(vm: &mut Vm, o: ObjRef, name: &str, value: &str) -> JsResult<()> {
    let value = normalize(value);
    if !valid_name(name) || !valid_value(&value) {
        return vm.type_error(&format!("invalid header {name}: {value}"));
    }
    let n = name.to_ascii_lowercase();
    match guard_of(vm, o)?.as_str() {
        "immutable" => return vm.type_error("these headers can't be changed"),
        "request" if forbidden(&n) => return Ok(()),
        "response" if n == "set-cookie" || n == "set-cookie2" => return Ok(()),
        _ => {}
    }
    let mut l = header_list(vm, o)?;
    l.push((n, value));
    store_headers(vm, o, &l);
    Ok(())
}

/// Fill Headers from a HeadersInit (sequence of pairs, record, or Headers).
fn fill_headers(vm: &mut Vm, o: ObjRef, init: &Value) -> JsResult<()> {
    let Value::Object(io) = init else {
        if init.is_undefined() {
            return Ok(());
        }
        return vm.type_error("headers must be an object");
    };
    if vm.host_data(init).is_some_and(|(c, _)| c == C::Headers as u32) {
        for (k, v) in header_list(vm, *io)? {
            headers_append(vm, o, &k, &v)?;
        }
        return Ok(());
    }
    let it_key = PropKey::Sym(vm.realm.sym_iterator.clone());
    let iter = vm.get(*io, &it_key)?;
    if !iter.is_nullish() {
        let pairs = vm.call(&iter, init.clone(), Vec::new())?;
        let Value::Object(it) = pairs else { return vm.type_error("not iterable") };
        let next = vm.get(it, &PropKey::lit("next"))?;
        loop {
            let r = vm.call(&next, Value::Object(it), Vec::new())?;
            let Value::Object(r) = r else { return vm.type_error("bad iterator result") };
            let done = vm.get(r, &PropKey::lit("done"))?;
            if vm.to_boolean(&done) {
                break;
            }
            let pair = vm.get(r, &PropKey::lit("value"))?;
            let items = vm.list_from_array_like(&pair)?;
            if items.len() != 2 {
                return vm.type_error("each header must be a [name, value] pair");
            }
            let (k, v) = (rstr(vm, &items[0])?, rstr(vm, &items[1])?);
            headers_append(vm, o, &k, &v)?;
        }
    } else {
        for k in vm.own_property_keys(*io)? {
            if matches!(k, PropKey::Sym(_)) {
                continue;
            }
            if vm.get_own_property(*io, &k)?.is_none_or(|d| d.enumerable != Some(true)) {
                continue;
            }
            let v = vm.get(*io, &k)?;
            let v = rstr(vm, &v)?;
            headers_append(vm, o, &k.display(), &v)?;
        }
    }
    Ok(())
}

pub fn headers_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("Headers requires 'new'") };
    let h = host(vm)?;
    let proto = proto_from(vm, nt)?;
    let o = new_obj(vm, &h, C::Headers, proto, vec![Value::Undefined, js("none")]);
    store_headers(vm, o, &[]);
    fill_headers(vm, o, &c.arg(0))?;
    Ok(Value::Object(o))
}

fn name_arg(vm: &mut Vm, v: &Value) -> JsResult<String> {
    let n = rstr(vm, v)?;
    if !valid_name(&n) {
        return vm.type_error(&format!("'{n}' is not a valid header name"));
    }
    Ok(n.to_ascii_lowercase())
}

pub fn setup_headers(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "append", 2, |vm, c| {
        let (n, v) = (rstr(vm, &c.arg(0))?, rstr(vm, &c.arg(1))?);
        let o = obj_of(vm, &c.this, C::Headers)?;
        headers_append(vm, o, &n, &v)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "delete", 1, |vm, c| {
        let n = name_arg(vm, &c.arg(0))?;
        let o = obj_of(vm, &c.this, C::Headers)?;
        match guard_of(vm, o)?.as_str() {
            "immutable" => return vm.type_error("these headers can't be changed"),
            "request" if forbidden(&n) => return Ok(Value::Undefined),
            _ => {}
        }
        let mut l = header_list(vm, o)?;
        l.retain(|x| x.0 != n);
        store_headers(vm, o, &l);
        Ok(Value::Undefined)
    });
    op(vm, proto, "get", 1, |vm, c| {
        let n = name_arg(vm, &c.arg(0))?;
        let o = obj_of(vm, &c.this, C::Headers)?;
        let l = header_list(vm, o)?;
        let vals: Vec<&str> = l.iter().filter(|x| x.0 == n).map(|x| x.1.as_str()).collect();
        Ok(if vals.is_empty() { Value::Null } else { js(&vals.join(", ")) })
    });
    op(vm, proto, "getSetCookie", 0, |vm, c| {
        let o = obj_of(vm, &c.this, C::Headers)?;
        let l = header_list(vm, o)?;
        let vals = l.iter().filter(|x| x.0 == "set-cookie").map(|x| js(&x.1)).collect();
        Ok(Value::Object(vm.new_array(vals)))
    });
    op(vm, proto, "has", 1, |vm, c| {
        let n = name_arg(vm, &c.arg(0))?;
        let o = obj_of(vm, &c.this, C::Headers)?;
        let l = header_list(vm, o)?;
        Ok(Value::Bool(l.iter().any(|x| x.0 == n)))
    });
    op(vm, proto, "set", 2, |vm, c| {
        let n = name_arg(vm, &c.arg(0))?;
        let v = normalize(&rstr(vm, &c.arg(1))?);
        if !valid_value(&v) {
            return vm.type_error("invalid header value");
        }
        let o = obj_of(vm, &c.this, C::Headers)?;
        match guard_of(vm, o)?.as_str() {
            "immutable" => return vm.type_error("these headers can't be changed"),
            "request" if forbidden(&n) => return Ok(Value::Undefined),
            _ => {}
        }
        let mut l = header_list(vm, o)?;
        match l.iter().position(|x| x.0 == n) {
            Some(i) => {
                l[i].1 = v;
                let mut k = i + 1;
                while k < l.len() {
                    if l[k].0 == n {
                        l.remove(k);
                    } else {
                        k += 1;
                    }
                }
            }
            None => l.push((n, v)),
        }
        store_headers(vm, o, &l);
        Ok(Value::Undefined)
    });
    op(vm, proto, "forEach", 1, |vm, c| {
        let f = c.arg(0);
        if !vm.is_callable(&f) {
            return vm.type_error("forEach needs a function");
        }
        let o = obj_of(vm, &c.this, C::Headers)?;
        let l = header_list(vm, o)?;
        for (k, v) in sort_and_combine(&l) {
            vm.call(&f, c.arg(1), vec![js(&v), js(&k), c.this.clone()])?;
        }
        Ok(Value::Undefined)
    });
    for (name, kind) in [("entries", 0.0), ("keys", 1.0), ("values", 2.0)] {
        let f = realm::native_with(vm, name, 0, headers_iter, false, vec![Value::Number(kind)]);
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
}

fn headers_iter(vm: &mut Vm, c: NativeCall) -> R {
    let kind = match callee_slots(vm, c.callee).first() {
        Some(Value::Number(n)) => *n as u8,
        _ => 0,
    };
    let o = obj_of(vm, &c.this, C::Headers)?;
    let l = header_list(vm, o)?;
    let mut items = Vec::new();
    for (k, v) in sort_and_combine(&l) {
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

// ---- bodies ----

/// Extract a body (Fetch §5.1 BodyInit): bytes and its default Content-Type.
fn extract_body(vm: &mut Vm, v: &Value) -> JsResult<Option<(Vec<u8>, Option<String>)>> {
    match v {
        Value::Undefined | Value::Null => Ok(None),
        Value::Object(_) => {
            if let Some(b) = vm.buffer_source_bytes(v) {
                return Ok(Some((b, None)));
            }
            if vm.host_data(v).is_some_and(|(c, _)| c == C::URLSearchParams as u32) {
                let s = rstr(vm, v)?;
                return Ok(Some((
                    s.into_bytes(),
                    Some("application/x-www-form-urlencoded;charset=UTF-8".into()),
                )));
            }
            let s = rstr(vm, v)?;
            Ok(Some((s.into_bytes(), Some("text/plain;charset=UTF-8".into()))))
        }
        v => {
            let s = rstr(vm, v)?;
            Ok(Some((s.into_bytes(), Some("text/plain;charset=UTF-8".into()))))
        }
    }
}

// Response slots.
const R_STATUS: usize = 0;
const R_STATUS_TEXT: usize = 1;
const R_HEADERS: usize = 2;
const R_URL: usize = 3;
const R_BODY: usize = 4;
const R_USED: usize = 5;
const R_TYPE: usize = 6;
const R_REDIRECTED: usize = 7;

fn new_response(
    vm: &mut Vm,
    h: &Host,
    proto: Option<ObjRef>,
    resp: &HttpResponse,
    ty: &str,
    redirected: bool,
) -> ObjRef {
    let headers = new_headers(vm, h, &resp.headers, "immutable");
    let body = Value::Object(vm.new_array_buffer(resp.body.clone()));
    new_obj(
        vm,
        h,
        C::Response,
        proto,
        vec![
            Value::Number(resp.status as f64),
            js(&resp.status_text),
            Value::Object(headers),
            js(&resp.url.href()),
            body,
            Value::Bool(false),
            js(ty),
            Value::Bool(redirected),
        ],
    )
}

pub fn response_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("Response requires 'new'") };
    let body = extract_body(vm, &c.arg(0))?;
    let init = c.arg(1);
    let status = match dict_get(vm, &init, "status")? {
        Value::Undefined => 200,
        v => vm.to_uint32(&v)? as u16,
    };
    if !(200..=599).contains(&status) {
        return vm.range_error("the status must be in 200–599");
    }
    let status_text = match dict_get(vm, &init, "statusText")? {
        Value::Undefined => String::new(),
        v => rstr(vm, &v)?,
    };
    let h = host(vm)?;
    let proto = proto_from(vm, nt)?;
    let headers = new_headers(vm, &h, &[], "response");
    let hi = dict_get(vm, &init, "headers")?;
    fill_headers(vm, headers, &hi)?;
    let (bytes, ct) = match body {
        Some((b, ct)) => {
            if matches!(status, 101 | 103 | 204 | 205 | 304) {
                return vm.type_error("this status can't have a body");
            }
            (Value::Object(vm.new_array_buffer(b)), ct)
        }
        None => (Value::Null, None),
    };
    if let Some(ct) = ct {
        if !header_list(vm, headers)?.iter().any(|x| x.0 == "content-type") {
            headers_append(vm, headers, "content-type", &ct)?;
        }
    }
    let o = new_obj(
        vm,
        &h,
        C::Response,
        proto,
        vec![
            Value::Number(status as f64),
            js(&status_text),
            Value::Object(headers),
            js(""),
            bytes,
            Value::Bool(false),
            js("default"),
            Value::Bool(false),
        ],
    );
    Ok(Value::Object(o))
}

/// Consume a body (Fetch §5.1 "consume body"): its bytes, or TypeError if already used.
fn consume(vm: &mut Vm, o: ObjRef, body_slot: usize, used_slot: usize) -> JsResult<Vec<u8>> {
    if matches!(slot(vm, o, used_slot), Value::Bool(true)) {
        return vm.type_error("the body has already been read");
    }
    let b = slot(vm, o, body_slot);
    if !b.is_nullish() {
        set_slot(vm, o, used_slot, Value::Bool(true));
    }
    Ok(match b {
        Value::Object(buf) => vm.buffer_bytes(buf).cloned().unwrap_or_default(),
        _ => Vec::new(),
    })
}

/// A promise already settled with the result of `f`.
fn settled<F: FnOnce(&mut Vm) -> R>(vm: &mut Vm, f: F) -> R {
    let p = vm.new_promise(None);
    let (res, rej) = vm.create_resolving_functions(p);
    match f(vm) {
        Ok(v) => vm.call(&res, Value::Undefined, vec![v])?,
        Err(e) => vm.call(&rej, Value::Undefined, vec![e])?,
    };
    Ok(Value::Object(p))
}

fn body_text(bytes: &[u8]) -> String {
    let (t, _, _) = encoding_rs::UTF_8.decode(bytes);
    t.into_owned()
}

fn json_parse(vm: &mut Vm, text: &str) -> R {
    let g = vm.realm.global;
    let json = vm.get(g, &PropKey::lit("JSON"))?;
    let Value::Object(j) = json else { return vm.type_error("no JSON") };
    let parse = vm.get(j, &PropKey::lit("parse"))?;
    vm.call(&parse, json, vec![js(text)])
}

/// The Body mixin's methods on an interface whose body and bodyUsed are in `slots`.
fn setup_body(vm: &mut Vm, proto: ObjRef, iface: C, body_slot: usize, used_slot: usize) {
    let data = vec![
        Value::Number(iface as u32 as f64),
        Value::Number(body_slot as f64),
        Value::Number(used_slot as f64),
    ];
    fn info(vm: &mut Vm, c: &NativeCall) -> JsResult<(ObjRef, usize, usize)> {
        let s = callee_slots(vm, c.callee);
        let n = |i: usize| match s.get(i) {
            Some(Value::Number(n)) => *n as usize,
            _ => 0,
        };
        let iface = crate::class::from_u32(n(0) as u32).unwrap_or(C::Response);
        let o = obj_of(vm, &c.this, iface)?;
        Ok((o, n(1), n(2)))
    }
    let def = |vm: &mut Vm, name: &str, f: bb_js::vm::heap::NativeFn| {
        let m = realm::native_with(vm, name, 0, f, false, data.clone());
        vm.heap.obj_mut(proto).props.insert(
            PropKey::named(name),
            bb_js::vm::heap::Prop {
                slot: bb_js::vm::heap::Slot::Data(Value::Object(m)),
                writable: true,
                enumerable: true,
                configurable: true,
            },
        );
    };
    def(vm, "text", |vm, c| {
        settled(vm, |vm| {
            let (o, b, u) = info(vm, &c)?;
            let bytes = consume(vm, o, b, u)?;
            Ok(js(&body_text(&bytes)))
        })
    });
    def(vm, "json", |vm, c| {
        settled(vm, |vm| {
            let (o, b, u) = info(vm, &c)?;
            let bytes = consume(vm, o, b, u)?;
            json_parse(vm, &body_text(&bytes))
        })
    });
    def(vm, "arrayBuffer", |vm, c| {
        settled(vm, |vm| {
            let (o, b, u) = info(vm, &c)?;
            let bytes = consume(vm, o, b, u)?;
            Ok(Value::Object(vm.new_array_buffer(bytes)))
        })
    });
    def(vm, "bytes", |vm, c| {
        settled(vm, |vm| {
            let (o, b, u) = info(vm, &c)?;
            let bytes = consume(vm, o, b, u)?;
            uint8_array(vm, bytes)
        })
    });
    attr_with(
        vm,
        proto,
        "bodyUsed",
        |vm, c| {
            let (o, _, u) = info(vm, &c)?;
            Ok(Value::Bool(matches!(slot(vm, o, u), Value::Bool(true))))
        },
        None,
        data.clone(),
    );
    attr_with(
        vm,
        proto,
        "body",
        |vm, c| {
            let (o, b, _) = info(vm, &c)?;
            // No ReadableStream yet: a body is exposed as null.
            let _ = slot(vm, o, b);
            Ok(Value::Null)
        },
        None,
        data,
    );
}

pub fn setup_response(vm: &mut Vm, proto: ObjRef, ctor: ObjRef) {
    setup_body(vm, proto, C::Response, R_BODY, R_USED);
    for (name, i) in [
        ("status", R_STATUS),
        ("statusText", R_STATUS_TEXT),
        ("headers", R_HEADERS),
        ("url", R_URL),
        ("type", R_TYPE),
        ("redirected", R_REDIRECTED),
    ] {
        attr_with(
            vm,
            proto,
            name,
            |vm, c| {
                let o = obj_of(vm, &c.this, C::Response)?;
                let i = match callee_slots(vm, c.callee).first() {
                    Some(Value::Number(n)) => *n as usize,
                    _ => 0,
                };
                Ok(slot(vm, o, i))
            },
            None,
            vec![Value::Number(i as f64)],
        );
    }
    attr(
        vm,
        proto,
        "ok",
        |vm, c| {
            let o = obj_of(vm, &c.this, C::Response)?;
            let s = vm.to_number(&slot(vm, o, R_STATUS))?;
            Ok(Value::Bool((200.0..=299.0).contains(&s)))
        },
        None,
    );
    op(vm, proto, "clone", 0, |vm, c| {
        let o = obj_of(vm, &c.this, C::Response)?;
        if matches!(slot(vm, o, R_USED), Value::Bool(true)) {
            return vm.type_error("the body has already been read");
        }
        let h = host(vm)?;
        let mut slots: Vec<Value> = (0..8).map(|i| slot(vm, o, i)).collect();
        if let Value::Object(hd) = slots[R_HEADERS].clone() {
            let l = header_list(vm, hd)?;
            let g = guard_of(vm, hd)?;
            slots[R_HEADERS] = Value::Object(new_headers(vm, &h, &l, &g));
        }
        if let Value::Object(b) = slots[R_BODY].clone() {
            let bytes = vm.buffer_bytes(b).cloned().unwrap_or_default();
            slots[R_BODY] = Value::Object(vm.new_array_buffer(bytes));
        }
        Ok(Value::Object(new_obj(vm, &h, C::Response, None, slots)))
    });
    op(vm, ctor, "error", 0, |vm, _| {
        let h = host(vm)?;
        let headers = new_headers(vm, &h, &[], "immutable");
        Ok(Value::Object(new_obj(
            vm,
            &h,
            C::Response,
            None,
            vec![
                Value::Number(0.0),
                js(""),
                Value::Object(headers),
                js(""),
                Value::Null,
                Value::Bool(false),
                js("error"),
                Value::Bool(false),
            ],
        )))
    });
    op(vm, ctor, "json", 1, |vm, c| {
        let g = vm.realm.global;
        let json = vm.get(g, &PropKey::lit("JSON"))?;
        let Value::Object(j) = json else { return vm.type_error("no JSON") };
        let stringify = vm.get(j, &PropKey::lit("stringify"))?;
        let s = vm.call(&stringify, json, vec![c.arg(0)])?;
        if s.is_undefined() {
            return vm.type_error("the data can't be serialized as JSON");
        }
        let s = rstr(vm, &s)?;
        let h = host(vm)?;
        let headers = new_headers(vm, &h, &[("content-type".into(), "application/json".into())], "response");
        let hi = dict_get(vm, &c.arg(1), "headers")?;
        fill_headers(vm, headers, &hi)?;
        let status = match dict_get(vm, &c.arg(1), "status")? {
            Value::Undefined => 200.0,
            v => vm.to_uint32(&v)? as f64,
        };
        let body = Value::Object(vm.new_array_buffer(s.into_bytes()));
        Ok(Value::Object(new_obj(
            vm,
            &h,
            C::Response,
            None,
            vec![
                Value::Number(status),
                js(""),
                Value::Object(headers),
                js(""),
                body,
                Value::Bool(false),
                js("default"),
                Value::Bool(false),
            ],
        )))
    });
    op(vm, ctor, "redirect", 1, |vm, c| {
        let u = rstr(vm, &c.arg(0))?;
        let h = host(vm)?;
        let base = base_url(&h, 0);
        let Ok(url) = Url::parse_with_base(&u, Some(&base)) else { return vm.type_error("invalid URL") };
        let status = match c.arg(1) {
            Value::Undefined => 302.0,
            v => vm.to_uint32(&v)? as f64,
        };
        if ![301.0, 302.0, 303.0, 307.0, 308.0].contains(&status) {
            return vm.range_error("not a redirect status");
        }
        let headers = new_headers(vm, &h, &[("location".into(), url.href())], "immutable");
        Ok(Value::Object(new_obj(
            vm,
            &h,
            C::Response,
            None,
            vec![
                Value::Number(status),
                js(""),
                Value::Object(headers),
                js(""),
                Value::Null,
                Value::Bool(false),
                js("default"),
                Value::Bool(false),
            ],
        )))
    });
}

// ---- Request (Fetch §5.4) ----

const Q_METHOD: usize = 0;
const Q_URL: usize = 1;
const Q_HEADERS: usize = 2;
const Q_BODY: usize = 3;
const Q_USED: usize = 4;
const Q_SIGNAL: usize = 5;

pub fn request_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("Request requires 'new'") };
    let h = host(vm)?;
    let (method, url, headers0, body0, signal0) = match vm.host_data(&c.arg(0)) {
        Some((cl, _)) if cl == C::Request as u32 => {
            let Value::Object(r) = c.arg(0) else { return vm.type_error("bad request") };
            let m = rstr(vm, &slot(vm, r, Q_METHOD))?;
            let u = rstr(vm, &slot(vm, r, Q_URL))?;
            let hl = match slot(vm, r, Q_HEADERS) {
                Value::Object(ho) => header_list(vm, ho)?,
                _ => Vec::new(),
            };
            (m, u, hl, slot(vm, r, Q_BODY), slot(vm, r, Q_SIGNAL))
        }
        _ => {
            let s = rstr(vm, &c.arg(0))?;
            let base = base_url(&h, 0);
            let Ok(u) = Url::parse_with_base(&s, Some(&base)) else {
                return vm.type_error(&format!("'{s}' is not a valid URL"));
            };
            if !u.username().is_empty() || !u.password().is_empty() {
                return vm.type_error("a request URL can't include credentials");
            }
            ("GET".to_string(), u.href(), Vec::new(), Value::Null, Value::Undefined)
        }
    };
    let init = c.arg(1);
    let mut method = method;
    if let v @ (Value::String(_) | Value::Object(_)) = dict_get(vm, &init, "method")? {
        let m = rstr(vm, &v)?;
        if !valid_name(&m) {
            return vm.type_error(&format!("'{m}' is not a valid method"));
        }
        let up = m.to_ascii_uppercase();
        if matches!(up.as_str(), "CONNECT" | "TRACE" | "TRACK") {
            return vm.type_error(&format!("the {up} method is forbidden"));
        }
        method = if matches!(up.as_str(), "DELETE" | "GET" | "HEAD" | "OPTIONS" | "POST" | "PUT") {
            up
        } else {
            m
        };
    }
    let headers = new_headers(vm, &h, &[], "request");
    let hi = dict_get(vm, &init, "headers")?;
    if hi.is_undefined() {
        store_headers(vm, headers, &headers0);
    } else {
        fill_headers(vm, headers, &hi)?;
    }
    let mut body = body0;
    let bi = dict_get(vm, &init, "body")?;
    if !bi.is_undefined() {
        if let Some((b, ct)) = extract_body(vm, &bi)? {
            if matches!(method.as_str(), "GET" | "HEAD") {
                return vm.type_error("a GET or HEAD request can't have a body");
            }
            body = Value::Object(vm.new_array_buffer(b));
            if let Some(ct) = ct {
                if !header_list(vm, headers)?.iter().any(|x| x.0 == "content-type") {
                    headers_append(vm, headers, "content-type", &ct)?;
                }
            }
        }
    }
    let signal = match dict_get(vm, &init, "signal")? {
        Value::Undefined => signal0,
        s => s,
    };
    let proto = proto_from(vm, nt)?;
    let o = new_obj(
        vm,
        &h,
        C::Request,
        proto,
        vec![js(&method), js(&url), Value::Object(headers), body, Value::Bool(false), signal],
    );
    Ok(Value::Object(o))
}

pub fn setup_request(vm: &mut Vm, proto: ObjRef) {
    setup_body(vm, proto, C::Request, Q_BODY, Q_USED);
    for (name, i) in [("method", Q_METHOD), ("url", Q_URL), ("headers", Q_HEADERS)] {
        attr_with(
            vm,
            proto,
            name,
            |vm, c| {
                let o = obj_of(vm, &c.this, C::Request)?;
                let i = match callee_slots(vm, c.callee).first() {
                    Some(Value::Number(n)) => *n as usize,
                    _ => 0,
                };
                Ok(slot(vm, o, i))
            },
            None,
            vec![Value::Number(i as f64)],
        );
    }
    attr(
        vm,
        proto,
        "signal",
        |vm, c| {
            let o = obj_of(vm, &c.this, C::Request)?;
            match slot(vm, o, Q_SIGNAL) {
                s @ Value::Object(_) => Ok(s),
                _ => {
                    let h = host(vm)?;
                    let s = new_signal(vm, &h);
                    set_slot(vm, o, Q_SIGNAL, Value::Object(s));
                    Ok(Value::Object(s))
                }
            }
        },
        None,
    );
    for (name, v) in [
        ("mode", "cors"),
        ("credentials", "same-origin"),
        ("cache", "default"),
        ("redirect", "follow"),
        ("referrer", "about:client"),
        ("referrerPolicy", ""),
        ("integrity", ""),
        ("destination", ""),
    ] {
        attr_with(
            vm,
            proto,
            name,
            |vm, c| {
                obj_of(vm, &c.this, C::Request)?;
                Ok(callee_slots(vm, c.callee).first().cloned().unwrap_or_default())
            },
            None,
            vec![js(v)],
        );
    }
    op(vm, proto, "clone", 0, |vm, c| {
        let o = obj_of(vm, &c.this, C::Request)?;
        let h = host(vm)?;
        let mut slots: Vec<Value> = (0..6).map(|i| slot(vm, o, i)).collect();
        if let Value::Object(hd) = slots[Q_HEADERS].clone() {
            let l = header_list(vm, hd)?;
            slots[Q_HEADERS] = Value::Object(new_headers(vm, &h, &l, "request"));
        }
        Ok(Value::Object(new_obj(vm, &h, C::Request, None, slots)))
    });
}

// ---- fetch() (Fetch §5.6) ----

/// Start a request; its completion is handled by `complete`.
fn start(vm: &mut Vm, h: &Host, req: HttpRequest, pending: Pending) {
    let id = h.next_fetch.get();
    h.next_fetch.set(id + 1);
    h.pending.borrow_mut().insert(id, pending);
    if !h.embedder.start_fetch(id, req.clone()) {
        h.tasks.borrow_mut().push_back(Task::Fetch(id, Box::new(req)));
    }
    let _ = vm;
}

pub fn fetch(vm: &mut Vm, c: NativeCall) -> R {
    let h = host(vm)?;
    let p = vm.new_promise(None);
    let (resolve, reject) = vm.create_resolving_functions(p);
    let ctor = Value::Object(h.ctor(C::Request));
    let req = match vm.construct(h.ctor(C::Request), vec![c.arg(0), c.arg(1)], None) {
        Ok(r) => r,
        Err(e) => {
            vm.call(&reject, Value::Undefined, vec![e])?;
            return Ok(Value::Object(p));
        }
    };
    let _ = ctor;
    let Value::Object(r) = req else { return Ok(Value::Object(p)) };
    let signal = slot(vm, r, Q_SIGNAL);
    if let Value::Object(s) = signal {
        if signal_aborted(vm, s) {
            let reason = slot(vm, s, 1);
            vm.call(&reject, Value::Undefined, vec![reason])?;
            return Ok(Value::Object(p));
        }
    }
    let method = rstr(vm, &slot(vm, r, Q_METHOD))?;
    let url_s = rstr(vm, &slot(vm, r, Q_URL))?;
    let Ok(url) = Url::parse(&url_s) else {
        let e = vm.error(bb_js::vm::ErrorKind::Type, "Failed to fetch");
        vm.call(&reject, Value::Undefined, vec![e])?;
        return Ok(Value::Object(p));
    };
    let headers = match slot(vm, r, Q_HEADERS) {
        Value::Object(ho) => header_list(vm, ho)?,
        _ => Vec::new(),
    };
    let body = match slot(vm, r, Q_BODY) {
        Value::Object(b) => vm.buffer_bytes(b).cloned(),
        _ => None,
    };
    start(vm, &h, HttpRequest { method, url, headers, body }, Pending::Fetch { resolve, reject, signal });
    Ok(Value::Object(p))
}

/// A request finished (as a task): settle its promise or advance its XMLHttpRequest.
pub fn complete(vm: &mut Vm, h: &Rc<Host>, id: u64, result: Result<HttpResponse, String>) -> JsResult<()> {
    let Some(p) = h.pending.borrow_mut().remove(&id) else { return Ok(()) };
    match p {
        Pending::Fetch { resolve, reject, signal } => {
            if let Value::Object(s) = signal {
                if signal_aborted(vm, s) {
                    return Ok(());
                }
            }
            match result {
                Ok(resp) => {
                    let r = new_response(vm, h, None, &resp, "basic", false);
                    vm.call(&resolve, Value::Undefined, vec![Value::Object(r)])?;
                }
                Err(msg) => {
                    log::debug!("fetch failed: {msg}");
                    let e = vm.error(bb_js::vm::ErrorKind::Type, "Failed to fetch");
                    vm.call(&reject, Value::Undefined, vec![e])?;
                }
            }
        }
        Pending::Xhr(x) => xhr_complete(vm, h, x, result)?,
    }
    Ok(())
}

// ---- AbortController / AbortSignal (DOM §3.1–3.2) ----

fn new_signal(vm: &mut Vm, h: &Host) -> ObjRef {
    new_obj(vm, h, C::AbortSignal, None, vec![Value::Bool(false), Value::Undefined])
}

pub fn signal_aborted(vm: &Vm, s: ObjRef) -> bool {
    matches!(slot(vm, s, 0), Value::Bool(true))
}

/// DOM §3.2 "signal abort": mark it, drop listeners added with this signal, reject its
/// fetches, and fire `abort`.
pub fn signal_abort(vm: &mut Vm, s: ObjRef, reason: Value) -> JsResult<()> {
    if signal_aborted(vm, s) {
        return Ok(());
    }
    let reason = if reason.is_undefined() {
        dom_exception(vm, "AbortError", "signal is aborted without reason")
    } else {
        reason
    };
    set_slot(vm, s, 0, Value::Bool(true));
    set_slot(vm, s, 1, reason.clone());
    let h = host(vm)?;
    for list in h.listeners.borrow_mut().values_mut() {
        list.retain(|l| {
            let hit = l.signal == Some(s);
            if hit {
                l.removed.set(true);
            }
            !hit
        });
    }
    let fetches: Vec<u64> = h
        .pending
        .borrow()
        .iter()
        .filter(|(_, p)| matches!(p, Pending::Fetch { signal: Value::Object(x), .. } if *x == s))
        .map(|(id, _)| *id)
        .collect();
    for id in fetches {
        if let Some(Pending::Fetch { reject, .. }) = h.pending.borrow_mut().remove(&id) {
            h.tasks.borrow_mut().push_back(Task::Call(reject, Value::Undefined, vec![reason.clone()]));
        }
    }
    crate::events::fire(vm, &h, Value::Object(s), "abort", false, false)?;
    Ok(())
}

pub fn abort_controller_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("AbortController requires 'new'") };
    let h = host(vm)?;
    let s = new_signal(vm, &h);
    let proto = proto_from(vm, nt)?;
    Ok(Value::Object(new_obj(vm, &h, C::AbortController, proto, vec![Value::Object(s)])))
}

pub fn setup_abort_controller(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "signal",
        |vm, c| {
            let o = obj_of(vm, &c.this, C::AbortController)?;
            Ok(slot(vm, o, 0))
        },
        None,
    );
    op(vm, proto, "abort", 0, |vm, c| {
        let o = obj_of(vm, &c.this, C::AbortController)?;
        if let Value::Object(s) = slot(vm, o, 0) {
            signal_abort(vm, s, c.arg(0))?;
        }
        Ok(Value::Undefined)
    });
}

pub fn setup_abort_signal(vm: &mut Vm, proto: ObjRef, ctor: ObjRef) {
    attr(
        vm,
        proto,
        "aborted",
        |vm, c| {
            let o = obj_of(vm, &c.this, C::AbortSignal)?;
            Ok(Value::Bool(signal_aborted(vm, o)))
        },
        None,
    );
    attr(
        vm,
        proto,
        "reason",
        |vm, c| {
            let o = obj_of(vm, &c.this, C::AbortSignal)?;
            Ok(slot(vm, o, 1))
        },
        None,
    );
    op(vm, proto, "throwIfAborted", 0, |vm, c| {
        let o = obj_of(vm, &c.this, C::AbortSignal)?;
        if signal_aborted(vm, o) {
            return Err(slot(vm, o, 1));
        }
        Ok(Value::Undefined)
    });
    crate::events::setup_named_handlers(vm, proto, &["onabort"]);
    op(vm, ctor, "abort", 0, |vm, c| {
        let h = host(vm)?;
        let s = new_signal(vm, &h);
        let reason = if c.arg(0).is_undefined() {
            dom_exception(vm, "AbortError", "signal is aborted without reason")
        } else {
            c.arg(0)
        };
        set_slot(vm, s, 0, Value::Bool(true));
        set_slot(vm, s, 1, reason);
        Ok(Value::Object(s))
    });
    op(vm, ctor, "timeout", 1, |vm, c| {
        let ms = vm.to_number(&c.arg(0))?;
        let h = host(vm)?;
        let s = new_signal(vm, &h);
        // A timer that aborts it with a TimeoutError.
        let f = realm::native_with(
            vm,
            "",
            0,
            |vm, c| {
                let s = callee_slots(vm, c.callee).first().cloned().unwrap_or_default();
                if let Value::Object(s) = s {
                    let e = dom_exception(vm, "TimeoutError", "signal timed out");
                    signal_abort(vm, s, e)?;
                }
                Ok(Value::Undefined)
            },
            false,
            vec![Value::Object(s)],
        );
        let id = h.next_timer.get();
        h.next_timer.set(id + 1);
        let seq = h.timer_seq.get();
        h.timer_seq.set(seq + 1);
        let due = h.embedder.now_ms() + ms.max(0.0);
        h.timers.borrow_mut().push(Timer {
            id,
            due,
            interval: None,
            callback: Value::Object(f),
            args: Vec::new(),
            seq,
        });
        Ok(Value::Object(s))
    });
    op(vm, ctor, "any", 1, |vm, c| {
        let signals = vm.list_from_array_like(&c.arg(0))?;
        let h = host(vm)?;
        let s = new_signal(vm, &h);
        for x in &signals {
            if let Value::Object(xo) = x {
                if signal_aborted(vm, *xo) {
                    let r = slot(vm, *xo, 1);
                    set_slot(vm, s, 0, Value::Bool(true));
                    set_slot(vm, s, 1, r);
                    return Ok(Value::Object(s));
                }
            }
        }
        // Follow each source: abort when any does.
        for x in signals {
            if let Value::Object(xo) = x {
                let f = realm::native_with(
                    vm,
                    "",
                    1,
                    |vm, c| {
                        let slots = callee_slots(vm, c.callee);
                        if let (Some(Value::Object(dep)), Some(Value::Object(src))) =
                            (slots.first(), slots.get(1))
                        {
                            let r = slot(vm, *src, 1);
                            signal_abort(vm, *dep, r)?;
                        }
                        Ok(Value::Undefined)
                    },
                    false,
                    vec![Value::Object(s), Value::Object(xo)],
                );
                h.listeners.borrow_mut().entry(xo).or_default().push(Listener {
                    ty: "abort".into(),
                    callback: Value::Object(f),
                    capture: false,
                    once: true,
                    passive: false,
                    removed: Rc::new(std::cell::Cell::new(false)),
                    handler: false,
                    signal: None,
                });
            }
        }
        Ok(Value::Object(s))
    });
}

// ---- XMLHttpRequest (XHR Standard) ----

/// The state of one XMLHttpRequest (by its object's payload).
#[derive(Default)]
pub struct XhrState {
    pub method: String,
    pub url: Option<Url>,
    pub is_async: bool,
    pub headers: Vec<(String, String)>,
    pub state: u8,
    pub send_flag: bool,
    pub response: Option<HttpResponse>,
    pub error: bool,
    pub response_type: String,
    pub timeout: u32,
    pub with_credentials: bool,
    /// Bumped by open()/abort(), so a stale completion is ignored.
    pub generation: u64,
    pub mime_override: Option<String>,
    /// The document response, once made: (generation it is for, document index or None).
    pub doc: Option<(u64, Option<u32>)>,
}

/// XMLHttpRequest's "document response": the response parsed as HTML (for responseType
/// "document" only) or XML, made once per request; null for other types or an XML error.
fn document_response(vm: &mut Vm, h: &Host, id: usize) -> R {
    let (cached, gen, ty, done, response, mime_override) = {
        let xs = h.xhrs.borrow();
        let Some(x) = xs.get(id) else { return Ok(Value::Null) };
        (
            x.doc,
            x.generation,
            x.response_type.clone(),
            x.state == 4 && !x.error,
            x.response.clone(),
            x.mime_override.clone(),
        )
    };
    if !done {
        return Ok(Value::Null);
    }
    if let Some((g, d)) = cached {
        if g == gen {
            return match d {
                Some(d) => wrap(vm, h, NodeRef { doc: d, id: bb_dom::Document::ROOT }),
                None => Ok(Value::Null),
            };
        }
    }
    let Some(r) = response else { return Ok(Value::Null) };
    // The final MIME type: the override, else Content-Type, else text/xml.
    let mime = mime_override
        .or_else(|| {
            r.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-type")).map(|(_, v)| v.clone())
        })
        .unwrap_or_else(|| "text/xml".to_string());
    let essence = mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let charset = mime.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("charset").then(|| v.trim().trim_matches('"').to_string())
    });
    let is_html = essence == "text/html";
    let is_xml = essence == "text/xml" || essence == "application/xml" || essence.ends_with("+xml");
    let doc = if is_html && ty == "document" {
        let enc = charset
            .as_deref()
            .and_then(|c| encoding_rs::Encoding::for_label(c.as_bytes()))
            .unwrap_or(encoding_rs::UTF_8);
        let (text, _, _) = enc.decode(&r.body);
        let idx = crate::dom::new_document(h, "text/html");
        let mut d = bb_html::parse_document(&text);
        d.url = r.url.href();
        h.docs.borrow_mut()[idx as usize] = d;
        Some(idx)
    } else if is_xml {
        let label = charset.or_else(|| bb_xml::sniff_encoding(&r.body));
        let enc = label
            .as_deref()
            .and_then(|c| encoding_rs::Encoding::for_label(c.as_bytes()))
            .unwrap_or(encoding_rs::UTF_8);
        let (text, _, _) = enc.decode(&r.body);
        match bb_xml::parse(&text) {
            Ok(mut d) => {
                d.url = r.url.href();
                let idx = crate::dom::new_document(h, &essence);
                h.docs.borrow_mut()[idx as usize] = d;
                Some(idx)
            }
            Err(_) => None,
        }
    } else {
        None
    };
    if let Some(d) = doc {
        crate::xml::mark_scripts_started(h, d);
    }
    if let Some(x) = h.xhrs.borrow_mut().get_mut(id) {
        x.doc = Some((gen, doc));
    }
    match doc {
        Some(d) => wrap(vm, h, NodeRef { doc: d, id: bb_dom::Document::ROOT }),
        None => Ok(Value::Null),
    }
}

fn xhr_of(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, ObjRef, usize)> {
    let (_, id) = this_of(vm, &c.this, C::XMLHttpRequest)?;
    let Value::Object(o) = c.this else { return vm.type_error("Illegal invocation") };
    Ok((host(vm)?, o, id as usize))
}

pub fn xhr_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("XMLHttpRequest requires 'new'") };
    let h = host(vm)?;
    let id = {
        let mut x = h.xhrs.borrow_mut();
        x.push(XhrState { is_async: true, ..Default::default() });
        x.len() as u64 - 1
    };
    let proto = proto_from(vm, nt)?.unwrap_or_else(|| h.proto(C::XMLHttpRequest));
    let o = vm.new_host_object(Some(proto), C::XMLHttpRequest as u32, id, false);
    let up =
        vm.new_host_object(Some(h.proto(C::XMLHttpRequestUpload)), C::XMLHttpRequestUpload as u32, 0, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = vec![Value::Object(up)];
    }
    Ok(Value::Object(o))
}

fn fire_xhr(vm: &mut Vm, h: &Host, o: ObjRef, ty: &str) -> JsResult<()> {
    crate::events::fire(vm, h, Value::Object(o), ty, false, false)?;
    Ok(())
}

fn fire_progress(
    vm: &mut Vm,
    h: &Host,
    target: Value,
    ty: &str,
    loaded: usize,
    total: Option<usize>,
) -> JsResult<()> {
    let e = crate::events::trusted_event(vm, h, C::ProgressEvent, ty, false, false);
    crate::events::set_field(vm, e, "lengthComputable", Value::Bool(total.is_some()));
    crate::events::set_field(vm, e, "loaded", Value::Number(loaded as f64));
    crate::events::set_field(vm, e, "total", Value::Number(total.unwrap_or(0) as f64));
    crate::events::dispatch(vm, h, e, target)?;
    Ok(())
}

fn set_ready_state(vm: &mut Vm, h: &Host, o: ObjRef, id: usize, s: u8) -> JsResult<()> {
    if let Some(x) = h.xhrs.borrow_mut().get_mut(id) {
        x.state = s;
    }
    fire_xhr(vm, h, o, "readystatechange")
}

fn xhr_complete(vm: &mut Vm, h: &Rc<Host>, o: ObjRef, result: Result<HttpResponse, String>) -> JsResult<()> {
    let id = match vm.host_data(&Value::Object(o)) {
        Some((_, id)) => id as usize,
        None => return Ok(()),
    };
    match result {
        Ok(resp) => {
            let len = resp.body.len();
            if let Some(x) = h.xhrs.borrow_mut().get_mut(id) {
                x.response = Some(resp);
                x.send_flag = false;
            }
            set_ready_state(vm, h, o, id, 2)?;
            set_ready_state(vm, h, o, id, 3)?;
            fire_progress(vm, h, Value::Object(o), "progress", len, Some(len))?;
            set_ready_state(vm, h, o, id, 4)?;
            fire_progress(vm, h, Value::Object(o), "load", len, Some(len))?;
            fire_progress(vm, h, Value::Object(o), "loadend", len, Some(len))?;
        }
        Err(msg) => {
            log::debug!("XMLHttpRequest failed: {msg}");
            if let Some(x) = h.xhrs.borrow_mut().get_mut(id) {
                x.error = true;
                x.send_flag = false;
                x.response = None;
            }
            set_ready_state(vm, h, o, id, 4)?;
            fire_progress(vm, h, Value::Object(o), "error", 0, None)?;
            fire_progress(vm, h, Value::Object(o), "loadend", 0, None)?;
        }
    }
    Ok(())
}

/// XHR §3.6.6 "the response MIME type"'s charset-free essence, from Content-Type.
fn response_type_is_json(x: &XhrState) -> bool {
    x.response_type == "json"
}

pub fn setup_xhr(vm: &mut Vm, proto: ObjRef, ctor: ObjRef) {
    for (n, v) in
        [("UNSENT", 0.0), ("OPENED", 1.0), ("HEADERS_RECEIVED", 2.0), ("LOADING", 3.0), ("DONE", 4.0)]
    {
        constant(vm, ctor, n, Value::Number(v));
        constant(vm, proto, n, Value::Number(v));
    }
    crate::events::setup_named_handlers(vm, proto, &["onreadystatechange"]);
    attr(
        vm,
        proto,
        "readyState",
        |vm, c| {
            let (h, _, id) = xhr_of(vm, &c)?;
            let s = h.xhrs.borrow().get(id).map_or(0, |x| x.state);
            Ok(Value::Number(s as f64))
        },
        None,
    );
    op(vm, proto, "open", 2, |vm, c| {
        let method = rstr(vm, &c.arg(0))?;
        let url = rstr(vm, &c.arg(1))?;
        let is_async = if c.args.len() > 2 { vm.to_boolean(&c.arg(2)) } else { true };
        let (h, o, id) = xhr_of(vm, &c)?;
        if !valid_name(&method) {
            return throw(vm, "SyntaxError", "invalid method");
        }
        let up = method.to_ascii_uppercase();
        if matches!(up.as_str(), "CONNECT" | "TRACE" | "TRACK") {
            return throw(vm, "SecurityError", "forbidden method");
        }
        let method = if matches!(up.as_str(), "DELETE" | "GET" | "HEAD" | "OPTIONS" | "POST" | "PUT") {
            up
        } else {
            method
        };
        let base = base_url(&h, 0);
        let Ok(u) = Url::parse_with_base(&url, Some(&base)) else {
            return throw(vm, "SyntaxError", &format!("'{url}' is not a valid URL"));
        };
        if let Some(x) = h.xhrs.borrow_mut().get_mut(id) {
            x.method = method;
            x.url = Some(u);
            x.is_async = is_async;
            x.headers.clear();
            x.response = None;
            x.error = false;
            x.send_flag = false;
            x.generation += 1;
        }
        set_ready_state(vm, &h, o, id, 1)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "setRequestHeader", 2, |vm, c| {
        let n = rstr(vm, &c.arg(0))?;
        let v = normalize(&rstr(vm, &c.arg(1))?);
        let (h, _, id) = xhr_of(vm, &c)?;
        let (state, sent) = h.xhrs.borrow().get(id).map_or((0, false), |x| (x.state, x.send_flag));
        if state != 1 || sent {
            return throw(vm, "InvalidStateError", "open() first, and before send()");
        }
        if !valid_name(&n) || !valid_value(&v) {
            return throw(vm, "SyntaxError", "invalid header");
        }
        let n = n.to_ascii_lowercase();
        if forbidden(&n) {
            return Ok(Value::Undefined);
        }
        if let Some(x) = h.xhrs.borrow_mut().get_mut(id) {
            match x.headers.iter_mut().find(|h| h.0 == n) {
                Some(e) => e.1 = format!("{}, {v}", e.1),
                None => x.headers.push((n, v)),
            }
        }
        Ok(Value::Undefined)
    });
    op(vm, proto, "send", 0, |vm, c| {
        let body = extract_body(vm, &c.arg(0))?;
        let (h, o, id) = xhr_of(vm, &c)?;
        let (state, sent) = h.xhrs.borrow().get(id).map_or((0, false), |x| (x.state, x.send_flag));
        if state != 1 || sent {
            return throw(vm, "InvalidStateError", "open() first");
        }
        let req = {
            let mut xs = h.xhrs.borrow_mut();
            let Some(x) = xs.get_mut(id) else { return Ok(Value::Undefined) };
            x.send_flag = true;
            let mut headers = x.headers.clone();
            let body = if matches!(x.method.as_str(), "GET" | "HEAD") { None } else { body };
            if let Some((_, Some(ct))) = &body {
                if !headers.iter().any(|h| h.0 == "content-type") {
                    headers.push(("content-type".into(), ct.clone()));
                }
            }
            let Some(url) = x.url.clone() else { return Ok(Value::Undefined) };
            (HttpRequest { method: x.method.clone(), url, headers, body: body.map(|b| b.0) }, x.is_async)
        };
        let (req, is_async) = req;
        if !is_async {
            let r = h.embedder.fetch_sync(req);
            xhr_complete(vm, &h, o, r)?;
            return Ok(Value::Undefined);
        }
        fire_progress(vm, &h, Value::Object(o), "loadstart", 0, Some(0))?;
        start(vm, &h, req, Pending::Xhr(o));
        Ok(Value::Undefined)
    });
    op(vm, proto, "abort", 0, |vm, c| {
        let (h, o, id) = xhr_of(vm, &c)?;
        let sent = h.xhrs.borrow().get(id).is_some_and(|x| x.send_flag);
        h.pending.borrow_mut().retain(|_, p| !matches!(p, Pending::Xhr(x) if *x == o));
        if let Some(x) = h.xhrs.borrow_mut().get_mut(id) {
            x.generation += 1;
            x.send_flag = false;
            x.response = None;
        }
        if sent {
            set_ready_state(vm, &h, o, id, 4)?;
            fire_progress(vm, &h, Value::Object(o), "abort", 0, None)?;
            fire_progress(vm, &h, Value::Object(o), "loadend", 0, None)?;
        }
        if let Some(x) = h.xhrs.borrow_mut().get_mut(id) {
            x.state = 0;
        }
        Ok(Value::Undefined)
    });
    attr(
        vm,
        proto,
        "status",
        |vm, c| {
            let (h, _, id) = xhr_of(vm, &c)?;
            let s = h.xhrs.borrow().get(id).and_then(|x| x.response.as_ref().map(|r| r.status)).unwrap_or(0);
            Ok(Value::Number(s as f64))
        },
        None,
    );
    attr(
        vm,
        proto,
        "statusText",
        |vm, c| {
            let (h, _, id) = xhr_of(vm, &c)?;
            let s = h
                .xhrs
                .borrow()
                .get(id)
                .and_then(|x| x.response.as_ref().map(|r| r.status_text.clone()))
                .unwrap_or_default();
            Ok(js(&s))
        },
        None,
    );
    attr(
        vm,
        proto,
        "responseURL",
        |vm, c| {
            let (h, _, id) = xhr_of(vm, &c)?;
            let s = h
                .xhrs
                .borrow()
                .get(id)
                .and_then(|x| x.response.as_ref().map(|r| r.url.href()))
                .unwrap_or_default();
            Ok(js(&s))
        },
        None,
    );
    op(vm, proto, "getResponseHeader", 1, |vm, c| {
        let n = rstr(vm, &c.arg(0))?.to_ascii_lowercase();
        let (h, _, id) = xhr_of(vm, &c)?;
        let vals: Vec<String> = h
            .xhrs
            .borrow()
            .get(id)
            .and_then(|x| x.response.as_ref())
            .map(|r| {
                r.headers
                    .iter()
                    .filter(|x| x.0.eq_ignore_ascii_case(&n) && n != "set-cookie")
                    .map(|x| x.1.clone())
                    .collect()
            })
            .unwrap_or_default();
        Ok(if vals.is_empty() { Value::Null } else { js(&vals.join(", ")) })
    });
    op(vm, proto, "getAllResponseHeaders", 0, |vm, c| {
        let (h, _, id) = xhr_of(vm, &c)?;
        let l: Vec<(String, String)> = h
            .xhrs
            .borrow()
            .get(id)
            .and_then(|x| x.response.as_ref())
            .map(|r| {
                r.headers
                    .iter()
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
                    .filter(|x| x.0 != "set-cookie")
                    .collect()
            })
            .unwrap_or_default();
        let s: String = sort_and_combine(&l).iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
        Ok(js(&s))
    });
    op(vm, proto, "overrideMimeType", 1, |vm, c| {
        let m = rstr(vm, &c.arg(0))?;
        let (h, _, id) = xhr_of(vm, &c)?;
        if let Some(x) = h.xhrs.borrow_mut().get_mut(id) {
            x.mime_override = Some(m);
        }
        Ok(Value::Undefined)
    });
    attr(
        vm,
        proto,
        "responseType",
        |vm, c| {
            let (h, _, id) = xhr_of(vm, &c)?;
            let t = h.xhrs.borrow().get(id).map(|x| x.response_type.clone()).unwrap_or_default();
            Ok(js(&t))
        },
        Some(|vm, c| {
            let v = rstr(vm, &c.arg(0))?;
            let (h, _, id) = xhr_of(vm, &c)?;
            if matches!(v.as_str(), "" | "text" | "json" | "arraybuffer" | "blob" | "document") {
                if h.xhrs.borrow().get(id).is_some_and(|x| x.state >= 3) {
                    return throw(vm, "InvalidStateError", "too late to set responseType");
                }
                if let Some(x) = h.xhrs.borrow_mut().get_mut(id) {
                    x.response_type = v;
                }
            }
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "responseText",
        |vm, c| {
            let (h, _, id) = xhr_of(vm, &c)?;
            let (ty, text) = {
                let xs = h.xhrs.borrow();
                let x = xs.get(id);
                (
                    x.map(|x| x.response_type.clone()).unwrap_or_default(),
                    x.and_then(|x| x.response.as_ref()).map(|r| body_text(&r.body)),
                )
            };
            if !(ty.is_empty() || ty == "text") {
                return throw(vm, "InvalidStateError", "responseText is only for text responses");
            }
            Ok(js(&text.unwrap_or_default()))
        },
        None,
    );
    attr(
        vm,
        proto,
        "response",
        |vm, c| {
            let (h, _, id) = xhr_of(vm, &c)?;
            let (ty, body, done) = {
                let xs = h.xhrs.borrow();
                let x = xs.get(id);
                (
                    x.map(|x| x.response_type.clone()).unwrap_or_default(),
                    x.and_then(|x| x.response.as_ref()).map(|r| r.body.clone()),
                    x.is_some_and(|x| x.state == 4 && !x.error),
                )
            };
            match ty.as_str() {
                "" | "text" => Ok(js(&body.map(|b| body_text(&b)).unwrap_or_default())),
                _ if !done => Ok(Value::Null),
                "json" => {
                    let text = body_text(&body.unwrap_or_default());
                    Ok(json_parse(vm, &text).unwrap_or(Value::Null))
                }
                "arraybuffer" => Ok(Value::Object(vm.new_array_buffer(body.unwrap_or_default()))),
                "document" => document_response(vm, &h, id),
                _ => Ok(Value::Null),
            }
        },
        None,
    );
    attr(
        vm,
        proto,
        "responseXML",
        |vm, c| {
            let (h, _, id) = xhr_of(vm, &c)?;
            let ty = h.xhrs.borrow().get(id).map(|x| x.response_type.clone()).unwrap_or_default();
            if !(ty.is_empty() || ty == "document") {
                return throw(vm, "InvalidStateError", "responseXML is only for document responses");
            }
            document_response(vm, &h, id)
        },
        None,
    );
    attr(
        vm,
        proto,
        "timeout",
        |vm, c| {
            let (h, _, id) = xhr_of(vm, &c)?;
            let t = h.xhrs.borrow().get(id).map_or(0, |x| x.timeout);
            Ok(Value::Number(t as f64))
        },
        Some(|vm, c| {
            let v = vm.to_uint32(&c.arg(0))?;
            let (h, _, id) = xhr_of(vm, &c)?;
            if let Some(x) = h.xhrs.borrow_mut().get_mut(id) {
                x.timeout = v;
            }
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "withCredentials",
        |vm, c| {
            let (h, _, id) = xhr_of(vm, &c)?;
            let w = h.xhrs.borrow().get(id).is_some_and(|x| x.with_credentials);
            Ok(Value::Bool(w))
        },
        Some(|vm, c| {
            let v = vm.to_boolean(&c.arg(0));
            let (h, _, id) = xhr_of(vm, &c)?;
            if let Some(x) = h.xhrs.borrow_mut().get_mut(id) {
                x.with_credentials = v;
            }
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "upload",
        |vm, c| {
            let (_, o, _) = xhr_of(vm, &c)?;
            Ok(slot(vm, o, 0))
        },
        None,
    );
    let _ = response_type_is_json;
}

pub fn setup_xhr_target(vm: &mut Vm, proto: ObjRef) {
    crate::events::setup_named_handlers(
        vm,
        proto,
        &["onloadstart", "onprogress", "onabort", "onerror", "onload", "ontimeout", "onloadend"],
    );
}

pub fn setup_progress_event(vm: &mut Vm, proto: ObjRef) {
    for (n, d) in [
        ("lengthComputable", Value::Bool(false)),
        ("loaded", Value::Number(0.0)),
        ("total", Value::Number(0.0)),
    ] {
        attr_with(vm, proto, n, crate::events::field_getter, None, vec![js(n), d]);
    }
}

// ---- TextEncoder / TextDecoder (Encoding §7.1–7.2) ----

pub fn text_encoder_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("TextEncoder requires 'new'") };
    let h = host(vm)?;
    let proto = proto_from(vm, nt)?;
    Ok(Value::Object(new_obj(vm, &h, C::TextEncoder, proto, Vec::new())))
}

pub fn setup_text_encoder(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "encoding",
        |vm, c| {
            obj_of(vm, &c.this, C::TextEncoder)?;
            Ok(js("utf-8"))
        },
        None,
    );
    op(vm, proto, "encode", 0, |vm, c| {
        obj_of(vm, &c.this, C::TextEncoder)?;
        let s = match c.arg(0) {
            Value::Undefined => String::new(),
            v => rstr(vm, &v)?,
        };
        uint8_array(vm, s.into_bytes())
    });
    op(vm, proto, "encodeInto", 2, |vm, c| {
        obj_of(vm, &c.this, C::TextEncoder)?;
        let s = rstr(vm, &c.arg(0))?;
        let Value::Object(dest) = c.arg(1) else {
            return vm.type_error("the destination must be a Uint8Array");
        };
        let cap = vm.ta_length(dest).unwrap_or(0);
        let Some((buf, off)) = vm.ta_buffer(dest) else {
            return vm.type_error("the destination must be a Uint8Array");
        };
        let (mut read, mut written) = (0usize, 0usize);
        let mut out = Vec::new();
        for ch in s.chars() {
            let n = ch.len_utf8();
            if written + n > cap {
                break;
            }
            let mut b = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
            written += n;
            read += ch.len_utf16();
        }
        if let Some(bytes) = vm.buffer_bytes_mut(buf) {
            if let Some(dst) = bytes.get_mut(off..off + out.len()) {
                dst.copy_from_slice(&out);
            }
        }
        let r = vm.new_object();
        realm::value(vm, r, "read", Value::Number(read as f64));
        realm::value(vm, r, "written", Value::Number(written as f64));
        Ok(Value::Object(r))
    });
}

pub fn text_decoder_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("TextDecoder requires 'new'") };
    let label = match c.arg(0) {
        Value::Undefined => "utf-8".to_string(),
        v => rstr(vm, &v)?,
    };
    let fatal = dict_get(vm, &c.arg(1), "fatal")?;
    let fatal = vm.to_boolean(&fatal);
    let ignore_bom = dict_get(vm, &c.arg(1), "ignoreBOM")?;
    let ignore_bom = vm.to_boolean(&ignore_bom);
    let Some(enc) = encoding_rs::Encoding::for_label(label.trim().as_bytes()) else {
        return vm.range_error(&format!("'{label}' is not a supported encoding"));
    };
    if enc == encoding_rs::REPLACEMENT {
        return vm.range_error(&format!("'{label}' is not a supported encoding"));
    }
    let h = host(vm)?;
    let proto = proto_from(vm, nt)?;
    let name = enc.name().to_ascii_lowercase();
    Ok(Value::Object(new_obj(
        vm,
        &h,
        C::TextDecoder,
        proto,
        vec![js(&name), Value::Bool(fatal), Value::Bool(ignore_bom), Value::Undefined],
    )))
}

pub fn setup_text_decoder(vm: &mut Vm, proto: ObjRef) {
    for (n, i) in [("encoding", 0.0), ("fatal", 1.0), ("ignoreBOM", 2.0)] {
        attr_with(
            vm,
            proto,
            n,
            |vm, c| {
                let o = obj_of(vm, &c.this, C::TextDecoder)?;
                let i = match callee_slots(vm, c.callee).first() {
                    Some(Value::Number(n)) => *n as usize,
                    _ => 0,
                };
                Ok(slot(vm, o, i))
            },
            None,
            vec![Value::Number(i)],
        );
    }
    op(vm, proto, "decode", 0, |vm, c| {
        let o = obj_of(vm, &c.this, C::TextDecoder)?;
        let bytes = match c.arg(0) {
            Value::Undefined => Vec::new(),
            v => match vm.buffer_source_bytes(&v) {
                Some(b) => b,
                None => return vm.type_error("decode needs a BufferSource"),
            },
        };
        let stream = dict_get(vm, &c.arg(1), "stream")?;
        let stream = vm.to_boolean(&stream);
        let name = rstr(vm, &slot(vm, o, 0))?;
        let fatal = matches!(slot(vm, o, 1), Value::Bool(true));
        let ignore_bom = matches!(slot(vm, o, 2), Value::Bool(true));
        // Bytes held back from a streaming call.
        let mut all = match slot(vm, o, 3) {
            Value::Object(b) => vm.buffer_bytes(b).cloned().unwrap_or_default(),
            _ => Vec::new(),
        };
        all.extend(bytes);
        let enc = encoding_rs::Encoding::for_label(name.as_bytes()).unwrap_or(encoding_rs::UTF_8);
        let mut dec = if ignore_bom {
            enc.new_decoder_without_bom_handling()
        } else {
            enc.new_decoder_with_bom_removal()
        };
        let mut out = String::with_capacity(all.len() + 4);
        let cap = dec.max_utf8_buffer_length(all.len()).unwrap_or(all.len() * 3 + 16);
        out.reserve(cap);
        if fatal {
            let (r, read) = dec.decode_to_string_without_replacement(&all, &mut out, !stream);
            if let encoding_rs::DecoderResult::Malformed(..) = r {
                set_slot(vm, o, 3, Value::Undefined);
                return vm.type_error("the data is not valid");
            }
            let rest = all[read..].to_vec();
            let held =
                if rest.is_empty() { Value::Undefined } else { Value::Object(vm.new_array_buffer(rest)) };
            set_slot(vm, o, 3, held);
        } else {
            let (_, read, _) = dec.decode_to_string(&all, &mut out, !stream);
            let rest = all[read..].to_vec();
            let held = if rest.is_empty() || !stream {
                Value::Undefined
            } else {
                Value::Object(vm.new_array_buffer(rest))
            };
            set_slot(vm, o, 3, held);
        }
        Ok(js(&out))
    });
}

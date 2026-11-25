//! The File API (Blob, File, FileReader, blob URLs), XMLHttpRequest's FormData (XHR §5)
//! with HTML's FormDataEvent, and `crypto.getRandomValues`/`randomUUID` (Web Crypto §10).
//!
//! A Blob keeps its bytes in an ArrayBuffer in its slots (so the collector frees them with
//! it); a FormData keeps its entry list in its slots as (name, value) pairs, values being
//! strings or File objects.

use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::typed::TaKind;
use bb_js::vm::{realm, PropKey};
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::exception::throw;
use crate::host::{dict_get, host, js, rstr, this_of, Host, Task};
use crate::idl::{attr, constant, op};

type R = JsResult<Value>;

// Blob slots.
const B_BYTES: usize = 0;
const B_TYPE: usize = 1;
const B_NAME: usize = 2;
const B_MODIFIED: usize = 3;

fn slot(vm: &Vm, o: ObjRef, i: usize) -> Value {
    vm.host_obj(o).and_then(|h| h.slots.get(i).cloned()).unwrap_or(Value::Undefined)
}

/// A promise already resolved (or rejected) with `f`'s result.
fn settled(vm: &mut Vm, f: impl FnOnce(&mut Vm) -> R) -> R {
    let p = vm.new_promise(None);
    let (res, rej) = vm.create_resolving_functions(p);
    match f(vm) {
        Ok(v) => vm.call(&res, Value::Undefined, vec![v])?,
        Err(e) => vm.call(&rej, Value::Undefined, vec![e])?,
    };
    Ok(Value::Object(p))
}

/// A new Blob (or File, with `file` = (name, lastModified)).
pub fn new_blob(
    vm: &mut Vm,
    h: &Host,
    bytes: Vec<u8>,
    ty: &str,
    file: Option<(&str, f64)>,
    proto: Option<ObjRef>,
) -> ObjRef {
    let class = if file.is_some() { C::File } else { C::Blob };
    let proto = proto.unwrap_or_else(|| h.proto(class));
    let buf = vm.new_array_buffer(bytes);
    let o = vm.new_host_object(Some(proto), class as u32, 0, false);
    let mut slots = vec![Value::Object(buf), js(ty)];
    if let Some((name, modified)) = file {
        slots.push(js(name));
        slots.push(Value::Number(modified));
    }
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = slots;
    }
    o
}

/// A Blob's contents: (bytes, type, file name if it is a File).
pub fn blob_data(vm: &Vm, v: &Value) -> Option<(Vec<u8>, String, Option<String>)> {
    let (c, _) = vm.host_data(v)?;
    if !crate::class::implements(c, C::Blob) {
        return None;
    }
    let Value::Object(o) = v else { return None };
    let bytes = match slot(vm, *o, B_BYTES) {
        Value::Object(b) => vm.buffer_bytes(b).cloned().unwrap_or_default(),
        _ => Vec::new(),
    };
    let ty = match slot(vm, *o, B_TYPE) {
        Value::String(s) => bb_js::vm::to_rust(&s),
        _ => String::new(),
    };
    let name = match slot(vm, *o, B_NAME) {
        Value::String(s) if c == C::File as u32 => Some(bb_js::vm::to_rust(&s)),
        _ => None,
    };
    Some((bytes, ty, name))
}

/// File API §3.1 type normalization: ASCII lowercase, or "" if any character is outside
/// U+0020–U+007E.
fn normalize_type(s: &str) -> String {
    if s.chars().all(|c| ('\u{20}'..='\u{7E}').contains(&c)) {
        s.to_ascii_lowercase()
    } else {
        String::new()
    }
}

/// BlobPropertyBag (and FilePropertyBag's lastModified), members in WebIDL order.
fn property_bag(vm: &mut Vm, d: &Value, file: bool) -> JsResult<(bool, String, Option<f64>)> {
    if !matches!(d, Value::Undefined | Value::Null | Value::Object(_)) {
        return vm.type_error("the options must be an object");
    }
    let native = match dict_get(vm, d, "endings")? {
        Value::Undefined => false,
        v => match rstr(vm, &v)?.as_str() {
            "transparent" => false,
            "native" => true,
            other => return vm.type_error(&format!("'{other}' is not an EndingType")),
        },
    };
    let ty = match dict_get(vm, d, "type")? {
        Value::Undefined => String::new(),
        v => normalize_type(&rstr(vm, &v)?),
    };
    let modified = if file {
        match dict_get(vm, d, "lastModified")? {
            Value::Undefined => None,
            v => Some(long_long(vm, &v)? as f64),
        }
    } else {
        None
    };
    Ok((native, ty, modified))
}

/// WebIDL `long long` conversion.
fn long_long(vm: &mut Vm, v: &Value) -> JsResult<i64> {
    let n = vm.to_number(v)?;
    Ok(if n.is_finite() { n.trunc() as i64 } else { 0 })
}

/// File API §3.1.1 "process blob parts".
fn blob_parts(vm: &mut Vm, parts: &Value, native: bool) -> JsResult<Vec<u8>> {
    if matches!(parts, Value::Undefined) {
        return Ok(Vec::new());
    }
    let Value::Object(o) = parts else { return vm.type_error("the blob parts must be a sequence") };
    let key = PropKey::Sym(vm.realm.sym_iterator.clone());
    let f = vm.get(*o, &key)?;
    let items = crate::url::iter_values(vm, parts, &f)?;
    let mut out = Vec::new();
    for item in items {
        if let Some((b, _, _)) = blob_data(vm, &item) {
            out.extend(b);
        } else if let Some(b) = vm.buffer_source_bytes(&item) {
            out.extend(b);
        } else {
            let s = rstr(vm, &item)?;
            let s = if native { convert_line_endings(&s) } else { s };
            out.extend(s.into_bytes());
        }
    }
    Ok(out)
}

/// File API §3.1.1 "convert line endings to native".
fn convert_line_endings(s: &str) -> String {
    let native = if cfg!(windows) { "\r\n" } else { "\n" };
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push_str(native);
            }
            '\n' => out.push_str(native),
            c => out.push(c),
        }
    }
    out
}

/// The current time as Date.now() has it (a File's default lastModified).
fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

fn proto_of(vm: &mut Vm, nt: ObjRef) -> JsResult<Option<ObjRef>> {
    Ok(match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => Some(p),
        _ => None,
    })
}

pub fn blob_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("Blob requires 'new'") };
    let parts = c.arg(0);
    let (native, ty, _) = property_bag(vm, &c.arg(1), false)?;
    let bytes = blob_parts(vm, &parts, native)?;
    let h = host(vm)?;
    let proto = proto_of(vm, nt)?;
    Ok(Value::Object(new_blob(vm, &h, bytes, &ty, None, proto)))
}

pub fn file_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("File requires 'new'") };
    if c.args.len() < 2 {
        return vm.type_error("File needs its bits and a name");
    }
    let parts = c.arg(0);
    let name = rstr(vm, &c.arg(1))?;
    let (native, ty, modified) = property_bag(vm, &c.arg(2), true)?;
    let bytes = blob_parts(vm, &parts, native)?;
    let h = host(vm)?;
    let modified = modified.unwrap_or_else(now_ms);
    let proto = proto_of(vm, nt)?;
    Ok(Value::Object(new_blob(vm, &h, bytes, &ty, Some((&name, modified)), proto)))
}

fn this_blob(vm: &mut Vm, c: &NativeCall) -> JsResult<(Vec<u8>, String)> {
    this_of(vm, &c.this, C::Blob)?;
    match blob_data(vm, &c.this) {
        Some((b, t, _)) => Ok((b, t)),
        None => vm.type_error("Illegal invocation"),
    }
}

pub fn setup_blob(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "size",
        |vm, c| {
            let (b, _) = this_blob(vm, &c)?;
            Ok(Value::Number(b.len() as f64))
        },
        None,
    );
    attr(
        vm,
        proto,
        "type",
        |vm, c| {
            let (_, t) = this_blob(vm, &c)?;
            Ok(js(&t))
        },
        None,
    );
    // File API §3.3.1 slice(start, end, contentType).
    op(vm, proto, "slice", 0, |vm, c| {
        let start = match c.arg(0) {
            Value::Undefined => 0,
            v => long_long(vm, &v)?,
        };
        let end = match c.arg(1) {
            Value::Undefined => None,
            v => Some(long_long(vm, &v)?),
        };
        let ty = match c.arg(2) {
            Value::Undefined => String::new(),
            v => normalize_type(&rstr(vm, &v)?),
        };
        let (b, _) = this_blob(vm, &c)?;
        let size = b.len() as i64;
        let rel = |x: i64| if x < 0 { (size + x).max(0) } else { x.min(size) };
        let s = rel(start);
        let e = rel(end.unwrap_or(size));
        let bytes = if e > s { b[s as usize..e as usize].to_vec() } else { Vec::new() };
        let h = host(vm)?;
        Ok(Value::Object(new_blob(vm, &h, bytes, &ty, None, None)))
    });
    // File API §3.3.4 stream(): the bytes in chunks of 64 KiB.
    op(vm, proto, "stream", 0, |vm, c| {
        let (b, _) = this_blob(vm, &c)?;
        let chunks: Vec<Vec<u8>> = b.chunks(65536).map(<[u8]>::to_vec).collect();
        crate::streams::from_chunks(vm, chunks)
    });
    op(vm, proto, "text", 0, |vm, c| {
        settled(vm, |vm| {
            let (b, _) = this_blob(vm, &c)?;
            let (t, _, _) = encoding_rs::UTF_8.decode(&b);
            Ok(js(&t))
        })
    });
    op(vm, proto, "arrayBuffer", 0, |vm, c| {
        settled(vm, |vm| {
            let (b, _) = this_blob(vm, &c)?;
            Ok(Value::Object(vm.new_array_buffer(b)))
        })
    });
    op(vm, proto, "bytes", 0, |vm, c| {
        settled(vm, |vm| {
            let (b, _) = this_blob(vm, &c)?;
            let buf = vm.new_array_buffer(b);
            let ctor = vm.realm.ta_ctors[TaKind::Uint8.index()];
            vm.construct(ctor, vec![Value::Object(buf)], None)
        })
    });
}

pub fn setup_file(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "name",
        |vm, c| {
            let (_, _) = this_of(vm, &c.this, C::File)?;
            let Value::Object(o) = c.this else { return Ok(js("")) };
            Ok(slot(vm, o, B_NAME))
        },
        None,
    );
    attr(
        vm,
        proto,
        "lastModified",
        |vm, c| {
            let (_, _) = this_of(vm, &c.this, C::File)?;
            let Value::Object(o) = c.this else { return Ok(Value::Number(0.0)) };
            Ok(slot(vm, o, B_MODIFIED))
        },
        None,
    );
    attr(
        vm,
        proto,
        "webkitRelativePath",
        |vm, c| {
            this_of(vm, &c.this, C::File)?;
            Ok(js(""))
        },
        None,
    );
}

// ---- FileReader (File API §6) ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadKind {
    ArrayBuffer,
    BinaryString,
    Text,
    DataUrl,
}

/// A queued step of a read, each its own task: 0 fires loadstart, 1 progress, 2 the result
/// (load and loadend).
pub struct ReadTask {
    pub reader: ObjRef,
    pub blob: ObjRef,
    pub generation: u64,
    pub kind: ReadKind,
    pub label: Option<String>,
    pub step: u8,
}

/// A FileReader's state: EMPTY, LOADING, DONE, and the read generation (bumped by each
/// read and by abort(), so stale steps are dropped: "remove the tasks").
#[derive(Clone, Copy, Default)]
pub struct ReaderState {
    pub state: u8,
    pub generation: u64,
}

// FileReader slots: its result and its error.
const FR_RESULT: usize = 0;
const FR_ERROR: usize = 1;

pub fn file_reader_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("FileReader requires 'new'") };
    let h = host(vm)?;
    let proto = proto_of(vm, nt)?.unwrap_or_else(|| h.proto(C::FileReader));
    let id = {
        let mut r = h.readers.borrow_mut();
        r.push(ReaderState::default());
        r.len() as u64 - 1
    };
    let o = vm.new_host_object(Some(proto), C::FileReader as u32, id, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = vec![Value::Null, Value::Null];
    }
    Ok(Value::Object(o))
}

fn reader_this(vm: &mut Vm, c: &NativeCall) -> JsResult<(std::rc::Rc<Host>, ObjRef, usize)> {
    let (_, id) = this_of(vm, &c.this, C::FileReader)?;
    let Value::Object(o) = c.this else { return vm.type_error("Illegal invocation") };
    Ok((host(vm)?, o, id as usize))
}

fn set_slot(vm: &mut Vm, o: ObjRef, i: usize, v: Value) {
    if let Some(ho) = vm.host_obj_mut(o) {
        if ho.slots.len() <= i {
            ho.slots.resize(i + 1, Value::Null);
        }
        ho.slots[i] = v;
    }
}

/// File API "read operation": steps 1–4 now, the rest in queued tasks.
fn read(vm: &mut Vm, c: &NativeCall, kind: ReadKind) -> R {
    let blob = c.arg(0);
    this_of(vm, &blob, C::Blob)?;
    let Value::Object(blob) = blob else { return vm.type_error("not a Blob") };
    let label = match c.arg(1) {
        Value::Undefined => None,
        v if kind == ReadKind::Text => Some(rstr(vm, &v)?),
        _ => None,
    };
    let (h, o, id) = reader_this(vm, c)?;
    let generation = {
        let mut rs = h.readers.borrow_mut();
        let Some(r) = rs.get_mut(id) else { return Ok(Value::Undefined) };
        if r.state == 1 {
            drop(rs);
            return throw(vm, "InvalidStateError", "the FileReader is already reading");
        }
        r.state = 1;
        r.generation += 1;
        r.generation
    };
    set_slot(vm, o, FR_RESULT, Value::Null);
    set_slot(vm, o, FR_ERROR, Value::Null);
    h.tasks.borrow_mut().push_back(Task::Read(Box::new(ReadTask {
        reader: o,
        blob,
        generation,
        kind,
        label,
        step: 0,
    })));
    Ok(Value::Undefined)
}

fn fire_progress(
    vm: &mut Vm,
    h: &Host,
    target: ObjRef,
    ty: &str,
    loaded: usize,
    total: usize,
) -> JsResult<()> {
    let e = crate::events::trusted_event(vm, h, C::ProgressEvent, ty, false, false);
    crate::events::set_field(vm, e, "lengthComputable", Value::Bool(true));
    crate::events::set_field(vm, e, "loaded", Value::Number(loaded as f64));
    crate::events::set_field(vm, e, "total", Value::Number(total as f64));
    crate::events::dispatch(vm, h, e, Value::Object(target))?;
    Ok(())
}

/// File API "package data" of `bytes` as `kind`.
fn package(vm: &mut Vm, bytes: Vec<u8>, ty: &str, kind: ReadKind, label: Option<&str>) -> Value {
    match kind {
        ReadKind::ArrayBuffer => Value::Object(vm.new_array_buffer(bytes)),
        ReadKind::BinaryString => Value::String(bytes.iter().map(|&b| b as u16).collect::<Vec<u16>>().into()),
        ReadKind::DataUrl => {
            let ty = if ty.is_empty() { "application/octet-stream" } else { ty };
            js(&format!("data:{ty};base64,{}", crate::window::base64_encode(&bytes)))
        }
        ReadKind::Text => {
            // The encoding: the argument's label, else the type's charset, else UTF-8; a BOM
            // overrides it (Encoding's "decode").
            let charset = ty.split(';').skip(1).find_map(|p| {
                let (k, v) = p.split_once('=')?;
                k.trim().eq_ignore_ascii_case("charset").then(|| v.trim().trim_matches('"').to_string())
            });
            let enc = label
                .and_then(|l| encoding_rs::Encoding::for_label(l.trim().as_bytes()))
                .or_else(|| charset.and_then(|c| encoding_rs::Encoding::for_label(c.as_bytes())))
                .unwrap_or(encoding_rs::UTF_8);
            let (t, _, _) = enc.decode(&bytes);
            js(&t)
        }
    }
}

/// Run a queued read step (called by the event loop).
pub fn run_read_task(vm: &mut Vm, h: &Host, t: ReadTask) -> JsResult<()> {
    let current = h
        .readers
        .borrow()
        .get(id_of(vm, t.reader))
        .is_some_and(|r| r.generation == t.generation && r.state == 1);
    if !current {
        return Ok(());
    }
    let Some((bytes, ty, _)) = blob_data(vm, &Value::Object(t.blob)) else { return Ok(()) };
    let n = bytes.len();
    if t.step < 2 {
        if t.step == 0 {
            fire_progress(vm, h, t.reader, "loadstart", 0, n)?;
        } else {
            fire_progress(vm, h, t.reader, "progress", n, n)?;
        }
        // No progress event for an empty blob: no data was read.
        let next = if t.step == 0 && n > 0 { 1 } else { 2 };
        let still = h.readers.borrow().get(id_of(vm, t.reader)).is_some_and(|r| r.generation == t.generation);
        if still {
            h.tasks.borrow_mut().push_back(Task::Read(Box::new(ReadTask { step: next, ..t })));
        }
        return Ok(());
    }
    let result = package(vm, bytes, &ty, t.kind, t.label.as_deref());
    if let Some(r) = h.readers.borrow_mut().get_mut(id_of(vm, t.reader)) {
        r.state = 2;
    }
    set_slot(vm, t.reader, FR_RESULT, result);
    fire_progress(vm, h, t.reader, "load", n, n)?;
    let loading = h.readers.borrow().get(id_of(vm, t.reader)).is_some_and(|r| r.state == 1);
    if !loading {
        fire_progress(vm, h, t.reader, "loadend", n, n)?;
    }
    Ok(())
}

fn id_of(vm: &Vm, o: ObjRef) -> usize {
    vm.host_data(&Value::Object(o)).map_or(usize::MAX, |(_, id)| id as usize)
}

pub fn setup_file_reader(vm: &mut Vm, proto: ObjRef, ctor: ObjRef) {
    for (name, v) in [("EMPTY", 0.0), ("LOADING", 1.0), ("DONE", 2.0)] {
        constant(vm, proto, name, Value::Number(v));
        constant(vm, ctor, name, Value::Number(v));
    }
    op(vm, proto, "readAsArrayBuffer", 1, |vm, c| read(vm, &c, ReadKind::ArrayBuffer));
    op(vm, proto, "readAsBinaryString", 1, |vm, c| read(vm, &c, ReadKind::BinaryString));
    op(vm, proto, "readAsText", 1, |vm, c| read(vm, &c, ReadKind::Text));
    op(vm, proto, "readAsDataURL", 1, |vm, c| read(vm, &c, ReadKind::DataUrl));
    // File API §6.3 abort().
    op(vm, proto, "abort", 0, |vm, c| {
        let (h, o, id) = reader_this(vm, &c)?;
        let state = h.readers.borrow().get(id).map_or(0, |r| r.state);
        if state == 0 || state == 2 {
            set_slot(vm, o, FR_RESULT, Value::Null);
            return Ok(Value::Undefined);
        }
        if let Some(r) = h.readers.borrow_mut().get_mut(id) {
            r.state = 2;
            r.generation += 1;
        }
        set_slot(vm, o, FR_RESULT, Value::Null);
        fire_progress(vm, &h, o, "abort", 0, 0)?;
        let loading = h.readers.borrow().get(id).is_some_and(|r| r.state == 1);
        if !loading {
            fire_progress(vm, &h, o, "loadend", 0, 0)?;
        }
        Ok(Value::Undefined)
    });
    attr(
        vm,
        proto,
        "readyState",
        |vm, c| {
            let (h, _, id) = reader_this(vm, &c)?;
            let s = h.readers.borrow().get(id).map_or(0, |r| r.state);
            Ok(Value::Number(s as f64))
        },
        None,
    );
    attr(
        vm,
        proto,
        "result",
        |vm, c| {
            let (_, o, _) = reader_this(vm, &c)?;
            Ok(slot(vm, o, FR_RESULT))
        },
        None,
    );
    attr(
        vm,
        proto,
        "error",
        |vm, c| {
            let (_, o, _) = reader_this(vm, &c)?;
            Ok(slot(vm, o, FR_ERROR))
        },
        None,
    );
    crate::events::setup_named_handlers(
        vm,
        proto,
        &["onloadstart", "onprogress", "onload", "onabort", "onerror", "onloadend"],
    );
}

// ---- FormData (XHR §5) ----

/// A FormData's entry list.
pub fn entries(vm: &Vm, o: ObjRef) -> Vec<(String, Value)> {
    let slots = vm.host_obj(o).map(|h| h.slots.clone()).unwrap_or_default();
    slots
        .chunks(2)
        .filter_map(|p| match p {
            [Value::String(n), v] => Some((bb_js::vm::to_rust(n), v.clone())),
            _ => None,
        })
        .collect()
}

fn store(vm: &mut Vm, o: ObjRef, list: Vec<(String, Value)>) {
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = list.into_iter().flat_map(|(n, v)| [js(&n), v]).collect();
    }
}

/// The entry list as form data entries (for the encodings).
pub fn entry_values(vm: &Vm, o: ObjRef) -> Vec<(String, bb_dom::form_data::EntryValue)> {
    entries(vm, o)
        .into_iter()
        .map(|(n, v)| {
            let ev = match blob_data(vm, &v) {
                Some((bytes, ty, name)) => bb_dom::form_data::EntryValue::File {
                    name: name.unwrap_or_default(),
                    content_type: ty,
                    bytes,
                },
                None => bb_dom::form_data::EntryValue::Text(match &v {
                    Value::String(s) => bb_js::vm::to_rust(s),
                    _ => String::new(),
                }),
            };
            (n, ev)
        })
        .collect()
}

/// A FormData object over `list` (entries already made).
pub fn new_form_data(vm: &mut Vm, h: &Host, list: Vec<(String, Value)>, proto: Option<ObjRef>) -> ObjRef {
    let proto = proto.unwrap_or_else(|| h.proto(C::FormData));
    let o = vm.new_host_object(Some(proto), C::FormData as u32, 0, false);
    store(vm, o, list);
    o
}

/// XHR §5 "create an entry": a string, or a File (a Blob becomes a File named "blob"; a
/// filename names a new File).
fn create_entry(vm: &mut Vm, h: &Host, value: &Value, filename: Option<String>) -> JsResult<Value> {
    if let Some((bytes, ty, name)) = blob_data(vm, value) {
        let is_file = name.is_some();
        if is_file && filename.is_none() {
            return Ok(value.clone());
        }
        let name = filename.or(name).unwrap_or_else(|| "blob".to_string());
        let modified = match value {
            Value::Object(o) if is_file => match slot(vm, *o, B_MODIFIED) {
                Value::Number(n) => n,
                _ => 0.0,
            },
            _ => now_ms(),
        };
        return Ok(Value::Object(new_blob(vm, h, bytes, &ty, Some((&name, modified)), None)));
    }
    Ok(js(&rstr(vm, value)?))
}

pub fn form_data_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("FormData requires 'new'") };
    let h = host(vm)?;
    let proto = proto_of(vm, nt)?;
    let mut list = Vec::new();
    let form = c.arg(0);
    let mut form_node = None;
    if !matches!(form, Value::Undefined) {
        let Some(f) = crate::host::node_of(vm, &form)
            .filter(|_| vm.host_data(&form).is_some_and(|(cl, _)| cl == C::HTMLFormElement as u32))
        else {
            return vm.type_error("FormData's argument is not a form");
        };
        // HTML §4.10.21.4 with the submitter, which must be a submit button of this form.
        let submitter = match c.arg(1) {
            Value::Undefined | Value::Null => None,
            s => {
                let Some(sn) = crate::host::node_of(vm, &s) else {
                    return vm.type_error("the submitter is not an element");
                };
                let (is_submit, owner) = crate::host::with_doc(vm, &h, sn.doc, |d| {
                    let b = (d.is_html(sn.id, "button")
                        && !d.attr(sn.id, "type").is_some_and(|t| {
                            let t = t.trim().to_ascii_lowercase();
                            t == "button" || t == "reset"
                        }))
                        || (d.is_html(sn.id, "input")
                            && matches!(
                                d.input_type(sn.id),
                                bb_dom::InputType::Submit | bb_dom::InputType::Image
                            ));
                    (b, d.form_owner(sn.id))
                })?;
                if !is_submit {
                    return vm.type_error("the submitter is not a submit button");
                }
                if sn.doc != f.doc || owner != Some(f.id) {
                    return throw(vm, "NotFoundError", "the submitter isn't in this form");
                }
                Some(sn.id)
            }
        };
        let raw = crate::host::with_doc(vm, &h, f.doc, |d| {
            bb_dom::form_data::entry_list(d, f.id, submitter, None, encoding_rs::UTF_8)
        })?;
        for (name, v) in raw {
            let value = match v {
                bb_dom::form_data::EntryValue::Text(t) => js(&t),
                bb_dom::form_data::EntryValue::File { name: fname, content_type, bytes } => {
                    let fname = if fname.is_empty() { String::new() } else { fname };
                    Value::Object(new_blob(vm, &h, bytes, &content_type, Some((&fname, 0.0)), None))
                }
            };
            list.push((name, value));
        }
        form_node = Some(f);
    }
    let o = new_form_data(vm, &h, list, proto);
    // HTML §4.10.21.4 step 6: the formdata event, whose listeners can change the list.
    if let Some(f) = form_node {
        let e = crate::events::trusted_event(vm, &h, C::FormDataEvent, "formdata", true, false);
        crate::events::set_field(vm, e, "formData", Value::Object(o));
        let t = crate::host::wrap(vm, &h, f)?;
        crate::events::dispatch(vm, &h, e, t)?;
    }
    Ok(Value::Object(o))
}

fn fd_this(vm: &mut Vm, c: &NativeCall) -> JsResult<ObjRef> {
    this_of(vm, &c.this, C::FormData)?;
    match c.this {
        Value::Object(o) => Ok(o),
        _ => vm.type_error("Illegal invocation"),
    }
}

fn append_or_set(vm: &mut Vm, c: &NativeCall, set: bool) -> R {
    if c.args.len() < 2 {
        return vm.type_error("FormData needs a name and a value");
    }
    let name = rstr(vm, &c.arg(0))?;
    let is_blob = blob_data(vm, &c.arg(1)).is_some();
    if c.args.len() > 2 && !is_blob {
        return vm.type_error("a filename needs a Blob value");
    }
    let filename = if is_blob && c.args.len() > 2 { Some(rstr(vm, &c.arg(2))?) } else { None };
    let o = fd_this(vm, c)?;
    let h = host(vm)?;
    let value = create_entry(vm, &h, &c.arg(1), filename)?;
    let mut list = entries(vm, o);
    if set {
        match list.iter().position(|(n, _)| *n == name) {
            Some(i) => {
                list[i].1 = value;
                let mut k = 0;
                list.retain(|(n, _)| {
                    let keep = *n != name || k == i;
                    k += 1;
                    keep
                });
            }
            None => list.push((name, value)),
        }
    } else {
        list.push((name, value));
    }
    store(vm, o, list);
    Ok(Value::Undefined)
}

pub fn setup_form_data(vm: &mut Vm, proto: ObjRef) {
    install_fd_iterator_proto(vm);
    op(vm, proto, "append", 2, |vm, c| append_or_set(vm, &c, false));
    op(vm, proto, "set", 2, |vm, c| append_or_set(vm, &c, true));
    op(vm, proto, "delete", 1, |vm, c| {
        let name = rstr(vm, &c.arg(0))?;
        let o = fd_this(vm, &c)?;
        let mut list = entries(vm, o);
        list.retain(|(n, _)| *n != name);
        store(vm, o, list);
        Ok(Value::Undefined)
    });
    op(vm, proto, "get", 1, |vm, c| {
        let name = rstr(vm, &c.arg(0))?;
        let o = fd_this(vm, &c)?;
        Ok(entries(vm, o).into_iter().find(|(n, _)| *n == name).map_or(Value::Null, |(_, v)| v))
    });
    op(vm, proto, "getAll", 1, |vm, c| {
        let name = rstr(vm, &c.arg(0))?;
        let o = fd_this(vm, &c)?;
        let vals: Vec<Value> =
            entries(vm, o).into_iter().filter(|(n, _)| *n == name).map(|(_, v)| v).collect();
        Ok(Value::Object(vm.new_array(vals)))
    });
    op(vm, proto, "has", 1, |vm, c| {
        let name = rstr(vm, &c.arg(0))?;
        let o = fd_this(vm, &c)?;
        Ok(Value::Bool(entries(vm, o).iter().any(|(n, _)| *n == name)))
    });
    // iterable<USVString, FormDataEntryValue>: iterators over a snapshot of the list.
    for (name, kind) in [("entries", 0.0), ("keys", 1.0), ("values", 2.0)] {
        let f = realm::native_with(vm, name, 0, fd_iter, false, vec![Value::Number(kind)]);
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
        let o = fd_this(vm, &c)?;
        for (k, v) in entries(vm, o) {
            vm.call(&f, c.arg(1), vec![v, js(&k), c.this.clone()])?;
        }
        Ok(Value::Undefined)
    });
}

/// The class of FormData's iterator objects (WebIDL default iterator objects; no interface).
const FD_ITER: u32 = 0xffff_0002;

/// entries()/keys()/values(): a live pair iterator (WebIDL §3.7.9.1) over the entry list.
fn fd_iter(vm: &mut Vm, c: NativeCall) -> R {
    let kind = match crate::idl::callee_slots(vm, c.callee).first() {
        Some(Value::Number(n)) => *n,
        _ => 0.0,
    };
    let o = fd_this(vm, &c)?;
    let h = host(vm)?;
    let proto = match h.misc.borrow().get("FormDataIterator").copied() {
        Some(p) => p,
        None => return vm.type_error("no FormData iterator prototype"),
    };
    let it = vm.new_host_object(Some(proto), FD_ITER, 0, false);
    if let Some(ho) = vm.host_obj_mut(it) {
        ho.slots = vec![Value::Object(o), Value::Number(kind), Value::Number(0.0)];
    }
    Ok(Value::Object(it))
}

fn fd_iter_next(vm: &mut Vm, c: NativeCall) -> R {
    let Value::Object(it) = c.this else { return vm.type_error("not a FormData iterator") };
    if vm.host_data(&c.this).map(|(k, _)| k) != Some(FD_ITER) {
        return vm.type_error("not a FormData iterator");
    }
    let (fd, kind, index) = match vm.host_obj(it).map(|h| h.slots.clone()).as_deref() {
        Some([Value::Object(fd), Value::Number(k), Value::Number(i)]) => (*fd, *k as u8, *i as usize),
        _ => return vm.type_error("not a FormData iterator"),
    };
    let list = entries(vm, fd);
    let result = vm.new_object();
    let Some((k, v)) = list.into_iter().nth(index) else {
        vm.create_data_property(result, PropKey::lit("value"), Value::Undefined)?;
        vm.create_data_property(result, PropKey::lit("done"), Value::Bool(true))?;
        return Ok(Value::Object(result));
    };
    if let Some(ho) = vm.host_obj_mut(it) {
        ho.slots[2] = Value::Number((index + 1) as f64);
    }
    let value = match kind {
        1 => js(&k),
        2 => v,
        _ => Value::Object(vm.new_array(vec![js(&k), v])),
    };
    vm.create_data_property(result, PropKey::lit("value"), value)?;
    vm.create_data_property(result, PropKey::lit("done"), Value::Bool(false))?;
    Ok(Value::Object(result))
}

/// The FormData iterator prototype (its [[Prototype]] is %IteratorPrototype%).
fn install_fd_iterator_proto(vm: &mut Vm) {
    let Ok(h) = host(vm) else { return };
    let ip = vm.realm.iterator_proto;
    let p = vm.new_object_with_proto(Some(ip));
    let next = realm::native(vm, "next", 0, fd_iter_next);
    vm.heap.obj_mut(p).props.insert(PropKey::lit("next"), bb_js::vm::heap::Prop::method(Value::Object(next)));
    realm::to_string_tag(vm, p, "FormData Iterator");
    h.misc.borrow_mut().insert("FormDataIterator", p);
}

/// FormDataEvent (HTML §4.10.21.4): `new FormDataEvent(type, { formData })`.
pub fn setup_form_data_event(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "formData",
        |vm, c| {
            this_of(vm, &c.this, C::FormDataEvent)?;
            let Value::Object(e) = c.this else { return Ok(Value::Null) };
            Ok(crate::events::get_field(vm, e, "formData").unwrap_or(Value::Null))
        },
        None,
    );
}

/// The multipart/form-data body of a FormData (HTML §4.10.21.8, UTF-8), and its type.
pub fn multipart_body(vm: &Vm, o: ObjRef) -> (Vec<u8>, String) {
    let list = entry_values(vm, o);
    let boundary = new_boundary();
    let body = bb_dom::form_data::multipart(&list, encoding_rs::UTF_8, &boundary);
    (body, format!("multipart/form-data; boundary={boundary}"))
}

/// A multipart boundary: 128 random bits (RFC 7578 §4.1).
pub fn new_boundary() -> String {
    let mut b = [0u8; 16];
    random_bytes(&mut b);
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("----BannedBrowserFormBoundary{hex}")
}

/// Parse a body as form data (Fetch §5.1 body mixin formData(), per Content-Type `ty`).
pub fn parse_form_body(vm: &mut Vm, h: &Host, bytes: &[u8], ty: &str) -> R {
    let essence = ty.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let mut list: Vec<(String, Value)> = Vec::new();
    if essence == "application/x-www-form-urlencoded" {
        let (s, _, _) = encoding_rs::UTF_8.decode(bytes);
        for (k, v) in bb_url::form_urldecode(&s) {
            list.push((k, js(&v)));
        }
    } else if essence == "multipart/form-data" {
        let boundary = ty.split(';').skip(1).find_map(|p| {
            let (k, v) = p.split_once('=')?;
            k.trim().eq_ignore_ascii_case("boundary").then(|| v.trim().trim_matches('"').to_string())
        });
        let Some(boundary) = boundary.filter(|b| !b.is_empty()) else {
            return vm.type_error("the multipart body has no boundary");
        };
        let Some(parts) = parse_multipart(bytes, &boundary) else {
            return vm.type_error("the multipart body couldn't be parsed");
        };
        for p in parts {
            let v = match p.filename {
                Some(f) => {
                    let ty = p.content_type.unwrap_or_else(|| "text/plain".to_string());
                    Value::Object(new_blob(vm, h, p.body, &ty, Some((&f, 0.0)), None))
                }
                None => {
                    let (t, _, _) = encoding_rs::UTF_8.decode(&p.body);
                    js(&t)
                }
            };
            list.push((p.name, v));
        }
    } else {
        return vm.type_error("the body isn't form data");
    }
    Ok(Value::Object(new_form_data(vm, h, list, None)))
}

struct Part {
    name: String,
    filename: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

/// RFC 7578 multipart/form-data parsing.
fn parse_multipart(b: &[u8], boundary: &str) -> Option<Vec<Part>> {
    let delim = format!("--{boundary}").into_bytes();
    let mut i = find(b, &delim, 0)? + delim.len();
    let mut out = Vec::new();
    loop {
        if b.get(i..i + 2) == Some(b"--") {
            return Some(out);
        }
        if b.get(i..i + 2) != Some(b"\r\n") {
            return None;
        }
        i += 2;
        let head_end = find(b, b"\r\n\r\n", i)?;
        let head = String::from_utf8_lossy(&b[i..head_end]).into_owned();
        let body_start = head_end + 4;
        let mut next_delim = b"\r\n".to_vec();
        next_delim.extend_from_slice(&delim);
        let body_end = find(b, &next_delim, body_start)?;
        let (mut name, mut filename, mut content_type) = (None, None, None);
        for line in head.split("\r\n") {
            let (k, v) = line.split_once(':')?;
            let k = k.trim().to_ascii_lowercase();
            if k == "content-disposition" {
                for p in v.split(';').skip(1) {
                    let Some((pk, pv)) = p.split_once('=') else { continue };
                    let pv = pv
                        .trim()
                        .trim_matches('"')
                        .replace("%22", "\"")
                        .replace("%0D", "\r")
                        .replace("%0A", "\n");
                    match pk.trim() {
                        "name" => name = Some(pv),
                        "filename" => filename = Some(pv),
                        _ => {}
                    }
                }
            } else if k == "content-type" {
                content_type = Some(v.trim().to_string());
            }
        }
        out.push(Part { name: name?, filename, content_type, body: b[body_start..body_end].to_vec() });
        i = body_end + next_delim.len();
    }
}

// ---- blob URLs (File API §8) ----

/// URL.createObjectURL and URL.revokeObjectURL (static operations on URL).
pub fn setup_url_statics(vm: &mut Vm, ctor: ObjRef) {
    op(vm, ctor, "createObjectURL", 1, |vm, c| {
        if blob_data(vm, &c.arg(0)).is_none() {
            return vm.type_error("createObjectURL needs a Blob");
        }
        let h = host(vm)?;
        let origin = h.url.borrow().origin().serialize();
        let url = format!("blob:{origin}/{}", uuid());
        h.blob_urls.borrow_mut().insert(url.clone(), c.arg(0));
        Ok(js(&url))
    });
    op(vm, ctor, "revokeObjectURL", 1, |vm, c| {
        let url = rstr(vm, &c.arg(0))?;
        let h = host(vm)?;
        if let Ok(u) = bb_url::Url::parse(&url) {
            if u.scheme() == "blob" {
                let mut key = u.href();
                if let Some(i) = key.find('#') {
                    key.truncate(i);
                }
                h.blob_urls.borrow_mut().remove(&key);
            }
        }
        Ok(Value::Undefined)
    });
}

/// Fetch "scheme fetch" for `blob:` URLs: the stored blob as a 200 response, or a
/// network error. None when `req` isn't a blob URL.
pub fn blob_fetch(
    vm: &Vm,
    h: &Host,
    req: &crate::net::HttpRequest,
) -> Option<Result<crate::net::HttpResponse, String>> {
    if req.url.scheme() != "blob" {
        return None;
    }
    let mut key = req.url.href();
    if let Some(i) = key.find('#') {
        key.truncate(i);
    }
    if req.method != "GET" {
        return Some(Err("a blob URL can only be fetched with GET".into()));
    }
    let blob = h.blob_urls.borrow().get(&key).cloned();
    let Some((bytes, ty, _)) = blob.and_then(|b| blob_data(vm, &b)) else {
        return Some(Err("the blob URL isn't registered".into()));
    };
    Some(Ok(crate::net::HttpResponse {
        url: req.url.clone(),
        status: 200,
        status_text: "OK".into(),
        headers: vec![("content-length".into(), bytes.len().to_string()), ("content-type".into(), ty)],
        body: bytes,
    }))
}

// ---- crypto (Web Crypto §10) ----

/// Fill `b` from the operating system's cryptographically secure generator.
pub fn random_bytes(b: &mut [u8]) {
    if getrandom::fill(b).is_err() {
        // The OS refused: fall back to hashing time-seeded state (not reached in practice).
        use std::hash::{BuildHasher, Hasher};
        for (i, chunk) in b.chunks_mut(8).enumerate() {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_usize(i);
            let v = h.finish().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
    }
}

/// A version 4 UUID (RFC 9562 §5.4), lowercase.
pub fn uuid() -> String {
    let mut b = [0u8; 16];
    random_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

pub fn setup_crypto(vm: &mut Vm, proto: ObjRef) {
    // Web Crypto §10.1.1 getRandomValues(array).
    op(vm, proto, "getRandomValues", 1, |vm, c| {
        this_of(vm, &c.this, C::Crypto)?;
        let a = c.arg(0);
        let Value::Object(o) = a else { return throw(vm, "TypeMismatchError", "not an integer typed array") };
        let Some((kind, buf, off, len)) = vm.ta_view(o) else {
            return throw(vm, "TypeMismatchError", "not an integer typed array");
        };
        if matches!(kind, TaKind::Float16 | TaKind::Float32 | TaKind::Float64) {
            return throw(vm, "TypeMismatchError", "not an integer typed array");
        }
        if len > 65536 {
            return throw(vm, "QuotaExceededError", "at most 65536 bytes at a time");
        }
        let mut bytes = vec![0u8; len];
        random_bytes(&mut bytes);
        if let Some(b) = vm.buffer_bytes_mut(buf) {
            if let Some(dst) = b.get_mut(off..off + len) {
                dst.copy_from_slice(&bytes);
            }
        }
        Ok(a)
    });
    op(vm, proto, "randomUUID", 0, |vm, c| {
        this_of(vm, &c.this, C::Crypto)?;
        Ok(js(&uuid()))
    });
}

/// The `crypto` attribute of the Window (one Crypto object).
pub fn install_crypto(vm: &mut Vm, h: &Host) {
    let o = vm.new_host_object(Some(h.proto(C::Crypto)), C::Crypto as u32, 0, false);
    h.misc.borrow_mut().insert("crypto", o);
    let g = vm.realm.global;
    attr(
        vm,
        g,
        "crypto",
        |vm, _c| {
            let h = host(vm)?;
            let o = h.misc.borrow().get("crypto").copied();
            Ok(o.map_or(Value::Undefined, Value::Object))
        },
        None,
    );
}

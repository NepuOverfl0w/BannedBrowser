//! The structured clone algorithm (HTML §2.7): values serialized into a [`Serialized`]
//! record that can cross threads (postMessage to workers) and deserialized in another realm
//! or agent. Platform objects are serialized by the host's hooks (`Vm::host_serialize`,
//! `Vm::host_deserialize`); a value that can't be cloned throws the host's DataCloneError
//! (`Vm::host_clone_error`, a TypeError without one).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use super::heap::{Kind, ObjRef, Object};
use super::object::PropDesc;
use super::typed::{Buffer, DataView, SharedBlock, TaKind, TypedArray};
use super::value::{PropKey, Value};
use super::{ErrorKind, JsResult, Vm};

/// A platform object's serialization (its interface id and whatever data it needs).
#[derive(Clone, Debug, Default)]
pub struct HostSer {
    pub class: u32,
    pub bytes: Vec<u8>,
    pub strings: Vec<String>,
    pub numbers: Vec<f64>,
}

/// A serialized value. Objects are numbered in the order they were first met; a later
/// occurrence is a [`Ser::Ref`] (so cycles and shared references survive).
#[derive(Clone)]
pub enum Ser {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    BigInt(String),
    String(Vec<u16>),
    Ref(u32),
    Object(u32, Box<SerObj>),
}

#[derive(Clone)]
pub enum SerObj {
    BoolObj(bool),
    NumObj(f64),
    BigIntObj(String),
    StrObj(Vec<u16>),
    Date(f64),
    RegExp(Vec<u16>, Vec<u16>),
    ArrayBuffer {
        bytes: Vec<u8>,
        max: Option<usize>,
    },
    /// An ArrayBuffer in the transfer list: index into [`Serialized::transferred`].
    Transferred(u32),
    Shared(Arc<SharedBlock>, Option<usize>),
    /// A typed array (`kind`) or a DataView (`None`).
    View {
        kind: Option<TaKind>,
        buffer: Ser,
        offset: usize,
        length: Option<usize>,
    },
    Map(Vec<(Ser, Ser)>),
    Set(Vec<Ser>),
    Error {
        kind: ErrorKind,
        message: Option<Vec<u16>>,
        cause: Option<Ser>,
    },
    Array {
        length: u32,
        props: Vec<(Vec<u16>, Ser)>,
    },
    Object(Vec<(Vec<u16>, Ser)>),
    Host(HostSer),
    /// A transferred platform object (a MessagePort): index into
    /// [`Serialized::host_transferred`].
    HostTransfer(u32),
}

/// What StructuredSerializeWithTransfer makes: the value, the transferred buffers, and the
/// transferred platform objects (what the host's `host_transfer` made of them).
pub struct Serialized {
    pub root: Ser,
    pub transferred: Vec<(Vec<u8>, Option<usize>)>,
    pub host_transferred: Vec<Box<dyn std::any::Any + Send>>,
}

// A Serialized crosses threads (that is its point): checked here at compile time.
const _: fn() = || {
    fn send<T: Send>() {}
    send::<Serialized>();
};

/// Nesting past this is a RangeError rather than a native stack overflow.
const MAX_DEPTH: usize = 4096;

fn key_units(k: &PropKey) -> Option<Vec<u16>> {
    match k {
        PropKey::Index(i) => Some(i.to_string().encode_utf16().collect()),
        PropKey::Str(s) => Some(s.to_vec()),
        PropKey::Sym(_) => None,
    }
}

struct SerState {
    memory: HashMap<ObjRef, u32>,
    next: u32,
    transfer: Vec<ObjRef>,
    host_transfer: Vec<ObjRef>,
    depth: usize,
}

impl Vm {
    fn clone_error<T>(&mut self, msg: &str) -> JsResult<T> {
        match self.host_clone_error {
            Some(f) => Err(f(self, msg)),
            None => self.type_error(msg),
        }
    }

    /// HTML §2.7.4 StructuredSerializeWithTransfer (ArrayBuffers in the transfer list are
    /// detached here and travel in [`Serialized::transferred`]).
    pub fn structured_serialize(&mut self, v: &Value, transfer: &[ObjRef]) -> JsResult<Serialized> {
        let mut st = SerState {
            memory: HashMap::new(),
            next: 0,
            transfer: Vec::new(),
            host_transfer: Vec::new(),
            depth: 0,
        };
        // Steps 1–2: the transfer list holds distinct transferables: non-shared ArrayBuffers
        // and the host's transferable platform objects.
        for (i, &t) in transfer.iter().enumerate() {
            if transfer[..i].contains(&t) {
                return self.clone_error("an object is in the transfer list twice");
            }
            let buffer =
                matches!(&self.heap.obj(t).kind, Kind::ArrayBuffer(b) if !b.shared && b.bytes.is_some());
            if buffer {
                st.transfer.push(t);
                continue;
            }
            if self.host_transferable.is_some_and(|f| f(self, t)) {
                st.host_transfer.push(t);
                continue;
            }
            return self.clone_error("only unshared ArrayBuffers and message ports can be transferred");
        }
        let root = self.ser(v, &mut st)?;
        // Step 5: detach the transferred buffers.
        let mut transferred = Vec::with_capacity(transfer.len());
        for &t in &st.transfer {
            let Kind::ArrayBuffer(b) = &mut self.heap.obj_mut(t).kind else { continue };
            let max = b.max;
            match b.bytes.take() {
                Some(bytes) => transferred.push((bytes, max)),
                None => return self.clone_error("a transferred ArrayBuffer was detached"),
            }
        }
        let mut host_transferred = Vec::with_capacity(st.host_transfer.len());
        for &t in &st.host_transfer {
            match self.host_transfer {
                Some(f) => host_transferred.push(f(self, t)?),
                None => return self.clone_error("this object can't be transferred"),
            }
        }
        Ok(Serialized { root, transferred, host_transferred })
    }

    fn ser(&mut self, v: &Value, st: &mut SerState) -> JsResult<Ser> {
        let o = match v {
            Value::Undefined => return Ok(Ser::Undefined),
            Value::Null => return Ok(Ser::Null),
            Value::Bool(b) => return Ok(Ser::Bool(*b)),
            Value::Number(n) => return Ok(Ser::Number(*n)),
            Value::BigInt(b) => return Ok(Ser::BigInt(b.to_string_radix(10))),
            Value::String(s) => return Ok(Ser::String(s.to_vec())),
            Value::Object(o) => *o,
            _ => return self.clone_error("a symbol can't be cloned"),
        };
        if let Some(&id) = st.memory.get(&o) {
            return Ok(Ser::Ref(id));
        }
        if let Some(i) = st.host_transfer.iter().position(|&t| t == o) {
            let id = st.next;
            st.next += 1;
            st.memory.insert(o, id);
            return Ok(Ser::Object(id, Box::new(SerObj::HostTransfer(i as u32))));
        }
        st.depth += 1;
        if st.depth > MAX_DEPTH {
            return self.range_error("the value is nested too deeply to clone");
        }
        let id = st.next;
        st.next += 1;
        st.memory.insert(o, id);
        let r = self.ser_object(o, st);
        st.depth -= 1;
        Ok(Ser::Object(id, Box::new(r?)))
    }

    fn ser_object(&mut self, o: ObjRef, st: &mut SerState) -> JsResult<SerObj> {
        // HTML §2.7.3 StructuredSerializeInternal steps 6–26, by kind.
        enum K {
            Prim(SerObj),
            Buffer,
            View(Option<TaKind>, ObjRef, usize, Option<usize>),
            Map(Vec<(Value, Value)>),
            Set(Vec<Value>),
            Error,
            Array,
            Ordinary,
            Host,
            Bad(&'static str),
        }
        let k = match &self.heap.obj(o).kind {
            Kind::Boolean(b) => K::Prim(SerObj::BoolObj(*b)),
            Kind::Number(n) => K::Prim(SerObj::NumObj(*n)),
            Kind::BigInt(b) => K::Prim(SerObj::BigIntObj(b.to_string_radix(10))),
            Kind::String(s) => K::Prim(SerObj::StrObj(s.to_vec())),
            Kind::Date(t) => K::Prim(SerObj::Date(*t)),
            Kind::RegExp(r) => K::Prim(SerObj::RegExp(r.source.to_vec(), r.flags.to_vec())),
            Kind::ArrayBuffer(_) => K::Buffer,
            Kind::TypedArray(t) => K::View(Some(t.kind), t.buffer, t.offset, t.length),
            Kind::DataView(d) => K::View(None, d.buffer, d.offset, d.length),
            Kind::Map(m) => K::Map(m.entries.iter().flatten().cloned().collect()),
            Kind::Set(m) => K::Set(m.entries.iter().flatten().map(|e| e.0.clone()).collect()),
            Kind::Error(_) => K::Error,
            Kind::Array => K::Array,
            Kind::Ordinary => K::Ordinary,
            Kind::Host(_) => K::Host,
            Kind::Function(_) | Kind::Native(_) | Kind::Bound(_) => K::Bad("a function can't be cloned"),
            Kind::Proxy(_) => K::Bad("a proxy can't be cloned"),
            _ => K::Bad("this object can't be cloned"),
        };
        Ok(match k {
            K::Prim(p) => p,
            K::Bad(m) => return self.clone_error(m),
            K::Buffer => {
                if let Some(i) = st.transfer.iter().position(|&t| t == o) {
                    return Ok(SerObj::Transferred(i as u32));
                }
                let Kind::ArrayBuffer(b) = &self.heap.obj(o).kind else {
                    return self.clone_error("not an ArrayBuffer");
                };
                if b.shared {
                    match &b.block {
                        Some(blk) => SerObj::Shared(blk.clone(), b.max),
                        None => return self.clone_error("a SharedArrayBuffer without memory"),
                    }
                } else {
                    match &b.bytes {
                        Some(bytes) => SerObj::ArrayBuffer { bytes: bytes.clone(), max: b.max },
                        None => return self.clone_error("a detached ArrayBuffer can't be cloned"),
                    }
                }
            }
            K::View(kind, buf, offset, length) => {
                // An out-of-bounds view (its buffer shrank or was detached) can't be cloned.
                let oob = match kind {
                    Some(_) => self.ta_view(o).is_none(),
                    None => {
                        let total = match &self.heap.obj(buf).kind {
                            Kind::ArrayBuffer(b) => {
                                b.bytes.as_ref().map(Vec::len).or_else(|| b.block.as_ref().map(|x| x.len()))
                            }
                            _ => None,
                        };
                        total.is_none_or(|t| offset > t || length.is_some_and(|l| offset + l > t))
                    }
                };
                if oob {
                    return self.clone_error("an out-of-bounds view can't be cloned");
                }
                let buffer = self.ser(&Value::Object(buf), st)?;
                SerObj::View { kind, buffer, offset, length }
            }
            K::Map(entries) => {
                let mut out = Vec::with_capacity(entries.len());
                for (k, v) in entries {
                    let k = self.ser(&k, st)?;
                    let v = self.ser(&v, st)?;
                    out.push((k, v));
                }
                SerObj::Map(out)
            }
            K::Set(items) => {
                let mut out = Vec::with_capacity(items.len());
                for v in items {
                    out.push(self.ser(&v, st)?);
                }
                SerObj::Set(out)
            }
            K::Error => {
                let name = self.get(o, &PropKey::lit("name"))?;
                let name = match &name {
                    Value::String(s) => String::from_utf16_lossy(s),
                    _ => String::new(),
                };
                let kind = match name.as_str() {
                    "EvalError" => ErrorKind::Eval,
                    "RangeError" => ErrorKind::Range,
                    "ReferenceError" => ErrorKind::Reference,
                    "SyntaxError" => ErrorKind::Syntax,
                    "TypeError" => ErrorKind::Type,
                    "URIError" => ErrorKind::Uri,
                    _ => ErrorKind::Error,
                };
                let message = match self.get_own_property(o, &PropKey::lit("message"))? {
                    Some(PropDesc { value: Some(v), .. }) => Some(self.to_string(&v)?.to_vec()),
                    _ => None,
                };
                let cause = match self.get_own_property(o, &PropKey::lit("cause"))? {
                    Some(PropDesc { value: Some(v), .. }) => Some(self.ser(&v, st)?),
                    _ => None,
                };
                SerObj::Error { kind, message, cause }
            }
            K::Array => {
                let len = self.get(o, &PropKey::lit("length"))?;
                let length = self.to_uint32(&len)?;
                SerObj::Array { length, props: self.ser_props(o, st, true)? }
            }
            K::Ordinary => SerObj::Object(self.ser_props(o, st, false)?),
            K::Host => match self.host_serialize {
                Some(f) => match f(self, o) {
                    Some(r) => SerObj::Host(r?),
                    None => return self.clone_error("this platform object can't be cloned"),
                },
                None => return self.clone_error("this platform object can't be cloned"),
            },
        })
    }

    /// The own enumerable string-keyed properties, read with [[Get]] (HTML §2.7.3 step 26).
    fn ser_props(&mut self, o: ObjRef, st: &mut SerState, array: bool) -> JsResult<Vec<(Vec<u16>, Ser)>> {
        let keys = self.own_property_keys(o)?;
        let mut out = Vec::new();
        for k in keys {
            let Some(units) = key_units(&k) else { continue };
            if array && units == "length".encode_utf16().collect::<Vec<u16>>() {
                continue;
            }
            match self.get_own_property(o, &k)? {
                Some(d) if d.enumerable == Some(true) => {}
                _ => continue,
            }
            let v = self.get(o, &k)?;
            out.push((units, self.ser(&v, st)?));
        }
        Ok(out)
    }

    /// HTML §2.7.6 StructuredDeserialize (with the transferred buffers of §2.7.7).
    pub fn structured_deserialize(&mut self, s: Serialized) -> JsResult<Value> {
        self.structured_deserialize_with_transfer(s).map(|(v, _)| v)
    }

    /// StructuredDeserializeWithTransfer (HTML §2.7.7): the value, and the transferred
    /// platform objects recreated (in transfer-list order: a MessageEvent's ports).
    pub fn structured_deserialize_with_transfer(&mut self, s: Serialized) -> JsResult<(Value, Vec<Value>)> {
        let mut revived = Vec::with_capacity(s.host_transferred.len());
        for t in s.host_transferred {
            match self.host_receive {
                Some(f) => revived.push(f(self, t)?),
                None => return self.clone_error("a transferred object can't be received here"),
            }
        }
        let mut memory: HashMap<u32, ObjRef> = HashMap::new();
        let mut transferred: Vec<Option<(Vec<u8>, Option<usize>)>> =
            s.transferred.into_iter().map(Some).collect();
        self.host_revived = revived;
        let r = self.de(&s.root, &mut memory, &mut transferred);
        let revived = std::mem::take(&mut self.host_revived);
        Ok((r?, revived))
    }

    fn de(
        &mut self,
        s: &Ser,
        memory: &mut HashMap<u32, ObjRef>,
        transferred: &mut [Option<(Vec<u8>, Option<usize>)>],
    ) -> JsResult<Value> {
        let (id, obj) = match s {
            Ser::Undefined => return Ok(Value::Undefined),
            Ser::Null => return Ok(Value::Null),
            Ser::Bool(b) => return Ok(Value::Bool(*b)),
            Ser::Number(n) => return Ok(Value::Number(*n)),
            Ser::BigInt(t) => return Ok(Value::BigInt(Rc::new(parse_big(t)))),
            Ser::String(u) => return Ok(Value::String(u.clone().into())),
            Ser::Ref(id) => {
                return match memory.get(id) {
                    Some(o) => Ok(Value::Object(*o)),
                    None => self.clone_error("a malformed serialization"),
                }
            }
            Ser::Object(id, obj) => (*id, obj),
        };
        let o = match &**obj {
            SerObj::BoolObj(b) => self.to_object(&Value::Bool(*b))?,
            SerObj::NumObj(n) => self.to_object(&Value::Number(*n))?,
            SerObj::BigIntObj(t) => self.to_object(&Value::BigInt(Rc::new(parse_big(t))))?,
            SerObj::StrObj(u) => self.to_object(&Value::String(u.clone().into()))?,
            SerObj::Date(t) => {
                let p = self.realm.date_proto;
                self.heap.alloc(Object::new(Some(p), Kind::Date(*t)))
            }
            SerObj::RegExp(src, flags) => self.regexp_create(src.clone().into(), flags.clone().into())?,
            SerObj::ArrayBuffer { bytes, max } => self.alloc_buffer(Some(bytes.clone()), None, *max),
            SerObj::Transferred(i) => {
                let Some((bytes, max)) = transferred.get_mut(*i as usize).and_then(Option::take) else {
                    return self.clone_error("a transferred buffer was used twice");
                };
                self.alloc_buffer(Some(bytes), None, max)
            }
            SerObj::Shared(blk, max) => self.alloc_buffer(None, Some(blk.clone()), *max),
            SerObj::View { .. }
            | SerObj::Map(_)
            | SerObj::Set(_)
            | SerObj::Array { .. }
            | SerObj::Object(_) => {
                // Containers: made first, so references inside them resolve to them.
                return self.de_container(id, obj, memory, transferred);
            }
            SerObj::Error { kind, message, cause } => {
                let p = self.realm.error_proto(*kind);
                let e = self.heap.alloc(Object::new(Some(p), Kind::Error(Rc::from(""))));
                memory.insert(id, e);
                if let Some(m) = message {
                    self.heap.obj_mut(e).props.insert(
                        PropKey::lit("message"),
                        super::heap::Prop::data(Value::String(m.clone().into()), true, false, true),
                    );
                }
                if let Some(c) = cause {
                    let v = self.de(c, memory, transferred)?;
                    self.heap
                        .obj_mut(e)
                        .props
                        .insert(PropKey::lit("cause"), super::heap::Prop::data(v, true, false, true));
                }
                e
            }
            SerObj::HostTransfer(i) => match self.host_revived.get(*i as usize) {
                Some(Value::Object(o)) => *o,
                _ => return self.clone_error("a malformed serialization"),
            },
            SerObj::Host(h) => match self.host_deserialize {
                Some(f) => match f(self, h)? {
                    Value::Object(o) => o,
                    _ => return self.clone_error("the platform object couldn't be recreated"),
                },
                None => return self.clone_error("the platform object can't be recreated here"),
            },
        };
        memory.insert(id, o);
        Ok(Value::Object(o))
    }

    fn de_container(
        &mut self,
        id: u32,
        obj: &SerObj,
        memory: &mut HashMap<u32, ObjRef>,
        transferred: &mut [Option<(Vec<u8>, Option<usize>)>],
    ) -> JsResult<Value> {
        match obj {
            SerObj::View { kind, buffer, offset, length } => {
                let b = match self.de(buffer, memory, transferred)? {
                    Value::Object(b) => b,
                    _ => return self.clone_error("a view without a buffer"),
                };
                let o = match kind {
                    Some(k) => {
                        let p = self.realm.ta_protos[k.index()];
                        let t = TypedArray { kind: *k, buffer: b, offset: *offset, length: *length };
                        self.heap.alloc(Object::new(Some(p), Kind::TypedArray(Box::new(t))))
                    }
                    None => {
                        let p = self.realm.dataview_proto;
                        let d = DataView { buffer: b, offset: *offset, length: *length };
                        self.heap.alloc(Object::new(Some(p), Kind::DataView(Box::new(d))))
                    }
                };
                memory.insert(id, o);
                Ok(Value::Object(o))
            }
            SerObj::Map(entries) => {
                let p = self.realm.map_proto;
                let o = self.heap.alloc(Object::new(Some(p), Kind::Map(Box::default())));
                memory.insert(id, o);
                for (k, v) in entries {
                    let k = self.de(k, memory, transferred)?;
                    let v = self.de(v, memory, transferred)?;
                    if let Kind::Map(m) = &mut self.heap.obj_mut(o).kind {
                        m.set(k, v);
                    }
                }
                Ok(Value::Object(o))
            }
            SerObj::Set(items) => {
                let p = self.realm.set_proto;
                let o = self.heap.alloc(Object::new(Some(p), Kind::Set(Box::default())));
                memory.insert(id, o);
                for v in items {
                    let v = self.de(v, memory, transferred)?;
                    if let Kind::Set(m) = &mut self.heap.obj_mut(o).kind {
                        m.set(v.clone(), v);
                    }
                }
                Ok(Value::Object(o))
            }
            SerObj::Array { length, props } => {
                let o = self.new_array(Vec::new());
                memory.insert(id, o);
                self.set(o, PropKey::lit("length"), Value::Number(*length as f64), Value::Object(o))?;
                for (k, v) in props {
                    let v = self.de(v, memory, transferred)?;
                    self.create_data_property(o, PropKey::from_js(k.clone().into()), v)?;
                }
                Ok(Value::Object(o))
            }
            SerObj::Object(props) => {
                let o = self.new_object();
                memory.insert(id, o);
                for (k, v) in props {
                    let v = self.de(v, memory, transferred)?;
                    self.create_data_property(o, PropKey::from_js(k.clone().into()), v)?;
                }
                Ok(Value::Object(o))
            }
            _ => self.clone_error("a malformed serialization"),
        }
    }

    fn alloc_buffer(
        &mut self,
        bytes: Option<Vec<u8>>,
        block: Option<Arc<SharedBlock>>,
        max: Option<usize>,
    ) -> ObjRef {
        let shared = block.is_some();
        let p = if shared { self.realm.shared_array_buffer_proto } else { self.realm.array_buffer_proto };
        let b = Buffer { bytes, block, max, shared, immutable: false };
        self.heap.alloc(Object::new(Some(p), Kind::ArrayBuffer(Box::new(b))))
    }
}

fn parse_big(t: &str) -> super::bigint::BigInt {
    match t.strip_prefix('-') {
        Some(d) => super::bigint::BigInt::parse_radix(d, 10).unwrap_or_default().neg(),
        None => super::bigint::BigInt::parse_radix(t, 10).unwrap_or_default(),
    }
}

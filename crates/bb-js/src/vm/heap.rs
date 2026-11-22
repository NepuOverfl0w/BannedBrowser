//! The garbage-collected heap: objects (ECMA-262 §6.1.7, §10) and environment records (§9.1)
//! live in arenas addressed by index; a mark-sweep collector frees what the roots can't reach.

use super::code::FuncTemplate;
use super::value::{JsString, PropKey, Sym, Value};
use super::Frame;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ObjRef(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EnvRef(pub u32);

/// A property's value part (§6.1.7.1): data or accessor.
#[derive(Clone, Debug)]
pub enum Slot {
    Data(Value),
    Accessor(Option<ObjRef>, Option<ObjRef>),
}

#[derive(Clone, Debug)]
pub struct Prop {
    pub slot: Slot,
    /// [[Writable]] (data properties only).
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
}

impl Prop {
    pub fn data(v: Value, writable: bool, enumerable: bool, configurable: bool) -> Prop {
        Prop { slot: Slot::Data(v), writable, enumerable, configurable }
    }
    /// writable, non-enumerable, configurable: built-in methods (§18).
    pub fn method(v: Value) -> Prop {
        Prop::data(v, true, false, true)
    }
}

/// Property storage keeping insertion order (§10.1.11.1 OrdinaryOwnPropertyKeys).
#[derive(Default, Debug, Clone)]
pub struct Props {
    map: HashMap<PropKey, usize>,
    entries: Vec<Option<(PropKey, Prop)>>,
}

impl Props {
    pub fn get(&self, k: &PropKey) -> Option<&Prop> {
        self.map.get(k).and_then(|&i| self.entries[i].as_ref().map(|e| &e.1))
    }
    pub fn get_mut(&mut self, k: &PropKey) -> Option<&mut Prop> {
        match self.map.get(k) {
            Some(&i) => self.entries[i].as_mut().map(|e| &mut e.1),
            None => None,
        }
    }
    pub fn contains(&self, k: &PropKey) -> bool {
        self.map.contains_key(k)
    }
    pub fn insert(&mut self, k: PropKey, p: Prop) {
        if let Some(&i) = self.map.get(&k) {
            self.entries[i] = Some((k, p));
        } else {
            self.map.insert(k.clone(), self.entries.len());
            self.entries.push(Some((k, p)));
        }
    }
    pub fn remove(&mut self, k: &PropKey) -> Option<Prop> {
        let i = self.map.remove(k)?;
        let e = self.entries[i].take();
        if self.entries.len() > 16 && self.map.len() * 2 < self.entries.len() {
            self.compact();
        }
        e.map(|e| e.1)
    }
    fn compact(&mut self) {
        self.entries.retain(|e| e.is_some());
        self.map.clear();
        for (i, e) in self.entries.iter().enumerate() {
            if let Some((k, _)) = e {
                self.map.insert(k.clone(), i);
            }
        }
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    /// Integer indices ascending, then strings, then symbols, each in creation order.
    pub fn keys(&self) -> Vec<PropKey> {
        let mut idx: Vec<u32> = Vec::new();
        let mut strs = Vec::new();
        let mut syms = Vec::new();
        for (k, _) in self.entries.iter().flatten() {
            match k {
                PropKey::Index(i) => idx.push(*i),
                PropKey::Str(_) => strs.push(k.clone()),
                PropKey::Sym(_) => syms.push(k.clone()),
            }
        }
        idx.sort_unstable();
        let mut out: Vec<PropKey> = idx.into_iter().map(PropKey::Index).collect();
        out.extend(strs);
        out.extend(syms);
        out
    }
    pub fn index_keys(&self) -> Vec<u32> {
        self.map
            .keys()
            .filter_map(|k| match k {
                PropKey::Index(i) => Some(*i),
                _ => None,
            })
            .collect()
    }
    pub fn values(&self) -> impl Iterator<Item = &Prop> {
        self.entries.iter().flatten().map(|e| &e.1)
    }
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Prop> {
        self.entries.iter_mut().flatten().map(|e| &mut e.1)
    }
}

/// A native function: `callee` lets it read its own slots.
pub type NativeFn = fn(&mut super::Vm, NativeCall) -> Result<Value, Value>;

pub struct NativeCall {
    pub callee: ObjRef,
    pub this: Value,
    pub args: Vec<Value>,
    /// `Some` for [[Construct]].
    pub new_target: Option<ObjRef>,
}

impl NativeCall {
    pub fn arg(&self, i: usize) -> Value {
        self.args.get(i).cloned().unwrap_or(Value::Undefined)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThisMode {
    Lexical,
    Strict,
    Global,
}

pub struct Closure {
    pub tmpl: Rc<FuncTemplate>,
    pub env: EnvRef,
    /// [[HomeObject]] for `super` (§10.2).
    pub home: Option<ObjRef>,
    pub this_mode: ThisMode,
    pub constructor: bool,
    /// [[ConstructorKind]] derived.
    pub derived: bool,
    /// Class constructors throw when called (§10.2.1 step 2).
    pub class_ctor: bool,
    /// [[Fields]] of a class constructor: (key, initializer function or none).
    pub fields: Vec<(FieldKey, Option<ObjRef>)>,
}

#[derive(Clone, Debug)]
pub enum FieldKey {
    Key(PropKey),
    Private(Rc<PrivateName>),
}

/// A Private Name (§6.2.12): unique per class evaluation.
#[derive(Debug)]
pub struct PrivateName {
    pub description: String,
}

pub struct Native {
    pub f: NativeFn,
    pub constructor: bool,
    pub slots: Vec<Value>,
}

pub struct Bound {
    pub target: ObjRef,
    pub this: Value,
    pub args: Vec<Value>,
}

pub struct ForIn {
    pub object: Option<ObjRef>,
    pub visited: std::collections::HashSet<PropKey>,
    pub queue: std::collections::VecDeque<PropKey>,
    pub started: bool,
}

pub struct IterRecord {
    pub iterator: Value,
    pub next: Value,
    pub done: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IterKind {
    Keys,
    Values,
    Entries,
}

pub enum GenState {
    SuspendedStart(Box<Frame>),
    SuspendedYield(Box<Frame>),
    Executing,
    Completed,
}

/// What kind of object this is: ordinary, exotic (§10.4) or one with internal slots.
pub enum Kind {
    Ordinary,
    Array,
    Arguments,
    Function(Box<Closure>),
    Native(Box<Native>),
    Bound(Box<Bound>),
    Boolean(bool),
    Number(f64),
    String(JsString),
    Symbol(Sym),
    Error,
    Date(f64),
    RegExp { source: JsString, flags: JsString },
    ArrayIter { target: Option<ObjRef>, index: u32, kind: IterKind, typed: bool },
    StringIter { s: JsString, pos: usize, done: bool },
    ForIn(Box<ForIn>),
    IterRecord(Box<IterRecord>),
    Generator(Box<GenState>),
    Map(Box<OrderedMap>),
    Set(Box<OrderedMap>),
    MapIter { map: Option<ObjRef>, index: usize, kind: IterKind },
}

/// Insertion-ordered map for Map/Set (§24.1): deleted entries leave holes so live iterators
/// keep their position.
#[derive(Default)]
pub struct OrderedMap {
    pub entries: Vec<Option<(Value, Value)>>,
    pub index: HashMap<MapKey, usize>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum MapKey {
    Undefined,
    Null,
    Bool(bool),
    /// SameValueZero: -0 is normalized to +0, NaN to one bit pattern.
    Number(u64),
    String(JsString),
    Symbol(Sym),
    Object(ObjRef),
}

impl MapKey {
    pub fn of(v: &Value) -> MapKey {
        match v {
            Value::Undefined => MapKey::Undefined,
            Value::Null => MapKey::Null,
            Value::Bool(b) => MapKey::Bool(*b),
            Value::Number(n) => {
                let n = if *n == 0.0 { 0.0 } else { *n };
                MapKey::Number(if n.is_nan() { f64::NAN.to_bits() } else { n.to_bits() })
            }
            Value::String(s) => MapKey::String(s.clone()),
            Value::Symbol(s) => MapKey::Symbol(s.clone()),
            Value::Object(o) => MapKey::Object(*o),
        }
    }
}

pub struct Object {
    pub proto: Option<ObjRef>,
    pub extensible: bool,
    pub props: Props,
    pub kind: Kind,
    /// Private elements (§7.3.27): fields, methods and accessors.
    pub private: Vec<(Rc<PrivateName>, Prop)>,
}

impl Object {
    pub fn new(proto: Option<ObjRef>, kind: Kind) -> Object {
        Object { proto, extensible: true, props: Props::default(), kind, private: Vec::new() }
    }
    pub fn is_callable(&self) -> bool {
        matches!(self.kind, Kind::Function(_) | Kind::Native(_) | Kind::Bound(_))
    }
}

/// A binding in a declarative record (§9.1.1.1). `value: None` = not yet initialized (TDZ).
#[derive(Clone, Debug)]
pub struct Binding {
    pub value: Option<Value>,
    pub mutable: bool,
    /// Assignment to an immutable binding throws even in sloppy code.
    pub strict: bool,
    /// Created by `eval` var declarations (deletable).
    pub deletable: bool,
}

pub enum EnvKind {
    Declarative,
    /// Function Environment Record (§9.1.1.3); arrows don't get one.
    Function {
        /// `None`: uninitialized (derived constructors before `super()`).
        this: Option<Value>,
        func: ObjRef,
        new_target: Value,
    },
    /// Object Environment Record (§9.1.1.2).
    Object {
        object: ObjRef,
        with: bool,
    },
    /// Global Environment Record (§9.1.1.4): declarative part in `bindings`.
    Global {
        object: ObjRef,
        this: Value,
        var_names: std::collections::HashSet<Rc<str>>,
    },
}

pub struct Env {
    pub outer: Option<EnvRef>,
    pub kind: EnvKind,
    pub bindings: HashMap<Rc<str>, Binding>,
}

#[derive(Default)]
pub struct Heap {
    pub objects: Vec<Option<Object>>,
    free: Vec<u32>,
    pub envs: Vec<Option<Env>>,
    env_free: Vec<u32>,
    /// Allocations since the last collection.
    pub allocated: usize,
}

impl Heap {
    pub fn alloc(&mut self, o: Object) -> ObjRef {
        self.allocated += 1;
        if let Some(i) = self.free.pop() {
            self.objects[i as usize] = Some(o);
            ObjRef(i)
        } else {
            self.objects.push(Some(o));
            ObjRef(self.objects.len() as u32 - 1)
        }
    }
    pub fn alloc_env(&mut self, e: Env) -> EnvRef {
        self.allocated += 1;
        if let Some(i) = self.env_free.pop() {
            self.envs[i as usize] = Some(e);
            EnvRef(i)
        } else {
            self.envs.push(Some(e));
            EnvRef(self.envs.len() as u32 - 1)
        }
    }
    pub fn obj(&self, r: ObjRef) -> &Object {
        self.objects[r.0 as usize].as_ref().expect("live object")
    }
    pub fn obj_mut(&mut self, r: ObjRef) -> &mut Object {
        self.objects[r.0 as usize].as_mut().expect("live object")
    }
    pub fn env(&self, r: EnvRef) -> &Env {
        self.envs[r.0 as usize].as_ref().expect("live env")
    }
    pub fn env_mut(&mut self, r: EnvRef) -> &mut Env {
        self.envs[r.0 as usize].as_mut().expect("live env")
    }
    pub fn live_objects(&self) -> usize {
        self.objects.len() - self.free.len()
    }

    /// Mark from `roots`, then sweep everything unmarked.
    pub fn collect(&mut self, roots: &Roots) {
        let mut omark = vec![false; self.objects.len()];
        let mut emark = vec![false; self.envs.len()];
        let mut ostack: Vec<ObjRef> = roots.objects.clone();
        let mut estack: Vec<EnvRef> = roots.envs.clone();
        for v in &roots.values {
            if let Value::Object(o) = v {
                ostack.push(*o);
            }
        }
        loop {
            if let Some(o) = ostack.pop() {
                let i = o.0 as usize;
                if omark[i] {
                    continue;
                }
                omark[i] = true;
                if let Some(obj) = &self.objects[i] {
                    trace_object(obj, &mut ostack, &mut estack);
                }
            } else if let Some(e) = estack.pop() {
                let i = e.0 as usize;
                if emark[i] {
                    continue;
                }
                emark[i] = true;
                if let Some(env) = &self.envs[i] {
                    trace_env(env, &mut ostack, &mut estack);
                }
            } else {
                break;
            }
        }
        for (i, m) in omark.iter().enumerate() {
            if !m && self.objects[i].is_some() {
                self.objects[i] = None;
                self.free.push(i as u32);
            }
        }
        for (i, m) in emark.iter().enumerate() {
            if !m && self.envs[i].is_some() {
                self.envs[i] = None;
                self.env_free.push(i as u32);
            }
        }
        self.allocated = 0;
    }
}

#[derive(Default)]
pub struct Roots {
    pub objects: Vec<ObjRef>,
    pub envs: Vec<EnvRef>,
    pub values: Vec<Value>,
}

fn push_val(v: &Value, os: &mut Vec<ObjRef>) {
    if let Value::Object(o) = v {
        os.push(*o);
    }
}

fn trace_prop(p: &Prop, os: &mut Vec<ObjRef>) {
    match &p.slot {
        Slot::Data(v) => push_val(v, os),
        Slot::Accessor(g, s) => {
            os.extend(g);
            os.extend(s);
        }
    }
}

fn trace_object(o: &Object, os: &mut Vec<ObjRef>, es: &mut Vec<EnvRef>) {
    os.extend(o.proto);
    for p in o.props.values() {
        trace_prop(p, os);
    }
    for (_, p) in &o.private {
        trace_prop(p, os);
    }
    match &o.kind {
        Kind::Function(c) => {
            es.push(c.env);
            os.extend(c.home);
            for (_, f) in &c.fields {
                os.extend(*f);
            }
        }
        Kind::Native(n) => n.slots.iter().for_each(|v| push_val(v, os)),
        Kind::Bound(b) => {
            os.push(b.target);
            push_val(&b.this, os);
            b.args.iter().for_each(|v| push_val(v, os));
        }
        Kind::ArrayIter { target, .. } => os.extend(*target),
        Kind::ForIn(f) => os.extend(f.object),
        Kind::IterRecord(r) => {
            push_val(&r.iterator, os);
            push_val(&r.next, os);
        }
        Kind::Generator(g) => {
            if let GenState::SuspendedStart(f) | GenState::SuspendedYield(f) = &**g {
                f.trace(os, es);
            }
        }
        Kind::Map(m) | Kind::Set(m) => {
            for (k, v) in m.entries.iter().flatten() {
                push_val(k, os);
                push_val(v, os);
            }
        }
        Kind::MapIter { map, .. } => os.extend(*map),
        _ => {}
    }
}

fn trace_env(e: &Env, os: &mut Vec<ObjRef>, es: &mut Vec<EnvRef>) {
    es.extend(e.outer);
    for b in e.bindings.values() {
        if let Some(v) = &b.value {
            push_val(v, os);
        }
    }
    match &e.kind {
        EnvKind::Function { this, func, new_target } => {
            if let Some(t) = this {
                push_val(t, os);
            }
            os.push(*func);
            push_val(new_target, os);
        }
        EnvKind::Object { object, .. } => os.push(*object),
        EnvKind::Global { object, this, .. } => {
            os.push(*object);
            push_val(this, os);
        }
        EnvKind::Declarative => {}
    }
}

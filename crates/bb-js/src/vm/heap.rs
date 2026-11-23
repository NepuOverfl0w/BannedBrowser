//! The garbage-collected heap: objects (ECMA-262 §6.1.7, §10) and environment records (§9.1)
//! live in arenas addressed by index; a mark-sweep collector frees what the roots can't reach.

use super::code::FuncTemplate;
use super::value::{JsString, PropKey, Sym, Value};
use super::Frame;
use crate::vm::fx::HashMap;
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
///
/// Integer indices from 0 up are kept densely (`dense[i]`, holes are `None`): array
/// elements don't go through the hash table. Every index in `map` is at least
/// `dense.len()`, so indices still come out in ascending order.
#[derive(Default, Debug, Clone)]
pub struct Props {
    map: HashMap<PropKey, usize>,
    entries: Vec<Option<(PropKey, Prop)>>,
    /// How many keys in `map` are integer indices: lookups of an index on an object
    /// without any (most prototypes) skip the hash table.
    indices: u32,
    dense: Vec<Option<Prop>>,
    dense_count: u32,
}

impl Props {
    #[inline]
    fn absent(&self, k: &PropKey) -> bool {
        matches!(k, PropKey::Index(_)) && self.indices == 0
    }
    pub fn get(&self, k: &PropKey) -> Option<&Prop> {
        if let PropKey::Index(i) = k {
            if let Some(d) = self.dense.get(*i as usize) {
                return d.as_ref();
            }
        }
        if self.absent(k) {
            return None;
        }
        self.map.get(k).and_then(|&i| self.entries[i].as_ref().map(|e| &e.1))
    }
    pub fn get_mut(&mut self, k: &PropKey) -> Option<&mut Prop> {
        if let PropKey::Index(i) = k {
            if (*i as usize) < self.dense.len() {
                return self.dense[*i as usize].as_mut();
            }
        }
        if self.absent(k) {
            return None;
        }
        match self.map.get(k) {
            Some(&i) => self.entries[i].as_mut().map(|e| &mut e.1),
            None => None,
        }
    }
    pub fn contains(&self, k: &PropKey) -> bool {
        self.get(k).is_some()
    }
    /// The entry index of a named key (stable until the property is deleted; for inline
    /// caches). Dense indices have none.
    pub fn index_of(&self, k: &PropKey) -> Option<usize> {
        if matches!(k, PropKey::Index(_)) {
            return None;
        }
        self.map.get(k).copied()
    }
    /// The property at entry `i` if its key is `k` (an inline cache's guess, verified).
    #[inline]
    pub fn at(&self, i: usize, k: &PropKey) -> Option<&Prop> {
        match self.entries.get(i) {
            Some(Some((key, p))) if key.same(k) => Some(p),
            _ => None,
        }
    }
    #[inline]
    pub fn at_mut(&mut self, i: usize, k: &PropKey) -> Option<&mut Prop> {
        match self.entries.get_mut(i) {
            Some(Some((key, p))) if key.same(k) => Some(p),
            _ => None,
        }
    }
    pub fn insert(&mut self, k: PropKey, p: Prop) {
        if let PropKey::Index(i) = k {
            let i = i as usize;
            if i < self.dense.len() {
                if self.dense[i].is_none() {
                    self.dense_count += 1;
                }
                self.dense[i] = Some(p);
                return;
            }
            if i == self.dense.len() {
                // Next to the dense run (never in `map`, which holds only higher indices);
                // map indices that now follow move over too.
                self.dense.push(Some(p));
                self.dense_count += 1;
                while self.indices > 0 {
                    let next = PropKey::Index(self.dense.len() as u32);
                    match self.remove_from_map(&next) {
                        Some(q) => {
                            self.dense.push(Some(q));
                            self.dense_count += 1;
                        }
                        None => break,
                    }
                }
                return;
            }
        }
        if let Some(&i) = self.map.get(&k) {
            self.entries[i] = Some((k, p));
        } else {
            if matches!(k, PropKey::Index(_)) {
                self.indices += 1;
            }
            self.map.insert(k.clone(), self.entries.len());
            self.entries.push(Some((k, p)));
        }
    }
    fn remove_from_map(&mut self, k: &PropKey) -> Option<Prop> {
        let i = self.map.remove(k)?;
        if matches!(k, PropKey::Index(_)) {
            self.indices -= 1;
        }
        let e = self.entries[i].take();
        if self.entries.len() > 16 && self.map.len() * 2 < self.entries.len() {
            self.compact();
        }
        e.map(|e| e.1)
    }
    pub fn remove(&mut self, k: &PropKey) -> Option<Prop> {
        if let PropKey::Index(i) = k {
            let i = *i as usize;
            if i < self.dense.len() {
                let p = self.dense[i].take();
                if p.is_some() {
                    self.dense_count -= 1;
                }
                // Trailing holes go away (keeps `dense.len()` meaning "length so far").
                while matches!(self.dense.last(), Some(None)) {
                    self.dense.pop();
                }
                return p;
            }
        }
        self.remove_from_map(k)
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
        self.map.len() + self.dense_count as usize
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Integer indices ascending, then strings, then symbols, each in creation order.
    pub fn keys(&self) -> Vec<PropKey> {
        let mut out: Vec<PropKey> = Vec::with_capacity(self.len());
        for (i, d) in self.dense.iter().enumerate() {
            if d.is_some() {
                out.push(PropKey::Index(i as u32));
            }
        }
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
        out.extend(idx.into_iter().map(PropKey::Index));
        out.extend(strs);
        out.extend(syms);
        out
    }
    pub fn index_keys(&self) -> Vec<u32> {
        let mut out: Vec<u32> =
            self.dense.iter().enumerate().filter(|(_, d)| d.is_some()).map(|(i, _)| i as u32).collect();
        out.extend(self.map.keys().filter_map(|k| match k {
            PropKey::Index(i) => Some(*i),
            _ => None,
        }));
        out
    }
    pub fn values(&self) -> impl Iterator<Item = &Prop> {
        self.dense.iter().flatten().chain(self.entries.iter().flatten().map(|e| &e.1))
    }
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Prop> {
        self.dense.iter_mut().flatten().chain(self.entries.iter_mut().flatten().map(|e| &mut e.1))
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
    /// [[Realm]] (an index into the VM's realms).
    pub realm: u32,
    pub env: EnvRef,
    /// [[HomeObject]] for `super` (§10.2).
    pub home: Option<ObjRef>,
    pub this_mode: ThisMode,
    pub constructor: bool,
    /// [[ConstructorKind]] derived.
    pub derived: bool,
    /// Class constructors throw when called (§10.2.1 step 2).
    pub class_ctor: bool,
    /// [[Fields]] of a class constructor (§15.7.10 ClassFieldDefinition).
    pub fields: Vec<ClassField>,
    /// [[PrivateMethods]]: instance private methods and accessors.
    pub private_methods: Vec<(Sym, PrivElem)>,
    /// Static fields and blocks, run once the class is defined.
    pub static_elems: Vec<StaticElem>,
    /// For field initializers: the field's name (NamedEvaluation of anonymous functions).
    pub field_name: Option<Value>,
    /// A class with decorators (proposal-decorators): their state and extra initializers.
    pub decor: Option<Box<ClassDecor>>,
}

/// Decorator state of a class constructor (proposal-decorators).
#[derive(Default)]
pub struct ClassDecor {
    /// Decorated elements, in document order, until ClassDecorate applies them.
    pub pending: Vec<Decoration>,
    /// Extra initializers of non-static methods and accessors, run on each new instance
    /// before its fields.
    pub instance_inits: Vec<ObjRef>,
    /// Extra initializers of static methods, run before the static fields.
    pub static_inits: Vec<ObjRef>,
    /// The class decorators' extra initializers, run last with the decorated class.
    pub class_inits: Vec<ObjRef>,
}

/// One decorated class element.
pub struct Decoration {
    pub decorators: Vec<Value>,
    /// The property key value, or the Private Name symbol.
    pub key: Value,
    pub is_static: bool,
    pub target: DecTarget,
}

pub enum DecTarget {
    /// A method (0), getter (1) or setter (2).
    Method { kind: u8, func: ObjRef },
    /// A field: its index in `fields` or `static_elems`.
    Field { index: usize },
    /// An auto-accessor: its storage field's index and the current getter and setter.
    Accessor { index: usize, get: ObjRef, set: ObjRef },
}

#[derive(Clone, Debug)]
pub enum FieldKey {
    Key(PropKey),
    Private(Sym),
}

#[derive(Clone, Debug)]
pub struct ClassField {
    pub key: FieldKey,
    pub init: Option<ObjRef>,
    /// Decorator initializers, applied to the initial value in order.
    pub transforms: Vec<ObjRef>,
    /// Decorator extra initializers, run right after the field is defined.
    pub extra: Vec<ObjRef>,
}

#[derive(Clone, Debug)]
pub enum StaticElem {
    Field(ClassField),
    Block(ObjRef),
}

/// A PrivateElement (§6.2.10).
#[derive(Clone, Debug)]
pub enum PrivElem {
    Field(Value),
    Method(ObjRef),
    Accessor(Option<ObjRef>, Option<ObjRef>),
}

/// [[ParameterMap]]: index → the parameter binding it aliases in `env` (`None`: unmapped).
pub struct ArgMap {
    pub env: EnvRef,
    pub names: Vec<Option<Rc<str>>>,
}

pub struct Native {
    pub f: NativeFn,
    pub constructor: bool,
    pub slots: Vec<Value>,
    /// [[Realm]].
    pub realm: u32,
}

pub struct Bound {
    pub target: ObjRef,
    pub this: Value,
    pub args: Vec<Value>,
}

pub struct ForIn {
    pub object: Option<ObjRef>,
    pub visited: crate::vm::fx::HashSet<PropKey>,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromiseState {
    Pending,
    Fulfilled,
    Rejected,
}

/// A PromiseReaction Record (§27.2.1.2): `capability` is (promise, resolve, reject).
#[derive(Clone)]
pub struct Reaction {
    pub capability: Option<(Value, Value, Value)>,
    pub fulfill: bool,
    /// `Undefined`: no handler (pass the value through).
    pub handler: Value,
}

pub struct PromiseData {
    pub state: PromiseState,
    pub result: Value,
    pub fulfill_reactions: Vec<Reaction>,
    pub reject_reactions: Vec<Reaction>,
    pub handled: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgPhase {
    Start,
    Yield,
    Executing,
    AwaitingReturn,
    Completed,
}

/// An AsyncGeneratorRequest (§27.6.3.1): the completion and its promise capability.
pub struct AgRequest {
    pub mode: super::Resume,
    pub value: Value,
    pub resolve: Value,
    pub reject: Value,
}

pub struct AsyncGen {
    pub frame: Option<Box<Frame>>,
    pub phase: AgPhase,
    pub queue: std::collections::VecDeque<AgRequest>,
}

pub struct AsyncFn {
    pub state: GenState,
    pub resolve: Value,
    pub reject: Value,
}

/// A queued job (§9.5): promise reactions and thenable resolution.
pub enum Job {
    Reaction { reaction: Reaction, argument: Value },
    Thenable { promise: ObjRef, thenable: Value, then: Value },
}

impl Job {
    pub fn trace(&self, os: &mut Vec<ObjRef>) {
        match self {
            Job::Reaction { reaction, argument } => {
                trace_reaction(reaction, os);
                push_val(argument, os);
            }
            Job::Thenable { promise, thenable, then } => {
                os.push(*promise);
                push_val(thenable, os);
                push_val(then, os);
            }
        }
    }
}

fn trace_reaction(r: &Reaction, os: &mut Vec<ObjRef>) {
    if let Some((p, a, b)) = &r.capability {
        push_val(p, os);
        push_val(a, os);
        push_val(b, os);
    }
    push_val(&r.handler, os);
}

/// What kind of object this is: ordinary, exotic (§10.4) or one with internal slots.
pub enum Kind {
    Ordinary,
    Array,
    Arguments,
    /// A mapped arguments exotic object (§10.4.4): its [[ParameterMap]].
    MappedArguments(Box<ArgMap>),
    Function(Box<Closure>),
    Native(Box<Native>),
    Bound(Box<Bound>),
    Boolean(bool),
    Number(f64),
    String(JsString),
    Symbol(Sym),
    BigInt(Rc<super::bigint::BigInt>),
    /// [[ErrorData]], holding the stack trace captured at creation (`Error.prototype.stack`).
    Error(Rc<str>),
    Date(f64),
    RegExp(Box<super::builtins::regexp::RegExpData>),
    RegExpStringIter(Box<super::builtins::regexp::RegExpStringIter>),
    ArrayIter {
        target: Option<ObjRef>,
        index: u32,
        kind: IterKind,
        typed: bool,
    },
    StringIter {
        s: JsString,
        pos: usize,
        done: bool,
    },
    ForIn(Box<ForIn>),
    IterRecord(Box<IterRecord>),
    Generator(Box<GenState>),
    Promise(Box<PromiseData>),
    /// A running async function: its parked frame and its promise's resolving functions.
    Async(Box<AsyncFn>),
    AsyncGen(Box<AsyncGen>),
    /// Module namespace exotic object (§10.4.6): (module id, sorted export names).
    ModuleNamespace(usize, Vec<JsString>, bool),
    Proxy(Box<super::proxy::ProxyData>),
    IterHelper(Box<super::builtins::iterator::Helper>),
    ArrayBuffer(Box<super::typed::Buffer>),
    TypedArray(Box<super::typed::TypedArray>),
    DataView(Box<super::typed::DataView>),
    Map(Box<OrderedMap>),
    Set(Box<OrderedMap>),
    /// Weak collections hold their entries strongly: unobservable except as memory (the
    /// spec lets implementations keep them).
    WeakMap(Box<OrderedMap>),
    WeakSet(Box<OrderedMap>),
    WeakRef(Value),
    /// (cleanup callback, registered (target, held, token) cells); callbacks never run.
    FinalizationRegistry(Value, Vec<(Value, Value, Value)>),
    /// A self-hosted class's internal slots (Temporal): (brand, the slots' array).
    Slots(u32, ObjRef),
    /// [[ShadowRealm]]: the realm's id.
    ShadowRealm(u32),
    /// A module source object ([[ModuleSourceClassName]], source-phase-imports).
    ModuleSource(Rc<str>),
    /// A Super Reference on the operand stack (§13.3.7.3 MakeSuperPropertyReference): the
    /// base taken when it was made, and the name (a property key once GetValue/PutValue
    /// converted it).
    SuperRef(Option<ObjRef>, Value),
    /// JSON.rawJSON's result ([[IsRawJSON]]).
    RawJSON,
    /// A `using` scope's DisposeCapability (on the operand stack).
    DisposeCap(Box<super::dispose::DisposeCap>),
    DisposableStack(Box<super::dispose::StackData>),
    MapIter {
        map: Option<ObjRef>,
        index: usize,
        kind: IterKind,
    },
    /// A host's platform object (`host.rs`).
    Host(Box<super::host::HostObj>),
}

/// Insertion-ordered map for Map/Set (§24.1): deleted entries leave holes so live iterators
/// keep their position.
#[derive(Default)]
pub struct OrderedMap {
    pub entries: Vec<Option<(Value, Value)>>,
    pub index: HashMap<MapKey, usize>,
}

impl OrderedMap {
    pub fn get(&self, k: &Value) -> Option<&Value> {
        self.index.get(&MapKey::of(k)).and_then(|&i| self.entries[i].as_ref().map(|e| &e.1))
    }
    pub fn has(&self, k: &Value) -> bool {
        self.index.contains_key(&MapKey::of(k))
    }
    /// Insert or update; -0 keys are stored as +0 (§24.1.3.9 step 5).
    pub fn set(&mut self, k: Value, v: Value) {
        let k = match k {
            Value::Number(n) if n == 0.0 && n.is_sign_negative() => Value::Number(0.0),
            k => k,
        };
        let mk = MapKey::of(&k);
        if let Some(&i) = self.index.get(&mk) {
            if let Some(e) = &mut self.entries[i] {
                e.1 = v;
            }
            return;
        }
        self.index.insert(mk, self.entries.len());
        self.entries.push(Some((k, v)));
    }
    pub fn delete(&mut self, k: &Value) -> bool {
        match self.index.remove(&MapKey::of(k)) {
            Some(i) => {
                self.entries[i] = None;
                true
            }
            None => false,
        }
    }
    /// Live iterators see the cleared entries as holes and finish.
    pub fn clear(&mut self) {
        self.entries.fill(None);
        self.index.clear();
    }
    pub fn size(&self) -> usize {
        self.index.len()
    }
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
    BigInt(Rc<super::bigint::BigInt>),
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
            Value::BigInt(b) => MapKey::BigInt(b.clone()),
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
    pub private: Vec<(Sym, PrivElem)>,
    /// [[IsHTMLDDA]] (§B.3.6): `typeof` is "undefined", ToBoolean is false, and it is
    /// loosely equal to undefined and null. Hosts set it on `document.all`.
    pub html_dda: bool,
}

impl Object {
    pub fn new(proto: Option<ObjRef>, kind: Kind) -> Object {
        Object {
            proto,
            extensible: true,
            props: Props::default(),
            kind,
            private: Vec::new(),
            html_dda: false,
        }
    }
    pub fn is_callable(&self) -> bool {
        match &self.kind {
            Kind::Function(_) | Kind::Native(_) | Kind::Bound(_) => true,
            Kind::Proxy(p) => p.callable,
            _ => false,
        }
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
    /// Module Environment Record (§9.1.1.5): import bindings point into other modules.
    Module {
        imports: crate::vm::fx::HashMap<Rc<str>, (EnvRef, Rc<str>)>,
    },
    /// Global Environment Record (§9.1.1.4): declarative part in `bindings`.
    Global {
        object: ObjRef,
        this: Value,
    },
}

pub struct Env {
    pub outer: Option<EnvRef>,
    pub kind: EnvKind,
    pub bindings: Bindings,
}

/// The bindings of an Environment Record by name. Scopes are mostly small: a vector,
/// searched comparing the name's pointer first (code refers to names through the same
/// interned `Rc<str>` that declared them); large scopes (global, module) add an index.
#[derive(Clone, Default)]
pub struct Bindings {
    list: Vec<(Rc<str>, Binding)>,
    index: Option<HashMap<Rc<str>, usize>>,
}

const INDEX_AT: usize = 12;

impl Bindings {
    /// Room for `n` bindings (a function environment's known names).
    pub fn with_capacity(n: usize) -> Bindings {
        Bindings { list: Vec::with_capacity(n.min(INDEX_AT)), index: None }
    }

    fn find(&self, n: &str) -> Option<usize> {
        if let Some(ix) = &self.index {
            return ix.get(n).copied();
        }
        let p = n.as_ptr();
        self.list.iter().position(|(k, _)| (k.as_ptr() == p && k.len() == n.len()) || **k == *n)
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn contains_key(&self, n: &str) -> bool {
        self.find(n).is_some()
    }

    pub fn get(&self, n: &str) -> Option<&Binding> {
        self.find(n).map(|i| &self.list[i].1)
    }

    pub fn get_mut(&mut self, n: &str) -> Option<&mut Binding> {
        self.find(n).map(|i| &mut self.list[i].1)
    }

    /// Insert or replace; the previous binding if there was one.
    pub fn insert(&mut self, n: Rc<str>, b: Binding) -> Option<Binding> {
        if let Some(i) = self.find(&n) {
            return Some(std::mem::replace(&mut self.list[i].1, b));
        }
        self.push(n, b);
        None
    }

    fn push(&mut self, n: Rc<str>, b: Binding) {
        if let Some(ix) = &mut self.index {
            ix.insert(n.clone(), self.list.len());
        }
        self.list.push((n, b));
        if self.index.is_none() && self.list.len() > INDEX_AT {
            self.index = Some(self.list.iter().enumerate().map(|(i, (k, _))| (k.clone(), i)).collect());
        }
    }

    /// The binding for `n`, inserted as `b` when there is none.
    pub fn get_or_insert(&mut self, n: Rc<str>, b: Binding) -> &mut Binding {
        let i = match self.find(&n) {
            Some(i) => i,
            None => {
                self.push(n, b);
                self.list.len() - 1
            }
        };
        &mut self.list[i].1
    }

    pub fn remove(&mut self, n: &str) -> Option<Binding> {
        let i = self.find(n)?;
        let (k, b) = self.list.swap_remove(i);
        if let Some(ix) = &mut self.index {
            ix.remove(&*k);
            if let Some((moved, _)) = self.list.get(i) {
                ix.insert(moved.clone(), i);
            }
        }
        Some(b)
    }

    /// The binding at position `k` if it is `n`'s (pointer or text match).
    pub fn at(&self, k: usize, n: &str) -> Option<&Binding> {
        let (key, b) = self.list.get(k)?;
        ((key.as_ptr() == n.as_ptr() && key.len() == n.len()) || **key == *n).then_some(b)
    }

    pub fn at_mut(&mut self, k: usize, n: &str) -> Option<&mut Binding> {
        let (key, b) = self.list.get_mut(k)?;
        ((key.as_ptr() == n.as_ptr() && key.len() == n.len()) || **key == *n).then_some(b)
    }

    pub fn keys(&self) -> impl Iterator<Item = &Rc<str>> {
        self.list.iter().map(|(k, _)| k)
    }

    pub fn values(&self) -> impl Iterator<Item = &Binding> {
        self.list.iter().map(|(_, b)| b)
    }
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

fn trace_priv(p: &PrivElem, os: &mut Vec<ObjRef>) {
    match p {
        PrivElem::Field(v) => push_val(v, os),
        PrivElem::Method(m) => os.push(*m),
        PrivElem::Accessor(g, s) => {
            os.extend(*g);
            os.extend(*s);
        }
    }
}

fn trace_object(o: &Object, os: &mut Vec<ObjRef>, es: &mut Vec<EnvRef>) {
    os.extend(o.proto);
    for p in o.props.values() {
        trace_prop(p, os);
    }
    for (_, p) in &o.private {
        trace_priv(p, os);
    }
    match &o.kind {
        Kind::Function(c) => {
            es.push(c.env);
            os.extend(c.home);
            let field = |f: &ClassField, os: &mut Vec<ObjRef>| {
                os.extend(f.init);
                os.extend(&f.transforms);
                os.extend(&f.extra);
            };
            for f in &c.fields {
                field(f, os);
            }
            for (_, p) in &c.private_methods {
                trace_priv(p, os);
            }
            for e in &c.static_elems {
                match e {
                    StaticElem::Field(f) => field(f, os),
                    StaticElem::Block(b) => os.push(*b),
                }
            }
            if let Some(d) = &c.decor {
                os.extend(&d.instance_inits);
                os.extend(&d.static_inits);
                os.extend(&d.class_inits);
                for p in &d.pending {
                    p.decorators.iter().for_each(|v| push_val(v, os));
                    push_val(&p.key, os);
                    match p.target {
                        DecTarget::Method { func, .. } => os.push(func),
                        DecTarget::Field { .. } => {}
                        DecTarget::Accessor { get, set, .. } => os.extend([get, set]),
                    }
                }
            }
            if let Some(v) = &c.field_name {
                push_val(v, os);
            }
        }
        Kind::Native(n) => n.slots.iter().for_each(|v| push_val(v, os)),
        Kind::SuperRef(b, k) => {
            os.extend(*b);
            push_val(k, os);
        }
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
        Kind::Async(a) => {
            if let GenState::SuspendedStart(f) | GenState::SuspendedYield(f) = &a.state {
                f.trace(os, es);
            }
            push_val(&a.resolve, os);
            push_val(&a.reject, os);
        }
        Kind::AsyncGen(a) => {
            if let Some(f) = &a.frame {
                f.trace(os, es);
            }
            for r in &a.queue {
                push_val(&r.value, os);
                push_val(&r.resolve, os);
                push_val(&r.reject, os);
            }
        }
        Kind::Promise(p) => {
            push_val(&p.result, os);
            for r in p.fulfill_reactions.iter().chain(&p.reject_reactions) {
                trace_reaction(r, os);
            }
        }
        Kind::WeakRef(v) => push_val(v, os),
        Kind::MappedArguments(m) => es.push(m.env),
        Kind::DisposeCap(c) => c.trace(os),
        Kind::Slots(_, a) => os.push(*a),
        Kind::Host(h) => {
            for v in &h.slots {
                push_val(v, os);
            }
        }
        Kind::DisposableStack(s) => s.cap.trace(os),
        Kind::TypedArray(t) => os.push(t.buffer),
        Kind::IterHelper(h) => h.trace(os),
        Kind::RegExpStringIter(r) => os.push(r.matcher),
        Kind::DataView(d) => os.push(d.buffer),
        Kind::Proxy(p) => {
            os.extend(p.target);
            os.extend(p.handler);
        }
        Kind::FinalizationRegistry(cb, cells) => {
            push_val(cb, os);
            for (a, b, c) in cells {
                push_val(a, os);
                push_val(b, os);
                push_val(c, os);
            }
        }
        Kind::Map(m) | Kind::Set(m) | Kind::WeakMap(m) | Kind::WeakSet(m) => {
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
        EnvKind::Module { imports } => es.extend(imports.values().map(|(e, _)| *e)),
        EnvKind::Global { object, this, .. } => {
            os.push(*object);
            push_val(this, os);
        }
        EnvKind::Declarative => {}
    }
}

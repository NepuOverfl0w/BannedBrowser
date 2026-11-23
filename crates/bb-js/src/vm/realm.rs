//! Realms (§9.3): the intrinsics, the global object and the global environment, and the
//! helpers the built-ins use to define themselves.

use super::heap::*;
use super::value::{js_str, JsString, PropKey, Sym, Value};
use super::{builtins, ErrorKind, Vm};
use crate::vm::fx::HashMap;

macro_rules! realm_objects {
    ($($f:ident),* $(,)?) => {
        pub struct Realm {
            $(pub $f: ObjRef,)*
            pub error_protos: [ObjRef; 7],
            pub ta_protos: [ObjRef; 12],
            pub ta_ctors: [ObjRef; 12],
            pub global_env: EnvRef,
            pub global_this: Value,
            pub sym_iterator: Sym,
            pub sym_async_iterator: Sym,
            pub sym_has_instance: Sym,
            pub sym_to_primitive: Sym,
            pub sym_to_string_tag: Sym,
            pub sym_species: Sym,
            pub sym_unscopables: Sym,
            pub sym_is_concat_spreadable: Sym,
            pub sym_match: Sym,
            pub sym_match_all: Sym,
            pub sym_replace: Sym,
            pub sym_search: Sym,
            pub sym_split: Sym,
            pub sym_dispose: Sym,
            pub sym_async_dispose: Sym,
            /// Symbol.metadata (proposal-decorator-metadata).
            pub sym_metadata: Sym,
            /// The legacy RegExp static slots (proposal-regexp-legacy-features): input,
            /// lastMatch, lastParen, leftContext, rightContext, $1–$9. `None` = ~empty~.
            pub regexp_statics: Option<Box<[JsString; 14]>>,
        }

        impl Realm {
            /// Every field points at a dummy until `init` fills it in.
            pub fn placeholder(heap: &mut Heap) -> Realm {
                let d = heap.alloc(Object::new(None, Kind::Ordinary));
                let genv = heap.alloc_env(Env { outer: None, kind: EnvKind::Declarative, bindings: HashMap::default() });
                let s = |n: &str| Sym::new(Some(js_str(n)));
                Realm {
                    $($f: d,)*
                    error_protos: [d; 7],
                    ta_protos: [d; 12],
                    ta_ctors: [d; 12],
                    global_env: genv,
                    global_this: Value::Undefined,
                    sym_iterator: s("Symbol.iterator"),
                    sym_async_iterator: s("Symbol.asyncIterator"),
                    sym_has_instance: s("Symbol.hasInstance"),
                    sym_to_primitive: s("Symbol.toPrimitive"),
                    sym_to_string_tag: s("Symbol.toStringTag"),
                    sym_species: s("Symbol.species"),
                    sym_unscopables: s("Symbol.unscopables"),
                    sym_is_concat_spreadable: s("Symbol.isConcatSpreadable"),
                    sym_match: s("Symbol.match"),
                    sym_match_all: s("Symbol.matchAll"),
                    sym_replace: s("Symbol.replace"),
                    sym_search: s("Symbol.search"),
                    sym_split: s("Symbol.split"),
                    sym_dispose: s("Symbol.dispose"),
                    sym_async_dispose: s("Symbol.asyncDispose"),
                    sym_metadata: s("Symbol.metadata"),
                    regexp_statics: Some(Box::new(std::array::from_fn(|_| js_str("")))),
                }
            }

            pub fn trace(&self, roots: &mut Roots) {
                $(roots.objects.push(self.$f);)*
                roots.objects.extend(self.error_protos);
                roots.objects.extend(self.ta_protos);
                roots.objects.extend(self.ta_ctors);
                roots.envs.push(self.global_env);
                roots.values.push(self.global_this.clone());
            }
        }
    };
}

realm_objects!(
    global,
    object_proto,
    function_proto,
    array_proto,
    string_proto,
    number_proto,
    boolean_proto,
    symbol_proto,
    iterator_proto,
    array_iterator_proto,
    string_iterator_proto,
    regexp_proto,
    date_proto,
    map_proto,
    set_proto,
    weakmap_proto,
    weakset_proto,
    weakref_proto,
    finreg_proto,
    bigint_proto,
    regexp_ctor,
    regexp_string_iterator_proto,
    iterator_ctor,
    iterator_helper_proto,
    wrap_iterator_proto,
    array_buffer_proto,
    shared_array_buffer_proto,
    dataview_proto,
    typed_array_ctor,
    typed_array_proto,
    map_iterator_proto,
    generator_function_proto,
    generator_proto,
    async_function_proto,
    async_generator_function_proto,
    async_generator_proto,
    async_iterator_proto,
    async_from_sync_iterator_proto,
    promise_proto,
    promise_ctor,
    aggregate_error_proto,
    suppressed_error_proto,
    disposable_stack_proto,
    async_disposable_stack_proto,
    abstract_module_source,
    abstract_module_source_proto,
    shadow_realm_proto,
    set_iterator_proto,
    eval,
    throw_type_error,
    array_proto_values,
    object_ctor,
    function_ctor,
    array_ctor,
);

impl Realm {
    pub fn error_proto(&self, k: ErrorKind) -> ObjRef {
        self.error_protos[k as usize]
    }
    /// Well-known symbols are shared by all realms (§6.1.5.1).
    pub fn share_symbols(&mut self, from: &Realm) {
        self.sym_iterator = from.sym_iterator.clone();
        self.sym_async_iterator = from.sym_async_iterator.clone();
        self.sym_has_instance = from.sym_has_instance.clone();
        self.sym_to_primitive = from.sym_to_primitive.clone();
        self.sym_to_string_tag = from.sym_to_string_tag.clone();
        self.sym_species = from.sym_species.clone();
        self.sym_unscopables = from.sym_unscopables.clone();
        self.sym_is_concat_spreadable = from.sym_is_concat_spreadable.clone();
        self.sym_match = from.sym_match.clone();
        self.sym_match_all = from.sym_match_all.clone();
        self.sym_replace = from.sym_replace.clone();
        self.sym_search = from.sym_search.clone();
        self.sym_split = from.sym_split.clone();
        self.sym_dispose = from.sym_dispose.clone();
        self.sym_async_dispose = from.sym_async_dispose.clone();
        self.sym_metadata = from.sym_metadata.clone();
    }

    pub fn well_known(&self) -> Vec<(&'static str, Sym)> {
        vec![
            ("asyncDispose", self.sym_async_dispose.clone()),
            ("asyncIterator", self.sym_async_iterator.clone()),
            ("dispose", self.sym_dispose.clone()),
            ("hasInstance", self.sym_has_instance.clone()),
            ("isConcatSpreadable", self.sym_is_concat_spreadable.clone()),
            ("iterator", self.sym_iterator.clone()),
            ("match", self.sym_match.clone()),
            ("matchAll", self.sym_match_all.clone()),
            ("metadata", self.sym_metadata.clone()),
            ("replace", self.sym_replace.clone()),
            ("search", self.sym_search.clone()),
            ("species", self.sym_species.clone()),
            ("split", self.sym_split.clone()),
            ("toPrimitive", self.sym_to_primitive.clone()),
            ("toStringTag", self.sym_to_string_tag.clone()),
            ("unscopables", self.sym_unscopables.clone()),
        ]
    }
}

/// CreateIntrinsics + SetDefaultGlobalBindings (§9.3.2, §9.3.4).
pub fn init(vm: &mut Vm) {
    let h = &mut vm.heap;
    let object_proto = h.alloc(Object::new(None, Kind::Ordinary));
    vm.realm.object_proto = object_proto;
    let function_proto = vm.heap.alloc(Object::new(
        Some(object_proto),
        Kind::Native(Box::new(Native {
            f: builtins::empty_fn,
            constructor: false,
            slots: Vec::new(),
            realm: vm.cur_realm,
        })),
    ));
    vm.realm.function_proto = function_proto;
    let global = vm.new_object();
    vm.realm.global = global;
    vm.realm.global_this = Value::Object(global);
    vm.realm.global_env = vm.heap.alloc_env(Env {
        outer: None,
        kind: EnvKind::Global { object: global, this: Value::Object(global) },
        bindings: HashMap::default(),
    });
    for f in [
        &mut vm.realm.array_proto,
        &mut vm.realm.string_proto,
        &mut vm.realm.number_proto,
        &mut vm.realm.boolean_proto,
        &mut vm.realm.symbol_proto,
        &mut vm.realm.iterator_proto,
    ] {
        *f = vm.heap.alloc(Object::new(Some(object_proto), Kind::Ordinary));
    }
    let ap = vm.realm.array_proto;
    vm.heap.obj_mut(ap).kind = Kind::Array;
    vm.heap
        .obj_mut(ap)
        .props
        .insert(PropKey::lit("length"), Prop::data(Value::Number(0.0), true, false, false));
    let sp = vm.realm.string_proto;
    vm.heap.obj_mut(sp).kind = Kind::String(js_str(""));
    vm.heap
        .obj_mut(sp)
        .props
        .insert(PropKey::lit("length"), Prop::data(Value::Number(0.0), false, false, false));
    let np = vm.realm.number_proto;
    vm.heap.obj_mut(np).kind = Kind::Number(0.0);
    let bp = vm.realm.boolean_proto;
    vm.heap.obj_mut(bp).kind = Kind::Boolean(false);
    // %ThrowTypeError% (§10.2.4.1).
    let tte = native(vm, "", 0, builtins::throw_type_error);
    let _ = vm.set_integrity(tte, true);
    vm.realm.throw_type_error = tte;
    builtins::install(vm);
}

// ---- helpers for defining built-ins ----

/// A built-in function object (§10.3.4 CreateBuiltinFunction).
pub fn native(vm: &mut Vm, name: &str, length: u32, f: NativeFn) -> ObjRef {
    native_with(vm, name, length, f, false, Vec::new())
}

pub fn native_with(
    vm: &mut Vm,
    name: &str,
    length: u32,
    f: NativeFn,
    constructor: bool,
    slots: Vec<Value>,
) -> ObjRef {
    let fp = vm.realm.function_proto;
    let o = vm.heap.alloc(Object::new(
        Some(fp),
        Kind::Native(Box::new(Native { f, constructor, slots, realm: vm.cur_realm })),
    ));
    let obj = vm.heap.obj_mut(o);
    obj.props.insert(PropKey::lit("length"), Prop::data(Value::Number(length as f64), false, false, true));
    obj.props.insert(PropKey::lit("name"), Prop::data(Value::str(name), false, false, true));
    o
}

/// A method: writable, non-enumerable, configurable (§18).
pub fn method(vm: &mut Vm, on: ObjRef, name: &str, length: u32, f: NativeFn) -> ObjRef {
    let m = native(vm, name, length, f);
    vm.heap.obj_mut(on).props.insert(PropKey::named(name), Prop::method(Value::Object(m)));
    m
}

/// A method keyed by a symbol; its name is `[description]`.
pub fn method_sym(vm: &mut Vm, on: ObjRef, sym: Sym, name: &str, length: u32, f: NativeFn) -> ObjRef {
    let m = native(vm, name, length, f);
    vm.heap.obj_mut(on).props.insert(PropKey::Sym(sym), Prop::method(Value::Object(m)));
    m
}

/// An accessor with a built-in getter (and optional setter) named `get x` / `set x`.
pub fn accessor(vm: &mut Vm, on: ObjRef, key: PropKey, getter: Option<NativeFn>, setter: Option<NativeFn>) {
    let base = match &key {
        PropKey::Sym(s) => {
            format!("[{}]", s.0.description.as_deref().map(super::to_rust).unwrap_or_default())
        }
        k => k.display(),
    };
    let g = getter.map(|f| native(vm, &format!("get {base}"), 0, f));
    let s = setter.map(|f| native(vm, &format!("set {base}"), 1, f));
    vm.heap.obj_mut(on).props.insert(
        key,
        Prop { slot: Slot::Accessor(g, s), writable: false, enumerable: false, configurable: true },
    );
}

pub fn value(vm: &mut Vm, on: ObjRef, name: &str, v: Value) {
    vm.heap.obj_mut(on).props.insert(PropKey::named(name), Prop::method(v));
}

/// A non-writable, non-enumerable, non-configurable constant.
pub fn constant(vm: &mut Vm, on: ObjRef, name: &str, v: Value) {
    vm.heap.obj_mut(on).props.insert(PropKey::named(name), Prop::data(v, false, false, false));
}

/// A constructor with its prototype object, installed on the global object.
pub fn constructor(vm: &mut Vm, name: &str, length: u32, f: NativeFn, proto: ObjRef) -> ObjRef {
    let c = native_with(vm, name, length, f, true, Vec::new());
    vm.heap
        .obj_mut(c)
        .props
        .insert(PropKey::lit("prototype"), Prop::data(Value::Object(proto), false, false, false));
    vm.heap.obj_mut(proto).props.insert(PropKey::lit("constructor"), Prop::method(Value::Object(c)));
    let g = vm.realm.global;
    value(vm, g, name, Value::Object(c));
    c
}

pub fn to_string_tag(vm: &mut Vm, on: ObjRef, tag: &str) {
    let k = PropKey::Sym(vm.realm.sym_to_string_tag.clone());
    vm.heap.obj_mut(on).props.insert(k, Prop::data(Value::str(tag), false, false, true));
}

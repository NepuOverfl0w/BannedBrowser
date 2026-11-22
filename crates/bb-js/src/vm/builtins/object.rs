//! Object (§20.1) and Function (§20.2).

use super::super::heap::*;
use super::super::object::PropDesc;
use super::super::realm::{self, accessor, method, method_sym, value};
use super::super::value::{js_str, to_rust, PropKey, Value};
use super::super::{ErrorKind, JsResult, Vm};
use super::object_arg;
use std::rc::Rc;

/// ToPropertyDescriptor (§6.2.6.5).
pub fn to_property_descriptor(vm: &mut Vm, v: &Value) -> JsResult<PropDesc> {
    let Value::Object(o) = v else { return vm.type_error("Property description must be an object") };
    let o = *o;
    let mut d = PropDesc::default();
    let field = |vm: &mut Vm, name: &str| -> JsResult<Option<Value>> {
        let k = PropKey::named(name);
        if vm.has_property(o, &k)? {
            Ok(Some(vm.get(o, &k)?))
        } else {
            Ok(None)
        }
    };
    if let Some(v) = field(vm, "enumerable")? {
        d.enumerable = Some(v.truthy());
    }
    if let Some(v) = field(vm, "configurable")? {
        d.configurable = Some(v.truthy());
    }
    if let Some(v) = field(vm, "value")? {
        d.value = Some(v);
    }
    if let Some(v) = field(vm, "writable")? {
        d.writable = Some(v.truthy());
    }
    if let Some(g) = field(vm, "get")? {
        if !g.is_undefined() && !vm.is_callable(&g) {
            return vm.type_error("Getter must be a function");
        }
        d.get = Some(g.as_object());
    }
    if let Some(s) = field(vm, "set")? {
        if !s.is_undefined() && !vm.is_callable(&s) {
            return vm.type_error("Setter must be a function");
        }
        d.set = Some(s.as_object());
    }
    if d.is_accessor() && d.is_data() {
        return vm.type_error(
            "Invalid property descriptor. Cannot both specify accessors and a value or writable attribute",
        );
    }
    Ok(d)
}

/// FromPropertyDescriptor (§6.2.6.4).
pub fn from_property_descriptor(vm: &mut Vm, d: Option<PropDesc>) -> Value {
    let Some(d) = d else { return Value::Undefined };
    let o = vm.new_object();
    let obj = vm.heap.obj_mut(o);
    let mut put = |k: &str, v: Value| obj.props.insert(PropKey::named(k), Prop::data(v, true, true, true));
    if let Some(v) = d.value {
        put("value", v);
    }
    if let Some(w) = d.writable {
        put("writable", Value::Bool(w));
    }
    if let Some(g) = d.get {
        put("get", g.map(Value::Object).unwrap_or_default());
    }
    if let Some(s) = d.set {
        put("set", s.map(Value::Object).unwrap_or_default());
    }
    if let Some(e) = d.enumerable {
        put("enumerable", Value::Bool(e));
    }
    if let Some(c) = d.configurable {
        put("configurable", Value::Bool(c));
    }
    Value::Object(o)
}

fn proto_arg(vm: &mut Vm, v: &Value) -> JsResult<Option<ObjRef>> {
    match v {
        Value::Object(p) => Ok(Some(*p)),
        Value::Null => Ok(None),
        _ => vm.type_error("Object prototype may only be an Object or null"),
    }
}

/// ObjectDefineProperties (§20.1.2.3.1).
fn define_properties(vm: &mut Vm, o: ObjRef, props: &Value) -> JsResult<()> {
    let p = vm.to_object(props)?;
    let mut descs = Vec::new();
    for k in vm.own_property_keys(p)? {
        if let Some(pd) = vm.get_own_property(p, &k)? {
            if pd.enumerable == Some(true) {
                let dv = vm.get(p, &k)?;
                descs.push((k, to_property_descriptor(vm, &dv)?));
            }
        }
    }
    for (k, d) in descs {
        vm.define_own_property_or_throw(o, k, d)?;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum KeysKind {
    Keys,
    Values,
    Entries,
}

/// EnumerableOwnProperties (§7.3.23).
fn enumerable_own(vm: &mut Vm, o: ObjRef, kind: KeysKind) -> JsResult<Vec<Value>> {
    let mut out = Vec::new();
    for k in vm.own_property_keys(o)? {
        if matches!(k, PropKey::Sym(_)) {
            continue;
        }
        if let Some(d) = vm.get_own_property(o, &k)? {
            if d.enumerable == Some(true) {
                match kind {
                    KeysKind::Keys => out.push(k.to_value()),
                    KeysKind::Values => out.push(vm.get(o, &k)?),
                    KeysKind::Entries => {
                        let v = vm.get(o, &k)?;
                        out.push(Value::Object(vm.new_array(vec![k.to_value(), v])));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// IsArray (§7.2.2), seeing through proxies.
pub(super) fn is_array(vm: &mut Vm, v: &Value) -> JsResult<bool> {
    let Value::Object(o) = v else { return Ok(false) };
    match &vm.heap.obj(*o).kind {
        Kind::Array => Ok(true),
        Kind::Proxy(p) => match p.target {
            Some(t) => is_array(vm, &Value::Object(t)),
            None => vm.type_error("Cannot perform 'IsArray' on a proxy that has been revoked"),
        },
        _ => Ok(false),
    }
}

/// Object.prototype.toString (§20.1.3.6).
fn object_to_string(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let tag = match &c.this {
        Value::Undefined => return Ok(Value::str("[object Undefined]")),
        Value::Null => return Ok(Value::str("[object Null]")),
        _ => {
            let o = vm.to_object(&c.this)?;
            let is_arr = is_array(vm, &Value::Object(o))?;
            let callable = vm.heap.obj(o).is_callable();
            let builtin = match &vm.heap.obj(o).kind {
                _ if is_arr => "Array",
                _ if callable => "Function",
                Kind::Array => "Array",
                Kind::Arguments => "Arguments",
                Kind::Function(_) | Kind::Native(_) | Kind::Bound(_) => "Function",
                Kind::Error => "Error",
                Kind::Boolean(_) => "Boolean",
                Kind::Number(_) => "Number",
                Kind::String(_) => "String",
                Kind::Date(_) => "Date",
                Kind::RegExp(_) => "RegExp",
                _ => "Object",
            };
            let k = PropKey::Sym(vm.realm.sym_to_string_tag.clone());
            match vm.get(o, &k)? {
                Value::String(s) => s,
                _ => js_str(builtin),
            }
        }
    };
    let mut v: Vec<u16> = "[object ".encode_utf16().collect();
    v.extend_from_slice(&tag);
    v.push(b']' as u16);
    Ok(Value::String(v.into()))
}

pub(super) fn install(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let ctor = realm::constructor(
        vm,
        "Object",
        1,
        |vm, c| {
            if let Some(nt) = c.new_target {
                if nt != c.callee {
                    let p = vm.prototype_from_ctor(nt, |r| r.object_proto)?;
                    return Ok(Value::Object(vm.new_object_with_proto(Some(p))));
                }
            }
            let v = c.arg(0);
            if v.is_nullish() {
                return Ok(Value::Object(vm.new_object()));
            }
            Ok(Value::Object(vm.to_object(&v)?))
        },
        op,
    );
    vm.realm.object_ctor = ctor;

    method(vm, ctor, "getPrototypeOf", 1, |vm, c| {
        let o = vm.to_object(&c.arg(0))?;
        Ok(vm.get_prototype_of(o)?.map(Value::Object).unwrap_or(Value::Null))
    });
    method(vm, ctor, "setPrototypeOf", 2, |vm, c| {
        let o = c.arg(0);
        if o.is_nullish() {
            return vm.type_error("Object.setPrototypeOf called on null or undefined");
        }
        let p = proto_arg(vm, &c.arg(1))?;
        let Value::Object(obj) = o else { return Ok(o) };
        if !vm.set_prototype_of(obj, p)? {
            return vm.type_error("Object.setPrototypeOf failed");
        }
        Ok(o)
    });
    method(vm, ctor, "create", 2, |vm, c| {
        let p = proto_arg(vm, &c.arg(0))?;
        let o = vm.new_object_with_proto(p);
        let props = c.arg(1);
        if !props.is_undefined() {
            define_properties(vm, o, &props)?;
        }
        Ok(Value::Object(o))
    });
    method(vm, ctor, "defineProperty", 3, |vm, c| {
        let o = object_arg(vm, &c.arg(0))?;
        let k = vm.to_property_key(&c.arg(1))?;
        let d = to_property_descriptor(vm, &c.arg(2))?;
        vm.define_own_property_or_throw(o, k, d)?;
        Ok(Value::Object(o))
    });
    method(vm, ctor, "defineProperties", 2, |vm, c| {
        let o = object_arg(vm, &c.arg(0))?;
        define_properties(vm, o, &c.arg(1))?;
        Ok(Value::Object(o))
    });
    method(vm, ctor, "getOwnPropertyDescriptor", 2, |vm, c| {
        let o = vm.to_object(&c.arg(0))?;
        let k = vm.to_property_key(&c.arg(1))?;
        let d = vm.get_own_property(o, &k)?;
        Ok(from_property_descriptor(vm, d))
    });
    method(vm, ctor, "getOwnPropertyDescriptors", 1, |vm, c| {
        let o = vm.to_object(&c.arg(0))?;
        let out = vm.new_object();
        for k in vm.own_property_keys(o)? {
            let d = vm.get_own_property(o, &k)?;
            if d.is_some() {
                let dv = from_property_descriptor(vm, d);
                vm.create_data_property_or_throw(out, k, dv)?;
            }
        }
        Ok(Value::Object(out))
    });
    method(vm, ctor, "getOwnPropertyNames", 1, |vm, c| {
        let o = vm.to_object(&c.arg(0))?;
        let keys: Vec<Value> = vm
            .own_property_keys(o)?
            .into_iter()
            .filter(|k| !matches!(k, PropKey::Sym(_)))
            .map(|k| k.to_value())
            .collect();
        Ok(Value::Object(vm.new_array(keys)))
    });
    method(vm, ctor, "getOwnPropertySymbols", 1, |vm, c| {
        let o = vm.to_object(&c.arg(0))?;
        let keys: Vec<Value> = vm
            .own_property_keys(o)?
            .into_iter()
            .filter(|k| matches!(k, PropKey::Sym(_)))
            .map(|k| k.to_value())
            .collect();
        Ok(Value::Object(vm.new_array(keys)))
    });
    method(vm, ctor, "keys", 1, |vm, c| {
        let o = vm.to_object(&c.arg(0))?;
        let v = enumerable_own(vm, o, KeysKind::Keys)?;
        Ok(Value::Object(vm.new_array(v)))
    });
    method(vm, ctor, "values", 1, |vm, c| {
        let o = vm.to_object(&c.arg(0))?;
        let v = enumerable_own(vm, o, KeysKind::Values)?;
        Ok(Value::Object(vm.new_array(v)))
    });
    method(vm, ctor, "entries", 1, |vm, c| {
        let o = vm.to_object(&c.arg(0))?;
        let v = enumerable_own(vm, o, KeysKind::Entries)?;
        Ok(Value::Object(vm.new_array(v)))
    });
    method(vm, ctor, "fromEntries", 1, |vm, c| {
        let it = c.arg(0);
        if it.is_nullish() {
            return vm.type_error("Object.fromEntries requires an iterable");
        }
        let o = vm.new_object();
        let r = vm.get_iterator(&it)?;
        while let Some(e) = vm.iter_step(r)? {
            let step = (|| -> JsResult<()> {
                let Value::Object(eo) = e else {
                    return vm.type_error("Iterator value is not an entry object");
                };
                let k = vm.get(eo, &PropKey::Index(0))?;
                let v = vm.get(eo, &PropKey::Index(1))?;
                let key = vm.to_property_key(&k)?;
                vm.create_data_property_or_throw(o, key, v)
            })();
            if let Err(err) = step {
                let _ = vm.iterator_close(r);
                return Err(err);
            }
        }
        Ok(Value::Object(o))
    });
    method(vm, ctor, "assign", 2, |vm, c| {
        let to = vm.to_object(&c.arg(0))?;
        for src in c.args.iter().skip(1) {
            if src.is_nullish() {
                continue;
            }
            let from = vm.to_object(src)?;
            for k in vm.own_property_keys(from)? {
                if let Some(d) = vm.get_own_property(from, &k)? {
                    if d.enumerable == Some(true) {
                        let v = vm.get(from, &k)?;
                        vm.set_or_throw(to, k, v)?;
                    }
                }
            }
        }
        Ok(Value::Object(to))
    });
    method(vm, ctor, "freeze", 1, |vm, c| {
        if let Value::Object(o) = c.arg(0) {
            if !vm.set_integrity(o, true)? {
                return vm.type_error("Cannot freeze");
            }
        }
        Ok(c.arg(0))
    });
    method(vm, ctor, "seal", 1, |vm, c| {
        if let Value::Object(o) = c.arg(0) {
            if !vm.set_integrity(o, false)? {
                return vm.type_error("Cannot seal");
            }
        }
        Ok(c.arg(0))
    });
    method(vm, ctor, "preventExtensions", 1, |vm, c| {
        if let Value::Object(o) = c.arg(0) {
            if !vm.prevent_extensions(o)? {
                return vm.type_error("Cannot prevent extensions");
            }
        }
        Ok(c.arg(0))
    });
    method(vm, ctor, "isFrozen", 1, |vm, c| match c.arg(0) {
        Value::Object(o) => Ok(Value::Bool(vm.test_integrity(o, true)?)),
        _ => Ok(Value::Bool(true)),
    });
    method(vm, ctor, "isSealed", 1, |vm, c| match c.arg(0) {
        Value::Object(o) => Ok(Value::Bool(vm.test_integrity(o, false)?)),
        _ => Ok(Value::Bool(true)),
    });
    method(vm, ctor, "isExtensible", 1, |vm, c| match c.arg(0) {
        Value::Object(o) => Ok(Value::Bool(vm.is_extensible(o)?)),
        _ => Ok(Value::Bool(false)),
    });
    method(vm, ctor, "is", 2, |_, c| Ok(Value::Bool(super::super::value::same_value(&c.arg(0), &c.arg(1)))));
    method(vm, ctor, "hasOwn", 2, |vm, c| {
        let o = vm.to_object(&c.arg(0))?;
        let k = vm.to_property_key(&c.arg(1))?;
        Ok(Value::Bool(vm.has_own_property(o, &k)?))
    });

    // Object.prototype
    method(vm, op, "hasOwnProperty", 1, |vm, c| {
        let k = vm.to_property_key(&c.arg(0))?;
        let o = vm.to_object(&c.this)?;
        Ok(Value::Bool(vm.has_own_property(o, &k)?))
    });
    method(vm, op, "isPrototypeOf", 1, |vm, c| {
        let Value::Object(mut v) = c.arg(0) else { return Ok(Value::Bool(false)) };
        let o = vm.to_object(&c.this)?;
        loop {
            match vm.get_prototype_of(v)? {
                None => return Ok(Value::Bool(false)),
                Some(p) if p == o => return Ok(Value::Bool(true)),
                Some(p) => v = p,
            }
        }
    });
    method(vm, op, "propertyIsEnumerable", 1, |vm, c| {
        let k = vm.to_property_key(&c.arg(0))?;
        let o = vm.to_object(&c.this)?;
        Ok(Value::Bool(vm.get_own_property(o, &k)?.is_some_and(|d| d.enumerable == Some(true))))
    });
    method(vm, op, "toString", 0, object_to_string);
    method(vm, op, "toLocaleString", 0, |vm, c| {
        let f = vm.get_value_key(&c.this, &PropKey::lit("toString"))?;
        vm.call(&f, c.this, Vec::new())
    });
    method(vm, op, "valueOf", 0, |vm, c| Ok(Value::Object(vm.to_object(&c.this)?)));
    accessor(
        vm,
        op,
        PropKey::lit("__proto__"),
        Some(|vm, c| {
            let o = vm.to_object(&c.this)?;
            Ok(vm.get_prototype_of(o)?.map(Value::Object).unwrap_or(Value::Null))
        }),
        Some(|vm, c| {
            if c.this.is_nullish() {
                return vm.type_error("Object.prototype.__proto__ called on null or undefined");
            }
            let p = match c.arg(0) {
                Value::Object(p) => Some(p),
                Value::Null => None,
                _ => return Ok(Value::Undefined),
            };
            let Value::Object(o) = c.this else { return Ok(Value::Undefined) };
            if !vm.set_prototype_of(o, p)? {
                return vm.type_error("Object.prototype.__proto__ setter failed");
            }
            Ok(Value::Undefined)
        }),
    );
    // Annex B.2.2.2–5.
    method(vm, op, "__defineGetter__", 2, |vm, c| {
        let o = vm.to_object(&c.this)?;
        if !vm.is_callable(&c.arg(1)) {
            return vm.type_error("getter is not a function");
        }
        let k = vm.to_property_key(&c.arg(0))?;
        let d = PropDesc {
            get: Some(c.arg(1).as_object()),
            enumerable: Some(true),
            configurable: Some(true),
            ..Default::default()
        };
        vm.define_own_property_or_throw(o, k, d)?;
        Ok(Value::Undefined)
    });
    method(vm, op, "__defineSetter__", 2, |vm, c| {
        let o = vm.to_object(&c.this)?;
        if !vm.is_callable(&c.arg(1)) {
            return vm.type_error("setter is not a function");
        }
        let k = vm.to_property_key(&c.arg(0))?;
        let d = PropDesc {
            set: Some(c.arg(1).as_object()),
            enumerable: Some(true),
            configurable: Some(true),
            ..Default::default()
        };
        vm.define_own_property_or_throw(o, k, d)?;
        Ok(Value::Undefined)
    });
    method(vm, op, "__lookupGetter__", 1, |vm, c| lookup_accessor(vm, c, true));
    method(vm, op, "__lookupSetter__", 1, |vm, c| lookup_accessor(vm, c, false));

    install_function(vm);
}

fn lookup_accessor(vm: &mut Vm, c: NativeCall, getter: bool) -> JsResult<Value> {
    let mut o = vm.to_object(&c.this)?;
    let k = vm.to_property_key(&c.arg(0))?;
    loop {
        if let Some(d) = vm.get_own_property(o, &k)? {
            let f = if getter { d.get } else { d.set };
            return Ok(f.flatten().map(Value::Object).unwrap_or_default());
        }
        match vm.get_prototype_of(o)? {
            Some(p) => o = p,
            None => return Ok(Value::Undefined),
        }
    }
}

// ---- Function (§20.2) ----

/// CreateDynamicFunction (§20.2.1.1.1) for normal functions.
fn function_ctor(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    // The kind is the constructor's slot: 0 Function, 1 GeneratorFunction,
    // 2 AsyncFunction, 3 AsyncGeneratorFunction.
    let kind = match &vm.heap.obj(c.callee).kind {
        Kind::Native(n) => match n.slots.first() {
            Some(Value::Number(k)) => *k as u8,
            _ => 0,
        },
        _ => 0,
    };
    let prefix = ["function", "function*", "async function", "async function*"][kind as usize];
    let n = c.args.len();
    let mut params = Vec::new();
    for a in c.args.iter().take(n.saturating_sub(1)) {
        params.push(to_rust(&vm.to_string(a)?));
    }
    let body = if n > 0 { to_rust(&vm.to_string(&c.args[n - 1])?) } else { String::new() };
    let p = params.join(",");
    let source = format!("{prefix} anonymous({p}\n) {{\n{body}\n}}");
    // Parameters and body must each parse on their own (no `){` injection).
    let check_params = format!("({prefix} ({p}\n) {{}})");
    let check_body = format!("({prefix} () {{\n{body}\n}})");
    let ok_alone = |s: &str| {
        matches!(crate::parse_script(s), Ok(prog) if prog.body.len() == 1
            && matches!(&prog.body[0], crate::ast::Stmt::Expr(crate::ast::Expr::Paren(e)) if matches!(**e, crate::ast::Expr::Function(_))))
    };
    if !ok_alone(&check_params) || !ok_alone(&check_body) {
        return vm.syntax_error("Invalid function arguments or body");
    }
    let prog = match crate::parse_script(&source) {
        Ok(p) => p,
        Err(e) => return vm.syntax_error(&e.msg),
    };
    let f = match prog.body.as_slice() {
        [crate::ast::Stmt::Function(f)] => f.clone(),
        _ => return vm.syntax_error("Invalid function body"),
    };
    let src: Rc<[char]> = source.chars().collect::<Vec<_>>().into();
    let tmpl = Rc::new(super::super::code::FuncTemplate {
        length: f.params.iter().take_while(|p| !matches!(p, crate::ast::Pat::Default(..))).count() as u32,
        ast: f,
        source: src,
        code: Default::default(),
        is_expression: false,
        given_name: None,
        default_ctor: false,
        text_span: None,
    });
    let genv = vm.realm.global_env;
    let fo = vm.make_function(tmpl, genv, None, true);
    if let Some(nt) = c.new_target {
        if nt != c.callee {
            let p = if kind == 1 {
                vm.prototype_from_ctor(nt, |r| r.generator_function_proto)?
            } else if kind == 2 {
                vm.prototype_from_ctor(nt, |r| r.async_function_proto)?
            } else if kind == 3 {
                vm.prototype_from_ctor(nt, |r| r.async_generator_function_proto)?
            } else {
                vm.prototype_from_ctor(nt, |r| r.function_proto)?
            };
            vm.heap.obj_mut(fo).proto = Some(p);
        }
    }
    Ok(Value::Object(fo))
}

fn function_to_string(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let Value::Object(o) = c.this else {
        return vm.type_error("Function.prototype.toString requires that 'this' be a Function");
    };
    let s = match &vm.heap.obj(o).kind {
        Kind::Function(cl) => cl.tmpl.source_text(),
        Kind::Native(_) | Kind::Bound(_) => {
            let name = match vm.heap.obj(o).props.get(&PropKey::lit("name")).map(|p| &p.slot) {
                Some(Slot::Data(Value::String(s))) if matches!(vm.heap.obj(o).kind, Kind::Native(_)) => {
                    to_rust(s)
                }
                _ => String::new(),
            };
            format!("function {name}() {{ [native code] }}")
        }
        _ => return vm.type_error("Function.prototype.toString requires that 'this' be a Function"),
    };
    Ok(Value::str(&s))
}

fn install_function(vm: &mut Vm) {
    let fp = vm.realm.function_proto;
    {
        let o = vm.heap.obj_mut(fp);
        o.props.insert(PropKey::lit("length"), Prop::data(Value::Number(0.0), false, false, true));
        o.props.insert(PropKey::lit("name"), Prop::data(Value::str(""), false, false, true));
    }
    let ctor = realm::constructor(vm, "Function", 1, function_ctor, fp);
    vm.realm.function_ctor = ctor;
    method(vm, fp, "call", 1, |vm, c| {
        if !vm.is_callable(&c.this) {
            return vm.type_error("Function.prototype.call called on non-function");
        }
        let this = c.arg(0);
        let args = c.args.iter().skip(1).cloned().collect();
        vm.call(&c.this, this, args)
    });
    method(vm, fp, "apply", 2, |vm, c| {
        if !vm.is_callable(&c.this) {
            return vm.type_error("Function.prototype.apply was called on non-function");
        }
        let a = c.arg(1);
        let args = if a.is_nullish() { Vec::new() } else { vm.list_from_array_like(&a)? };
        vm.call(&c.this, c.arg(0), args)
    });
    method(vm, fp, "bind", 1, |vm, c| {
        let Value::Object(target) = c.this else { return vm.type_error("Bind must be called on a function") };
        if !vm.heap.obj(target).is_callable() {
            return vm.type_error("Bind must be called on a function");
        }
        let args: Vec<Value> = c.args.iter().skip(1).cloned().collect();
        let nargs = args.len() as f64;
        let proto = vm.get_prototype_of(target)?;
        let b =
            vm.heap.alloc(Object::new(proto, Kind::Bound(Box::new(Bound { target, this: c.arg(0), args }))));
        // SetFunctionLength / SetFunctionName (§20.2.3.2 steps 4–10).
        let mut len = 0.0;
        let lk = PropKey::lit("length");
        if vm.has_own_property(target, &lk)? {
            if let Value::Number(n) = vm.get(target, &lk)? {
                len = if n == f64::INFINITY {
                    f64::INFINITY
                } else if n == f64::NEG_INFINITY {
                    0.0
                } else {
                    (n.trunc() - nargs).max(0.0)
                };
            }
        }
        let name = match vm.get(target, &PropKey::lit("name"))? {
            Value::String(s) => s,
            _ => js_str(""),
        };
        let mut n: Vec<u16> = "bound ".encode_utf16().collect();
        n.extend_from_slice(&name);
        let o = vm.heap.obj_mut(b);
        o.props.insert(lk, Prop::data(Value::Number(len + 0.0), false, false, true));
        o.props.insert(PropKey::lit("name"), Prop::data(Value::String(n.into()), false, false, true));
        Ok(Value::Object(b))
    });
    method(vm, fp, "toString", 0, function_to_string);
    let hi = vm.realm.sym_has_instance.clone();
    method_sym(vm, fp, hi.clone(), "[Symbol.hasInstance]", 1, |vm, c| {
        Ok(Value::Bool(vm.ordinary_has_instance(&c.this, &c.arg(0))?))
    });
    if let Some(p) = vm.heap.obj_mut(fp).props.get_mut(&PropKey::Sym(hi)) {
        p.writable = false;
        p.configurable = false;
    }
    let tte = vm.realm.throw_type_error;
    for k in ["caller", "arguments"] {
        vm.heap.obj_mut(fp).props.insert(
            PropKey::named(k),
            Prop {
                slot: Slot::Accessor(Some(tte), Some(tte)),
                writable: false,
                enumerable: false,
                configurable: true,
            },
        );
    }
    let _ = (value, ErrorKind::Type);
}

/// %GeneratorFunction% (§27.3.1): not a global; reached through generator functions.
pub(super) fn install_dynamic_kinds(vm: &mut Vm) {
    let gfp = vm.realm.generator_function_proto;
    let f = realm::native_with(vm, "GeneratorFunction", 1, function_ctor, true, vec![Value::Number(1.0)]);
    let fc = vm.realm.function_ctor;
    vm.heap.obj_mut(f).proto = Some(fc);
    vm.heap
        .obj_mut(f)
        .props
        .insert(PropKey::lit("prototype"), Prop::data(Value::Object(gfp), false, false, false));
    vm.heap
        .obj_mut(gfp)
        .props
        .insert(PropKey::lit("constructor"), Prop::data(Value::Object(f), false, false, true));
    let afp = vm.realm.async_function_proto;
    let af = realm::native_with(vm, "AsyncFunction", 1, function_ctor, true, vec![Value::Number(2.0)]);
    vm.heap.obj_mut(af).proto = Some(fc);
    vm.heap
        .obj_mut(af)
        .props
        .insert(PropKey::lit("prototype"), Prop::data(Value::Object(afp), false, false, false));
    vm.heap
        .obj_mut(afp)
        .props
        .insert(PropKey::lit("constructor"), Prop::data(Value::Object(af), false, false, true));
    let agfp = vm.realm.async_generator_function_proto;
    let ag =
        realm::native_with(vm, "AsyncGeneratorFunction", 1, function_ctor, true, vec![Value::Number(3.0)]);
    vm.heap.obj_mut(ag).proto = Some(fc);
    vm.heap
        .obj_mut(ag)
        .props
        .insert(PropKey::lit("prototype"), Prop::data(Value::Object(agfp), false, false, false));
    vm.heap
        .obj_mut(agfp)
        .props
        .insert(PropKey::lit("constructor"), Prop::data(Value::Object(ag), false, false, true));
}

//! Classes at run time: ClassDefinitionEvaluation (§15.7.14), fields and private elements
//! (§7.3.26–§7.3.33), `super` calls and property references (§13.3.7).

use super::heap::*;
use super::object::PropDesc;
use super::value::{to_rust, PropKey, Sym, Value};
use super::{JsResult, Vm};

impl Vm {
    fn this_env(&self, env: EnvRef) -> Option<EnvRef> {
        let mut e = Some(env);
        while let Some(r) = e {
            match &self.heap.env(r).kind {
                EnvKind::Function { .. } | EnvKind::Global { .. } => return Some(r),
                _ => e = self.heap.env(r).outer,
            }
        }
        None
    }

    fn closure_mut(&mut self, f: ObjRef) -> Option<&mut Closure> {
        match &mut self.heap.obj_mut(f).kind {
            Kind::Function(c) => Some(c),
            _ => None,
        }
    }

    /// Steps 5–18 of ClassDefinitionEvaluation: `[superclass?] → [F proto]`.
    pub(super) fn class_create(&mut self, t: u32, heritage: bool) -> JsResult<()> {
        let (proto_parent, ctor_parent) = if heritage {
            let s = self.pop();
            match &s {
                Value::Null => (None, self.realm.function_proto),
                Value::Object(so) if self.is_constructor(&s) => {
                    let so = *so;
                    match self.get(so, &PropKey::lit("prototype"))? {
                        Value::Object(p) => (Some(p), so),
                        Value::Null => (None, so),
                        _ => {
                            return self
                                .type_error("Class extends value does not have valid prototype property")
                        }
                    }
                }
                _ => {
                    let d = self.describe_short(&s);
                    return self.type_error(&format!("Class extends value {d} is not a constructor or null"));
                }
            }
        } else {
            (Some(self.realm.object_proto), self.realm.function_proto)
        };
        let proto = self.new_object_with_proto(proto_parent);
        let tmpl = self.top().code.funcs[t as usize].clone();
        let env = self.top().env;
        let f = self.make_function(tmpl, env, Some(proto), false);
        if let Some(c) = self.closure_mut(f) {
            c.constructor = true;
            c.class_ctor = true;
            c.derived = heritage;
            c.this_mode = ThisMode::Strict;
        }
        let o = self.heap.obj_mut(f);
        o.proto = Some(ctor_parent);
        o.props.insert(PropKey::lit("prototype"), Prop::data(Value::Object(proto), false, false, false));
        self.heap.obj_mut(proto).props.insert(PropKey::lit("constructor"), Prop::method(Value::Object(f)));
        self.push(Value::Object(f));
        self.push(Value::Object(proto));
        Ok(())
    }

    fn class_parts(&self) -> (ObjRef, ObjRef) {
        let proto = self.peek(0).as_object().expect("class prototype");
        let f = self.peek(1).as_object().expect("class constructor");
        (f, proto)
    }

    /// A method, getter or setter (§15.4.4 / §15.7.13 ClassElementEvaluation).
    pub(super) fn class_method(&mut self, t: u32, kind: u8, flags: u8) -> JsResult<()> {
        let key = self.pop();
        let (f, proto) = self.class_parts();
        let is_static = flags & 1 != 0;
        let home = if is_static { f } else { proto };
        let tmpl = self.top().code.funcs[t as usize].clone();
        let env = self.top().env;
        let m = self.make_function(tmpl, env, Some(home), false);
        let prefix = match kind {
            1 => Some("get"),
            2 => Some("set"),
            _ => None,
        };
        if let Value::Symbol(s) = &key {
            if s.0.private {
                self.set_function_name(m, &PropKey::Str(s.0.description.clone().unwrap_or_default()), prefix);
                let elem = match kind {
                    1 => PrivElem::Accessor(Some(m), None),
                    2 => PrivElem::Accessor(None, Some(m)),
                    _ => PrivElem::Method(m),
                };
                let list = if is_static {
                    &mut self.heap.obj_mut(f).private
                } else {
                    &mut self.closure_mut(f).expect("class constructor").private_methods
                };
                merge_private(list, s, elem);
                return Ok(());
            }
        }
        let k = self.to_property_key(&key)?;
        self.set_function_name(m, &k, prefix);
        let d = match kind {
            0 => PropDesc::data(Value::Object(m), true, false, true),
            1 => PropDesc {
                get: Some(Some(m)),
                enumerable: Some(false),
                configurable: Some(true),
                ..Default::default()
            },
            _ => PropDesc {
                set: Some(Some(m)),
                enumerable: Some(false),
                configurable: Some(true),
                ..Default::default()
            },
        };
        self.define_own_property_or_throw(home, k, d)
    }

    fn field_key(&mut self, key: &Value) -> JsResult<FieldKey> {
        if let Value::Symbol(s) = key {
            if s.0.private {
                return Ok(FieldKey::Private(s.clone()));
            }
        }
        Ok(FieldKey::Key(self.to_property_key(key)?))
    }

    /// ClassFieldDefinitionEvaluation (§15.7.10).
    pub(super) fn class_field(&mut self, t: u32, is_static: bool) -> JsResult<()> {
        let key = self.pop();
        let (f, proto) = self.class_parts();
        let fk = self.field_key(&key)?;
        let init = if t == u32::MAX {
            None
        } else {
            let tmpl = self.top().code.funcs[t as usize].clone();
            let env = self.top().env;
            let i = self.make_function(tmpl, env, Some(if is_static { f } else { proto }), false);
            if let Some(c) = self.closure_mut(i) {
                c.field_name = Some(key);
            }
            Some(i)
        };
        let field = ClassField { key: fk, init };
        let c = self.closure_mut(f).expect("class constructor");
        if is_static {
            c.static_elems.push(StaticElem::Field(field));
        } else {
            c.fields.push(field);
        }
        Ok(())
    }

    pub(super) fn class_static_block(&mut self, t: u32) -> JsResult<()> {
        let (f, _) = self.class_parts();
        let tmpl = self.top().code.funcs[t as usize].clone();
        let env = self.top().env;
        let b = self.make_function(tmpl, env, Some(f), false);
        self.closure_mut(f).expect("class constructor").static_elems.push(StaticElem::Block(b));
        Ok(())
    }

    /// Steps 31–36: static fields and blocks in order, then `[F proto] → [F]`.
    pub(super) fn class_finish(&mut self) -> JsResult<()> {
        self.pop();
        let Value::Object(f) = self.peek(0) else { return Ok(()) };
        let elems = std::mem::take(&mut self.closure_mut(f).expect("class constructor").static_elems);
        for e in elems {
            match e {
                StaticElem::Field(field) => self.define_field(f, &field)?,
                StaticElem::Block(b) => {
                    self.call(&Value::Object(b), Value::Object(f), Vec::new())?;
                }
            }
        }
        Ok(())
    }

    /// DefineField (§7.3.33).
    fn define_field(&mut self, receiver: ObjRef, field: &ClassField) -> JsResult<()> {
        let v = match field.init {
            Some(i) => self.call(&Value::Object(i), Value::Object(receiver), Vec::new())?,
            None => Value::Undefined,
        };
        match &field.key {
            FieldKey::Private(s) => self.private_add(receiver, s, PrivElem::Field(v)),
            FieldKey::Key(k) => self.create_data_property_or_throw(receiver, k.clone(), v),
        }
    }

    /// InitializeInstanceElements (§7.3.34).
    pub(super) fn init_instance(&mut self, o: ObjRef, ctor: ObjRef) -> JsResult<()> {
        let (methods, fields) = match &self.heap.obj(ctor).kind {
            Kind::Function(c) => (c.private_methods.clone(), c.fields.clone()),
            _ => return Ok(()),
        };
        for (s, m) in methods {
            self.private_add(o, &s, m)?;
        }
        for f in &fields {
            self.define_field(o, f)?;
        }
        Ok(())
    }

    /// PrivateFieldAdd / PrivateMethodOrAccessorAdd (§7.3.31–32).
    fn private_add(&mut self, o: ObjRef, s: &Sym, e: PrivElem) -> JsResult<()> {
        if self.heap.obj(o).private.iter().any(|(k, _)| k == s) {
            let d = s.0.description.as_deref().map(to_rust).unwrap_or_default();
            return self.type_error(&format!("Cannot initialize {d} twice on the same object"));
        }
        self.heap.obj_mut(o).private.push((s.clone(), e));
        Ok(())
    }

    fn private_find(&self, o: ObjRef, s: &Sym) -> Option<PrivElem> {
        self.heap.obj(o).private.iter().find(|(k, _)| k == s).map(|(_, e)| e.clone())
    }

    fn private_missing<T>(&mut self, s: &Sym) -> JsResult<T> {
        let d = s.0.description.as_deref().map(to_rust).unwrap_or_default();
        self.type_error(&format!(
            "Cannot read private member {d} from an object whose class did not declare it"
        ))
    }

    /// PrivateGet (§7.3.30).
    pub(super) fn private_get(&mut self, o: &Value, name: &Value) -> JsResult<Value> {
        let Value::Symbol(s) = name else { return self.type_error("not a private name") };
        let Value::Object(obj) = o else { return self.private_missing(s) };
        match self.private_find(*obj, s) {
            None => self.private_missing(s),
            Some(PrivElem::Field(v)) => Ok(v),
            Some(PrivElem::Method(m)) => Ok(Value::Object(m)),
            Some(PrivElem::Accessor(Some(g), _)) => self.call(&Value::Object(g), o.clone(), Vec::new()),
            Some(PrivElem::Accessor(None, _)) => {
                let d = s.0.description.as_deref().map(to_rust).unwrap_or_default();
                self.type_error(&format!("'{d}' was defined without a getter"))
            }
        }
    }

    /// PrivateSet (§7.3.31).
    pub(super) fn private_set(&mut self, o: &Value, name: &Value, v: Value) -> JsResult<()> {
        let Value::Symbol(s) = name else { return self.type_error("not a private name") };
        let Value::Object(obj) = o else { return self.private_missing(s) };
        let d = s.0.description.as_deref().map(to_rust).unwrap_or_default();
        match self.private_find(*obj, s) {
            None => self.private_missing(s),
            Some(PrivElem::Field(_)) => {
                if let Some((_, e)) = self.heap.obj_mut(*obj).private.iter_mut().find(|(k, _)| k == s) {
                    *e = PrivElem::Field(v);
                }
                Ok(())
            }
            Some(PrivElem::Method(_)) => self.type_error(&format!("Private method '{d}' is not writable")),
            Some(PrivElem::Accessor(_, Some(st))) => {
                self.call(&Value::Object(st), o.clone(), vec![v])?;
                Ok(())
            }
            Some(PrivElem::Accessor(_, None)) => {
                self.type_error(&format!("'{d}' was defined without a setter"))
            }
        }
    }

    /// `#x in obj` (§13.10.1 step 5).
    pub(super) fn private_in(&mut self, name: &Value, o: &Value) -> JsResult<bool> {
        let Value::Object(obj) = o else {
            return self.type_error("Cannot use 'in' operator to search for a private field in a non-object");
        };
        let Value::Symbol(s) = name else { return Ok(false) };
        Ok(self.private_find(*obj, s).is_some())
    }

    /// SuperCall evaluation (§13.3.7.1): construct the parent, bind `this`, init fields.
    pub(super) fn super_call(&mut self, args: Vec<Value>) -> JsResult<Value> {
        let env = self.top().env;
        let Some(fenv) = self.this_env(env) else {
            return self.syntax_error("'super' keyword unexpected here");
        };
        let (func, new_target) = match &self.heap.env(fenv).kind {
            EnvKind::Function { func, new_target, .. } => (*func, new_target.clone()),
            _ => return self.syntax_error("'super' keyword unexpected here"),
        };
        let parent = self.get_prototype_of(func)?;
        let Some(parent) = parent.filter(|p| self.is_constructor(&Value::Object(*p))) else {
            return self.type_error("Super constructor is not a constructor");
        };
        let Value::Object(nt) = new_target else {
            return self.syntax_error("'super' keyword unexpected here");
        };
        let result = self.construct(parent, args, Some(nt))?;
        let Value::Object(r) = result else {
            return self.type_error("super constructor returned a non-object");
        };
        if let EnvKind::Function { this, .. } = &mut self.heap.env_mut(fenv).kind {
            if this.is_some() {
                return self.reference_error("Super constructor may only be called once");
            }
            *this = Some(result.clone());
        }
        self.init_instance(r, func)?;
        Ok(result)
    }

    /// GetSuperBase (§9.1.1.3.5).
    fn super_base(&mut self) -> JsResult<Option<ObjRef>> {
        let env = self.top().env;
        let home = self.this_env(env).and_then(|e| match &self.heap.env(e).kind {
            EnvKind::Function { func, .. } => match &self.heap.obj(*func).kind {
                Kind::Function(c) => c.home,
                _ => None,
            },
            _ => None,
        });
        match home {
            Some(h) => self.get_prototype_of(h),
            None => self.syntax_error("'super' keyword unexpected here"),
        }
    }

    pub(super) fn super_get(&mut self, this: Value, key: &Value) -> JsResult<Value> {
        let k = self.to_property_key(key)?;
        match self.super_base()? {
            Some(b) => self.get_with_receiver(b, &k, this),
            None => self.type_error(&format!("Cannot read properties of null (reading '{}')", k.display())),
        }
    }

    pub(super) fn super_set(&mut self, this: Value, key: &Value, v: Value) -> JsResult<()> {
        let k = self.to_property_key(key)?;
        let name = k.display();
        let Some(b) = self.super_base()? else {
            return self.type_error(&format!("Cannot set properties of null (setting '{name}')"));
        };
        if !self.set(b, k, v, this)? && self.strict() {
            return self.type_error(&format!("Cannot assign to read only property '{name}'"));
        }
        Ok(())
    }

    /// The field name of the running initializer (for SetFnNameField).
    pub(super) fn current_field_name(&self) -> Option<PropKey> {
        let f = self.frames.last()?.callee?;
        match &self.heap.obj(f).kind {
            Kind::Function(c) => match &c.field_name {
                Some(Value::Symbol(s)) if s.0.private => {
                    Some(PropKey::Str(s.0.description.clone().unwrap_or_default()))
                }
                Some(Value::Symbol(s)) => Some(PropKey::Sym(s.clone())),
                Some(Value::String(st)) => Some(PropKey::from_js(st.clone())),
                Some(Value::Number(n)) => Some(PropKey::from_f64(*n)),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Add a private method/accessor half, pairing getters with setters.
fn merge_private(list: &mut Vec<(Sym, PrivElem)>, s: &Sym, e: PrivElem) {
    if let Some((_, cur)) = list.iter_mut().find(|(k, _)| k == s) {
        if let (PrivElem::Accessor(g, st), PrivElem::Accessor(ng, ns)) = (cur, &e) {
            if ng.is_some() {
                *g = *ng;
            }
            if ns.is_some() {
                *st = *ns;
            }
            return;
        }
    }
    list.push((s.clone(), e));
}

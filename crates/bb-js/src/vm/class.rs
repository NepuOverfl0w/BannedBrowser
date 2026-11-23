//! Classes at run time: ClassDefinitionEvaluation (§15.7.14), fields and private elements
//! (§7.3.26–§7.3.33), `super` calls and property references (§13.3.7).

use super::heap::*;
use super::object::PropDesc;
use super::realm;
use super::value::{to_rust, PropKey, Sym, Value};
use super::{JsResult, Vm};

impl Vm {
    fn this_env(&self, env: EnvRef) -> Option<EnvRef> {
        let mut e = Some(env);
        while let Some(r) = e {
            match &self.heap.env(r).kind {
                EnvKind::Function { .. } | EnvKind::Global { .. } | EnvKind::Module { .. } => return Some(r),
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
        let decorators = if flags & 4 != 0 { Some(self.pop()) } else { None };
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
                if let Some(d) = decorators {
                    self.add_decoration(f, d, key.clone(), is_static, DecTarget::Method { kind, func: m })?;
                }
                return Ok(());
            }
        }
        let k = self.to_property_key(&key)?;
        self.set_function_name(m, &k, prefix);
        if let Some(d) = decorators {
            self.add_decoration(f, d, k.to_value(), is_static, DecTarget::Method { kind, func: m })?;
        }
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

    /// ClassFieldDefinitionEvaluation (§15.7.10); with flag 2 an auto-accessor
    /// (proposal-decorators): the value lives in a fresh private field, and a getter and
    /// setter for the element's key read and write it.
    pub(super) fn class_field(&mut self, t: u32, flags: u8) -> JsResult<()> {
        let is_static = flags & 1 != 0;
        let key = self.pop();
        let decorators = if flags & 4 != 0 { Some(self.pop()) } else { None };
        let (f, proto) = self.class_parts();
        let mut fk = self.field_key(&key)?;
        let mut accessor = None;
        if flags & 2 != 0 {
            let desc = match &fk {
                FieldKey::Private(s) => s.0.description.as_deref().map(to_rust).unwrap_or_default(),
                FieldKey::Key(k) => k.display(),
            };
            let storage = Sym::private(&format!("{desc} accessor storage"));
            let home = if is_static { f } else { proto };
            let (g, st) = self.make_auto_accessor(&fk, &storage)?;
            self.install_accessor(f, home, &fk, is_static, Some(g), Some(st))?;
            accessor = Some((g, st, fk.clone()));
            fk = FieldKey::Private(storage);
        }
        let init = if t == u32::MAX {
            None
        } else {
            let tmpl = self.top().code.funcs[t as usize].clone();
            let env = self.top().env;
            let i = self.make_function(tmpl, env, Some(if is_static { f } else { proto }), false);
            if let Some(c) = self.closure_mut(i) {
                c.field_name = Some(key.clone());
            }
            Some(i)
        };
        let field = ClassField { key: fk, init, transforms: Vec::new(), extra: Vec::new() };
        let c = self.closure_mut(f).expect("class constructor");
        let index = if is_static {
            c.static_elems.push(StaticElem::Field(field));
            c.static_elems.len() - 1
        } else {
            c.fields.push(field);
            c.fields.len() - 1
        };
        if let Some(d) = decorators {
            let (key, target) = match accessor {
                Some((get, set, k)) => (field_key_value(&k), DecTarget::Accessor { index, get, set }),
                None => (key_value_of(self, &key)?, DecTarget::Field { index }),
            };
            self.add_decoration(f, d, key, is_static, target)?;
        }
        Ok(())
    }

    /// The getter and setter of an auto-accessor over the private field `storage`.
    fn make_auto_accessor(&mut self, key: &FieldKey, storage: &Sym) -> JsResult<(ObjRef, ObjRef)> {
        let base = match key {
            FieldKey::Private(s) => s.0.description.as_deref().map(to_rust).unwrap_or_default(),
            FieldKey::Key(k) => match k {
                PropKey::Sym(s) => {
                    format!("[{}]", s.0.description.as_deref().map(to_rust).unwrap_or_default())
                }
                k => k.display(),
            },
        };
        let slots = vec![Value::Symbol(storage.clone())];
        let g = realm::native_with(self, &format!("get {base}"), 0, accessor_get, false, slots.clone());
        let st = realm::native_with(self, &format!("set {base}"), 1, accessor_set, false, slots);
        Ok((g, st))
    }

    /// Define (or replace halves of) an accessor element: a property of `home`, or a private
    /// accessor of the class (static) or its instances.
    fn install_accessor(
        &mut self,
        f: ObjRef,
        home: ObjRef,
        key: &FieldKey,
        is_static: bool,
        get: Option<ObjRef>,
        set: Option<ObjRef>,
    ) -> JsResult<()> {
        match key {
            FieldKey::Private(s) => {
                let list = if is_static {
                    &mut self.heap.obj_mut(f).private
                } else {
                    &mut self.closure_mut(f).expect("class constructor").private_methods
                };
                merge_private(list, s, PrivElem::Accessor(get, set));
                Ok(())
            }
            FieldKey::Key(k) => {
                let d = PropDesc {
                    get: get.map(Some),
                    set: set.map(Some),
                    enumerable: Some(false),
                    configurable: Some(true),
                    ..Default::default()
                };
                self.define_own_property_or_throw(home, k.clone(), d)
            }
        }
    }

    fn add_decoration(
        &mut self,
        f: ObjRef,
        decorators: Value,
        key: Value,
        is_static: bool,
        target: DecTarget,
    ) -> JsResult<()> {
        let decorators = self.list_from_array_like(&decorators)?;
        let c = self.closure_mut(f).expect("class constructor");
        c.decor.get_or_insert_with(Default::default).pending.push(Decoration {
            decorators,
            key,
            is_static,
            target,
        });
        Ok(())
    }

    /// ClassDecorate: `[decorators F proto] → [F proto C]` (proposal-decorators,
    /// ClassDefinitionEvaluation): the element decorators in document order (each list from
    /// the innermost decorator out), Symbol.metadata, then the class decorators.
    pub(super) fn class_decorate(&mut self) -> JsResult<()> {
        let proto_v = self.pop();
        let f_v = self.pop();
        let class_decs = self.pop();
        let (Value::Object(f), Value::Object(proto)) = (f_v.clone(), proto_v.clone()) else {
            return self.type_error("bad class");
        };
        // The metadata object inherits the superclass's (proposal-decorator-metadata).
        let derived = matches!(&self.heap.obj(f).kind, Kind::Function(c) if c.derived);
        let mk = PropKey::Sym(self.realm.sym_metadata.clone());
        let parent = match self.heap.obj(f).proto {
            Some(p) if derived => match self.get(p, &mk)? {
                Value::Object(o) => Some(o),
                _ => None,
            },
            _ => None,
        };
        let metadata = self.new_object_with_proto(parent);
        let pending = self
            .closure_mut(f)
            .and_then(|c| c.decor.as_mut())
            .map(|d| std::mem::take(&mut d.pending))
            .unwrap_or_default();
        for mut d in pending {
            self.apply_element_decorators(f, proto, &mut d, metadata)?;
        }
        self.define_own_property_or_throw(
            f,
            mk,
            PropDesc::data(Value::Object(metadata), false, false, true),
        )?;
        let mut class = f_v.clone();
        let decs = self.list_from_array_like(&class_decs)?;
        let name = match &self.heap.obj(f).kind {
            Kind::Function(c) => c.tmpl.given_name.clone().unwrap_or_else(|| "".into()),
            _ => "".into(),
        };
        let mut inits = Vec::new();
        for dec in decs.iter().rev() {
            let (ctx, add) = self.decorator_context("class", Value::str(&name), None, metadata)?;
            let r = self.call(dec, Value::Undefined, vec![class.clone(), Value::Object(ctx)])?;
            inits.extend(self.finish_context(add)?);
            if !r.is_undefined() {
                if !self.is_callable(&r) {
                    return self.type_error("a class decorator must return a function or undefined");
                }
                class = r;
            }
        }
        if let Some(c) = self.closure_mut(f) {
            c.decor.get_or_insert_with(Default::default).class_inits = inits;
        }
        self.push(f_v);
        self.push(proto_v);
        self.push(class);
        Ok(())
    }

    /// ApplyDecoratorsToElementDefinition for one element.
    fn apply_element_decorators(
        &mut self,
        f: ObjRef,
        proto: ObjRef,
        d: &mut Decoration,
        metadata: ObjRef,
    ) -> JsResult<()> {
        let private = matches!(&d.key, Value::Symbol(s) if s.0.private);
        let name = match &d.key {
            Value::Symbol(s) if s.0.private => Value::String(s.0.description.clone().unwrap_or_default()),
            k => k.clone(),
        };
        let home = if d.is_static { f } else { proto };
        let fkey = if private {
            match &d.key {
                Value::Symbol(s) => FieldKey::Private(s.clone()),
                _ => unreachable!("private key"),
            }
        } else {
            FieldKey::Key(self.to_property_key(&d.key)?)
        };
        let (kind, access) = match &d.target {
            DecTarget::Method { kind: 0, .. } => ("method", (true, false)),
            DecTarget::Method { kind: 1, .. } => ("getter", (true, false)),
            DecTarget::Method { .. } => ("setter", (false, true)),
            DecTarget::Field { .. } => ("field", (true, true)),
            DecTarget::Accessor { .. } => ("accessor", (true, true)),
        };
        let mut extra = Vec::new();
        for dec in d.decorators.clone().iter().rev() {
            let acc = self.access_object(&d.key, private, access)?;
            let (ctx, add) =
                self.decorator_context(kind, name.clone(), Some((d.is_static, private, acc)), metadata)?;
            let value = match &d.target {
                DecTarget::Method { func, .. } => Value::Object(*func),
                DecTarget::Field { .. } => Value::Undefined,
                DecTarget::Accessor { get, set, .. } => {
                    let o = self.new_object();
                    self.create_data_property_or_throw(o, PropKey::lit("get"), Value::Object(*get))?;
                    self.create_data_property_or_throw(o, PropKey::lit("set"), Value::Object(*set))?;
                    Value::Object(o)
                }
            };
            let r = self.call(dec, Value::Undefined, vec![value, Value::Object(ctx)])?;
            extra.extend(self.finish_context(add)?);
            match &mut d.target {
                DecTarget::Method { kind: mk, func } => {
                    if r.is_undefined() {
                        continue;
                    }
                    let Value::Object(nf) = r else {
                        return self.type_error("a method decorator must return a function or undefined");
                    };
                    if !self.is_callable(&r) {
                        return self.type_error("a method decorator must return a function or undefined");
                    }
                    *func = nf;
                    let mk = *mk;
                    match mk {
                        0 => match &fkey {
                            FieldKey::Private(s) => {
                                let list = if d.is_static {
                                    &mut self.heap.obj_mut(f).private
                                } else {
                                    &mut self.closure_mut(f).expect("class constructor").private_methods
                                };
                                if let Some((_, e)) = list.iter_mut().find(|(k, _)| k == s) {
                                    *e = PrivElem::Method(nf);
                                }
                            }
                            FieldKey::Key(k) => self.define_own_property_or_throw(
                                home,
                                k.clone(),
                                PropDesc::data(Value::Object(nf), true, false, true),
                            )?,
                        },
                        1 => self.install_accessor(f, home, &fkey, d.is_static, Some(nf), None)?,
                        _ => self.install_accessor(f, home, &fkey, d.is_static, None, Some(nf))?,
                    }
                }
                DecTarget::Field { index } => {
                    if r.is_undefined() {
                        continue;
                    }
                    let Value::Object(t) = r.clone() else {
                        return self.type_error("a field decorator must return a function or undefined");
                    };
                    if !self.is_callable(&r) {
                        return self.type_error("a field decorator must return a function or undefined");
                    }
                    let index = *index;
                    self.field_record(f, d.is_static, index, |fr| fr.transforms.push(t));
                }
                DecTarget::Accessor { index, get, set } => {
                    if r.is_undefined() {
                        continue;
                    }
                    let Value::Object(ro) = r else {
                        return self.type_error("an accessor decorator must return an object or undefined");
                    };
                    let mut parts = [None, None, None];
                    for (i, n) in ["get", "set", "init"].into_iter().enumerate() {
                        let v = self.get(ro, &PropKey::lit(n))?;
                        if v.is_undefined() {
                            continue;
                        }
                        let Value::Object(o) = v else {
                            return self
                                .type_error(&format!("the accessor decorator's {n} is not a function"));
                        };
                        if !self.is_callable(&Value::Object(o)) {
                            return self
                                .type_error(&format!("the accessor decorator's {n} is not a function"));
                        }
                        parts[i] = Some(o);
                    }
                    if let Some(g) = parts[0] {
                        *get = g;
                    }
                    if let Some(st) = parts[1] {
                        *set = st;
                    }
                    let index = *index;
                    if parts[0].is_some() || parts[1].is_some() {
                        self.install_accessor(f, home, &fkey, d.is_static, parts[0], parts[1])?;
                    }
                    if let Some(i) = parts[2] {
                        self.field_record(f, d.is_static, index, |fr| fr.transforms.push(i));
                    }
                }
            }
        }
        // Extra initializers: fields and accessors run theirs right after the field is
        // defined; methods before the fields (of each instance, or the static ones).
        match d.target {
            DecTarget::Field { index } | DecTarget::Accessor { index, .. } => {
                self.field_record(f, d.is_static, index, |fr| fr.extra.extend(extra));
            }
            DecTarget::Method { .. } => {
                let c = self.closure_mut(f).expect("class constructor");
                let dc = c.decor.get_or_insert_with(Default::default);
                if d.is_static {
                    dc.static_inits.extend(extra);
                } else {
                    dc.instance_inits.extend(extra);
                }
            }
        }
        Ok(())
    }

    fn field_record(&mut self, f: ObjRef, is_static: bool, index: usize, g: impl FnOnce(&mut ClassField)) {
        let Some(c) = self.closure_mut(f) else { return };
        let fr = if is_static {
            match c.static_elems.get_mut(index) {
                Some(StaticElem::Field(fr)) => fr,
                _ => return,
            }
        } else {
            match c.fields.get_mut(index) {
                Some(fr) => fr,
                None => return,
            }
        };
        g(fr);
    }

    /// The context object (proposal-decorators CreateDecoratorContextObject) and its
    /// `addInitializer`, whose list `finish_context` closes and returns.
    fn decorator_context(
        &mut self,
        kind: &str,
        name: Value,
        element: Option<(bool, bool, ObjRef)>,
        metadata: ObjRef,
    ) -> JsResult<(ObjRef, ObjRef)> {
        let ctx = self.new_object();
        self.create_data_property_or_throw(ctx, PropKey::lit("kind"), Value::str(kind))?;
        if let Some((is_static, private, access)) = element {
            self.create_data_property_or_throw(ctx, PropKey::lit("access"), Value::Object(access))?;
            self.create_data_property_or_throw(ctx, PropKey::lit("static"), Value::Bool(is_static))?;
            self.create_data_property_or_throw(ctx, PropKey::lit("private"), Value::Bool(private))?;
        }
        self.create_data_property_or_throw(ctx, PropKey::lit("name"), name)?;
        let list = self.new_array(Vec::new());
        let add = realm::native_with(
            self,
            "addInitializer",
            1,
            add_initializer,
            false,
            vec![Value::Object(list), Value::Bool(false)],
        );
        self.create_data_property_or_throw(ctx, PropKey::lit("addInitializer"), Value::Object(add))?;
        self.create_data_property_or_throw(ctx, PropKey::lit("metadata"), Value::Object(metadata))?;
        Ok((ctx, add))
    }

    /// Mark a context's decoration finished (`addInitializer` throws from now on) and take
    /// the initializers it collected.
    fn finish_context(&mut self, add: ObjRef) -> JsResult<Vec<ObjRef>> {
        let list = match &mut self.heap.obj_mut(add).kind {
            Kind::Native(n) => {
                n.slots[1] = Value::Bool(true);
                n.slots[0].clone()
            }
            _ => return Ok(Vec::new()),
        };
        Ok(self.list_from_array_like(&list)?.into_iter().filter_map(|v| v.as_object()).collect())
    }

    /// The context's `access` object: `get`, `set` and `has` for the element's key.
    fn access_object(&mut self, key: &Value, private: bool, (get, set): (bool, bool)) -> JsResult<ObjRef> {
        let o = self.new_object();
        let slots = vec![key.clone(), Value::Bool(private)];
        if get {
            let g = realm::native_with(self, "get", 1, access_get, false, slots.clone());
            self.create_data_property_or_throw(o, PropKey::lit("get"), Value::Object(g))?;
        }
        if set {
            let s = realm::native_with(self, "set", 2, access_set, false, slots.clone());
            self.create_data_property_or_throw(o, PropKey::lit("set"), Value::Object(s))?;
        }
        let h = realm::native_with(self, "has", 1, access_has, false, slots);
        self.create_data_property_or_throw(o, PropKey::lit("has"), Value::Object(h))?;
        Ok(o)
    }

    pub(super) fn class_static_block(&mut self, t: u32) -> JsResult<()> {
        let (f, _) = self.class_parts();
        let tmpl = self.top().code.funcs[t as usize].clone();
        let env = self.top().env;
        let b = self.make_function(tmpl, env, Some(f), false);
        self.closure_mut(f).expect("class constructor").static_elems.push(StaticElem::Block(b));
        Ok(())
    }

    /// Steps 31–36: static fields and blocks in order, then `[F proto] → [F]`. Decorated:
    /// `[F proto C] → [C]`, the static methods' extra initializers first and the class
    /// decorators' last (with `C`).
    pub(super) fn class_finish(&mut self, decorated: bool) -> JsResult<()> {
        let class = if decorated { Some(self.pop()) } else { None };
        self.pop();
        let Value::Object(f) = self.peek(0) else { return Ok(()) };
        let (static_inits, class_inits) = match self.closure_mut(f).and_then(|c| c.decor.as_mut()) {
            Some(d) => (std::mem::take(&mut d.static_inits), std::mem::take(&mut d.class_inits)),
            None => (Vec::new(), Vec::new()),
        };
        for i in static_inits {
            self.call(&Value::Object(i), Value::Object(f), Vec::new())?;
        }
        let elems = std::mem::take(&mut self.closure_mut(f).expect("class constructor").static_elems);
        for e in elems {
            match e {
                StaticElem::Field(field) => self.define_field(f, &field)?,
                StaticElem::Block(b) => {
                    self.call(&Value::Object(b), Value::Object(f), Vec::new())?;
                }
            }
        }
        if let Some(c) = class {
            for i in class_inits {
                self.call(&Value::Object(i), c.clone(), Vec::new())?;
            }
            self.pop();
            self.push(c);
        }
        Ok(())
    }

    /// DefineField (§7.3.33).
    fn define_field(&mut self, receiver: ObjRef, field: &ClassField) -> JsResult<()> {
        let mut v = match field.init {
            Some(i) => self.call(&Value::Object(i), Value::Object(receiver), Vec::new())?,
            None => Value::Undefined,
        };
        for t in &field.transforms {
            v = self.call(&Value::Object(*t), Value::Object(receiver), vec![v])?;
        }
        match &field.key {
            FieldKey::Private(s) => self.private_add(receiver, s, PrivElem::Field(v))?,
            FieldKey::Key(k) => self.create_data_property_or_throw(receiver, k.clone(), v)?,
        }
        for e in &field.extra {
            self.call(&Value::Object(*e), Value::Object(receiver), Vec::new())?;
        }
        Ok(())
    }

    /// InitializeInstanceElements (§7.3.34).
    pub(super) fn init_instance(&mut self, o: ObjRef, ctor: ObjRef) -> JsResult<()> {
        let (methods, fields, inits) = match &self.heap.obj(ctor).kind {
            Kind::Function(c) => (
                c.private_methods.clone(),
                c.fields.clone(),
                c.decor.as_ref().map(|d| d.instance_inits.clone()).unwrap_or_default(),
            ),
            _ => return Ok(()),
        };
        for (s, m) in methods {
            self.private_add(o, &s, m)?;
        }
        for i in inits {
            self.call(&Value::Object(i), Value::Object(o), Vec::new())?;
        }
        for f in &fields {
            self.define_field(o, f)?;
        }
        Ok(())
    }

    /// PrivateFieldAdd / PrivateMethodOrAccessorAdd (§7.3.31–32), with
    /// nonextensible-applies-to-private: a non-extensible object takes no new private elements.
    fn private_add(&mut self, o: ObjRef, s: &Sym, e: PrivElem) -> JsResult<()> {
        if !self.heap.obj(o).extensible {
            let d = s.0.description.as_deref().map(to_rust).unwrap_or_default();
            return self.type_error(&format!("Cannot add {d} to a non-extensible object"));
        }
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

    /// MakeSuperPropertyReference (§13.3.7.3): GetSuperBase now, the key as given.
    pub(super) fn super_ref(&mut self, key: Value) -> JsResult<Value> {
        let base = self.super_base()?;
        Ok(Value::Object(self.heap.alloc(Object::new(None, Kind::SuperRef(base, key)))))
    }

    /// A Super Reference's base and its name as a property key (GetValue/PutValue step 3.c,
    /// which stores the converted key back into the reference).
    fn super_parts(&mut self, r: &Value) -> JsResult<(Option<ObjRef>, PropKey)> {
        let Value::Object(ro) = r else { return self.type_error("bad super reference") };
        let (base, key) = match &self.heap.obj(*ro).kind {
            Kind::SuperRef(b, k) => (*b, k.clone()),
            _ => return self.type_error("bad super reference"),
        };
        let k = self.to_property_key(&key)?;
        if let Kind::SuperRef(_, slot) = &mut self.heap.obj_mut(*ro).kind {
            *slot = k.to_value();
        }
        Ok((base, k))
    }

    pub(super) fn super_get(&mut self, this: Value, r: &Value) -> JsResult<Value> {
        let (base, k) = self.super_parts(r)?;
        match base {
            Some(b) => self.get_with_receiver(b, &k, this),
            None => self.type_error(&format!("Cannot read properties of null (reading '{}')", k.display())),
        }
    }

    pub(super) fn super_set(&mut self, this: Value, r: &Value, v: Value) -> JsResult<()> {
        let (base, k) = self.super_parts(r)?;
        let Some(b) = base else {
            return self.type_error(&format!("Cannot set properties of null (setting '{}')", k.display()));
        };
        if !self.set(b, k.clone(), v, this)? && self.strict() {
            return self.type_error(&format!("Cannot assign to read only property '{}'", k.display()));
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

/// A field key as a decorator context's key value.
fn field_key_value(k: &FieldKey) -> Value {
    match k {
        FieldKey::Private(s) => Value::Symbol(s.clone()),
        FieldKey::Key(k) => k.to_value(),
    }
}

fn key_value_of(vm: &mut Vm, key: &Value) -> JsResult<Value> {
    if matches!(key, Value::Symbol(s) if s.0.private) {
        return Ok(key.clone());
    }
    Ok(vm.to_property_key(key)?.to_value())
}

fn slots(vm: &Vm, f: ObjRef) -> Vec<Value> {
    match &vm.heap.obj(f).kind {
        Kind::Native(n) => n.slots.clone(),
        _ => Vec::new(),
    }
}

/// An auto-accessor's getter: the private storage field of `this`.
fn accessor_get(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    vm.private_get(&c.this, &s[0])
}

fn accessor_set(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    vm.private_set(&c.this, &s[0], c.arg(0))?;
    Ok(Value::Undefined)
}

/// `context.addInitializer(f)`: only while the decorator runs, and only functions.
fn add_initializer(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    if matches!(s[1], Value::Bool(true)) {
        return vm.type_error("addInitializer can't be called after decoration has finished");
    }
    let f = c.arg(0);
    if !vm.is_callable(&f) {
        return vm.type_error("addInitializer requires a function");
    }
    if let Value::Object(list) = s[0] {
        let n = vm.length_of_array_like(list)?;
        vm.create_data_property_or_throw(list, PropKey::from_f64(n), f)?;
    }
    Ok(Value::Undefined)
}

fn access_target(vm: &mut Vm, o: &Value) -> JsResult<ObjRef> {
    match o {
        Value::Object(o) => Ok(*o),
        _ => vm.type_error("the decorator access target is not an object"),
    }
}

/// `context.access.get(o)`.
fn access_get(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    let o = c.arg(0);
    if matches!(s[1], Value::Bool(true)) {
        return vm.private_get(&o, &s[0]);
    }
    let t = access_target(vm, &o)?;
    let k = vm.to_property_key(&s[0])?;
    vm.get_with_receiver(t, &k, o)
}

/// `context.access.set(o, v)`.
fn access_set(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    let o = c.arg(0);
    if matches!(s[1], Value::Bool(true)) {
        vm.private_set(&o, &s[0], c.arg(1))?;
        return Ok(Value::Undefined);
    }
    let t = access_target(vm, &o)?;
    let k = vm.to_property_key(&s[0])?;
    vm.set_or_throw(t, k, c.arg(1))?;
    Ok(Value::Undefined)
}

/// `context.access.has(o)`.
fn access_has(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    let o = c.arg(0);
    if matches!(s[1], Value::Bool(true)) {
        return Ok(Value::Bool(vm.private_in(&s[0], &o)?));
    }
    let t = access_target(vm, &o)?;
    let k = vm.to_property_key(&s[0])?;
    Ok(Value::Bool(vm.has_property(t, &k)?))
}

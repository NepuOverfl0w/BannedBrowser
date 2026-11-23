//! Identifier resolution over environment records (§9.1, §9.4) and the declaration
//! instantiation of scripts and eval code (§16.1.7, §19.2.1.3).

use super::heap::*;
use super::object::PropDesc;
use super::value::{PropKey, Value};
use super::{ErrorKind, JsResult, Vm};
use crate::parser::EvalContext;
use std::rc::Rc;

impl Vm {
    /// The property key of a binding name (cached by the name's allocation).
    pub(super) fn name_key(&mut self, name: &Rc<str>) -> PropKey {
        let addr = Rc::as_ptr(name) as *const u8 as usize;
        if let Some((_, k)) = self.name_keys.get(&addr) {
            return k.clone();
        }
        let k = PropKey::named(name);
        self.name_keys.insert(addr, (name.clone(), k.clone()));
        k
    }

    /// HasBinding (§9.1.1.*.1).
    fn has_binding(&mut self, r: EnvRef, name: &Rc<str>) -> JsResult<bool> {
        if name.starts_with('#') {
            // Private names live only in class scopes (declarative records).
            return Ok(self.heap.env(r).bindings.contains_key(name));
        }
        let (obj, with) = match &self.heap.env(r).kind {
            EnvKind::Declarative | EnvKind::Function { .. } => {
                return Ok(self.heap.env(r).bindings.contains_key(name))
            }
            EnvKind::Module { imports } => {
                return Ok(imports.contains_key(name) || self.heap.env(r).bindings.contains_key(name))
            }
            EnvKind::Global { object, .. } => {
                if self.heap.env(r).bindings.contains_key(name) {
                    return Ok(true);
                }
                (*object, false)
            }
            EnvKind::Object { object, with } => (*object, *with),
        };
        let key = self.name_key(name);
        if !self.has_property(obj, &key)? {
            return Ok(false);
        }
        if !with {
            return Ok(true);
        }
        // @@unscopables (§9.1.1.2.1 steps 4–7).
        let us = PropKey::Sym(self.realm.sym_unscopables.clone());
        if let Value::Object(u) = self.get(obj, &us)? {
            let cond = self.get(u, &key)?;
            if self.to_boolean(&cond) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// ResolveBinding (§9.4.2): the record holding `name`, or `None` (unresolvable).
    pub(super) fn resolve(&mut self, env: EnvRef, name: &Rc<str>) -> JsResult<Option<EnvRef>> {
        let mut e = Some(env);
        while let Some(r) = e {
            if self.has_binding(r, name)? {
                return Ok(Some(r));
            }
            e = self.heap.env(r).outer;
        }
        Ok(None)
    }

    /// GetBindingValue (§9.1.1.*.6).
    pub(super) fn get_binding_value(&mut self, r: EnvRef, name: &Rc<str>, strict: bool) -> JsResult<Value> {
        let obj = match &self.heap.env(r).kind {
            EnvKind::Object { object, .. } => *object,
            EnvKind::Global { object, .. } if !self.heap.env(r).bindings.contains_key(name) => *object,
            _ => return self.decl_value(r, name),
        };
        let key = self.name_key(name);
        if !self.has_property(obj, &key)? {
            if strict {
                return self.reference_error(&format!("{name} is not defined"));
            }
            return Ok(Value::Undefined);
        }
        self.get(obj, &key)
    }

    fn decl_value(&mut self, r: EnvRef, name: &Rc<str>) -> JsResult<Value> {
        if let EnvKind::Module { imports } = &self.heap.env(r).kind {
            if let Some((target, tname)) = imports.get(name).cloned() {
                // An import binding reads the exporting module's binding (§9.1.1.5.1).
                return self.decl_value(target, &tname);
            }
        }
        match self.heap.env(r).bindings.get(name) {
            Some(Binding { value: Some(v), .. }) => Ok(v.clone()),
            Some(_) => self.reference_error(&format!("Cannot access '{name}' before initialization")),
            None => self.reference_error(&format!("{name} is not defined")),
        }
    }

    /// SetMutableBinding (§9.1.1.*.5).
    pub(super) fn set_mutable_binding(
        &mut self,
        r: EnvRef,
        name: &Rc<str>,
        v: Value,
        strict: bool,
    ) -> JsResult<()> {
        let obj = match &self.heap.env(r).kind {
            EnvKind::Object { object, .. } => Some(*object),
            EnvKind::Global { object, .. } => {
                (!self.heap.env(r).bindings.contains_key(name)).then_some(*object)
            }
            _ => None,
        };
        if let Some(obj) = obj {
            let key = self.name_key(name);
            let still = self.has_property(obj, &key)?;
            if !still && strict {
                return self.reference_error(&format!("{name} is not defined"));
            }
            let ok = self.set(obj, key, v, Value::Object(obj))?;
            if !ok && strict {
                return self.type_error(&format!("Cannot assign to read only property '{name}'"));
            }
            return Ok(());
        }
        if let EnvKind::Module { imports } = &self.heap.env(r).kind {
            if imports.contains_key(name) {
                return self.type_error("Assignment to constant variable.");
            }
        }
        let env = self.heap.env_mut(r);
        match env.bindings.get_mut(name) {
            None => {
                if strict {
                    return self.reference_error(&format!("{name} is not defined"));
                }
                env.bindings.insert(
                    name.clone(),
                    Binding { value: Some(v), mutable: true, strict: false, deletable: true },
                );
                Ok(())
            }
            Some(b) if b.value.is_none() => {
                self.reference_error(&format!("Cannot access '{name}' before initialization"))
            }
            Some(b) if b.mutable => {
                b.value = Some(v);
                Ok(())
            }
            Some(b) => {
                if b.strict || strict {
                    self.type_error("Assignment to constant variable.")
                } else {
                    Ok(())
                }
            }
        }
    }

    pub(super) fn get_var(&mut self, env: EnvRef, name: &Rc<str>) -> JsResult<Value> {
        match self.resolve(env, name)? {
            None => self.reference_error(&format!("{name} is not defined")),
            Some(r) => {
                let strict = self.strict();
                self.get_binding_value(r, name, strict)
            }
        }
    }

    /// PutValue on an identifier reference (§6.2.5.6).
    pub(super) fn set_var(&mut self, env: EnvRef, name: &Rc<str>, v: Value) -> JsResult<()> {
        let strict = self.strict();
        match self.resolve(env, name)? {
            None => {
                if strict {
                    return self.reference_error(&format!("{name} is not defined"));
                }
                let g = self.realm.global;
                let k = self.name_key(name);
                self.set(g, k, v, Value::Object(g))?;
                Ok(())
            }
            Some(r) => self.set_mutable_binding(r, name, v, strict),
        }
    }

    /// InitializeBinding in the current scope (or the object record when there is none).
    pub(super) fn init_binding(&mut self, env: EnvRef, name: &Rc<str>, v: Value) -> JsResult<()> {
        let e = self.heap.env_mut(env);
        if let Some(b) = e.bindings.get_mut(name) {
            b.value = Some(v);
            return Ok(());
        }
        match &e.kind {
            EnvKind::Declarative | EnvKind::Function { .. } => {
                e.bindings.insert(
                    name.clone(),
                    Binding { value: Some(v), mutable: true, strict: false, deletable: false },
                );
                Ok(())
            }
            _ => self.set_var(env, name, v),
        }
    }

    /// `delete x` (§13.5.1.2 step 5).
    pub(super) fn delete_var(&mut self, env: EnvRef, name: &Rc<str>) -> JsResult<bool> {
        let Some(r) = self.resolve(env, name)? else { return Ok(true) };
        let obj = match &self.heap.env(r).kind {
            EnvKind::Object { object, .. } => Some(*object),
            EnvKind::Global { object, .. } => {
                (!self.heap.env(r).bindings.contains_key(name)).then_some(*object)
            }
            _ => None,
        };
        if let Some(o) = obj {
            let k = self.name_key(name);
            return self.delete(o, &k);
        }
        let e = self.heap.env_mut(r);
        match e.bindings.get(name) {
            Some(b) if b.deletable => {
                e.bindings.remove(name);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// GetThisEnvironment (§9.4.3) + ResolveThisBinding (§9.4.4).
    pub(super) fn resolve_this(&mut self, env: EnvRef) -> JsResult<Value> {
        let mut e = Some(env);
        while let Some(r) = e {
            match &self.heap.env(r).kind {
                EnvKind::Function { this, .. } => {
                    return match this {
                        Some(t) => Ok(t.clone()),
                        None => self.reference_error(
                            "Must call super constructor in derived class before accessing 'this'",
                        ),
                    };
                }
                EnvKind::Global { this, .. } => return Ok(this.clone()),
                EnvKind::Module { .. } => return Ok(Value::Undefined),
                _ => e = self.heap.env(r).outer,
            }
        }
        Ok(Value::Undefined)
    }

    pub(super) fn new_target_of(&self, env: EnvRef) -> Value {
        let mut e = Some(env);
        while let Some(r) = e {
            match &self.heap.env(r).kind {
                EnvKind::Function { new_target, .. } => return new_target.clone(),
                EnvKind::Global { .. } | EnvKind::Module { .. } => return Value::Undefined,
                _ => e = self.heap.env(r).outer,
            }
        }
        Value::Undefined
    }

    /// What eval code may contain, from the calling context (§19.2.1.1 steps 5–7).
    pub(super) fn eval_context(&self, env: EnvRef, strict: bool) -> EvalContext {
        let mut ctx = EvalContext { strict, ..Default::default() };
        let mut e = Some(env);
        while let Some(r) = e {
            if let EnvKind::Function { func, .. } = &self.heap.env(r).kind {
                ctx.new_target = true;
                if let Kind::Function(c) = &self.heap.obj(*func).kind {
                    ctx.super_prop = c.home.is_some();
                    ctx.super_call = c.derived;
                    ctx.no_arguments = c.tmpl.ast.kind == crate::ast::FuncKind::ClassField;
                }
                break;
            }
            e = self.heap.env(r).outer;
        }
        ctx
    }

    fn global_parts(&self, genv: EnvRef) -> ObjRef {
        match &self.heap.env(genv).kind {
            EnvKind::Global { object, .. } => *object,
            _ => self.realm.global,
        }
    }

    fn has_lexical_declaration(&self, genv: EnvRef, n: &Rc<str>) -> bool {
        self.heap.env(genv).bindings.contains_key(n)
    }

    /// HasRestrictedGlobalProperty (§9.1.1.4.14).
    fn has_restricted_global(&mut self, g: ObjRef, n: &str) -> JsResult<bool> {
        Ok(match self.get_own_property(g, &PropKey::named(n))? {
            Some(d) => !d.configurable.unwrap_or(false),
            None => false,
        })
    }

    /// CanDeclareGlobalVar (§9.1.1.4.15).
    fn can_declare_global_var(&mut self, g: ObjRef, n: &str) -> JsResult<bool> {
        if self.get_own_property(g, &PropKey::named(n))?.is_some() {
            return Ok(true);
        }
        self.is_extensible(g)
    }

    /// CanDeclareGlobalFunction (§9.1.1.4.16).
    fn can_declare_global_function(&mut self, g: ObjRef, n: &str) -> JsResult<bool> {
        match self.get_own_property(g, &PropKey::named(n))? {
            None => self.is_extensible(g),
            Some(d) => Ok(d.configurable == Some(true)
                || (d.is_data() && d.writable == Some(true) && d.enumerable == Some(true))),
        }
    }

    /// CreateGlobalVarBinding (§9.1.1.4.17).
    fn create_global_var(&mut self, genv: EnvRef, n: &Rc<str>, deletable: bool) -> JsResult<()> {
        let g = self.global_parts(genv);
        let key = self.name_key(n);
        if self.get_own_property(g, &key)?.is_none() && self.is_extensible(g)? {
            self.define_own_property_or_throw(
                g,
                key,
                PropDesc::data(Value::Undefined, true, true, deletable),
            )?;
        }
        Ok(())
    }

    /// CreateGlobalFunctionBinding (§9.1.1.4.18).
    fn create_global_function(
        &mut self,
        genv: EnvRef,
        n: &Rc<str>,
        f: Value,
        deletable: bool,
    ) -> JsResult<()> {
        let g = self.global_parts(genv);
        let key = self.name_key(n);
        let existing = self.get_own_property(g, &key)?;
        let desc = match existing {
            None => PropDesc::data(f.clone(), true, true, deletable),
            Some(d) if d.configurable == Some(true) => PropDesc::data(f.clone(), true, true, deletable),
            Some(_) => PropDesc { value: Some(f.clone()), ..Default::default() },
        };
        self.define_own_property_or_throw(g, key.clone(), desc)?;
        self.set(g, key, f, Value::Object(g))?;
        Ok(())
    }

    /// GlobalDeclarationInstantiation (§16.1.7).
    pub(super) fn global_declaration_instantiation(&mut self) -> JsResult<()> {
        let code = self.top().code.clone();
        let genv = self.top().env;
        let g = self.global_parts(genv);
        let d = &code.decls;
        for (n, _) in &d.lexical {
            // Global vars and functions are non-configurable, so HasRestrictedGlobalProperty
            // catches them; eval-introduced ones are configurable and don't clash.
            if self.has_lexical_declaration(genv, n) {
                return self.syntax_error(&format!("Identifier '{n}' has already been declared"));
            }
            if self.has_restricted_global(g, n)? {
                return self.syntax_error(&format!("Cannot redeclare restricted global '{n}'"));
            }
        }
        for n in d.vars.iter().chain(d.functions.iter().map(|f| &f.0)) {
            if self.has_lexical_declaration(genv, n) {
                return self.syntax_error(&format!("Identifier '{n}' has already been declared"));
            }
        }
        for (n, _) in &d.functions {
            if !self.can_declare_global_function(g, n)? {
                return self.type_error(&format!("Cannot declare global function '{n}'"));
            }
        }
        for n in &d.vars {
            if !self.can_declare_global_var(g, n)? {
                return self.type_error(&format!("Cannot declare global variable '{n}'"));
            }
        }
        // Annex B.3.2.2: only names that wouldn't clash with lexical declarations.
        let mut annex = Vec::new();
        for n in &d.annex_b {
            if !self.has_lexical_declaration(genv, n)
                && !d.lexical.iter().any(|l| &l.0 == n)
                && self.can_declare_global_var(g, n)?
            {
                annex.push(n.clone());
            }
        }
        for (n, is_const) in &d.lexical {
            self.heap.env_mut(genv).bindings.insert(
                n.clone(),
                Binding { value: None, mutable: !is_const, strict: true, deletable: false },
            );
        }
        for (n, t) in &d.functions {
            let f = self.make_function(code.funcs[*t as usize].clone(), genv, None, true);
            self.create_global_function(genv, n, Value::Object(f), false)?;
        }
        for n in d.vars.iter().chain(annex.iter()) {
            self.create_global_var(genv, n, false)?;
        }
        Ok(())
    }

    /// EvalDeclarationInstantiation (§19.2.1.3).
    pub(super) fn eval_declaration_instantiation(&mut self) -> JsResult<()> {
        let code = self.top().code.clone();
        let lex = self.top().env;
        let var_env = self.top().var_env;
        let strict = code.strict;
        let d = &code.decls;
        let var_is_global = matches!(self.heap.env(var_env).kind, EnvKind::Global { .. });
        if !strict {
            let names: Vec<&Rc<str>> = d.vars.iter().chain(d.functions.iter().map(|f| &f.0)).collect();
            if var_is_global {
                for n in &names {
                    if self.has_lexical_declaration(var_env, n) {
                        return self.syntax_error(&format!("Identifier '{n}' has already been declared"));
                    }
                }
            }
            // Lexical declarations between the eval and its var scope conflict too.
            let mut e = self.heap.env(lex).outer;
            while let Some(r) = e {
                if r == var_env {
                    break;
                }
                if !matches!(self.heap.env(r).kind, EnvKind::Object { .. }) {
                    for n in &names {
                        if let Some(b) = self.heap.env(r).bindings.get(*n) {
                            // B.3.4: a catch parameter may be redeclared by eval's var
                            // and function declarations; simple catch bindings are mutable
                            // and non-strict.
                            let catch_param = b.mutable && !b.strict;
                            if !catch_param {
                                return self
                                    .syntax_error(&format!("Identifier '{n}' has already been declared"));
                            }
                        }
                    }
                }
                e = self.heap.env(r).outer;
            }
        }
        let g = self.realm.global;
        if !strict && var_is_global {
            for (n, _) in &d.functions {
                if !self.can_declare_global_function(g, n)? {
                    return self.type_error(&format!("Cannot declare global function '{n}'"));
                }
            }
            for n in &d.vars {
                if !self.can_declare_global_var(g, n)? {
                    return self.type_error(&format!("Cannot declare global variable '{n}'"));
                }
            }
        }
        for (n, is_const) in &d.lexical {
            self.heap.env_mut(lex).bindings.insert(
                n.clone(),
                Binding { value: None, mutable: !is_const, strict: true, deletable: false },
            );
        }
        let annex: Vec<Rc<str>> = if strict { Vec::new() } else { d.annex_b.clone() };
        for (n, t) in &d.functions {
            let f = self.make_function(code.funcs[*t as usize].clone(), lex, None, true);
            if !strict && var_is_global {
                self.create_global_function(var_env, n, Value::Object(f), true)?;
            } else {
                let env = self.heap.env_mut(var_env);
                match env.bindings.get_mut(n) {
                    Some(b) => b.value = Some(Value::Object(f)),
                    None => {
                        env.bindings.insert(
                            n.clone(),
                            Binding {
                                value: Some(Value::Object(f)),
                                mutable: true,
                                strict: false,
                                deletable: true,
                            },
                        );
                    }
                }
            }
        }
        for n in d.vars.iter().chain(annex.iter()) {
            if d.functions.iter().any(|f| &f.0 == n) {
                continue;
            }
            if !strict && var_is_global {
                if annex.contains(n) && !d.vars.contains(n) && !self.can_declare_global_var(g, n)? {
                    continue;
                }
                self.create_global_var(var_env, n, true)?;
            } else {
                let env = self.heap.env_mut(var_env);
                env.bindings.entry(n.clone()).or_insert(Binding {
                    value: Some(Value::Undefined),
                    mutable: true,
                    strict: false,
                    deletable: true,
                });
            }
        }
        let _ = ErrorKind::Syntax;
        Ok(())
    }
}

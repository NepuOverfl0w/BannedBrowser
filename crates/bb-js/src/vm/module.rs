//! Source text modules (ECMA-262 §16.2): loading through the host's loader, linking
//! (ResolveExport, InitializeEnvironment), evaluation in dependency order (modules with
//! top-level await run as async bodies; the rest of the graph continues when they settle),
//! namespace objects (§10.4.6), `import()` and `import.meta`.
//!
//! Simplification: async modules in one graph are evaluated one after another instead of
//! concurrently (§16.2.1.5.3.4 allows siblings to run in parallel); ordering-sensitive tests
//! of that can differ.

use super::code::Code;
use super::heap::*;
use super::object::PropDesc;
use super::realm::native_with;
use super::value::{js_str, to_rust, JsString, PropKey, Value};
use super::{Compiler, ErrorKind, JsResult, Unit, Vm};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    New,
    Linked,
    Evaluating,
    Evaluated,
}

pub struct ModuleRecord {
    pub key: String,
    pub code: Rc<Code>,
    pub status: Status,
    pub error: Option<Value>,
    pub env: Option<EnvRef>,
    pub namespace: Option<ObjRef>,
    pub meta: Option<ObjRef>,
    /// specifier → module id
    pub loaded: Vec<(String, usize)>,
    /// The promise of this module's evaluation (shared by the graph it was evaluated in).
    pub eval_promise: Option<Value>,
}

impl ModuleRecord {
    pub fn trace(&self, roots: &mut Roots) {
        roots.envs.extend(self.env);
        roots.objects.extend(self.namespace);
        roots.objects.extend(self.meta);
        if let Some(e) = &self.error {
            roots.values.push(e.clone());
        }
        if let Some(p) = &self.eval_promise {
            roots.values.push(p.clone());
        }
    }
}

/// An evaluation in progress: the modules in order and the graph's promise capability.
pub struct PendingEval {
    pub order: Vec<usize>,
    pub next: usize,
    pub resolve: Value,
    pub reject: Value,
}

impl PendingEval {
    pub fn trace(&self, roots: &mut Roots) {
        roots.values.push(self.resolve.clone());
        roots.values.push(self.reject.clone());
    }
}

enum Resolution {
    Found(usize, Option<Rc<str>>),
    NotFound,
    Ambiguous,
}

fn slots(vm: &Vm, f: ObjRef) -> Vec<Value> {
    match &vm.heap.obj(f).kind {
        Kind::Native(n) => n.slots.clone(),
        _ => Vec::new(),
    }
}

impl Vm {
    /// The key of the script or module whose code is running (the `import()` referrer).
    pub fn current_referrer(&self) -> Option<Rc<str>> {
        self.frames.iter().rev().find_map(|f| f.code.referrer.clone())
    }

    fn module_info(&self, id: usize) -> &super::code::ModuleInfo {
        self.modules[id].code.module.as_deref().expect("module code")
    }

    /// ParseModule + register (§16.2.1.6.1), or the already loaded record for `key`.
    fn load_module_source(&mut self, key: &str, src: &str) -> JsResult<usize> {
        if let Some(&id) = self.module_map.get(key) {
            return Ok(id);
        }
        let prog = match crate::parse_module(src) {
            Ok(p) => p,
            Err(e) => return self.syntax_error(&e.msg),
        };
        let source: Rc<[char]> = src.chars().collect::<Vec<_>>().into();
        let code = Rc::new(Compiler::compile_unit(Unit::Module(&prog), source, Some(key.into())));
        let id = self.modules.len();
        self.modules.push(ModuleRecord {
            key: key.to_string(),
            code,
            status: Status::New,
            error: None,
            env: None,
            namespace: None,
            meta: None,
            loaded: Vec::new(),
            eval_promise: None,
        });
        self.module_map.insert(key.to_string(), id);
        Ok(id)
    }

    /// HostLoadImportedModule through the host's loader.
    fn host_load(&mut self, specifier: &str, referrer: Option<&str>) -> JsResult<usize> {
        let Some(mut loader) = self.loader.take() else {
            return self.type_error(&format!("Cannot load module '{specifier}': no module loader"));
        };
        let r = loader(specifier, referrer);
        self.loader = Some(loader);
        match r {
            Ok((key, src)) => self.load_module_source(&key, &src),
            Err(msg) => self.type_error(&format!("Cannot load module '{specifier}': {msg}")),
        }
    }

    /// LoadRequestedModules (§16.2.1.5.1), depth first.
    fn load_graph(&mut self, id: usize) -> JsResult<()> {
        let mut stack = vec![id];
        let mut seen = vec![id];
        while let Some(m) = stack.pop() {
            let requests = self.module_info(m).requests.clone();
            let key = self.modules[m].key.clone();
            for r in requests {
                if self.modules[m].loaded.iter().any(|(s, _)| *s == r) {
                    let child =
                        self.modules[m].loaded.iter().find(|(s, _)| *s == r).map(|x| x.1).unwrap_or(0);
                    if !seen.contains(&child) {
                        seen.push(child);
                        stack.push(child);
                    }
                    continue;
                }
                let child = self.host_load(&r, Some(&key))?;
                self.modules[m].loaded.push((r, child));
                if !seen.contains(&child) {
                    seen.push(child);
                    stack.push(child);
                }
            }
        }
        Ok(())
    }

    fn child(&self, id: usize, request: &str) -> usize {
        self.modules[id].loaded.iter().find(|(s, _)| s == request).map(|x| x.1).unwrap_or(id)
    }

    /// ResolveExport (§16.2.1.6.3).
    fn resolve_export(&self, id: usize, name: &[u16], set: &mut Vec<(usize, Vec<u16>)>) -> Resolution {
        if set.iter().any(|(m, n)| *m == id && n == name) {
            return Resolution::NotFound;
        }
        set.push((id, name.to_vec()));
        let info = self.module_info(id);
        for (e, local) in &info.local_exports {
            if &e[..] == name {
                return Resolution::Found(id, Some(local.clone()));
            }
        }
        for (e, req, imp) in &info.indirect_exports {
            if &e[..] == name {
                let m = self.child(id, req);
                return match imp {
                    None => Resolution::Found(m, None),
                    Some(i) => self.resolve_export(m, i, set),
                };
            }
        }
        if name.iter().copied().eq("default".encode_utf16()) {
            return Resolution::NotFound;
        }
        let mut star: Option<(usize, Option<Rc<str>>)> = None;
        for req in &info.star_exports {
            let m = self.child(id, req);
            match self.resolve_export(m, name, set) {
                Resolution::Ambiguous => return Resolution::Ambiguous,
                Resolution::NotFound => {}
                Resolution::Found(rm, rn) => match &star {
                    None => star = Some((rm, rn)),
                    Some((sm, sn)) => {
                        if *sm != rm || sn != &rn {
                            return Resolution::Ambiguous;
                        }
                    }
                },
            }
        }
        match star {
            Some((m, n)) => Resolution::Found(m, n),
            None => Resolution::NotFound,
        }
    }

    /// GetExportedNames (§16.2.1.6.2).
    fn exported_names(&self, id: usize, set: &mut Vec<usize>) -> Vec<JsString> {
        if set.contains(&id) {
            return Vec::new();
        }
        set.push(id);
        let info = self.module_info(id);
        let mut out: Vec<JsString> = info.local_exports.iter().map(|e| e.0.clone()).collect();
        out.extend(info.indirect_exports.iter().map(|e| e.0.clone()));
        for req in info.star_exports.clone() {
            let m = self.child(id, &req);
            for n in self.exported_names(m, set) {
                if !n.iter().copied().eq("default".encode_utf16()) && !out.contains(&n) {
                    out.push(n);
                }
            }
        }
        out
    }

    /// Link (§16.2.1.5.2): InitializeEnvironment for every module of the graph.
    fn link(&mut self, id: usize) -> JsResult<()> {
        let mut order = Vec::new();
        let mut seen = Vec::new();
        self.link_order(id, &mut seen, &mut order);
        for m in &order {
            if self.modules[*m].status == Status::New {
                self.create_env(*m);
            }
        }
        for m in order {
            if self.modules[m].status == Status::New {
                if let Err(e) = self.initialize_environment(m) {
                    // A failed link leaves the graph unlinked (§16.2.1.5.2 step 4).
                    for r in &mut self.modules {
                        if r.status == Status::New {
                            r.env = None;
                        }
                    }
                    return Err(e);
                }
            }
        }
        for m in seen {
            if self.modules[m].status == Status::New {
                self.modules[m].status = Status::Linked;
            }
        }
        Ok(())
    }

    fn link_order(&self, id: usize, seen: &mut Vec<usize>, order: &mut Vec<usize>) {
        if seen.contains(&id) {
            return;
        }
        seen.push(id);
        for (_, c) in &self.modules[id].loaded {
            self.link_order(*c, seen, order);
        }
        order.push(id);
    }

    fn create_env(&mut self, id: usize) {
        let genv = self.realm.global_env;
        let env = self.heap.alloc_env(Env {
            outer: Some(genv),
            kind: EnvKind::Module { imports: Default::default() },
            bindings: Default::default(),
        });
        self.modules[id].env = Some(env);
    }

    /// InitializeEnvironment (§16.2.1.6.4).
    fn initialize_environment(&mut self, id: usize) -> JsResult<()> {
        let info = self.module_info(id).clone();
        for (e, _, _) in &info.indirect_exports {
            match self.resolve_export(id, e, &mut Vec::new()) {
                Resolution::Found(..) => {}
                Resolution::NotFound => {
                    return self.syntax_error(&format!(
                        "The requested module does not provide an export named '{}'",
                        to_rust(e)
                    ))
                }
                Resolution::Ambiguous => {
                    return self.syntax_error(&format!("The export '{}' is ambiguous", to_rust(e)))
                }
            }
        }
        let env = self.modules[id].env.expect("module env");
        for ie in &info.imports {
            let m = self.child(id, &ie.request);
            match &ie.import_name {
                None => {
                    let ns = self.module_namespace(m)?;
                    self.heap.env_mut(env).bindings.insert(
                        ie.local.clone(),
                        Binding {
                            value: Some(Value::Object(ns)),
                            mutable: false,
                            strict: true,
                            deletable: false,
                        },
                    );
                }
                Some(name) => {
                    if name.iter().copied().eq("*source*".encode_utf16()) {
                        return self
                            .syntax_error("Source phase imports of source text modules are not allowed");
                    }
                    match self.resolve_export(m, name, &mut Vec::new()) {
                        Resolution::Found(rm, None) => {
                            let ns = self.module_namespace(rm)?;
                            self.heap.env_mut(env).bindings.insert(
                                ie.local.clone(),
                                Binding {
                                    value: Some(Value::Object(ns)),
                                    mutable: false,
                                    strict: true,
                                    deletable: false,
                                },
                            );
                        }
                        Resolution::Found(rm, Some(bn)) => {
                            let Some(target) = self.modules[rm].env else {
                                return self.reference_error("module not linked");
                            };
                            if let EnvKind::Module { imports } = &mut self.heap.env_mut(env).kind {
                                imports.insert(ie.local.clone(), (target, bn));
                            }
                        }
                        Resolution::NotFound => {
                            return self.syntax_error(&format!(
                                "The requested module '{}' does not provide an export named '{}'",
                                ie.request,
                                to_rust(name)
                            ))
                        }
                        Resolution::Ambiguous => {
                            return self.syntax_error(&format!("The export '{}' is ambiguous", to_rust(name)))
                        }
                    }
                }
            }
        }
        let code = self.modules[id].code.clone();
        let d = &code.decls;
        for v in &d.vars {
            self.heap.env_mut(env).bindings.entry(v.clone()).or_insert(Binding {
                value: Some(Value::Undefined),
                mutable: true,
                strict: false,
                deletable: false,
            });
        }
        for (n, is_const) in &d.lexical {
            self.heap.env_mut(env).bindings.insert(
                n.clone(),
                Binding { value: None, mutable: !is_const, strict: true, deletable: false },
            );
        }
        for (n, t) in &d.functions {
            let f = self.make_function(code.funcs[*t as usize].clone(), env, None, true);
            self.heap.env_mut(env).bindings.insert(
                n.clone(),
                Binding { value: Some(Value::Object(f)), mutable: true, strict: false, deletable: false },
            );
        }
        Ok(())
    }

    /// GetModuleNamespace (§16.2.1.10).
    fn module_namespace(&mut self, id: usize) -> JsResult<ObjRef> {
        if let Some(ns) = self.modules[id].namespace {
            return Ok(ns);
        }
        let mut names: Vec<JsString> = self
            .exported_names(id, &mut Vec::new())
            .into_iter()
            .filter(|n| matches!(self.resolve_export(id, n, &mut Vec::new()), Resolution::Found(..)))
            .collect();
        names.sort_by(|a, b| a[..].cmp(&b[..]));
        let ns = self.heap.alloc(Object::new(None, Kind::ModuleNamespace(id, names)));
        let tag = PropKey::Sym(self.realm.sym_to_string_tag.clone());
        let o = self.heap.obj_mut(ns);
        o.props.insert(tag, Prop::data(Value::str("Module"), false, false, false));
        o.extensible = false;
        self.modules[id].namespace = Some(ns);
        Ok(ns)
    }

    // ---- the namespace exotic object's methods (§10.4.6) ----

    fn ns_names(&self, o: ObjRef) -> Option<(usize, &[JsString])> {
        match &self.heap.obj(o).kind {
            Kind::ModuleNamespace(id, names) => Some((*id, names)),
            _ => None,
        }
    }

    pub(super) fn is_namespace(&self, o: ObjRef) -> bool {
        matches!(self.heap.obj(o).kind, Kind::ModuleNamespace(..))
    }

    fn ns_has_export(&self, o: ObjRef, k: &PropKey) -> bool {
        let Some((_, names)) = self.ns_names(o) else { return false };
        match k.to_value() {
            Value::String(s) => names.iter().any(|n| n[..] == s[..]),
            _ => false,
        }
    }

    /// [[Get]] of an export: the binding's current value (TDZ → ReferenceError).
    pub(super) fn ns_get(&mut self, o: ObjRef, k: &PropKey) -> JsResult<Value> {
        if let PropKey::Sym(_) = k {
            return Ok(self
                .heap
                .obj(o)
                .props
                .get(k)
                .and_then(|p| if let Slot::Data(v) = &p.slot { Some(v.clone()) } else { None })
                .unwrap_or_default());
        }
        if !self.ns_has_export(o, k) {
            return Ok(Value::Undefined);
        }
        let Some((id, _)) = self.ns_names(o) else { return Ok(Value::Undefined) };
        let Value::String(name) = k.to_value() else { return Ok(Value::Undefined) };
        match self.resolve_export(id, &name, &mut Vec::new()) {
            Resolution::Found(m, None) => Ok(Value::Object(self.module_namespace(m)?)),
            Resolution::Found(m, Some(bn)) => {
                let Some(env) = self.modules[m].env else { return self.reference_error("module not linked") };
                self.get_binding_value(env, &bn, true)
            }
            _ => self.reference_error("unresolvable export"),
        }
    }

    pub(super) fn ns_get_own(&mut self, o: ObjRef, k: &PropKey) -> JsResult<Option<PropDesc>> {
        if let PropKey::Sym(_) = k {
            return Ok(self.heap.obj(o).props.get(k).map(PropDesc::from_prop));
        }
        if !self.ns_has_export(o, k) {
            return Ok(None);
        }
        let v = self.ns_get(o, k)?;
        Ok(Some(PropDesc::data(v, true, true, false)))
    }

    pub(super) fn ns_define(&mut self, o: ObjRef, k: PropKey, d: PropDesc) -> JsResult<bool> {
        if let PropKey::Sym(_) = &k {
            let cur = self.heap.obj(o).props.get(&k).map(PropDesc::from_prop);
            let obj = self.heap.obj_mut(o);
            return Ok(super::object::validate_and_apply(Some(&mut obj.props), &k, false, &d, cur.as_ref()));
        }
        let Some(cur) = self.ns_get_own(o, &k)? else { return Ok(false) };
        if d.configurable == Some(true)
            || d.enumerable == Some(false)
            || d.is_accessor()
            || d.writable == Some(false)
        {
            return Ok(false);
        }
        if let Some(v) = &d.value {
            return Ok(super::value::same_value(v, cur.value.as_ref().unwrap_or(&Value::Undefined)));
        }
        Ok(true)
    }

    pub(super) fn ns_has(&self, o: ObjRef, k: &PropKey) -> bool {
        if let PropKey::Sym(_) = k {
            return self.heap.obj(o).props.contains(k);
        }
        self.ns_has_export(o, k)
    }

    pub(super) fn ns_delete(&mut self, o: ObjRef, k: &PropKey) -> bool {
        if let PropKey::Sym(_) = k {
            let configurable = self.heap.obj(o).props.get(k).is_none_or(|p| p.configurable);
            if configurable {
                self.heap.obj_mut(o).props.remove(k);
            }
            return configurable;
        }
        !self.ns_has_export(o, k)
    }

    pub(super) fn ns_keys(&self, o: ObjRef) -> Vec<PropKey> {
        let mut out: Vec<PropKey> = self
            .ns_names(o)
            .map(|(_, n)| n.iter().map(|s| PropKey::from_js(s.clone())).collect())
            .unwrap_or_default();
        out.extend(self.heap.obj(o).props.keys());
        out
    }

    // ---- evaluation ----

    /// Load, link and evaluate a module; the result is its evaluation promise.
    pub fn run_module(&mut self, key: &str, src: &str) -> JsResult<Value> {
        let id = self.load_module_source(key, src)?;
        self.load_graph(id)?;
        self.link(id)?;
        self.evaluate(id)
    }

    /// The state of a promise, for hosts: Some(Ok) fulfilled, Some(Err) rejected.
    pub fn promise_result(&self, p: &Value) -> Option<Result<Value, Value>> {
        let Value::Object(o) = p else { return None };
        match &self.heap.obj(*o).kind {
            Kind::Promise(d) => match d.state {
                PromiseState::Pending => None,
                PromiseState::Fulfilled => Some(Ok(d.result.clone())),
                PromiseState::Rejected => Some(Err(d.result.clone())),
            },
            _ => None,
        }
    }

    /// Evaluate (§16.2.1.5.3) as a promise.
    fn evaluate(&mut self, id: usize) -> JsResult<Value> {
        if let Some(p) = &self.modules[id].eval_promise {
            return Ok(p.clone());
        }
        let p = self.new_promise(None);
        let (resolve, reject) = self.create_resolving_functions(p);
        let mut order = Vec::new();
        let mut seen = Vec::new();
        self.eval_order(id, &mut seen, &mut order);
        for m in &order {
            self.modules[*m].eval_promise = Some(Value::Object(p));
        }
        if let Some(e) = self.modules[id].error.clone() {
            self.call(&reject, Value::Undefined, vec![e])?;
            return Ok(Value::Object(p));
        }
        self.continue_eval(PendingEval { order, next: 0, resolve, reject })?;
        Ok(Value::Object(p))
    }

    fn eval_order(&self, id: usize, seen: &mut Vec<usize>, order: &mut Vec<usize>) {
        if seen.contains(&id) || self.modules[id].status == Status::Evaluated {
            return;
        }
        seen.push(id);
        for (_, c) in &self.modules[id].loaded {
            self.eval_order(*c, seen, order);
        }
        order.push(id);
    }

    fn continue_eval(&mut self, mut pe: PendingEval) -> JsResult<()> {
        while pe.next < pe.order.len() {
            let m = pe.order[pe.next];
            // A failed dependency fails its dependents (§16.2.1.5.3.1).
            let dep_error = self.modules[m].loaded.iter().find_map(|(_, c)| self.modules[*c].error.clone());
            if let Some(e) = dep_error.or_else(|| self.modules[m].error.clone()) {
                self.modules[m].error = Some(e.clone());
                self.modules[m].status = Status::Evaluated;
                self.call(&pe.reject, Value::Undefined, vec![e])?;
                return Ok(());
            }
            if self.modules[m].status == Status::Evaluated {
                pe.next += 1;
                continue;
            }
            self.modules[m].status = Status::Evaluating;
            let code = self.modules[m].code.clone();
            let env = self.modules[m].env.expect("linked module");
            let frame = self.frame(code.clone(), env, env);
            let r = self.execute(frame);
            if self.terminated {
                return r.map(|_| ());
            }
            match r {
                Err(e) => {
                    self.modules[m].status = Status::Evaluated;
                    self.modules[m].error = Some(e.clone());
                    self.call(&pe.reject, Value::Undefined, vec![e])?;
                    return Ok(());
                }
                Ok(v) if code.module.as_ref().is_some_and(|i| i.has_tla) => {
                    // Wait for the async module, then go on (§16.2.1.5.3.4).
                    let slot = self.module_evals.len();
                    self.module_evals.push(Some(pe));
                    let ful = native_with(
                        self,
                        "",
                        1,
                        module_fulfilled,
                        false,
                        vec![Value::Number(slot as f64), Value::Number(m as f64)],
                    );
                    let rej = native_with(
                        self,
                        "",
                        1,
                        module_rejected,
                        false,
                        vec![Value::Number(slot as f64), Value::Number(m as f64)],
                    );
                    if let Value::Object(po) = v {
                        self.perform_then(po, Value::Object(ful), Value::Object(rej), None);
                    }
                    return Ok(());
                }
                Ok(_) => {
                    self.modules[m].status = Status::Evaluated;
                    pe.next += 1;
                }
            }
        }
        self.call(&pe.resolve, Value::Undefined, vec![Value::Undefined])?;
        Ok(())
    }

    fn take_pending(&mut self, f: ObjRef) -> Option<(PendingEval, usize)> {
        let s = slots(self, f);
        let (Value::Number(slot), Value::Number(m)) = (s[0].clone(), s[1].clone()) else { return None };
        let pe = self.module_evals.get_mut(slot as usize)?.take()?;
        Some((pe, m as usize))
    }

    /// `import(specifier, options)` (§13.3.10.1): always a promise.
    pub(super) fn dynamic_import(&mut self, spec: Value, opts: Value) -> Value {
        let p = self.new_promise(None);
        let (resolve, reject) = self.create_resolving_functions(p);
        let r = (|| -> JsResult<(usize, Value)> {
            let s = self.to_string(&spec)?;
            if !opts.is_undefined() {
                let Value::Object(o) = opts else {
                    return self.type_error("The second argument of import() must be an object");
                };
                let with = self.get(o, &PropKey::lit("with"))?;
                if !with.is_undefined() {
                    let Value::Object(w) = with else {
                        return self.type_error("The 'with' option of import() must be an object");
                    };
                    for k in self.own_property_keys(w)? {
                        if self.get_own_property(w, &k)?.is_some_and(|d| d.enumerable == Some(true)) {
                            if let PropKey::Sym(_) = k {
                                continue;
                            }
                            if !matches!(self.get(w, &k)?, Value::String(_)) {
                                return self.type_error("Import attribute values must be strings");
                            }
                        }
                    }
                }
            }
            let referrer = self.current_referrer();
            let id = self.host_load(&to_rust(&s), referrer.as_deref())?;
            self.load_graph(id)?;
            self.link(id)?;
            let ep = self.evaluate(id)?;
            Ok((id, ep))
        })();
        match r {
            Err(e) => {
                if !self.terminated {
                    let _ = self.call(&reject, Value::Undefined, vec![e]);
                }
            }
            Ok((id, ep)) => {
                let ful = native_with(
                    self,
                    "",
                    1,
                    import_fulfilled,
                    false,
                    vec![resolve, Value::Number(id as f64), reject.clone()],
                );
                if let Value::Object(po) = ep {
                    self.perform_then(po, Value::Object(ful), reject, None);
                }
            }
        }
        Value::Object(p)
    }

    /// `import.source(…)`: only modules with a source phase have one; ours don't.
    pub(super) fn import_source(&mut self, spec: Value) -> Value {
        let p = self.new_promise(None);
        let r = match self.to_string(&spec) {
            Ok(_) => {
                self.error(ErrorKind::Syntax, "Source phase imports of source text modules are not supported")
            }
            Err(e) => e,
        };
        self.reject_promise(p, r);
        Value::Object(p)
    }

    /// `import.meta` (§13.3.12.1): a null-prototype object per module.
    pub(super) fn import_meta(&mut self) -> Value {
        let Some(key) = self.current_referrer() else { return Value::Undefined };
        let Some(&id) = self.module_map.get(&*key) else { return Value::Undefined };
        if let Some(m) = self.modules[id].meta {
            return Value::Object(m);
        }
        let m = self.new_object_with_proto(None);
        let url = js_str(&self.modules[id].key);
        self.heap
            .obj_mut(m)
            .props
            .insert(PropKey::lit("url"), Prop::data(Value::String(url), true, true, true));
        self.modules[id].meta = Some(m);
        Value::Object(m)
    }
}

fn module_fulfilled(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    if let Some((mut pe, m)) = vm.take_pending(c.callee) {
        vm.modules[m].status = Status::Evaluated;
        pe.next += 1;
        vm.continue_eval(pe)?;
    }
    Ok(Value::Undefined)
}

fn module_rejected(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    if let Some((pe, m)) = vm.take_pending(c.callee) {
        vm.modules[m].status = Status::Evaluated;
        vm.modules[m].error = Some(c.arg(0));
        vm.call(&pe.reject, Value::Undefined, vec![c.arg(0)])?;
    }
    Ok(Value::Undefined)
}

fn import_fulfilled(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    let Value::Number(id) = s[1] else { return Ok(Value::Undefined) };
    match vm.module_namespace(id as usize) {
        Ok(ns) => vm.call(&s[0], Value::Undefined, vec![Value::Object(ns)])?,
        Err(e) => vm.call(&s[2], Value::Undefined, vec![e])?,
    };
    Ok(Value::Undefined)
}

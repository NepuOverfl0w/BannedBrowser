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
    /// evaluating-async: waiting for asynchronous dependencies or its own top-level await.
    EvaluatingAsync,
    Evaluated,
}

/// [[AsyncEvaluationOrder]] (§16.2.1.6): unset, an integer, or done.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AsyncOrder {
    Unset,
    Order(u64),
    Done,
}

pub struct ModuleRecord {
    /// The module map key (an inline module's is unique to it).
    pub key: String,
    /// Its URL: `import.meta.url`, and the referrer its specifiers resolve against (an
    /// inline module's is the document's base URL).
    pub base: String,
    pub code: Rc<Code>,
    pub status: Status,
    pub error: Option<Value>,
    pub env: Option<EnvRef>,
    pub namespace: Option<ObjRef>,
    /// The deferred namespace (`import defer`, import-defer proposal).
    pub deferred_ns: Option<ObjRef>,
    pub meta: Option<ObjRef>,
    /// specifier → module id
    pub loaded: Vec<(String, usize)>,
    /// [[TopLevelCapability]]: (promise, resolve, reject), on the module Evaluate() ran for.
    pub capability: Option<(Value, Value, Value)>,
    /// [[DFSIndex]], [[DFSAncestorIndex]], [[CycleRoot]] (§16.2.1.6).
    pub dfs_index: usize,
    pub dfs_ancestor: usize,
    pub cycle_root: Option<usize>,
    pub async_order: AsyncOrder,
    /// [[AsyncParentModules]] and [[PendingAsyncDependencies]].
    pub async_parents: Vec<usize>,
    pub pending_async: usize,
    /// A synthetic module's default export (JSON, text and bytes modules).
    pub synthetic: Option<Value>,
    /// [[ModuleSource]] (source-phase-imports): `import source x` binds it. Source text
    /// modules have none.
    pub source_object: Option<ObjRef>,
}

impl ModuleRecord {
    pub fn trace(&self, roots: &mut Roots) {
        roots.envs.extend(self.env);
        roots.objects.extend(self.namespace);
        roots.objects.extend(self.deferred_ns);
        roots.objects.extend(self.meta);
        if let Some(e) = &self.error {
            roots.values.push(e.clone());
        }
        if let Some((p, r, j)) = &self.capability {
            roots.values.extend([p.clone(), r.clone(), j.clone()]);
        }
        if let Some(v) = &self.synthetic {
            roots.values.push(v.clone());
        }
        roots.objects.extend(self.source_object);
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
            base: key.to_string(),
            code,
            status: Status::New,
            error: None,
            env: None,
            namespace: None,
            deferred_ns: None,
            meta: None,
            loaded: Vec::new(),
            capability: None,
            dfs_index: 0,
            dfs_ancestor: 0,
            cycle_root: None,
            async_order: AsyncOrder::Unset,
            async_parents: Vec::new(),
            pending_async: 0,
            synthetic: None,
            source_object: None,
        });
        self.module_map.insert(key.to_string(), id);
        Ok(id)
    }

    /// A synthetic module with one export, `default` (CreateDefaultExportSyntheticModule,
    /// §16.2.1.8 for JSON modules; the import-text and import-bytes proposals).
    fn load_synthetic(&mut self, key: &str, ty: &str, bytes: &[u8]) -> JsResult<usize> {
        let mkey = format!("{ty}\0{key}");
        if let Some(&id) = self.module_map.get(&mkey) {
            return Ok(id);
        }
        // UTF-8 decode (BOM stripped, errors replaced) for the textual types.
        let text = || {
            let b = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
            String::from_utf8_lossy(b).into_owned()
        };
        let value = match ty {
            "json" => {
                let src = Value::str(&text());
                let call = NativeCall {
                    callee: self.realm.global,
                    this: Value::Undefined,
                    args: vec![src],
                    new_target: None,
                };
                super::builtins::json::parse(self, call)?
            }
            "text" => Value::str(&text()),
            "bytes" => Value::Object(super::builtins::typed::immutable_bytes(self, bytes)?),
            _ => return self.type_error(&format!("Unsupported module type '{ty}'")),
        };
        let prog = match crate::parse_module("") {
            Ok(p) => p,
            Err(e) => return self.syntax_error(&e.msg),
        };
        let mut code = Compiler::compile_unit(Unit::Module(&prog), Rc::from(Vec::new()), Some(key.into()));
        if let Some(info) = code.module.as_mut() {
            info.local_exports.push((js_str("default"), "*default*".into()));
        }
        code.decls.lexical.push(("*default*".into(), true));
        let id = self.modules.len();
        self.modules.push(ModuleRecord {
            key: key.to_string(),
            base: key.to_string(),
            code: Rc::new(code),
            status: Status::New,
            error: None,
            env: None,
            namespace: None,
            deferred_ns: None,
            meta: None,
            loaded: Vec::new(),
            capability: None,
            dfs_index: 0,
            dfs_ancestor: 0,
            cycle_root: None,
            async_order: AsyncOrder::Unset,
            async_parents: Vec::new(),
            pending_async: 0,
            synthetic: Some(value),
            source_object: None,
        });
        self.module_map.insert(mkey, id);
        Ok(id)
    }

    /// GetModuleSource: the module's [[ModuleSource]]; a source text module has none
    /// (SyntaxError, source-phase-imports).
    fn module_source(&mut self, id: usize) -> JsResult<Value> {
        match self.modules[id].source_object {
            Some(o) => Ok(Value::Object(o)),
            None => self.syntax_error("Source phase imports of source text modules are not supported"),
        }
    }

    /// Register a host module under `key` that has a module source: an object of class
    /// `class_name` inheriting from %AbstractModuleSource.prototype% (what a WebAssembly
    /// module is for `import source`). The module itself has no exports.
    pub fn register_module_source(&mut self, key: &str, class_name: &str) -> JsResult<ObjRef> {
        let ap = self.realm.abstract_module_source_proto;
        let proto = self.new_object_with_proto(Some(ap));
        let src = self.heap.alloc(Object::new(Some(proto), Kind::ModuleSource(class_name.into())));
        let id = self.load_module_source(key, "")?;
        self.modules[id].source_object = Some(src);
        Ok(src)
    }

    /// HostLoadImportedModule through the host's loader. `request` is a ModuleRequest as the
    /// compiler writes it: `type\0specifier`, or a bare specifier.
    fn host_load(&mut self, request: &str, referrer: Option<&str>) -> JsResult<usize> {
        let (ty, specifier) = request.split_once('\0').unwrap_or(("", request));
        let Some(mut loader) = self.loader.take() else {
            return self.type_error(&format!("Cannot load module '{specifier}': no module loader"));
        };
        let r = loader(self, specifier, referrer);
        self.loader = Some(loader);
        match r {
            Ok((key, bytes)) if ty.is_empty() => {
                let src = String::from_utf8_lossy(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes))
                    .into_owned();
                self.load_module_source(&key, &src)
            }
            Ok((key, bytes)) => self.load_synthetic(&key, ty, &bytes),
            Err(msg) => self.type_error(&format!("Cannot load module '{specifier}': {msg}")),
        }
    }

    /// LoadRequestedModules (§16.2.1.5.1), depth first.
    fn load_graph(&mut self, id: usize) -> JsResult<()> {
        let mut stack = vec![id];
        let mut seen = vec![id];
        while let Some(m) = stack.pop() {
            let requests = self.module_info(m).requests.clone();
            let key = self.modules[m].base.clone();
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
                    // A re-exported source binding resolves to the module's ~source~.
                    Some(i) if is_source(i) => Resolution::Found(m, Some(SOURCE.into())),
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
                    let ns =
                        if ie.deferred { self.deferred_namespace(m)? } else { self.module_namespace(m)? };
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
                Some(name) if is_source(name) => {
                    let v = self.module_source(m)?;
                    self.heap.env_mut(env).bindings.insert(
                        ie.local.clone(),
                        Binding { value: Some(v), mutable: false, strict: true, deletable: false },
                    );
                }
                Some(name) => match self.resolve_export(m, name, &mut Vec::new()) {
                    Resolution::Found(rm, Some(bn)) if &*bn == SOURCE => {
                        let v = self.module_source(rm)?;
                        self.heap.env_mut(env).bindings.insert(
                            ie.local.clone(),
                            Binding { value: Some(v), mutable: false, strict: true, deletable: false },
                        );
                    }
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
                            ie.request.split_once('\0').map_or(ie.request.as_str(), |r| r.1),
                            to_rust(name)
                        ))
                    }
                    Resolution::Ambiguous => {
                        return self.syntax_error(&format!("The export '{}' is ambiguous", to_rust(name)))
                    }
                },
            }
        }
        let code = self.modules[id].code.clone();
        let d = &code.decls;
        for v in &d.vars {
            self.heap.env_mut(env).bindings.get_or_insert(
                v.clone(),
                Binding { value: Some(Value::Undefined), mutable: true, strict: false, deletable: false },
            );
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
        // SetSyntheticModuleExport: a synthetic module has no code to run.
        if let Some(v) = self.modules[id].synthetic.clone() {
            if let Some(b) = self.heap.env_mut(env).bindings.get_mut("*default*") {
                b.value = Some(v);
            }
        }
        Ok(())
    }

    /// GetModuleNamespace (§16.2.1.10).
    fn module_namespace(&mut self, id: usize) -> JsResult<ObjRef> {
        self.namespace_object(id, false)
    }

    /// GetModuleNamespace(module, defer) (import-defer).
    fn deferred_namespace(&mut self, id: usize) -> JsResult<ObjRef> {
        self.namespace_object(id, true)
    }

    fn namespace_object(&mut self, id: usize, deferred: bool) -> JsResult<ObjRef> {
        let cached = if deferred { self.modules[id].deferred_ns } else { self.modules[id].namespace };
        if let Some(ns) = cached {
            return Ok(ns);
        }
        let mut names: Vec<JsString> = self
            .exported_names(id, &mut Vec::new())
            .into_iter()
            .filter(|n| matches!(self.resolve_export(id, n, &mut Vec::new()), Resolution::Found(..)))
            .collect();
        names.sort_by(|a, b| a[..].cmp(&b[..]));
        let ns = self.heap.alloc(Object::new(None, Kind::ModuleNamespace(id, names, deferred)));
        let tag = PropKey::Sym(self.realm.sym_to_string_tag.clone());
        let o = self.heap.obj_mut(ns);
        let label = if deferred { "Deferred Module" } else { "Module" };
        o.props.insert(tag, Prop::data(Value::str(label), false, false, false));
        o.extensible = false;
        if deferred {
            self.modules[id].deferred_ns = Some(ns);
        } else {
            self.modules[id].namespace = Some(ns);
        }
        Ok(ns)
    }

    // ---- the namespace exotic object's methods (§10.4.6) ----

    fn ns_names(&self, o: ObjRef) -> Option<(usize, &[JsString])> {
        match &self.heap.obj(o).kind {
            Kind::ModuleNamespace(id, names, _) => Some((*id, names)),
            _ => None,
        }
    }

    /// IsSymbolLikeNamespaceKey: symbols, and `then` of a deferred namespace.
    fn ns_symbol_like(&self, o: ObjRef, k: &PropKey) -> bool {
        match k {
            PropKey::Sym(_) => true,
            _ => matches!(self.heap.obj(o).kind, Kind::ModuleNamespace(_, _, true)) && k.is_str("then"),
        }
    }

    /// GetModuleExportsList's evaluation step: a deferred namespace evaluates its module
    /// synchronously on first use (ReadyForSyncExecution, EvaluateSync).
    fn ns_trigger(&mut self, o: ObjRef) -> JsResult<()> {
        let Kind::ModuleNamespace(id, _, true) = self.heap.obj(o).kind else { return Ok(()) };
        if self.modules[id].status == Status::Evaluated {
            return match self.modules[id].error.clone() {
                Some(e) => Err(e),
                None => Ok(()),
            };
        }
        if !self.ready_for_sync(id, &mut Vec::new()) {
            return self.type_error("the deferred module can't be evaluated synchronously now");
        }
        self.evaluate_sync(id)
    }

    /// ReadyForSyncExecution (import-defer).
    fn ready_for_sync(&self, id: usize, seen: &mut Vec<usize>) -> bool {
        if seen.contains(&id) {
            return true;
        }
        seen.push(id);
        if self.scc_evaluated(id) {
            return true;
        }
        let m = &self.modules[id];
        if matches!(m.status, Status::Evaluating | Status::EvaluatingAsync | Status::Evaluated) {
            return false;
        }
        if m.code.module.as_ref().is_some_and(|i| i.has_tla) {
            return false;
        }
        // Every requested module, deferred ones included.
        m.loaded.iter().all(|(_, c)| self.ready_for_sync(*c, seen))
    }

    /// EvaluateModuleSync (import-defer): Evaluate() settles at once for a graph that is
    /// ready for synchronous execution; its error throws.
    fn evaluate_sync(&mut self, id: usize) -> JsResult<()> {
        let p = self.evaluate(id)?;
        match self.promise_result(&p) {
            Some(Err(e)) => {
                if let Some(d) = p.as_object().and_then(|po| self.promise_data(po)) {
                    d.handled = true;
                }
                Err(e)
            }
            _ => Ok(()),
        }
    }

    /// IsModuleSCCEvaluated (import-defer): evaluated once its whole cycle is.
    fn scc_evaluated(&self, id: usize) -> bool {
        let m = self.modules[id].cycle_root.unwrap_or(id);
        self.modules[m].status == Status::Evaluated
    }

    /// GatherAsynchronousTransitiveDependencies (import-defer): the modules with top-level
    /// await that a deferred import still evaluates eagerly.
    fn gather_async(&self, id: usize, seen: &mut Vec<usize>, out: &mut Vec<usize>) {
        if seen.contains(&id) {
            return;
        }
        seen.push(id);
        if self.modules[id].status == Status::Evaluating || self.scc_evaluated(id) {
            return;
        }
        if self.modules[id].code.module.as_ref().is_some_and(|i| i.has_tla) {
            if !out.contains(&id) {
                out.push(id);
            }
            return;
        }
        for (_, c) in &self.modules[id].loaded {
            self.gather_async(*c, seen, out);
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
        if self.ns_symbol_like(o, k) {
            return Ok(self
                .heap
                .obj(o)
                .props
                .get(k)
                .and_then(|p| if let Slot::Data(v) = &p.slot { Some(v.clone()) } else { None })
                .unwrap_or_default());
        }
        self.ns_trigger(o)?;
        if !self.ns_has_export(o, k) {
            return Ok(Value::Undefined);
        }
        let Some((id, _)) = self.ns_names(o) else { return Ok(Value::Undefined) };
        let Value::String(name) = k.to_value() else { return Ok(Value::Undefined) };
        match self.resolve_export(id, &name, &mut Vec::new()) {
            Resolution::Found(m, None) => Ok(Value::Object(self.module_namespace(m)?)),
            Resolution::Found(m, Some(bn)) if &*bn == SOURCE => self.module_source(m),
            Resolution::Found(m, Some(bn)) => {
                let Some(env) = self.modules[m].env else { return self.reference_error("module not linked") };
                self.get_binding_value(env, &bn, true)
            }
            _ => self.reference_error("unresolvable export"),
        }
    }

    pub(super) fn ns_get_own(&mut self, o: ObjRef, k: &PropKey) -> JsResult<Option<PropDesc>> {
        if self.ns_symbol_like(o, k) {
            return Ok(self.heap.obj(o).props.get(k).map(PropDesc::from_prop));
        }
        self.ns_trigger(o)?;
        if !self.ns_has_export(o, k) {
            return Ok(None);
        }
        let v = self.ns_get(o, k)?;
        Ok(Some(PropDesc::data(v, true, true, false)))
    }

    pub(super) fn ns_define(&mut self, o: ObjRef, k: PropKey, d: PropDesc) -> JsResult<bool> {
        if self.ns_symbol_like(o, &k) {
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

    pub(super) fn ns_has(&mut self, o: ObjRef, k: &PropKey) -> JsResult<bool> {
        if self.ns_symbol_like(o, k) {
            return Ok(self.heap.obj(o).props.contains(k));
        }
        self.ns_trigger(o)?;
        Ok(self.ns_has_export(o, k))
    }

    pub(super) fn ns_delete(&mut self, o: ObjRef, k: &PropKey) -> JsResult<bool> {
        if self.ns_symbol_like(o, k) {
            let configurable = self.heap.obj(o).props.get(k).is_none_or(|p| p.configurable);
            if configurable {
                self.heap.obj_mut(o).props.remove(k);
            }
            return Ok(configurable);
        }
        self.ns_trigger(o)?;
        Ok(!self.ns_has_export(o, k))
    }

    pub(super) fn ns_keys(&mut self, o: ObjRef) -> JsResult<Vec<PropKey>> {
        self.ns_trigger(o)?;
        let mut out: Vec<PropKey> = self
            .ns_names(o)
            .map(|(_, n)| n.iter().map(|s| PropKey::from_js(s.clone())).collect())
            .unwrap_or_default();
        out.extend(self.heap.obj(o).props.keys());
        Ok(out)
    }

    // ---- evaluation ----

    /// Load, link and evaluate a module; the result is its evaluation promise.
    pub fn run_module(&mut self, key: &str, src: &str) -> JsResult<Value> {
        let id = self.prepare_module(key, src)?;
        self.run_prepared_module(id)
    }

    /// Run an inline module script (HTML: one that isn't in the module map) whose base URL is
    /// `base`.
    pub fn run_inline_module(&mut self, base: &str, src: &str) -> JsResult<Value> {
        let id = self.prepare_inline_module(base, src)?;
        self.run_prepared_module(id)
    }

    /// Parse the module at `key` and load its graph (fetching and resolving through the
    /// host's loader), without linking or evaluating it: its id, for
    /// [`Vm::run_prepared_module`].
    pub fn prepare_module(&mut self, key: &str, src: &str) -> JsResult<usize> {
        let id = self.load_module_source(key, src)?;
        self.load_graph(id)?;
        Ok(id)
    }

    /// [`Vm::prepare_module`] for an inline module script whose base URL is `base`.
    pub fn prepare_inline_module(&mut self, base: &str, src: &str) -> JsResult<usize> {
        let key = format!("{base}\0inline {}", self.modules.len());
        let id = self.load_module_source(&key, src)?;
        self.modules[id].base = base.to_string();
        self.load_graph(id)?;
        Ok(id)
    }

    /// Link and evaluate a prepared module: the promise of its evaluation.
    pub fn run_prepared_module(&mut self, id: usize) -> JsResult<Value> {
        if id >= self.modules.len() {
            return self.type_error("no such module");
        }
        self.link(id)?;
        self.evaluate(id)
    }

    /// The URL a referrer key stands for: a module's base URL, or the script's own URL.
    fn referrer_url(&self, key: &str) -> String {
        match self.module_map.get(key) {
            Some(&id) => self.modules[id].base.clone(),
            None => key.to_string(),
        }
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

    /// Evaluate (§16.2.1.5.3): the promise of the module's (or its cycle root's) evaluation.
    fn evaluate(&mut self, id: usize) -> JsResult<Value> {
        let mut id = id;
        if matches!(self.modules[id].status, Status::EvaluatingAsync | Status::Evaluated) {
            if let Some(r) = self.modules[id].cycle_root {
                id = r;
            }
        }
        if let Some((p, _, _)) = &self.modules[id].capability {
            return Ok(p.clone());
        }
        let p = self.new_promise(None);
        let (resolve, reject) = self.create_resolving_functions(p);
        self.modules[id].capability = Some((Value::Object(p), resolve.clone(), reject.clone()));
        let mut stack = Vec::new();
        match self.inner_module_evaluation(id, &mut stack, 0) {
            Err(e) => {
                if self.terminated {
                    return Err(e);
                }
                for m in stack {
                    self.modules[m].status = Status::Evaluated;
                    self.modules[m].error = Some(e.clone());
                }
                self.call(&reject, Value::Undefined, vec![e])?;
            }
            Ok(_) => {
                if self.modules[id].async_order == AsyncOrder::Unset {
                    self.call(&resolve, Value::Undefined, vec![Value::Undefined])?;
                }
            }
        }
        Ok(Value::Object(p))
    }

    /// Evaluate several modules' graphs (`import.defer()`'s asynchronous dependencies) as
    /// one promise that fulfills when all of theirs have.
    fn evaluate_list(&mut self, ids: &[usize]) -> JsResult<Value> {
        let p = self.new_promise(None);
        let (resolve, reject) = self.create_resolving_functions(p);
        let counter = self.new_array(vec![Value::Number(ids.len() as f64)]);
        if ids.is_empty() {
            self.call(&resolve, Value::Undefined, vec![Value::Undefined])?;
        }
        for &id in ids {
            let ep = self.evaluate(id)?;
            let ful =
                native_with(self, "", 1, all_step, false, vec![Value::Object(counter), resolve.clone()]);
            if let Value::Object(po) = ep {
                self.perform_then(po, Value::Object(ful), reject.clone(), None);
            }
        }
        Ok(Value::Object(p))
    }

    /// The modules InnerModuleEvaluation visits from `id`: its non-deferred requests, and for
    /// a deferred one the asynchronous dependencies it still evaluates (import-defer).
    fn required_modules(&self, id: usize) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for (r, defer) in &self.module_info(id).phased {
            let c = self.child(id, r);
            if *defer {
                let mut asyncs = Vec::new();
                self.gather_async(c, &mut Vec::new(), &mut asyncs);
                for a in asyncs {
                    if !out.contains(&a) {
                        out.push(a);
                    }
                }
            } else if !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }

    /// InnerModuleEvaluation (§16.2.1.5.3.1).
    fn inner_module_evaluation(
        &mut self,
        id: usize,
        stack: &mut Vec<usize>,
        index: usize,
    ) -> JsResult<usize> {
        match self.modules[id].status {
            Status::EvaluatingAsync | Status::Evaluated => {
                return match self.modules[id].error.clone() {
                    Some(e) => Err(e),
                    None => Ok(index),
                }
            }
            Status::Evaluating => return Ok(index),
            _ => {}
        }
        let m = &mut self.modules[id];
        m.status = Status::Evaluating;
        m.dfs_index = index;
        m.dfs_ancestor = index;
        m.pending_async = 0;
        let mut index = index + 1;
        stack.push(id);
        for r in self.required_modules(id) {
            index = self.inner_module_evaluation(r, stack, index)?;
            let mut r = r;
            if self.modules[r].status == Status::Evaluating {
                let a = self.modules[id].dfs_ancestor.min(self.modules[r].dfs_ancestor);
                self.modules[id].dfs_ancestor = a;
            } else {
                r = self.modules[r].cycle_root.unwrap_or(r);
                if let Some(e) = self.modules[r].error.clone() {
                    return Err(e);
                }
            }
            if let AsyncOrder::Order(_) = self.modules[r].async_order {
                self.modules[id].pending_async += 1;
                self.modules[r].async_parents.push(id);
            }
        }
        let has_tla = self.modules[id].code.module.as_ref().is_some_and(|i| i.has_tla);
        if self.modules[id].pending_async > 0 || has_tla {
            self.module_async_count += 1;
            self.modules[id].async_order = AsyncOrder::Order(self.module_async_count);
            if self.modules[id].pending_async == 0 {
                self.execute_async_module(id)?;
            }
        } else {
            self.execute_module(id)?;
        }
        if self.modules[id].dfs_ancestor == self.modules[id].dfs_index {
            while let Some(r) = stack.pop() {
                self.modules[r].status = if self.modules[r].async_order == AsyncOrder::Unset {
                    Status::Evaluated
                } else {
                    Status::EvaluatingAsync
                };
                self.modules[r].cycle_root = Some(id);
                if r == id {
                    break;
                }
            }
        }
        Ok(index)
    }

    /// ExecuteModule of a module without top-level await: run its body.
    fn execute_module(&mut self, id: usize) -> JsResult<Value> {
        let code = self.modules[id].code.clone();
        let env = self.modules[id].env.expect("linked module");
        let frame = self.frame(code, env, env);
        self.execute(frame)
    }

    /// ExecuteAsyncModule (§16.2.1.5.3.2): start the body; its promise settles the module.
    fn execute_async_module(&mut self, id: usize) -> JsResult<()> {
        let v = self.execute_module(id)?;
        let slots = vec![Value::Number(id as f64)];
        let ful = native_with(self, "", 1, module_fulfilled, false, slots.clone());
        let rej = native_with(self, "", 1, module_rejected, false, slots);
        if let Value::Object(po) = v {
            self.perform_then(po, Value::Object(ful), Value::Object(rej), None);
        }
        Ok(())
    }

    /// GatherAvailableAncestors (§16.2.1.5.3.3).
    fn gather_available_ancestors(&mut self, id: usize, list: &mut Vec<usize>) {
        for m in self.modules[id].async_parents.clone() {
            let root = self.modules[m].cycle_root.unwrap_or(m);
            if list.contains(&m) || self.modules[root].error.is_some() {
                continue;
            }
            self.modules[m].pending_async = self.modules[m].pending_async.saturating_sub(1);
            if self.modules[m].pending_async == 0 {
                list.push(m);
                if !self.modules[m].code.module.as_ref().is_some_and(|i| i.has_tla) {
                    self.gather_available_ancestors(m, list);
                }
            }
        }
    }

    fn resolve_capability(&mut self, id: usize) -> JsResult<()> {
        if let Some((_, res, _)) = self.modules[id].capability.clone() {
            self.call(&res, Value::Undefined, vec![Value::Undefined])?;
        }
        Ok(())
    }

    /// AsyncModuleExecutionFulfilled (§16.2.1.5.3.4).
    fn async_module_fulfilled(&mut self, id: usize) -> JsResult<()> {
        if self.modules[id].status == Status::Evaluated {
            return Ok(());
        }
        self.modules[id].async_order = AsyncOrder::Done;
        self.modules[id].status = Status::Evaluated;
        self.resolve_capability(id)?;
        let mut list = Vec::new();
        self.gather_available_ancestors(id, &mut list);
        list.sort_by_key(|m| match self.modules[*m].async_order {
            AsyncOrder::Order(n) => n,
            _ => u64::MAX,
        });
        for m in list {
            if self.modules[m].status == Status::Evaluated {
                continue;
            }
            if self.modules[m].code.module.as_ref().is_some_and(|i| i.has_tla) {
                self.execute_async_module(m)?;
                continue;
            }
            match self.execute_module(m) {
                Err(e) => {
                    if self.terminated {
                        return Err(e);
                    }
                    self.async_module_rejected(m, e)?;
                }
                Ok(_) => {
                    self.modules[m].async_order = AsyncOrder::Done;
                    self.modules[m].status = Status::Evaluated;
                    self.resolve_capability(m)?;
                }
            }
        }
        Ok(())
    }

    /// AsyncModuleExecutionRejected (§16.2.1.5.3.5).
    fn async_module_rejected(&mut self, id: usize, e: Value) -> JsResult<()> {
        if self.modules[id].status == Status::Evaluated {
            return Ok(());
        }
        self.modules[id].error = Some(e.clone());
        self.modules[id].status = Status::Evaluated;
        self.modules[id].async_order = AsyncOrder::Done;
        // The module's own capability first, then its parents' (rejection order).
        if let Some((_, _, rej)) = self.modules[id].capability.clone() {
            self.call(&rej, Value::Undefined, vec![e.clone()])?;
        }
        for m in self.modules[id].async_parents.clone() {
            self.async_module_rejected(m, e.clone())?;
        }
        Ok(())
    }

    /// `import(specifier, options)` (§13.3.10.1): always a promise.
    pub(super) fn dynamic_import(&mut self, spec: Value, opts: Value) -> Value {
        self.dynamic_import_phase(spec, opts, false)
    }

    /// `import(spec)`, or `import.defer(spec)` (`deferred`): that one evaluates only the
    /// module's asynchronous dependencies and resolves to the deferred namespace.
    pub(super) fn dynamic_import_phase(&mut self, spec: Value, opts: Value, deferred: bool) -> Value {
        let p = self.new_promise(None);
        let (resolve, reject) = self.create_resolving_functions(p);
        let r = (|| -> JsResult<String> {
            let s = self.to_string(&spec)?;
            let mut ty = String::new();
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
                            let Value::String(v) = self.get(w, &k)? else {
                                return self.type_error("Import attribute values must be strings");
                            };
                            if k == PropKey::lit("type") {
                                ty = to_rust(&v);
                            }
                        }
                    }
                }
            }
            Ok(format!("{ty}\0{}", to_rust(&s)))
        })();
        match r {
            Err(e) => {
                if !self.terminated {
                    let _ = self.call(&reject, Value::Undefined, vec![e]);
                }
            }
            Ok(request) => {
                let referrer_key = self.current_referrer().map(|k| self.referrer_url(&k));
                // HTML's HostLoadImportedModule resolves the specifier now (so that import
                // maps added later don't change it) and fetches afterwards.
                if let Some(mut resolver) = self.resolver.take() {
                    let specifier = request.split_once('\0').map_or(request.as_str(), |x| x.1);
                    let r = resolver(self, specifier, referrer_key.as_deref());
                    self.resolver = Some(resolver);
                    if let Err(msg) = r {
                        let msg = format!("Cannot load module '{specifier}': {msg}");
                        let e = self.error(ErrorKind::Type, &msg);
                        let _ = self.call(&reject, Value::Undefined, vec![e]);
                        return Value::Object(p);
                    }
                }
                let referrer = referrer_key.map(|r| Value::str(&r)).unwrap_or_default();
                let cont = native_with(
                    self,
                    "",
                    1,
                    continue_import,
                    false,
                    vec![Value::str(&request), referrer, resolve, reject, Value::Bool(deferred)],
                );
                // The load finishes in a later task (the host's) or job, as in browsers: a
                // module that is already in a graph being evaluated is not evaluated in the
                // middle of it.
                if let Some(enqueue) = self.host_enqueue_task {
                    enqueue(self, Value::Object(cont));
                } else {
                    let start = self.new_promise(None);
                    let (go, _) = self.create_resolving_functions(start);
                    self.perform_then(start, Value::Object(cont), Value::Undefined, None);
                    let _ = self.call(&go, Value::Undefined, vec![Value::Undefined]);
                }
            }
        }
        Value::Object(p)
    }

    /// ContinueDynamicImport (§16.2.1.10): load, link and evaluate, then resolve with the
    /// (deferred) namespace.
    fn continue_dynamic_import(
        &mut self,
        request: &str,
        referrer: Option<&str>,
        resolve: Value,
        reject: Value,
        deferred: bool,
    ) -> JsResult<()> {
        let r = (|| -> JsResult<(usize, Value)> {
            let id = self.host_load(request, referrer)?;
            self.load_graph(id)?;
            self.link(id)?;
            let ep = if deferred {
                let mut asyncs = Vec::new();
                self.gather_async(id, &mut Vec::new(), &mut asyncs);
                self.evaluate_list(&asyncs)?
            } else {
                self.evaluate(id)?
            };
            Ok((id, ep))
        })();
        match r {
            Err(e) => {
                if self.terminated {
                    return Err(e);
                }
                self.call(&reject, Value::Undefined, vec![e])?;
            }
            Ok((id, ep)) => {
                let ful = native_with(
                    self,
                    "",
                    1,
                    import_fulfilled,
                    false,
                    vec![resolve, Value::Number(id as f64), reject.clone(), Value::Bool(deferred)],
                );
                if let Value::Object(po) = ep {
                    self.perform_then(po, Value::Object(ful), reject, None);
                }
            }
        }
        Ok(())
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
        let url = js_str(&self.modules[id].base);
        self.heap
            .obj_mut(m)
            .props
            .insert(PropKey::lit("url"), Prop::data(Value::String(url.clone()), true, true, true));
        // HTML's HostGetImportMetaProperties (§8.1.6.7.1): the host's `resolve(specifier)`.
        if self.resolver.is_some() {
            let f = native_with(self, "resolve", 1, import_meta_resolve, false, vec![Value::String(url)]);
            self.heap
                .obj_mut(m)
                .props
                .insert(PropKey::lit("resolve"), Prop::data(Value::Object(f), true, true, true));
        }
        self.modules[id].meta = Some(m);
        Value::Object(m)
    }
}

/// `import.meta.resolve(specifier)` (HTML §8.1.6.7.1 HostGetImportMetaProperties): the specifier resolved for this
/// module, serialized; a TypeError if it doesn't resolve.
fn import_meta_resolve(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let key = match &slots(vm, c.callee)[0] {
        Value::String(s) => to_rust(s),
        _ => String::new(),
    };
    let specifier = vm.to_string(&c.arg(0))?;
    let specifier = to_rust(&specifier);
    let Some(mut resolver) = vm.resolver.take() else {
        return vm.type_error(&format!("Cannot resolve '{specifier}'"));
    };
    let r = resolver(vm, &specifier, Some(&key));
    vm.resolver = Some(resolver);
    match r {
        Ok(url) => Ok(Value::String(js_str(&url))),
        Err(msg) => vm.type_error(&format!("Cannot resolve '{specifier}': {msg}")),
    }
}

fn module_fulfilled(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    if let Value::Number(id) = slots(vm, c.callee)[0] {
        vm.async_module_fulfilled(id as usize)?;
    }
    Ok(Value::Undefined)
}

fn module_rejected(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    if let Value::Number(id) = slots(vm, c.callee)[0] {
        vm.async_module_rejected(id as usize, c.arg(0))?;
    }
    Ok(Value::Undefined)
}

/// One graph of `evaluate_list` fulfilled: resolve once all have.
fn all_step(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    let Value::Object(counter) = s[0] else { return Ok(Value::Undefined) };
    let n = match vm.get(counter, &PropKey::Index(0))? {
        Value::Number(n) => n - 1.0,
        _ => return Ok(Value::Undefined),
    };
    vm.set_or_throw(counter, PropKey::Index(0), Value::Number(n))?;
    if n == 0.0 {
        vm.call(&s[1], Value::Undefined, vec![Value::Undefined])?;
    }
    Ok(Value::Undefined)
}

fn continue_import(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    let (Value::String(req), deferred) = (&s[0], matches!(s[4], Value::Bool(true))) else {
        return Ok(Value::Undefined);
    };
    let req = to_rust(req);
    let referrer = match &s[1] {
        Value::String(r) => Some(to_rust(r)),
        _ => None,
    };
    vm.continue_dynamic_import(&req, referrer.as_deref(), s[2].clone(), s[3].clone(), deferred)?;
    Ok(Value::Undefined)
}

fn import_fulfilled(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    let Value::Number(id) = s[1] else { return Ok(Value::Undefined) };
    let ns = if matches!(s.get(3), Some(Value::Bool(true))) {
        vm.deferred_namespace(id as usize)
    } else {
        vm.module_namespace(id as usize)
    };
    match ns {
        Ok(ns) => vm.call(&s[0], Value::Undefined, vec![Value::Object(ns)])?,
        Err(e) => vm.call(&s[2], Value::Undefined, vec![e])?,
    };
    Ok(Value::Undefined)
}

/// The import name of `import source x` (~source~).
const SOURCE: &str = "*source*";

fn is_source(name: &[u16]) -> bool {
    name.iter().copied().eq(SOURCE.encode_utf16())
}

/// %AbstractModuleSource% and its prototype (source-phase-imports §28.1.1): an abstract
/// class (the constructor always throws) whose `@@toStringTag` getter reports a module
/// source's [[ModuleSourceClassName]]. Not a global; hosts expose it.
pub(super) fn install_abstract_module_source(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let proto = vm.new_object_with_proto(Some(op));
    let c = super::realm::constructor(
        vm,
        "AbstractModuleSource",
        0,
        |vm, _| vm.type_error("AbstractModuleSource is an abstract class"),
        proto,
    );
    let tag = PropKey::Sym(vm.realm.sym_to_string_tag.clone());
    super::realm::accessor(
        vm,
        proto,
        tag,
        Some(|vm, c| match &c.this {
            Value::Object(o) => match &vm.heap.obj(*o).kind {
                Kind::ModuleSource(n) => Ok(Value::str(n)),
                _ => Ok(Value::Undefined),
            },
            _ => Ok(Value::Undefined),
        }),
        None,
    );
    vm.realm.abstract_module_source = c;
    vm.realm.abstract_module_source_proto = proto;
}

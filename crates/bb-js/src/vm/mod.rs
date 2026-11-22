//! The interpreter: a bytecode stack machine over a garbage-collected heap.
//!
//! * `value`: language values and number/string conversions.
//! * `heap`: objects, properties, environment records, mark-sweep GC.
//! * `code`, `compiler`: bytecode and the AST → bytecode compiler.
//! * `object`: the essential internal methods (§10.1) incl. array and string exotics.
//! * `ops`: type conversions and operators (§7, §13).
//! * `env`: identifier resolution over environment records (§9.1).
//! * `realm` and `builtins`: the intrinsics and the global object (§9.3, §19–§28).
//!
//! The collector runs only between instructions of the outermost execution (no Rust frame
//! then holds a heap reference that isn't on a VM stack); nested runs (getters, natives
//! calling back) defer it.

pub mod builtins;
mod class;
pub mod code;
pub mod compiler;
mod env;
pub mod heap;
pub mod object;
pub mod ops;
pub mod realm;
pub mod value;

use code::{Code, FuncTemplate, Op};
use compiler::{Compiler, Unit};
use heap::*;
use realm::Realm;
use std::collections::HashMap;
use std::rc::Rc;
pub use value::{js_str, to_rust, JsString, PropKey, Value};

pub type JsResult<T> = Result<T, Value>;

/// A try handler: where to continue and what to restore.
#[derive(Clone)]
pub struct Handler {
    pc: u32,
    stack: usize,
    env: EnvRef,
}

/// An activation: a function body, script or eval code being run.
pub struct Frame {
    pub code: Rc<Code>,
    pub pc: usize,
    pub stack: Vec<Value>,
    pub env: EnvRef,
    pub var_env: EnvRef,
    handlers: Vec<Handler>,
    pub callee: Option<ObjRef>,
    pub args: Vec<Value>,
    ret: Value,
    completion: Value,
    /// The function environment (for `this` of derived constructors at return).
    fenv: Option<EnvRef>,
    /// [[Construct]]: the allocated `this` (base constructors) or `None` (derived).
    construct: Option<Option<Value>>,
}

impl Frame {
    pub fn trace(&self, os: &mut Vec<ObjRef>, es: &mut Vec<EnvRef>) {
        let push = |v: &Value, os: &mut Vec<ObjRef>| {
            if let Value::Object(o) = v {
                os.push(*o)
            }
        };
        self.stack.iter().for_each(|v| push(v, os));
        self.args.iter().for_each(|v| push(v, os));
        push(&self.ret, os);
        push(&self.completion, os);
        if let Some(Some(v)) = &self.construct {
            push(v, os);
        }
        es.push(self.env);
        es.push(self.var_env);
        es.extend(self.fenv);
        es.extend(self.handlers.iter().map(|h| h.env));
        os.extend(self.callee);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorKind {
    Error,
    Type,
    Range,
    Reference,
    Syntax,
    Eval,
    Uri,
}

/// Limits that keep hostile scripts from exhausting the host.
const MAX_FRAMES: usize = 4000;
const MAX_NESTED_RUNS: usize = 200;
const GC_EVERY: usize = 200_000;

pub struct Vm {
    pub heap: Heap,
    pub realm: Realm,
    frames: Vec<Frame>,
    nested: usize,
    /// Symbol.for registry (§20.4.2.2).
    pub registry: HashMap<Vec<u16>, value::Sym>,
    templates: HashMap<(usize, u32), ObjRef>,
    /// Values hosts keep alive across calls.
    pub roots: Vec<Value>,
    /// Text written by `print` (test harnesses).
    pub output: Vec<String>,
    /// Instructions left before the script is terminated (`None`: unlimited).
    pub fuel: Option<u64>,
    /// Set when execution was terminated (out of fuel): no handler runs, every run unwinds.
    pub terminated: bool,
}

impl Default for Vm {
    fn default() -> Self {
        Vm::new()
    }
}

impl Vm {
    pub fn new() -> Vm {
        let mut heap = Heap::default();
        let realm = Realm::placeholder(&mut heap);
        let mut vm = Vm {
            heap,
            realm,
            frames: Vec::new(),
            nested: 0,
            registry: HashMap::new(),
            templates: HashMap::new(),
            roots: Vec::new(),
            output: Vec::new(),
            fuel: None,
            terminated: false,
        };
        realm::init(&mut vm);
        vm
    }

    // ---- entry points ----

    /// Run a script in the global environment (§16.1.6 ScriptEvaluation); the result is its
    /// completion value.
    pub fn run_script(&mut self, src: &str) -> JsResult<Value> {
        let prog = match crate::parse_script(src) {
            Ok(p) => p,
            Err(e) => return Err(self.error(ErrorKind::Syntax, &e.msg)),
        };
        let source: Rc<[char]> = src.chars().collect::<Vec<_>>().into();
        let code = Rc::new(Compiler::compile_unit(Unit::Script(&prog), source));
        let genv = self.realm.global_env;
        let frame = self.frame(code, genv, genv);
        self.execute(frame)
    }

    /// The message of a thrown value, for hosts: `Name: message` for errors.
    pub fn describe(&mut self, v: &Value) -> String {
        if let Value::Object(o) = v {
            let name = self.get(*o, &PropKey::named("name")).ok();
            let msg = self.get(*o, &PropKey::named("message")).ok();
            if let (Some(Value::String(n)), Some(Value::String(m))) = (&name, &msg) {
                return format!("{}: {}", to_rust(n), to_rust(m));
            }
        }
        match self.to_string(v) {
            Ok(s) => to_rust(&s),
            Err(_) => "<exception>".into(),
        }
    }

    fn frame(&self, code: Rc<Code>, env: EnvRef, var_env: EnvRef) -> Frame {
        Frame {
            code,
            pc: 0,
            stack: Vec::new(),
            env,
            var_env,
            handlers: Vec::new(),
            callee: None,
            args: Vec::new(),
            ret: Value::Undefined,
            completion: Value::Undefined,
            fenv: None,
            construct: None,
        }
    }

    /// Charge one unit of fuel from native code that loops (property access).
    pub fn burn(&mut self) -> JsResult<()> {
        if self.terminated {
            return Err(Value::Undefined);
        }
        if let Some(f) = &mut self.fuel {
            if *f == 0 {
                self.terminated = true;
                return Err(Value::str("execution terminated: out of fuel"));
            }
            *f -= 1;
        }
        Ok(())
    }

    pub fn collect_garbage(&mut self) {
        let mut roots = Roots::default();
        self.realm.trace(&mut roots);
        for f in &self.frames {
            f.trace(&mut roots.objects, &mut roots.envs);
        }
        roots.objects.extend(self.templates.values());
        roots.values.extend(self.roots.iter().cloned());
        self.heap.collect(&roots);
    }

    // ---- errors ----

    pub fn error(&mut self, kind: ErrorKind, msg: &str) -> Value {
        let proto = self.realm.error_proto(kind);
        let o = self.heap.alloc(Object::new(Some(proto), Kind::Error));
        if !msg.is_empty() {
            self.heap.obj_mut(o).props.insert(PropKey::named("message"), Prop::method(Value::str(msg)));
        }
        Value::Object(o)
    }
    pub fn type_error<T>(&mut self, msg: &str) -> JsResult<T> {
        Err(self.error(ErrorKind::Type, msg))
    }
    pub fn range_error<T>(&mut self, msg: &str) -> JsResult<T> {
        Err(self.error(ErrorKind::Range, msg))
    }
    pub fn reference_error<T>(&mut self, msg: &str) -> JsResult<T> {
        Err(self.error(ErrorKind::Reference, msg))
    }
    pub fn syntax_error<T>(&mut self, msg: &str) -> JsResult<T> {
        Err(self.error(ErrorKind::Syntax, msg))
    }

    // ---- calls ----

    /// Call (§7.3.14).
    pub fn call(&mut self, f: &Value, this: Value, args: Vec<Value>) -> JsResult<Value> {
        let Value::Object(fo) = f else {
            return self.type_error("not a function");
        };
        match self.begin_call(*fo, this, args, None)? {
            Some(v) => Ok(v),
            None => {
                let frame = self.frames.pop().expect("pushed frame");
                self.execute(frame)
            }
        }
    }

    /// Construct (§7.3.15).
    pub fn construct(&mut self, f: ObjRef, args: Vec<Value>, new_target: Option<ObjRef>) -> JsResult<Value> {
        let nt = new_target.unwrap_or(f);
        match self.begin_call(f, Value::Undefined, args, Some(nt))? {
            Some(v) => Ok(v),
            None => {
                let frame = self.frames.pop().expect("pushed frame");
                self.execute(frame)
            }
        }
    }

    /// Start a call: natives run to completion (`Some`), closures push a frame (`None`).
    fn begin_call(
        &mut self,
        f: ObjRef,
        this: Value,
        args: Vec<Value>,
        new_target: Option<ObjRef>,
    ) -> JsResult<Option<Value>> {
        enum K {
            Closure,
            Native(NativeFn, bool),
            Bound(ObjRef, Value, Vec<Value>),
            None,
        }
        let k = match &self.heap.obj(f).kind {
            Kind::Function(_) => K::Closure,
            Kind::Native(n) => K::Native(n.f, n.constructor),
            Kind::Bound(b) => K::Bound(b.target, b.this.clone(), b.args.clone()),
            _ => K::None,
        };
        match k {
            K::None => {
                if new_target.is_some() {
                    self.type_error("not a constructor")
                } else {
                    self.type_error("not a function")
                }
            }
            K::Native(nf, ctor) => {
                if new_target.is_some() && !ctor {
                    return self.type_error("not a constructor");
                }
                if self.nested >= MAX_NESTED_RUNS || self.frames.len() >= MAX_FRAMES {
                    return self.range_error("Maximum call stack size exceeded");
                }
                self.nested += 1;
                let r = nf(self, NativeCall { callee: f, this, args, new_target });
                self.nested -= 1;
                r.map(Some)
            }
            K::Bound(target, bthis, mut bargs) => {
                bargs.extend(args);
                match new_target {
                    Some(nt) => {
                        if !self.is_constructor(&Value::Object(target)) {
                            return self.type_error("not a constructor");
                        }
                        let nt = if nt == f { target } else { nt };
                        self.begin_call(target, Value::Undefined, bargs, Some(nt))
                    }
                    None => self.begin_call(target, bthis, bargs, None),
                }
            }
            K::Closure => {
                self.enter_closure(f, this, args, new_target)?;
                Ok(None)
            }
        }
    }

    /// PrepareForOrdinaryCall + OrdinaryCallBindThis (§10.2.1.1–2): push the frame.
    fn enter_closure(
        &mut self,
        f: ObjRef,
        this: Value,
        args: Vec<Value>,
        new_target: Option<ObjRef>,
    ) -> JsResult<()> {
        if self.frames.len() >= MAX_FRAMES {
            return self.range_error("Maximum call stack size exceeded");
        }
        let (tmpl, scope, this_mode, constructor, derived, class_ctor) = match &self.heap.obj(f).kind {
            Kind::Function(c) => (c.tmpl.clone(), c.env, c.this_mode, c.constructor, c.derived, c.class_ctor),
            _ => unreachable!("closure"),
        };
        if class_ctor && new_target.is_none() {
            return self.type_error("Class constructor cannot be invoked without 'new'");
        }
        if new_target.is_some() && !constructor {
            return self.type_error("not a constructor");
        }
        let code = self.code_of(&tmpl);
        let mut construct = None;
        let this_val = match new_target {
            Some(nt) if !derived => {
                let proto = self.prototype_from_ctor(nt, |r| r.object_proto)?;
                let o = self.heap.alloc(Object::new(Some(proto), Kind::Ordinary));
                construct = Some(Some(Value::Object(o)));
                Some(Value::Object(o))
            }
            Some(_) => {
                construct = Some(None);
                None
            }
            None => match this_mode {
                ThisMode::Lexical => None,
                ThisMode::Strict => Some(this),
                ThisMode::Global => Some(if this.is_nullish() {
                    self.realm.global_this.clone()
                } else {
                    Value::Object(self.to_object(&this)?)
                }),
            },
        };
        let env = if this_mode == ThisMode::Lexical {
            self.heap.alloc_env(Env {
                outer: Some(scope),
                kind: EnvKind::Declarative,
                bindings: HashMap::new(),
            })
        } else {
            self.heap.alloc_env(Env {
                outer: Some(scope),
                kind: EnvKind::Function {
                    this: this_val,
                    func: f,
                    new_target: new_target.map(Value::Object).unwrap_or(Value::Undefined),
                },
                bindings: HashMap::new(),
            })
        };
        let mut frame = self.frame(code, env, env);
        frame.callee = Some(f);
        frame.args = args;
        frame.fenv = (this_mode != ThisMode::Lexical).then_some(env);
        frame.construct = construct;
        self.frames.push(frame);
        Ok(())
    }

    pub fn code_of(&mut self, tmpl: &Rc<FuncTemplate>) -> Rc<Code> {
        tmpl.code.get_or_init(|| Rc::new(Compiler::compile_function(tmpl))).clone()
    }

    /// GetPrototypeFromConstructor (§10.1.14).
    pub fn prototype_from_ctor(&mut self, ctor: ObjRef, default: fn(&Realm) -> ObjRef) -> JsResult<ObjRef> {
        let p = self.get(ctor, &PropKey::named("prototype"))?;
        Ok(match p {
            Value::Object(o) => o,
            _ => default(&self.realm),
        })
    }

    // ---- the interpreter loop ----

    /// Run `frame` (and whatever it calls) until it returns.
    fn execute(&mut self, frame: Frame) -> JsResult<Value> {
        if self.nested >= MAX_NESTED_RUNS {
            return self.range_error("Maximum call stack size exceeded");
        }
        self.nested += 1;
        let base = self.frames.len();
        self.frames.push(frame);
        let r = self.run(base);
        self.nested -= 1;
        r
    }

    fn run(&mut self, base: usize) -> JsResult<Value> {
        loop {
            let r = self.step(base);
            match r {
                Ok(Some(v)) => return Ok(v),
                Ok(None) => {}
                Err(e) => {
                    if self.terminated {
                        self.frames.truncate(base);
                        return Err(e);
                    }
                    // Unwind to the nearest handler within this run.
                    loop {
                        if self.frames.len() <= base {
                            return Err(e);
                        }
                        let f = self.frames.last_mut().expect("frame");
                        if let Some(h) = f.handlers.pop() {
                            f.stack.truncate(h.stack);
                            f.env = h.env;
                            f.pc = h.pc as usize;
                            f.stack.push(e);
                            break;
                        }
                        self.frames.pop();
                        if self.frames.len() <= base {
                            return Err(e);
                        }
                    }
                }
            }
        }
    }

    fn pop(&mut self) -> Value {
        self.frames.last_mut().and_then(|f| f.stack.pop()).unwrap_or_default()
    }
    fn push(&mut self, v: Value) {
        if let Some(f) = self.frames.last_mut() {
            f.stack.push(v);
        }
    }
    fn peek(&self, n: usize) -> Value {
        let s = &self.frames.last().expect("frame").stack;
        s.get(s.len().wrapping_sub(n + 1)).cloned().unwrap_or_default()
    }
    fn pop_n(&mut self, n: usize) -> Vec<Value> {
        let f = self.frames.last_mut().expect("frame");
        let at = f.stack.len().saturating_sub(n);
        f.stack.split_off(at)
    }
    fn top(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("frame")
    }
    fn strict(&self) -> bool {
        self.frames.last().is_some_and(|f| f.code.strict)
    }
    fn name(&self, i: u32) -> Rc<str> {
        self.frames.last().expect("frame").code.names[i as usize].clone()
    }
    fn jump(&mut self, to: u32) {
        let f = self.top();
        if (to as usize) < f.pc {
            self.maybe_gc();
        }
        self.top().pc = to as usize;
    }
    fn maybe_gc(&mut self) {
        if self.nested == 1 && self.heap.allocated > GC_EVERY {
            self.collect_garbage();
        }
    }

    /// Return from the top frame; `Some` when it was this run's base frame.
    fn do_return(&mut self, v: Value, base: usize) -> JsResult<Option<Value>> {
        let f = self.frames.pop().expect("frame");
        let v = match f.construct {
            None => v,
            Some(this) => {
                if let Value::Object(_) = v {
                    v
                } else if let Some(t) = this {
                    t
                } else {
                    if !v.is_undefined() {
                        return self.type_error("Derived constructors may only return object or undefined");
                    }
                    match f.fenv.map(|e| &self.heap.env(e).kind) {
                        Some(EnvKind::Function { this: Some(t), .. }) => t.clone(),
                        _ => return self.reference_error("Must call super constructor before returning"),
                    }
                }
            }
        };
        if self.frames.len() <= base {
            return Ok(Some(v));
        }
        self.push(v);
        Ok(None)
    }

    /// One instruction. `Ok(Some(v))`: the base frame returned `v`.
    fn step(&mut self, base: usize) -> JsResult<Option<Value>> {
        if self.terminated {
            return Err(Value::Undefined);
        }
        if let Some(f) = &mut self.fuel {
            if *f == 0 {
                self.terminated = true;
                return Err(Value::str("execution terminated: out of fuel"));
            }
            *f -= 1;
        }
        let f = self.frames.last_mut().expect("frame");
        let Some(&op) = f.code.ops.get(f.pc) else {
            return self.do_return(Value::Undefined, base);
        };
        f.pc += 1;
        match op {
            Op::Undef => self.push(Value::Undefined),
            Op::Null => self.push(Value::Null),
            Op::True => self.push(Value::Bool(true)),
            Op::False => self.push(Value::Bool(false)),
            Op::Num(n) => self.push(Value::Number(n)),
            Op::Str(i) => {
                let s = self.top().code.strings[i as usize].clone();
                self.push(Value::String(s));
            }
            Op::Pop => {
                self.pop();
            }
            Op::Dup => {
                let v = self.peek(0);
                self.push(v);
            }
            Op::Dup2 => {
                let a = self.peek(1);
                let b = self.peek(0);
                self.push(a);
                self.push(b);
            }
            Op::Swap => {
                let s = &mut self.top().stack;
                let n = s.len();
                if n >= 2 {
                    s.swap(n - 1, n - 2);
                }
            }
            Op::Bury(n) => {
                let s = &mut self.top().stack;
                if let Some(v) = s.pop() {
                    let at = s.len().saturating_sub(n as usize);
                    s.insert(at, v);
                }
            }
            Op::Pick(n) => {
                let v = self.peek(n as usize);
                self.push(v);
            }
            Op::Roll(n) => {
                let s = &mut self.top().stack;
                let at = s.len().saturating_sub(n as usize + 1);
                if at < s.len() {
                    let v = s.remove(at);
                    s.push(v);
                }
            }
            Op::GetVar(i) => {
                let name = self.name(i);
                let env = self.top().env;
                let v = self.get_var(env, &name)?;
                self.push(v);
            }
            Op::TypeofVar(i) => {
                let name = self.name(i);
                let env = self.top().env;
                let v = match self.resolve(env, &name)? {
                    None => Value::str("undefined"),
                    Some(r) => {
                        let strict = self.strict();
                        let v = self.get_binding_value(r, &name, strict)?;
                        Value::String(self.type_of(&v))
                    }
                };
                self.push(v);
            }
            Op::GetVarThis(i) => {
                let name = self.name(i);
                let env = self.top().env;
                let (v, this) = match self.resolve(env, &name)? {
                    None => return self.reference_error(&format!("{name} is not defined")),
                    Some(r) => {
                        let strict = self.strict();
                        let v = self.get_binding_value(r, &name, strict)?;
                        let this = match &self.heap.env(r).kind {
                            EnvKind::Object { object, with: true } => Value::Object(*object),
                            _ => Value::Undefined,
                        };
                        (v, this)
                    }
                };
                self.push(v);
                self.push(this);
            }
            Op::SetVar(i) => {
                let name = self.name(i);
                let v = self.peek(0);
                let env = self.top().env;
                self.set_var(env, &name, v)?;
            }
            Op::InitBinding(i) => {
                let name = self.name(i);
                let v = self.pop();
                let env = self.top().env;
                self.init_binding(env, &name, v)?;
            }
            Op::DeclareLet(i) | Op::DeclareConst(i) => {
                let name = self.name(i);
                let env = self.top().env;
                let mutable = matches!(op, Op::DeclareLet(_));
                self.heap.env_mut(env).bindings.entry(name).or_insert(Binding {
                    value: None,
                    mutable,
                    strict: true,
                    deletable: false,
                });
            }
            Op::DeclareVar(i) => {
                let name = self.name(i);
                let env = self.top().var_env;
                self.heap.env_mut(env).bindings.entry(name).or_insert(Binding {
                    value: Some(Value::Undefined),
                    mutable: true,
                    strict: false,
                    deletable: false,
                });
            }
            Op::SetVarEnv(i) => {
                let name = self.name(i);
                let v = self.pop();
                let env = self.top().var_env;
                let strict = self.strict();
                if matches!(self.heap.env(env).kind, EnvKind::Global { .. })
                    || self.heap.env(env).bindings.contains_key(&name)
                {
                    self.set_mutable_binding(env, &name, v, strict)?;
                } else {
                    self.heap.env_mut(env).bindings.insert(
                        name,
                        Binding { value: Some(v), mutable: true, strict: false, deletable: false },
                    );
                }
            }
            Op::DeleteVar(i) => {
                let name = self.name(i);
                let env = self.top().env;
                let r = self.delete_var(env, &name)?;
                self.push(Value::Bool(r));
            }
            Op::PushScope => {
                let outer = self.top().env;
                let e = self.heap.alloc_env(Env {
                    outer: Some(outer),
                    kind: EnvKind::Declarative,
                    bindings: HashMap::new(),
                });
                self.top().env = e;
            }
            Op::PopScope => {
                let e = self.top().env;
                if let Some(o) = self.heap.env(e).outer {
                    self.top().env = o;
                }
            }
            Op::CopyScope => {
                let e = self.top().env;
                let (outer, bindings) = {
                    let env = self.heap.env(e);
                    (env.outer, env.bindings.clone())
                };
                let n = self.heap.alloc_env(Env { outer, kind: EnvKind::Declarative, bindings });
                self.top().env = n;
            }
            Op::PushWith => {
                let v = self.pop();
                let o = self.to_object(&v)?;
                let outer = self.top().env;
                let e = self.heap.alloc_env(Env {
                    outer: Some(outer),
                    kind: EnvKind::Object { object: o, with: true },
                    bindings: HashMap::new(),
                });
                self.top().env = e;
            }
            Op::PushVarScope => {
                let outer = self.top().env;
                let e = self.heap.alloc_env(Env {
                    outer: Some(outer),
                    kind: EnvKind::Declarative,
                    bindings: HashMap::new(),
                });
                let f = self.top();
                f.env = e;
                f.var_env = e;
            }
            Op::GlobalInit => self.global_declaration_instantiation()?,
            Op::EvalInit => self.eval_declaration_instantiation()?,
            Op::This => {
                let env = self.top().env;
                let v = self.resolve_this(env)?;
                self.push(v);
            }
            Op::NewTarget => {
                let env = self.top().env;
                let v = self.new_target_of(env);
                self.push(v);
            }
            Op::Object => {
                let o = self.new_object();
                self.push(Value::Object(o));
            }
            Op::Array => {
                let a = self.new_array(Vec::new());
                self.push(Value::Object(a));
            }
            Op::ArrayPush => {
                let v = self.pop();
                if let Value::Object(a) = self.peek(0) {
                    self.array_push(a, v)?;
                }
            }
            Op::ArrayHole => {
                if let Value::Object(a) = self.peek(0) {
                    let len = self.length_of_array(a);
                    self.set_array_length_raw(a, len + 1);
                }
            }
            Op::ArraySpread => {
                let it = self.pop();
                if let Value::Object(a) = self.peek(0) {
                    let vals = self.iterate_to_vec(&it)?;
                    for v in vals {
                        self.array_push(a, v)?;
                    }
                }
            }
            Op::GetProp => {
                let k = self.pop();
                let o = self.pop();
                let v = self.get_value_prop(&o, &k)?;
                self.push(v);
            }
            Op::GetNamed(i) => {
                let k = self.top().code.keys[i as usize].clone();
                let o = self.pop();
                let v = self.get_value_key(&o, &k)?;
                self.push(v);
            }
            Op::SetProp => {
                let v = self.pop();
                let k = self.pop();
                let o = self.pop();
                if o.is_nullish() {
                    let ks = self.to_property_key(&k).map(|k| k.display()).unwrap_or_default();
                    return self.type_error(&format!(
                        "Cannot set properties of {} (setting '{ks}')",
                        ops::nullish_name(&o)
                    ));
                }
                let key = self.to_property_key(&k)?;
                self.put_value(&o, key, v.clone())?;
                self.push(v);
            }
            Op::SetNamed(i) => {
                let k = self.top().code.keys[i as usize].clone();
                let v = self.pop();
                let o = self.pop();
                if o.is_nullish() {
                    return self.type_error(&format!(
                        "Cannot set properties of {} (setting '{}')",
                        ops::nullish_name(&o),
                        k.display()
                    ));
                }
                self.put_value(&o, k, v.clone())?;
                self.push(v);
            }
            Op::DefineProp => {
                let v = self.pop();
                let k = self.pop();
                let key = self.to_property_key(&k)?;
                if let Value::Object(o) = self.peek(0) {
                    self.create_data_property_or_throw(o, key, v)?;
                }
            }
            Op::DefineNamed(i) => {
                let k = self.top().code.keys[i as usize].clone();
                let v = self.pop();
                if let Value::Object(o) = self.peek(0) {
                    self.create_data_property_or_throw(o, k, v)?;
                }
            }
            Op::DefineMethod(t, kind) => {
                let k = self.pop();
                let key = self.to_property_key(&k)?;
                let Value::Object(obj) = self.peek(0) else { return Ok(None) };
                let tmpl = self.top().code.funcs[t as usize].clone();
                let env = self.top().env;
                let prefix = match kind {
                    1 => Some("get"),
                    2 => Some("set"),
                    _ => None,
                };
                let f = self.make_function(tmpl, env, Some(obj), false);
                self.set_function_name(f, &key, prefix);
                let pk = key;
                match kind {
                    0 => {
                        self.define_own_property_or_throw(
                            obj,
                            pk,
                            object::PropDesc::data(Value::Object(f), true, true, true),
                        )?;
                    }
                    1 => {
                        let d = object::PropDesc {
                            get: Some(Some(f)),
                            enumerable: Some(true),
                            configurable: Some(true),
                            ..Default::default()
                        };
                        self.define_own_property_or_throw(obj, pk, d)?;
                    }
                    _ => {
                        let d = object::PropDesc {
                            set: Some(Some(f)),
                            enumerable: Some(true),
                            configurable: Some(true),
                            ..Default::default()
                        };
                        self.define_own_property_or_throw(obj, pk, d)?;
                    }
                }
            }
            Op::SetProto => {
                let v = self.pop();
                if let Value::Object(o) = self.peek(0) {
                    match v {
                        Value::Object(p) => {
                            self.set_prototype_of(o, Some(p))?;
                        }
                        Value::Null => {
                            self.set_prototype_of(o, None)?;
                        }
                        _ => {}
                    }
                }
            }
            Op::CopyData => {
                let src = self.pop();
                if let Value::Object(t) = self.peek(0) {
                    self.copy_data_properties(t, &src, &[])?;
                }
            }
            Op::ObjRest => {
                let keys = self.pop();
                let src = self.pop();
                let excluded = match keys {
                    Value::Object(a) => {
                        let n = self.length_of_array(a);
                        let mut out = Vec::new();
                        for i in 0..n {
                            let v = self.get(a, &PropKey::from_u32(i))?;
                            out.push(self.to_property_key(&v)?);
                        }
                        out
                    }
                    _ => Vec::new(),
                };
                let t = self.new_object();
                self.copy_data_properties(t, &src, &excluded)?;
                self.push(Value::Object(t));
            }
            Op::DeleteProp => {
                let k = self.pop();
                let o = self.pop();
                let obj = self.to_object(&o)?;
                let key = self.to_property_key(&k)?;
                let ok = self.delete(obj, &key)?;
                if !ok && self.strict() {
                    return self.type_error(&format!("Cannot delete property '{}'", key.display()));
                }
                self.push(Value::Bool(ok));
            }
            Op::AppendKey => {
                let k = self.peek(0);
                if let Value::Object(a) = self.peek(3) {
                    self.array_push(a, k)?;
                }
            }
            Op::RequireCoercible => {
                let v = self.peek(0);
                if v.is_nullish() {
                    return self.type_error(&format!("Cannot destructure '{}'", ops::nullish_name(&v)));
                }
            }
            Op::Call(n) => {
                let args = self.pop_n(n as usize);
                let this = self.pop();
                let f = self.pop();
                return self.op_call(f, this, args);
            }
            Op::CallSpread => {
                let arr = self.pop();
                let this = self.pop();
                let f = self.pop();
                let args = self.array_values(&arr)?;
                return self.op_call(f, this, args);
            }
            Op::CallEval(n) => {
                let args = self.pop_n(n as usize);
                let this = self.pop();
                let f = self.pop();
                return self.op_call_eval(f, this, args);
            }
            Op::CallEvalSpread => {
                let arr = self.pop();
                let this = self.pop();
                let f = self.pop();
                let args = self.array_values(&arr)?;
                return self.op_call_eval(f, this, args);
            }
            Op::New(n) => {
                let args = self.pop_n(n as usize);
                let c = self.pop();
                return self.op_new(c, args);
            }
            Op::NewSpread => {
                let arr = self.pop();
                let c = self.pop();
                let args = self.array_values(&arr)?;
                return self.op_new(c, args);
            }
            Op::Closure(i) => {
                let tmpl = self.top().code.funcs[i as usize].clone();
                let env = self.top().env;
                let f = if tmpl.is_expression {
                    // A named function expression sees its own name (§15.2.5).
                    let name: Rc<str> = tmpl.ast.name.as_deref().unwrap_or_default().into();
                    let fenv = self.heap.alloc_env(Env {
                        outer: Some(env),
                        kind: EnvKind::Declarative,
                        bindings: HashMap::new(),
                    });
                    let f = self.make_function(tmpl, fenv, None, true);
                    self.heap.env_mut(fenv).bindings.insert(
                        name,
                        Binding {
                            value: Some(Value::Object(f)),
                            mutable: false,
                            strict: false,
                            deletable: false,
                        },
                    );
                    f
                } else {
                    self.make_function(tmpl, env, None, true)
                };
                self.push(Value::Object(f));
            }
            Op::SetFnName => {
                let f = self.peek(0);
                let k = self.peek(1);
                if let (Value::Object(fo), Ok(key)) = (f, self.to_property_key(&k)) {
                    // Only functions without an own name (anonymous definitions).
                    let has_name = matches!(
                        self.heap.obj(fo).props.get(&PropKey::named("name")).map(|p| &p.slot),
                        Some(heap::Slot::Data(Value::String(s))) if !s.is_empty()
                    );
                    if !has_name {
                        self.set_function_name(fo, &key, None);
                    }
                }
            }
            Op::Arg(i) => {
                let v = self.top().args.get(i as usize).cloned().unwrap_or_default();
                self.push(v);
            }
            Op::RestArgs(i) => {
                let rest: Vec<Value> = self.top().args.iter().skip(i as usize).cloned().collect();
                let a = self.new_array(rest);
                self.push(Value::Object(a));
            }
            Op::CreateArguments => {
                let a = self.create_arguments()?;
                self.push(Value::Object(a));
            }
            Op::TemplateObject(i) => {
                let o = self.template_object(i);
                self.push(Value::Object(o));
            }
            Op::RegExp(i) => {
                let (src, flags) = self.top().code.regexps[i as usize].clone();
                let r = self.regexp_create(src, flags)?;
                self.push(Value::Object(r));
            }
            Op::Add => {
                let b = self.pop();
                let a = self.pop();
                let v = self.add(&a, &b)?;
                self.push(v);
            }
            Op::Sub
            | Op::Mul
            | Op::Div
            | Op::Mod
            | Op::Exp
            | Op::Shl
            | Op::Shr
            | Op::UShr
            | Op::BitAnd
            | Op::BitOr
            | Op::BitXor => {
                let b = self.pop();
                let a = self.pop();
                let v = self.arith(op, &a, &b)?;
                self.push(v);
            }
            Op::Lt | Op::Gt | Op::Le | Op::Ge => {
                let b = self.pop();
                let a = self.pop();
                let v = self.compare(op, &a, &b)?;
                self.push(Value::Bool(v));
            }
            Op::Eq | Op::Ne => {
                let b = self.pop();
                let a = self.pop();
                let r = self.loosely_equal(&a, &b)?;
                self.push(Value::Bool(r == (op == Op::Eq)));
            }
            Op::StrictEq | Op::StrictNe => {
                let b = self.pop();
                let a = self.pop();
                let r = value::strict_equals(&a, &b);
                self.push(Value::Bool(r == (op == Op::StrictEq)));
            }
            Op::InstanceOf => {
                let b = self.pop();
                let a = self.pop();
                let r = self.instance_of(&a, &b)?;
                self.push(Value::Bool(r));
            }
            Op::In => {
                let b = self.pop();
                let a = self.pop();
                let Value::Object(o) = b else {
                    return self.type_error("Cannot use 'in' operator to search for a key in a non-object");
                };
                let k = self.to_property_key(&a)?;
                let r = self.has_property(o, &k)?;
                self.push(Value::Bool(r));
            }
            Op::Neg => {
                let a = self.pop();
                let n = self.to_number(&a)?;
                self.push(Value::Number(-n));
            }
            Op::Plus | Op::ToNumeric => {
                let a = self.pop();
                let n = self.to_number(&a)?;
                self.push(Value::Number(n));
            }
            Op::Not => {
                let a = self.pop();
                self.push(Value::Bool(!a.truthy()));
            }
            Op::BitNot => {
                let a = self.pop();
                let n = self.to_int32(&a)?;
                self.push(Value::Number(!n as f64));
            }
            Op::TypeOf => {
                let a = self.pop();
                let t = self.type_of(&a);
                self.push(Value::String(t));
            }
            Op::Inc | Op::Dec => {
                let a = self.pop();
                let n = self.to_number(&a)?;
                self.push(Value::Number(if op == Op::Inc { n + 1.0 } else { n - 1.0 }));
            }
            Op::ToKey => {
                let a = self.pop();
                let k = self.to_property_key(&a)?;
                self.push(k.to_value());
            }
            Op::ToStr => {
                let a = self.pop();
                let s = self.to_string(&a)?;
                self.push(Value::String(s));
            }
            Op::Jump(t) => self.jump(t),
            Op::JumpIfFalse(t) => {
                if !self.pop().truthy() {
                    self.jump(t);
                }
            }
            Op::JumpIfTrue(t) => {
                if self.pop().truthy() {
                    self.jump(t);
                }
            }
            Op::JumpIfFalseKeep(t) => {
                if !self.peek(0).truthy() {
                    self.jump(t);
                } else {
                    self.pop();
                }
            }
            Op::JumpIfTrueKeep(t) => {
                if self.peek(0).truthy() {
                    self.jump(t);
                } else {
                    self.pop();
                }
            }
            Op::JumpIfNotNullishKeep(t) => {
                if !self.peek(0).is_nullish() {
                    self.jump(t);
                } else {
                    self.pop();
                }
            }
            Op::JumpIfNotUndef(t) => {
                if !self.peek(0).is_undefined() {
                    self.jump(t);
                }
            }
            Op::OptCheck(t, extra) => {
                if self.peek(0).is_nullish() {
                    self.pop_n(extra as usize + 1);
                    self.push(Value::Undefined);
                    self.jump(t);
                }
            }
            Op::Throw => {
                let v = self.pop();
                return Err(v);
            }
            Op::SetRet => {
                let v = self.pop();
                self.top().ret = v;
            }
            Op::GetRet => {
                let v = std::mem::take(&mut self.top().ret);
                self.push(v);
            }
            Op::Ret => {
                let v = self.pop();
                // Script and eval code return their completion value.
                let v = if self.top().code.decls_kind_unit() {
                    std::mem::take(&mut self.top().completion)
                } else {
                    v
                };
                return self.do_return(v, base);
            }
            Op::TryStart(t) => {
                let f = self.top();
                let h = Handler { pc: t, stack: f.stack.len(), env: f.env };
                f.handlers.push(h);
            }
            Op::TryEnd => {
                self.top().handlers.pop();
            }
            Op::ForInStart => {
                let v = self.pop();
                let object = if v.is_nullish() { None } else { Some(self.to_object(&v)?) };
                let st =
                    ForIn { object, visited: Default::default(), queue: Default::default(), started: false };
                let it = self.heap.alloc(Object::new(None, Kind::ForIn(Box::new(st))));
                self.push(Value::Object(it));
            }
            Op::ForInNext(t) => {
                let Value::Object(it) = self.peek(0) else { return Ok(None) };
                match self.for_in_next(it)? {
                    Some(k) => self.push(k),
                    None => {
                        self.pop();
                        self.jump(t);
                    }
                }
            }
            Op::GetIter => {
                let v = self.pop();
                let r = self.get_iterator(&v)?;
                self.push(Value::Object(r));
            }
            Op::IterNext(t) => {
                let Value::Object(r) = self.peek(0) else { return Ok(None) };
                match self.iter_step(r)? {
                    Some(v) => self.push(v),
                    None => {
                        self.pop();
                        self.jump(t);
                    }
                }
            }
            Op::IterValue => {
                let Value::Object(r) = self.peek(0) else { return Ok(None) };
                let v = if self.iter_done(r) { None } else { self.iter_step(r)? };
                self.push(v.unwrap_or_default());
            }
            Op::IterRest => {
                let Value::Object(r) = self.peek(0) else { return Ok(None) };
                let mut vals = Vec::new();
                while !self.iter_done(r) {
                    match self.iter_step(r)? {
                        Some(v) => vals.push(v),
                        None => break,
                    }
                }
                let a = self.new_array(vals);
                self.push(Value::Object(a));
            }
            Op::IterClose => {
                let Value::Object(r) = self.pop() else { return Ok(None) };
                if !self.iter_done(r) {
                    self.iterator_close(r)?;
                }
            }
            Op::IterCloseThrow => {
                let exc = self.pop();
                if let Value::Object(r) = self.pop() {
                    if !self.iter_done(r) {
                        let _ = self.iterator_close(r);
                    }
                }
                return Err(exc);
            }
            Op::SetCompletion => {
                let v = self.pop();
                self.top().completion = v;
            }
            Op::Unsupported(i) => {
                let s = to_rust(&self.top().code.strings[i as usize]);
                if let Some(m) = s.strip_prefix("ReferenceError:") {
                    return self.reference_error(m);
                }
                return self.syntax_error(&s);
            }
            Op::NewPrivateName(i) => {
                let d = to_rust(&self.top().code.strings[i as usize]);
                self.push(Value::Symbol(value::Sym::private(&d)));
            }
            Op::ClassCreate(t, h) => self.class_create(t, h)?,
            Op::ClassMethod(t, k, fl) => self.class_method(t, k, fl)?,
            Op::ClassField(t, st) => self.class_field(t, st)?,
            Op::ClassStaticBlock(t) => self.class_static_block(t)?,
            Op::ClassFinish => self.class_finish()?,
            Op::InitInstance => {
                let env = self.top().env;
                let this = self.resolve_this(env)?;
                if let (Value::Object(o), Some(f)) = (this, self.top().callee) {
                    self.init_instance(o, f)?;
                }
            }
            Op::SetFnNameField => {
                if let (Value::Object(f), Some(k)) = (self.peek(0), self.current_field_name()) {
                    self.set_function_name(f, &k, None);
                }
            }
            Op::SuperCall(n) => {
                let args = self.pop_n(n as usize);
                let r = self.super_call(args)?;
                self.push(r);
            }
            Op::SuperCallSpread => {
                let arr = self.pop();
                let args = self.array_values(&arr)?;
                let r = self.super_call(args)?;
                self.push(r);
            }
            Op::SuperCallForward => {
                let args = self.top().args.clone();
                let r = self.super_call(args)?;
                self.push(r);
            }
            Op::SuperGet => {
                let k = self.pop();
                let this = self.pop();
                let v = self.super_get(this, &k)?;
                self.push(v);
            }
            Op::SuperSet => {
                let v = self.pop();
                let k = self.pop();
                let this = self.pop();
                self.super_set(this, &k, v.clone())?;
                self.push(v);
            }
            Op::PrivateGet => {
                let n = self.pop();
                let o = self.pop();
                let v = self.private_get(&o, &n)?;
                self.push(v);
            }
            Op::PrivateSet => {
                let v = self.pop();
                let n = self.pop();
                let o = self.pop();
                self.private_set(&o, &n, v.clone())?;
                self.push(v);
            }
            Op::PrivateIn => {
                let o = self.pop();
                let n = self.pop();
                let r = self.private_in(&n, &o)?;
                self.push(Value::Bool(r));
            }
            Op::Nop => {}
        }
        Ok(None)
    }

    fn op_call(&mut self, f: Value, this: Value, args: Vec<Value>) -> JsResult<Option<Value>> {
        let Value::Object(fo) = f else {
            return self.type_error(&format!("{} is not a function", self.describe_short(&f)));
        };
        if !self.heap.obj(fo).is_callable() {
            return self.type_error("not a function");
        }
        if let Some(v) = self.begin_call(fo, this, args, None)? {
            self.push(v);
        } else {
            self.maybe_gc();
        }
        Ok(None)
    }

    fn op_new(&mut self, c: Value, args: Vec<Value>) -> JsResult<Option<Value>> {
        if !self.is_constructor(&c) {
            return self.type_error(&format!("{} is not a constructor", self.describe_short(&c)));
        }
        let Value::Object(co) = c else { unreachable!("constructor is an object") };
        if let Some(v) = self.begin_call(co, Value::Undefined, args, Some(co))? {
            self.push(v);
        }
        Ok(None)
    }

    /// A short description of a value for error messages (no user code runs).
    pub fn describe_short(&self, v: &Value) -> String {
        match v {
            Value::Undefined => "undefined".into(),
            Value::Null => "null".into(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => value::number_to_string(*n),
            Value::String(s) => format!("\"{}\"", to_rust(s)),
            Value::Symbol(_) => "a symbol".into(),
            Value::Object(o) => {
                if self.heap.obj(*o).is_callable() {
                    "function".into()
                } else {
                    "object".into()
                }
            }
        }
    }

    /// `eval(...)` written as a call: a direct eval when it is %eval% (§13.3.6.1).
    fn op_call_eval(&mut self, f: Value, this: Value, args: Vec<Value>) -> JsResult<Option<Value>> {
        let is_eval = matches!(&f, Value::Object(o) if *o == self.realm.eval);
        if !is_eval {
            return self.op_call(f, this, args);
        }
        let Some(Value::String(src)) = args.first().cloned() else {
            self.push(args.into_iter().next().unwrap_or_default());
            return Ok(None);
        };
        let strict = self.strict();
        let env = self.top().env;
        let var_env = self.top().var_env;
        let ctx = self.eval_context(env, strict);
        let src = to_rust(&src);
        let prog = match crate::parser::parse_eval(&src, ctx) {
            Ok(p) => p,
            Err(e) => return self.syntax_error(&e.msg),
        };
        let source: Rc<[char]> = src.chars().collect::<Vec<_>>().into();
        let code = Rc::new(Compiler::compile_unit(Unit::Eval(&prog), source));
        let lex = self.heap.alloc_env(Env {
            outer: Some(env),
            kind: EnvKind::Declarative,
            bindings: HashMap::new(),
        });
        let var = if prog.strict { lex } else { var_env };
        if self.frames.len() >= MAX_FRAMES {
            return self.range_error("Maximum call stack size exceeded");
        }
        let frame = self.frame(code, lex, var);
        self.frames.push(frame);
        Ok(None)
    }

    /// Indirect eval (§19.2.1.1 PerformEval with direct = false).
    pub fn indirect_eval(&mut self, x: Value) -> JsResult<Value> {
        let Value::String(src) = x else { return Ok(x) };
        let src = to_rust(&src);
        let prog = match crate::parser::parse_eval(&src, Default::default()) {
            Ok(p) => p,
            Err(e) => return self.syntax_error(&e.msg),
        };
        let source: Rc<[char]> = src.chars().collect::<Vec<_>>().into();
        let code = Rc::new(Compiler::compile_unit(Unit::Eval(&prog), source));
        let genv = self.realm.global_env;
        let lex = self.heap.alloc_env(Env {
            outer: Some(genv),
            kind: EnvKind::Declarative,
            bindings: HashMap::new(),
        });
        let var = if prog.strict { lex } else { genv };
        let frame = self.frame(code, lex, var);
        self.execute(frame)
    }

    /// Make a function object from a template (§10.2.3 OrdinaryFunctionCreate, §10.2.5
    /// MakeConstructor, §10.2.9 SetFunctionName, §10.2.10 SetFunctionLength).
    pub fn make_function(
        &mut self,
        tmpl: Rc<FuncTemplate>,
        env: EnvRef,
        home: Option<ObjRef>,
        may_construct: bool,
    ) -> ObjRef {
        let f = &tmpl.ast;
        let this_mode = if f.kind == crate::ast::FuncKind::Arrow {
            ThisMode::Lexical
        } else if f.strict {
            ThisMode::Strict
        } else {
            ThisMode::Global
        };
        let constructor =
            may_construct && f.kind == crate::ast::FuncKind::Normal && !f.is_generator && !f.is_async;
        let length = tmpl.length;
        let name: JsString = match (&f.name, &tmpl.given_name) {
            (Some(n), _) => js_str(n),
            (None, Some(g)) => js_str(g),
            _ => js_str(""),
        };
        let proto = self.realm.function_proto;
        let closure = Closure {
            tmpl,
            env,
            home,
            this_mode,
            constructor,
            derived: false,
            class_ctor: false,
            fields: Vec::new(),
            private_methods: Vec::new(),
            static_elems: Vec::new(),
            field_name: None,
        };
        let fo = self.heap.alloc(Object::new(Some(proto), Kind::Function(Box::new(closure))));
        let o = self.heap.obj_mut(fo);
        o.props
            .insert(PropKey::named("length"), Prop::data(Value::Number(length as f64), false, false, true));
        o.props.insert(PropKey::named("name"), Prop::data(Value::String(name), false, false, true));
        if constructor {
            let p = self.new_object();
            self.heap.obj_mut(p).props.insert(PropKey::named("constructor"), Prop::method(Value::Object(fo)));
            self.heap
                .obj_mut(fo)
                .props
                .insert(PropKey::named("prototype"), Prop::data(Value::Object(p), true, false, false));
        }
        fo
    }

    /// SetFunctionName (§10.2.9).
    pub fn set_function_name(&mut self, f: ObjRef, key: &PropKey, prefix: Option<&str>) {
        let mut name = match key {
            PropKey::Sym(s) => match &s.0.description {
                Some(d) => {
                    let mut v: Vec<u16> = "[".encode_utf16().collect();
                    v.extend_from_slice(d);
                    v.push(b']' as u16);
                    v
                }
                None => Vec::new(),
            },
            k => match k.to_value() {
                Value::String(s) => s.to_vec(),
                _ => Vec::new(),
            },
        };
        if let Some(p) = prefix {
            let mut v: Vec<u16> = p.encode_utf16().collect();
            v.push(b' ' as u16);
            v.extend(name);
            name = v;
        }
        self.heap
            .obj_mut(f)
            .props
            .insert(PropKey::named("name"), Prop::data(Value::String(name.into()), false, false, true));
    }

    /// CreateUnmappedArgumentsObject (§10.4.4.6). Mapped arguments come later.
    fn create_arguments(&mut self) -> JsResult<ObjRef> {
        let args = self.top().args.clone();
        let strict = self.strict();
        let callee = self.top().callee;
        let proto = self.realm.object_proto;
        let a = self.heap.alloc(Object::new(Some(proto), Kind::Arguments));
        let n = args.len();
        let values_fn = self.realm.array_proto_values;
        let iter_key = PropKey::Sym(self.realm.sym_iterator.clone());
        let thrower = self.realm.throw_type_error;
        let o = self.heap.obj_mut(a);
        o.props.insert(PropKey::named("length"), Prop::method(Value::Number(n as f64)));
        for (i, v) in args.into_iter().enumerate() {
            o.props.insert(PropKey::from_u32(i as u32), Prop::data(v, true, true, true));
        }
        o.props.insert(iter_key, Prop::method(Value::Object(values_fn)));
        if strict {
            o.props.insert(
                PropKey::named("callee"),
                Prop {
                    slot: Slot::Accessor(Some(thrower), Some(thrower)),
                    writable: false,
                    enumerable: false,
                    configurable: false,
                },
            );
        } else if let Some(c) = callee {
            o.props.insert(PropKey::named("callee"), Prop::method(Value::Object(c)));
        }
        Ok(a)
    }

    /// GetTemplateObject (§13.2.8.4), cached per site.
    fn template_object(&mut self, i: u32) -> ObjRef {
        let code = self.top().code.clone();
        let key = (Rc::as_ptr(&code) as usize, i);
        if let Some(&o) = self.templates.get(&key) {
            return o;
        }
        let (cooked, raw) = &code.templates[i as usize];
        let raw_vals: Vec<Value> = raw.iter().map(|s| Value::String(s.clone())).collect();
        let cooked_vals: Vec<Value> =
            cooked.iter().map(|c| c.clone().map(Value::String).unwrap_or_default()).collect();
        let raw_arr = self.new_array(raw_vals);
        let t = self.new_array(cooked_vals);
        self.heap
            .obj_mut(t)
            .props
            .insert(PropKey::named("raw"), Prop::data(Value::Object(raw_arr), false, false, false));
        let _ = self.set_integrity(raw_arr, true);
        let _ = self.set_integrity(t, true);
        self.templates.insert(key, t);
        t
    }
}

impl Code {
    /// Script and eval code (they instantiate declarations through GlobalInit/EvalInit).
    fn decls_kind_unit(&self) -> bool {
        matches!(self.ops.first(), Some(Op::GlobalInit | Op::EvalInit))
    }
}

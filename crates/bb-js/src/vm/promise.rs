//! Promises (§27.2), the job queue (§9.5) and async functions (§27.7): an async function's
//! frame is parked at each `await` and resumed by the promise reaction jobs.

use super::heap::*;
use super::realm::{self, accessor, method, native_with};
use super::value::{PropKey, Value};
use super::{ErrorKind, JsResult, Resume, Vm};

fn slots(vm: &Vm, f: ObjRef) -> Vec<Value> {
    match &vm.heap.obj(f).kind {
        Kind::Native(n) => n.slots.clone(),
        _ => Vec::new(),
    }
}

fn set_slot(vm: &mut Vm, f: ObjRef, i: usize, v: Value) {
    if let Kind::Native(n) = &mut vm.heap.obj_mut(f).kind {
        if let Some(s) = n.slots.get_mut(i) {
            *s = v;
        }
    }
}

/// A mutable boolean cell shared by natives (alreadyResolved, alreadyCalled).
fn new_flag(vm: &mut Vm) -> Value {
    Value::Object(vm.heap.alloc(Object::new(None, Kind::Boolean(false))))
}

fn test_and_set(vm: &mut Vm, flag: &Value) -> bool {
    let Value::Object(o) = flag else { return true };
    match &mut vm.heap.obj_mut(*o).kind {
        Kind::Boolean(b) => std::mem::replace(b, true),
        _ => true,
    }
}

impl Vm {
    pub fn is_promise(&self, v: &Value) -> bool {
        matches!(v, Value::Object(o) if matches!(self.heap.obj(*o).kind, Kind::Promise(_)))
    }

    fn promise_data(&mut self, p: ObjRef) -> Option<&mut PromiseData> {
        match &mut self.heap.obj_mut(p).kind {
            Kind::Promise(d) => Some(d),
            _ => None,
        }
    }

    /// A new pending promise with the given prototype.
    pub fn new_promise(&mut self, proto: Option<ObjRef>) -> ObjRef {
        let proto = proto.unwrap_or(self.realm.promise_proto);
        self.heap.alloc(Object::new(
            Some(proto),
            Kind::Promise(Box::new(PromiseData {
                state: PromiseState::Pending,
                result: Value::Undefined,
                fulfill_reactions: Vec::new(),
                reject_reactions: Vec::new(),
                handled: false,
            })),
        ))
    }

    /// CreateResolvingFunctions (§27.2.1.3).
    pub fn create_resolving_functions(&mut self, p: ObjRef) -> (Value, Value) {
        let flag = new_flag(self);
        let res = native_with(self, "", 1, resolve_fn, false, vec![Value::Object(p), flag.clone()]);
        let rej = native_with(self, "", 1, reject_fn, false, vec![Value::Object(p), flag]);
        (Value::Object(res), Value::Object(rej))
    }

    /// The body of a promise resolve function (§27.2.1.3.2 steps 7–16).
    fn resolve_promise(&mut self, p: ObjRef, resolution: Value) -> JsResult<()> {
        if let Value::Object(r) = &resolution {
            if *r == p {
                let e = self.error(ErrorKind::Type, "Chaining cycle detected for promise");
                self.reject_promise(p, e);
                return Ok(());
            }
            let then = match self.get(*r, &PropKey::lit("then")) {
                Ok(t) => t,
                Err(e) => {
                    if self.terminated {
                        return Err(e);
                    }
                    self.reject_promise(p, e);
                    return Ok(());
                }
            };
            if self.is_callable(&then) {
                self.jobs.push_back(Job::Thenable { promise: p, thenable: resolution, then });
                return Ok(());
            }
        }
        self.settle(p, resolution, true);
        Ok(())
    }

    pub fn reject_promise(&mut self, p: ObjRef, reason: Value) {
        self.settle(p, reason, false);
    }

    /// FulfillPromise / RejectPromise + TriggerPromiseReactions (§27.2.1.4, .7, .8).
    fn settle(&mut self, p: ObjRef, v: Value, fulfill: bool) {
        let Some(d) = self.promise_data(p) else { return };
        if d.state != PromiseState::Pending {
            return;
        }
        let reactions = if fulfill {
            std::mem::take(&mut d.fulfill_reactions)
        } else {
            std::mem::take(&mut d.reject_reactions)
        };
        d.fulfill_reactions.clear();
        d.reject_reactions.clear();
        d.state = if fulfill { PromiseState::Fulfilled } else { PromiseState::Rejected };
        d.result = v.clone();
        for r in reactions {
            self.jobs.push_back(Job::Reaction { reaction: r, argument: v.clone() });
        }
    }

    /// PerformPromiseThen (§27.2.5.4.1).
    pub fn perform_then(
        &mut self,
        p: ObjRef,
        on_ful: Value,
        on_rej: Value,
        cap: Option<(Value, Value, Value)>,
    ) {
        let on_ful = if self.is_callable(&on_ful) { on_ful } else { Value::Undefined };
        let on_rej = if self.is_callable(&on_rej) { on_rej } else { Value::Undefined };
        let fr = Reaction { capability: cap.clone(), fulfill: true, handler: on_ful };
        let rr = Reaction { capability: cap, fulfill: false, handler: on_rej };
        let Some(d) = self.promise_data(p) else { return };
        d.handled = true;
        match d.state {
            PromiseState::Pending => {
                d.fulfill_reactions.push(fr);
                d.reject_reactions.push(rr);
            }
            PromiseState::Fulfilled => {
                let v = d.result.clone();
                self.jobs.push_back(Job::Reaction { reaction: fr, argument: v });
            }
            PromiseState::Rejected => {
                let v = d.result.clone();
                self.jobs.push_back(Job::Reaction { reaction: rr, argument: v });
            }
        }
    }

    /// NewPromiseCapability (§27.2.1.5): (promise, resolve, reject).
    pub fn new_promise_capability(&mut self, c: &Value) -> JsResult<(Value, Value, Value)> {
        if !self.is_constructor(c) {
            return self.type_error("Promise capability constructor is not a constructor");
        }
        let co = c.as_object().expect("constructor");
        if co == self.realm.promise_ctor {
            // The common case, without the observable executor dance being any different.
        }
        let exec =
            native_with(self, "", 2, capability_executor, false, vec![Value::Undefined, Value::Undefined]);
        let p = self.construct(co, vec![Value::Object(exec)], None)?;
        let s = slots(self, exec);
        if !self.is_callable(&s[0]) || !self.is_callable(&s[1]) {
            return self.type_error("Promise resolve or reject function is not callable");
        }
        Ok((p, s[0].clone(), s[1].clone()))
    }

    /// PromiseResolve (§27.2.4.7.1).
    pub fn promise_resolve(&mut self, c: &Value, x: Value) -> JsResult<Value> {
        if self.is_promise(&x) {
            let xc = self.get_value_key(&x, &PropKey::lit("constructor"))?;
            if super::value::same_value(&xc, c) {
                return Ok(x);
            }
        }
        let (p, res, _) = self.new_promise_capability(c)?;
        self.call(&res, Value::Undefined, vec![x])?;
        Ok(p)
    }

    /// SpeciesConstructor (§7.3.22).
    pub fn species_constructor(&mut self, o: ObjRef, default: ObjRef) -> JsResult<Value> {
        let c = self.get(o, &PropKey::lit("constructor"))?;
        if c.is_undefined() {
            return Ok(Value::Object(default));
        }
        let Value::Object(co) = c else { return self.type_error("object.constructor is not an object") };
        let sk = PropKey::Sym(self.realm.sym_species.clone());
        let s = self.get(co, &sk)?;
        if s.is_nullish() {
            return Ok(Value::Object(default));
        }
        if self.is_constructor(&s) {
            return Ok(s);
        }
        self.type_error("object.constructor[Symbol.species] is not a constructor")
    }

    /// Run queued jobs until the queue is empty (a microtask checkpoint).
    pub fn run_jobs(&mut self) {
        loop {
            self.run_job_queue();
            // Atomics.waitAsync promises settle as their own agent's jobs.
            match self.poll_async_waits() {
                Ok(true) if !self.terminated => continue,
                _ => break,
            }
        }
    }

    fn run_job_queue(&mut self) {
        let mut n = 0u32;
        while let Some(job) = self.jobs.pop_front() {
            if self.terminated {
                self.jobs.clear();
                return;
            }
            // A queue that keeps refilling still lets waitAsync timeouts and notifications in.
            n = n.wrapping_add(1);
            if n % 64 == 0 && self.has_pending_waits() {
                let _ = self.poll_async_waits();
            }
            // The job is off the queue: keep what it holds alive while it runs (a GC in
            // the handler must not free its capability's functions).
            let base = self.roots.len();
            let mut held = Vec::new();
            job.trace(&mut held);
            self.roots.extend(held.into_iter().map(Value::Object));
            let r = match job {
                Job::Reaction { reaction, argument } => self.reaction_job(reaction, argument),
                Job::Thenable { promise, thenable, then } => {
                    let (res, rej) = self.create_resolving_functions(promise);
                    match self.call(&then, thenable, vec![res, rej.clone()]) {
                        Ok(_) => Ok(()),
                        Err(e) => self.call(&rej, Value::Undefined, vec![e]).map(|_| ()),
                    }
                }
            };
            self.roots.truncate(base);
            if let Err(e) = r {
                // HostReportErrors: uncaught errors in jobs go to the host's output.
                let d = self.describe(&e);
                self.output.push(format!("Uncaught (in promise) {d}"));
            }
        }
    }

    /// NewPromiseReactionJob (§27.2.2.1).
    fn reaction_job(&mut self, r: Reaction, arg: Value) -> JsResult<()> {
        let result = if r.handler.is_undefined() {
            if r.fulfill {
                Ok(arg)
            } else {
                Err(arg)
            }
        } else {
            self.call(&r.handler, Value::Undefined, vec![arg])
        };
        if self.terminated {
            return result.map(|_| ());
        }
        let Some((_, res, rej)) = r.capability else { return result.map(|_| ()) };
        match result {
            Ok(v) => self.call(&res, Value::Undefined, vec![v]).map(|_| ()),
            Err(e) => self.call(&rej, Value::Undefined, vec![e]).map(|_| ()),
        }
    }

    // ---- async functions ----

    /// AsyncFunctionStart: run the frame (as the base of a nested run) until it awaits,
    /// returns or throws; `[] → [promise]` for the caller.
    pub(super) fn async_start(&mut self, base: usize) -> JsResult<Option<Value>> {
        let mut frame = self.frames.pop().expect("async frame");
        let p = self.new_promise(None);
        let (res, rej) = self.create_resolving_functions(p);
        let st = self.heap.alloc(Object::new(
            None,
            Kind::Async(Box::new(AsyncFn { state: GenState::Executing, resolve: res, reject: rej })),
        ));
        frame.generator = Some(st);
        self.async_run(st, frame)?;
        if self.frames.len() <= base {
            return Ok(Some(Value::Object(p)));
        }
        self.push(Value::Object(p));
        Ok(None)
    }

    fn async_run(&mut self, st: ObjRef, frame: super::Frame) -> JsResult<()> {
        let r = self.execute(frame);
        if self.terminated {
            return r.map(|_| ());
        }
        let (suspended, res, rej) = match &self.heap.obj(st).kind {
            Kind::Async(a) => {
                (matches!(a.state, GenState::SuspendedYield(_)), a.resolve.clone(), a.reject.clone())
            }
            _ => return Ok(()),
        };
        if suspended {
            return Ok(());
        }
        match r {
            Ok(v) => self.call(&res, Value::Undefined, vec![v])?,
            Err(e) => self.call(&rej, Value::Undefined, vec![e])?,
        };
        Ok(())
    }

    /// Await (§27.7.5.3): `[v] →` park the frame until the promise settles.
    pub(super) fn await_value(&mut self, base: usize) -> JsResult<Option<Value>> {
        let v = self.pop();
        let pc = self.realm.promise_ctor;
        let promise = self.promise_resolve(&Value::Object(pc), v)?;
        let Some(st) = self.top().generator else {
            return self.syntax_error("await is only valid in async functions and modules");
        };
        let on_ful = native_with(self, "", 1, await_fulfilled, false, vec![Value::Object(st)]);
        let on_rej = native_with(self, "", 1, await_rejected, false, vec![Value::Object(st)]);
        if let Value::Object(po) = promise {
            self.perform_then(po, Value::Object(on_ful), Value::Object(on_rej), None);
        }
        self.yield_raw = false;
        self.suspend(Value::Undefined, base, false)
    }

    fn async_resume(&mut self, st: ObjRef, mode: Resume, v: Value) -> JsResult<Value> {
        let state = match &mut self.heap.obj_mut(st).kind {
            Kind::Async(a) => std::mem::replace(&mut a.state, GenState::Executing),
            Kind::AsyncGen(a) => {
                let Some(mut frame) = a.frame.take() else { return Ok(Value::Undefined) };
                frame.stack.push(v);
                frame.resume = mode;
                self.async_gen_run(st, *frame)?;
                return Ok(Value::Undefined);
            }
            _ => return Ok(Value::Undefined),
        };
        let GenState::SuspendedYield(mut frame) = state else { return Ok(Value::Undefined) };
        frame.stack.push(v);
        frame.resume = mode;
        self.async_run(st, *frame)?;
        Ok(Value::Undefined)
    }
}

fn resolve_fn(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    if test_and_set(vm, &s[1]) {
        return Ok(Value::Undefined);
    }
    if let Value::Object(p) = s[0] {
        vm.resolve_promise(p, c.arg(0))?;
    }
    Ok(Value::Undefined)
}

fn reject_fn(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    if test_and_set(vm, &s[1]) {
        return Ok(Value::Undefined);
    }
    if let Value::Object(p) = s[0] {
        vm.reject_promise(p, c.arg(0));
    }
    Ok(Value::Undefined)
}

/// GetCapabilitiesExecutor (§27.2.1.5 step 4).
fn capability_executor(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    if !s[0].is_undefined() || !s[1].is_undefined() {
        return vm.type_error("Promise executor has already been invoked with non-undefined arguments");
    }
    set_slot(vm, c.callee, 0, c.arg(0));
    set_slot(vm, c.callee, 1, c.arg(1));
    Ok(Value::Undefined)
}

fn await_fulfilled(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let Value::Object(st) = slots(vm, c.callee)[0] else { return Ok(Value::Undefined) };
    vm.async_resume(st, Resume::Next, c.arg(0))
}

fn await_rejected(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let Value::Object(st) = slots(vm, c.callee)[0] else { return Ok(Value::Undefined) };
    vm.async_resume(st, Resume::Throw, c.arg(0))
}

fn this_promise(vm: &mut Vm, v: &Value) -> JsResult<ObjRef> {
    match v {
        Value::Object(o) if matches!(vm.heap.obj(*o).kind, Kind::Promise(_)) => Ok(*o),
        _ => vm.type_error("Receiver is not a Promise"),
    }
}

// ---- the Promise constructor and prototype (§27.2.3–5) ----

fn promise_ctor(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let Some(nt) = c.new_target else {
        return vm.type_error("Promise constructor cannot be invoked without 'new'");
    };
    let exec = c.arg(0);
    if !vm.is_callable(&exec) {
        return vm.type_error("Promise resolver is not a function");
    }
    let proto = vm.prototype_from_ctor(nt, |r| r.promise_proto)?;
    let p = vm.new_promise(Some(proto));
    let (res, rej) = vm.create_resolving_functions(p);
    if let Err(e) = vm.call(&exec, Value::Undefined, vec![res, rej.clone()]) {
        if vm.terminated {
            return Err(e);
        }
        vm.call(&rej, Value::Undefined, vec![e])?;
    }
    Ok(Value::Object(p))
}

fn then(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let p = this_promise(vm, &c.this)?;
    let pc = vm.realm.promise_ctor;
    let ctor = vm.species_constructor(p, pc)?;
    let cap = vm.new_promise_capability(&ctor)?;
    let result = cap.0.clone();
    vm.perform_then(p, c.arg(0), c.arg(1), Some(cap));
    Ok(result)
}

fn invoke(vm: &mut Vm, target: &Value, name: &str, args: Vec<Value>) -> JsResult<Value> {
    let f = vm.get_value_key(target, &PropKey::named(name))?;
    vm.call(&f, target.clone(), args)
}

/// Promise.prototype.finally (§27.2.5.3).
fn finally(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let Value::Object(p) = c.this else {
        return vm.type_error("Promise.prototype.finally called on a non-object");
    };
    let pc = vm.realm.promise_ctor;
    let ctor = vm.species_constructor(p, pc)?;
    let on_finally = c.arg(0);
    let (a, b) = if !vm.is_callable(&on_finally) {
        (on_finally.clone(), on_finally)
    } else {
        let tf = native_with(vm, "", 1, then_finally, false, vec![on_finally.clone(), ctor.clone()]);
        let cf = native_with(vm, "", 1, catch_finally, false, vec![on_finally, ctor]);
        (Value::Object(tf), Value::Object(cf))
    };
    invoke(vm, &c.this, "then", vec![a, b])
}

fn then_finally(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    let r = vm.call(&s[0], Value::Undefined, Vec::new())?;
    let p = vm.promise_resolve(&s[1], r)?;
    let thunk = native_with(vm, "", 0, |vm, c| Ok(slots(vm, c.callee)[0].clone()), false, vec![c.arg(0)]);
    invoke(vm, &p, "then", vec![Value::Object(thunk)])
}

fn catch_finally(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    let r = vm.call(&s[0], Value::Undefined, Vec::new())?;
    let p = vm.promise_resolve(&s[1], r)?;
    let thrower = native_with(vm, "", 0, |vm, c| Err(slots(vm, c.callee)[0].clone()), false, vec![c.arg(0)]);
    invoke(vm, &p, "then", vec![Value::Object(thrower)])
}

#[derive(Clone, Copy, PartialEq)]
enum Combinator {
    All,
    AllSettled,
    Any,
    Race,
    AllKeyed,
    AllSettledKeyed,
}

/// Promise.all / allSettled / any / race (§27.2.4.1–5): the shared iteration.
fn combinator(vm: &mut Vm, c: NativeCall, kind: Combinator) -> JsResult<Value> {
    let ctor = c.this.clone();
    let (p, res, rej) = vm.new_promise_capability(&ctor)?;
    let resolve_fn = match vm.get_value_key(&ctor, &PropKey::lit("resolve")) {
        Ok(f) if vm.is_callable(&f) => f,
        Ok(_) => {
            let e = vm.error(ErrorKind::Type, "Promise resolve is not a function");
            vm.call(&rej, Value::Undefined, vec![e])?;
            return Ok(p);
        }
        Err(e) => {
            if vm.terminated {
                return Err(e);
            }
            vm.call(&rej, Value::Undefined, vec![e])?;
            return Ok(p);
        }
    };
    let rec = match vm.get_iterator(&c.arg(0)) {
        Ok(r) => r,
        Err(e) => {
            if vm.terminated {
                return Err(e);
            }
            vm.call(&rej, Value::Undefined, vec![e])?;
            return Ok(p);
        }
    };
    let values = Value::Object(vm.new_array(Vec::new()));
    // remainingElementsCount (a shared counter cell).
    let remaining = Value::Object(vm.heap.alloc(Object::new(None, Kind::Number(1.0))));
    let r = (|| -> JsResult<()> {
        let mut index = 0u32;
        loop {
            let next = vm.iter_step(rec)?;
            let Some(next) = next else {
                if kind != Combinator::Race && dec(vm, &remaining) == 0.0 {
                    finish(vm, kind, &values, &res, &rej)?;
                }
                return Ok(());
            };
            if kind != Combinator::Race {
                vm.set_or_throw(
                    values.as_object().expect("array"),
                    PropKey::from_u32(index),
                    Value::Undefined,
                )?;
            }
            let np = vm.call(&resolve_fn, ctor.clone(), vec![next])?;
            let (on_ful, on_rej) = match kind {
                Combinator::Race => (res.clone(), rej.clone()),
                _ => {
                    inc(vm, &remaining);
                    let flag = new_flag(vm);
                    let el = |vm: &mut Vm, f: NativeFn| {
                        Value::Object(native_with(
                            vm,
                            "",
                            1,
                            f,
                            false,
                            vec![
                                flag.clone(),
                                Value::Number(index as f64),
                                values.clone(),
                                res.clone(),
                                rej.clone(),
                                remaining.clone(),
                                Value::Number(kind as u8 as f64),
                            ],
                        ))
                    };
                    match kind {
                        Combinator::All => (el(vm, element_fulfilled), rej.clone()),
                        Combinator::AllSettled => (el(vm, element_fulfilled), el(vm, element_rejected)),
                        _ => (res.clone(), el(vm, element_rejected)),
                    }
                }
            };
            invoke(vm, &np, "then", vec![on_ful, on_rej])?;
            index += 1;
        }
    })();
    if let Err(e) = r {
        if vm.terminated {
            return Err(e);
        }
        if !vm.iter_done(rec) {
            let _ = vm.iterator_close(rec);
        }
        vm.call(&rej, Value::Undefined, vec![e])?;
    }
    Ok(p)
}

fn counter(vm: &mut Vm, v: &Value, delta: f64) -> f64 {
    let Value::Object(o) = v else { return 0.0 };
    match &mut vm.heap.obj_mut(*o).kind {
        Kind::Number(n) => {
            *n += delta;
            *n
        }
        _ => 0.0,
    }
}
fn dec(vm: &mut Vm, v: &Value) -> f64 {
    counter(vm, v, -1.0)
}
fn inc(vm: &mut Vm, v: &Value) {
    counter(vm, v, 1.0);
}

fn finish(vm: &mut Vm, kind: Combinator, values: &Value, res: &Value, rej: &Value) -> JsResult<()> {
    if kind == Combinator::Any {
        let errors = vm.list_from_array_like(values)?;
        let e = vm.aggregate_error(errors, "All promises were rejected");
        vm.call(rej, Value::Undefined, vec![e])?;
    } else {
        vm.call(res, Value::Undefined, vec![values.clone()])?;
    }
    Ok(())
}

/// The element functions of all/allSettled/any.
fn element(vm: &mut Vm, c: NativeCall, fulfilled: bool) -> JsResult<Value> {
    let s = slots(vm, c.callee);
    if test_and_set(vm, &s[0]) {
        return Ok(Value::Undefined);
    }
    let kind = match s[6] {
        Value::Number(1.0) => Combinator::AllSettled,
        Value::Number(2.0) => Combinator::Any,
        Value::Number(4.0) => Combinator::AllKeyed,
        Value::Number(5.0) => Combinator::AllSettledKeyed,
        _ => Combinator::All,
    };
    let idx = match s[1] {
        Value::Number(n) => n as u32,
        _ => 0,
    };
    let v = if matches!(kind, Combinator::AllSettled | Combinator::AllSettledKeyed) {
        let o = vm.new_object();
        let st = if fulfilled { "fulfilled" } else { "rejected" };
        vm.create_data_property_or_throw(o, PropKey::lit("status"), Value::str(st))?;
        vm.create_data_property_or_throw(
            o,
            PropKey::named(if fulfilled { "value" } else { "reason" }),
            c.arg(0),
        )?;
        Value::Object(o)
    } else {
        c.arg(0)
    };
    if let Value::Object(a) = s[2] {
        vm.create_data_property_or_throw(a, PropKey::from_u32(idx), v)?;
    }
    if dec(vm, &s[5]) == 0.0 {
        match s.get(7) {
            Some(keys) => finish_keyed(vm, keys, &s[2], &s[3])?,
            None => finish(vm, kind, &s[2], &s[3], &s[4])?,
        }
    }
    Ok(Value::Undefined)
}

/// CreateKeyedPromiseCombinatorResultObject, then resolve.
fn finish_keyed(vm: &mut Vm, keys: &Value, values: &Value, res: &Value) -> JsResult<()> {
    let keys = vm.list_from_array_like(keys)?;
    let vals = vm.list_from_array_like(values)?;
    let o = vm.heap.alloc(Object::new(None, Kind::Ordinary));
    for (k, v) in keys.into_iter().zip(vals) {
        let k = vm.to_property_key(&k)?;
        vm.create_data_property_or_throw(o, k, v)?;
    }
    vm.call(res, Value::Undefined, vec![Value::Object(o)])?;
    Ok(())
}

/// Promise.allKeyed / Promise.allSettledKeyed (await-dictionary, PerformPromiseAllKeyed).
fn combinator_keyed(vm: &mut Vm, c: NativeCall, settled: bool) -> JsResult<Value> {
    let ctor = c.this.clone();
    let (p, res, rej) = vm.new_promise_capability(&ctor)?;
    let resolve_fn = match vm.get_value_key(&ctor, &PropKey::lit("resolve")) {
        Ok(f) if vm.is_callable(&f) => f,
        Ok(_) => {
            let e = vm.error(ErrorKind::Type, "Promise resolve is not a function");
            vm.call(&rej, Value::Undefined, vec![e])?;
            return Ok(p);
        }
        Err(e) if vm.terminated => return Err(e),
        Err(e) => {
            vm.call(&rej, Value::Undefined, vec![e])?;
            return Ok(p);
        }
    };
    let Value::Object(promises) = c.arg(0) else {
        let e = vm.error(ErrorKind::Type, "the argument must be an object");
        vm.call(&rej, Value::Undefined, vec![e])?;
        return Ok(p);
    };
    let kind = if settled { Combinator::AllSettledKeyed } else { Combinator::AllKeyed };
    let values = Value::Object(vm.new_array(Vec::new()));
    let keys = Value::Object(vm.new_array(Vec::new()));
    let remaining = Value::Object(vm.heap.alloc(Object::new(None, Kind::Number(1.0))));
    let r = (|| -> JsResult<()> {
        let mut index = 0u32;
        for key in vm.own_property_keys(promises)? {
            let Some(d) = vm.get_own_property(promises, &key)? else { continue };
            if d.enumerable != Some(true) {
                continue;
            }
            let v = vm.get(promises, &key)?;
            if let (Value::Object(ka), Value::Object(va)) = (&keys, &values) {
                vm.create_data_property_or_throw(*ka, PropKey::from_u32(index), key.to_value())?;
                vm.create_data_property_or_throw(*va, PropKey::from_u32(index), Value::Undefined)?;
            }
            let np = vm.call(&resolve_fn, ctor.clone(), vec![v])?;
            let flag = new_flag(vm);
            let el = |vm: &mut Vm, f: NativeFn| {
                Value::Object(native_with(
                    vm,
                    "",
                    1,
                    f,
                    false,
                    vec![
                        flag.clone(),
                        Value::Number(index as f64),
                        values.clone(),
                        res.clone(),
                        rej.clone(),
                        remaining.clone(),
                        Value::Number(kind as u8 as f64),
                        keys.clone(),
                    ],
                ))
            };
            let on_ful = el(vm, element_fulfilled);
            let on_rej = if settled { el(vm, element_rejected) } else { rej.clone() };
            inc(vm, &remaining);
            invoke(vm, &np, "then", vec![on_ful, on_rej])?;
            index += 1;
        }
        if dec(vm, &remaining) == 0.0 {
            finish_keyed(vm, &keys, &values, &res)?;
        }
        Ok(())
    })();
    if let Err(e) = r {
        if vm.terminated {
            return Err(e);
        }
        vm.call(&rej, Value::Undefined, vec![e])?;
    }
    Ok(p)
}

fn element_fulfilled(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    element(vm, c, true)
}
fn element_rejected(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    element(vm, c, false)
}

impl Vm {
    /// An AggregateError with `errors` (§20.5.7).
    pub fn aggregate_error(&mut self, errors: Vec<Value>, msg: &str) -> Value {
        let e = self.error(ErrorKind::Error, msg);
        if let Value::Object(o) = e {
            self.heap.obj_mut(o).proto = Some(self.realm.aggregate_error_proto);
            let arr = self.new_array(errors);
            self.heap.obj_mut(o).props.insert(PropKey::lit("errors"), Prop::method(Value::Object(arr)));
        }
        e
    }
}

pub(super) fn install(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let pp = vm.new_object_with_proto(Some(op));
    vm.realm.promise_proto = pp;
    let ctor = realm::constructor(vm, "Promise", 1, promise_ctor, pp);
    vm.realm.promise_ctor = ctor;
    let sp = vm.realm.sym_species.clone();
    accessor(vm, ctor, PropKey::Sym(sp), Some(|_, c| Ok(c.this)), None);
    realm::to_string_tag(vm, pp, "Promise");
    method(vm, pp, "then", 2, then);
    method(vm, pp, "catch", 1, |vm, c| invoke(vm, &c.this, "then", vec![Value::Undefined, c.arg(0)]));
    method(vm, pp, "finally", 1, finally);
    method(vm, ctor, "resolve", 1, |vm, c| {
        if !matches!(c.this, Value::Object(_)) {
            return vm.type_error("PromiseResolve called on non-object");
        }
        vm.promise_resolve(&c.this, c.arg(0))
    });
    method(vm, ctor, "reject", 1, |vm, c| {
        let (p, _, rej) = vm.new_promise_capability(&c.this)?;
        vm.call(&rej, Value::Undefined, vec![c.arg(0)])?;
        Ok(p)
    });
    method(vm, ctor, "withResolvers", 0, |vm, c| {
        let (p, res, rej) = vm.new_promise_capability(&c.this)?;
        let o = vm.new_object();
        vm.create_data_property_or_throw(o, PropKey::lit("promise"), p)?;
        vm.create_data_property_or_throw(o, PropKey::lit("resolve"), res)?;
        vm.create_data_property_or_throw(o, PropKey::lit("reject"), rej)?;
        Ok(Value::Object(o))
    });
    method(vm, ctor, "try", 1, |vm, c| {
        if !matches!(c.this, Value::Object(_)) {
            return vm.type_error("Promise.try called on non-object");
        }
        let (p, res, rej) = vm.new_promise_capability(&c.this)?;
        let args = c.args.iter().skip(1).cloned().collect();
        match vm.call(&c.arg(0), Value::Undefined, args) {
            Ok(v) => vm.call(&res, Value::Undefined, vec![v])?,
            Err(e) => {
                if vm.terminated {
                    return Err(e);
                }
                vm.call(&rej, Value::Undefined, vec![e])?
            }
        };
        Ok(p)
    });
    method(vm, ctor, "all", 1, |vm, c| combinator(vm, c, Combinator::All));
    method(vm, ctor, "allSettled", 1, |vm, c| combinator(vm, c, Combinator::AllSettled));
    method(vm, ctor, "any", 1, |vm, c| combinator(vm, c, Combinator::Any));
    method(vm, ctor, "race", 1, |vm, c| combinator(vm, c, Combinator::Race));
    method(vm, ctor, "allKeyed", 1, |vm, c| combinator_keyed(vm, c, false));
    method(vm, ctor, "allSettledKeyed", 1, |vm, c| combinator_keyed(vm, c, true));
}

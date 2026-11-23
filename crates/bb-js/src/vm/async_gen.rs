//! Async generators (§27.6), async iteration (`for await`, `yield*` in async generators) and
//! %AsyncFromSyncIteratorPrototype% (§27.1.6).

use super::heap::*;
use super::realm::{self, method, method_sym, native_with};
use super::value::{PropKey, Value};
use super::{ErrorKind, Frame, JsResult, Resume, Vm};

fn slot0(vm: &Vm, f: ObjRef) -> Value {
    match &vm.heap.obj(f).kind {
        Kind::Native(n) => n.slots.first().cloned().unwrap_or_default(),
        _ => Value::Undefined,
    }
}

impl Vm {
    pub(super) fn iter_parts(&self, r: ObjRef) -> (Value, Value) {
        match &self.heap.obj(r).kind {
            Kind::IterRecord(x) => (x.iterator.clone(), x.next.clone()),
            _ => (Value::Undefined, Value::Undefined),
        }
    }

    fn ag(&mut self, g: ObjRef) -> Option<&mut AsyncGen> {
        match &mut self.heap.obj_mut(g).kind {
            Kind::AsyncGen(a) => Some(a),
            _ => None,
        }
    }

    fn ag_phase(&self, g: ObjRef) -> Option<AgPhase> {
        match &self.heap.obj(g).kind {
            Kind::AsyncGen(a) => Some(a.phase),
            _ => None,
        }
    }

    /// Park the frame in a new async generator after FunctionDeclarationInstantiation.
    pub(super) fn async_gen_start(&mut self, base: usize) -> JsResult<Option<Value>> {
        let mut frame = self.frames.pop().expect("async generator frame");
        let proto = match frame.callee {
            Some(f) => match self.get(f, &PropKey::lit("prototype")) {
                Ok(Value::Object(p)) => p,
                Ok(_) => self.realm.async_generator_proto,
                Err(e) => {
                    self.frames.push(frame);
                    return Err(e);
                }
            },
            None => self.realm.async_generator_proto,
        };
        let g = self.heap.alloc(Object::new(
            Some(proto),
            Kind::AsyncGen(Box::new(AsyncGen {
                frame: None,
                phase: AgPhase::Start,
                queue: Default::default(),
            })),
        ));
        frame.generator = Some(g);
        if let Some(a) = self.ag(g) {
            a.frame = Some(Box::new(frame));
        }
        if self.frames.len() <= base {
            return Ok(Some(Value::Object(g)));
        }
        self.push(Value::Object(g));
        Ok(None)
    }

    /// AsyncGeneratorCompleteStep (§27.6.3.4): settle the front request.
    fn ag_complete_step(&mut self, g: ObjRef, r: Result<Value, Value>, done: bool) -> JsResult<()> {
        let Some(req) = self.ag(g).and_then(|a| a.queue.pop_front()) else { return Ok(()) };
        match r {
            Ok(v) => {
                let res = self.iter_result(v, done);
                self.call(&req.resolve, Value::Undefined, vec![res])?;
            }
            Err(e) => {
                self.call(&req.reject, Value::Undefined, vec![e])?;
            }
        }
        Ok(())
    }

    /// AsyncGeneratorYield (§27.6.3.8) after the operand was awaited.
    pub(super) fn async_yield(&mut self, base: usize) -> JsResult<Option<Value>> {
        let v = self.pop();
        let g = self.top().generator.expect("yield outside an async generator");
        self.ag_complete_step(g, Ok(v), false)?;
        let next = self.ag(g).and_then(|a| a.queue.front().map(|r| (r.mode, r.value.clone())));
        if let Some((mode, value)) = next {
            self.push(value);
            self.top().resume = mode;
            return Ok(None);
        }
        if let Some(a) = self.ag(g) {
            a.phase = AgPhase::Yield;
        }
        self.suspend(Value::Undefined, base, false)
    }

    /// Run (resume) the async generator's frame, then settle requests if it completed.
    pub(super) fn async_gen_run(&mut self, g: ObjRef, frame: Frame) -> JsResult<()> {
        if let Some(a) = self.ag(g) {
            a.phase = AgPhase::Executing;
        }
        let r = self.execute(frame);
        if self.terminated {
            return r.map(|_| ());
        }
        let (phase, parked) = match &self.heap.obj(g).kind {
            Kind::AsyncGen(a) => (a.phase, a.frame.is_some()),
            _ => return Ok(()),
        };
        if phase == AgPhase::Yield || (phase == AgPhase::Executing && parked) {
            return Ok(());
        }
        if let Some(a) = self.ag(g) {
            a.phase = AgPhase::Completed;
        }
        self.ag_complete_step(g, r, true)?;
        self.ag_resume_next(g)
    }

    /// Serve queued requests while the generator isn't running (§27.6.3.5–10).
    fn ag_resume_next(&mut self, g: ObjRef) -> JsResult<()> {
        loop {
            let Some(phase) = self.ag_phase(g) else { return Ok(()) };
            if matches!(phase, AgPhase::Executing | AgPhase::AwaitingReturn) {
                return Ok(());
            }
            let Some((mode, value)) =
                self.ag(g).and_then(|a| a.queue.front().map(|r| (r.mode, r.value.clone())))
            else {
                return Ok(());
            };
            let mut phase = phase;
            if mode != Resume::Next && phase == AgPhase::Start {
                phase = AgPhase::Completed;
                if let Some(a) = self.ag(g) {
                    a.phase = phase;
                    a.frame = None;
                }
            }
            if phase == AgPhase::Completed {
                match mode {
                    Resume::Next => self.ag_complete_step(g, Ok(Value::Undefined), true)?,
                    Resume::Throw => self.ag_complete_step(g, Err(value), true)?,
                    Resume::Return => {
                        // AsyncGeneratorAwaitReturn (§27.6.3.9).
                        if let Some(a) = self.ag(g) {
                            a.phase = AgPhase::AwaitingReturn;
                        }
                        let pc = Value::Object(self.realm.promise_ctor);
                        match self.promise_resolve(&pc, value) {
                            Ok(Value::Object(p)) => {
                                let f = native_with(
                                    self,
                                    "",
                                    1,
                                    ag_return_fulfilled,
                                    false,
                                    vec![Value::Object(g)],
                                );
                                let r = native_with(
                                    self,
                                    "",
                                    1,
                                    ag_return_rejected,
                                    false,
                                    vec![Value::Object(g)],
                                );
                                self.perform_then(p, Value::Object(f), Value::Object(r), None);
                                return Ok(());
                            }
                            Ok(_) => return Ok(()),
                            Err(e) => {
                                if self.terminated {
                                    return Err(e);
                                }
                                if let Some(a) = self.ag(g) {
                                    a.phase = AgPhase::Completed;
                                }
                                self.ag_complete_step(g, Err(e), true)?;
                            }
                        }
                    }
                }
                continue;
            }
            // Start or Yield: resume the frame with the request's completion.
            let Some(mut frame) = self.ag(g).and_then(|a| a.frame.take()) else { return Ok(()) };
            if phase == AgPhase::Yield {
                frame.stack.push(value);
                frame.resume = mode;
            }
            return self.async_gen_run(g, *frame);
        }
    }

    /// AsyncGenerator.prototype.next/return/throw (§27.6.1.2–4) via AsyncGeneratorEnqueue.
    fn ag_enqueue(&mut self, this: &Value, mode: Resume, value: Value) -> JsResult<Value> {
        let pc = Value::Object(self.realm.promise_ctor);
        let (p, res, rej) = self.new_promise_capability(&pc)?;
        let g = match this {
            Value::Object(o) if matches!(self.heap.obj(*o).kind, Kind::AsyncGen(_)) => *o,
            _ => {
                let e = self.error(ErrorKind::Type, "not an async generator");
                self.call(&rej, Value::Undefined, vec![e])?;
                return Ok(p);
            }
        };
        if let Some(a) = self.ag(g) {
            a.queue.push_back(AgRequest { mode, value, resolve: res, reject: rej });
        }
        self.ag_resume_next(g)?;
        Ok(p)
    }

    /// CreateAsyncFromSyncIterator (§27.1.6.1) on a sync (iterator, next) pair.
    pub(super) fn async_from_sync(&mut self, iterator: Value, next: Value) -> ObjRef {
        let proto = self.realm.async_from_sync_iterator_proto;
        let rec = IterRecord { iterator, next, done: false };
        self.heap.alloc(Object::new(Some(proto), Kind::IterRecord(Box::new(rec))))
    }

    /// GetIterator(obj, async) (§7.4.3).
    pub(super) fn get_async_iterator(&mut self, v: &Value) -> JsResult<ObjRef> {
        let ak = PropKey::Sym(self.realm.sym_async_iterator.clone());
        if let Some(m) = self.get_method(v, &ak)? {
            return self.get_iterator_from_method(v, &m);
        }
        let sk = PropKey::Sym(self.realm.sym_iterator.clone());
        let Some(m) = self.get_method(v, &sk)? else {
            return self.type_error(&format!("{} is not async iterable", self.describe_short(v)));
        };
        let sync = self.get_iterator_from_method(v, &m)?;
        // CreateAsyncFromSyncIterator (§27.1.6.1): the wrapper holds the sync record.
        let kind = std::mem::replace(&mut self.heap.obj_mut(sync).kind, Kind::Ordinary);
        let proto = self.realm.async_from_sync_iterator_proto;
        let w = self.heap.alloc(Object::new(Some(proto), kind));
        let next = self.get(w, &PropKey::lit("next"))?;
        Ok(self.heap.alloc(Object::new(
            None,
            Kind::IterRecord(Box::new(IterRecord { iterator: Value::Object(w), next, done: false })),
        )))
    }

    /// `[rec received] → [rec mode innerResult]` (§14.4.14 step 7, async).
    pub(super) fn yield_star_async(&mut self, ret_at: u32) -> JsResult<()> {
        let received = self.pop();
        let Value::Object(rec) = self.peek(0) else { return Ok(()) };
        let mode = std::mem::take(&mut self.top().resume);
        let (it, next) = self.iter_parts(rec);
        let r = match mode {
            Resume::Next => self.call(&next, it, vec![received])?,
            Resume::Throw => match self.get_method(&it, &PropKey::lit("throw"))? {
                Some(t) => self.call(&t, it, vec![received])?,
                None => {
                    if let Some(f) = self.get_method(&it, &PropKey::lit("return"))? {
                        self.call(&f, it, Vec::new())?;
                    }
                    return self.type_error("The iterator does not provide a 'throw' method");
                }
            },
            Resume::Return => match self.get_method(&it, &PropKey::lit("return"))? {
                None => {
                    self.pop();
                    self.push(received);
                    self.jump(ret_at);
                    return Ok(());
                }
                Some(f) => self.call(&f, it, vec![received])?,
            },
        };
        self.push(Value::Number(mode as u8 as f64));
        self.push(r);
        Ok(())
    }

    /// `[rec mode result] → [rec value]`, or the done paths.
    pub(super) fn yield_star_async_check(&mut self, done_at: u32, ret_at: u32) -> JsResult<()> {
        let res = self.pop();
        let mode = self.pop();
        let Value::Object(ro) = res else { return self.type_error("Iterator result is not an object") };
        let done = {
            let b = self.get(ro, &PropKey::lit("done"))?;
            self.to_boolean(&b)
        };
        let v = self.get(ro, &PropKey::lit("value"))?;
        if done {
            self.pop();
            self.push(v);
            let is_return = matches!(mode, Value::Number(n) if n == Resume::Return as u8 as f64);
            self.jump(if is_return { ret_at } else { done_at });
        } else {
            self.push(v);
        }
        Ok(())
    }
}

fn ag_gen(vm: &Vm, c: &NativeCall) -> Option<ObjRef> {
    slot0(vm, c.callee).as_object()
}

fn ag_return_fulfilled(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let Some(g) = ag_gen(vm, &c) else { return Ok(Value::Undefined) };
    if let Some(a) = vm.ag(g) {
        a.phase = AgPhase::Completed;
    }
    vm.ag_complete_step(g, Ok(c.arg(0)), true)?;
    vm.ag_resume_next(g)?;
    Ok(Value::Undefined)
}

fn ag_return_rejected(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let Some(g) = ag_gen(vm, &c) else { return Ok(Value::Undefined) };
    if let Some(a) = vm.ag(g) {
        a.phase = AgPhase::Completed;
    }
    vm.ag_complete_step(g, Err(c.arg(0)), true)?;
    vm.ag_resume_next(g)?;
    Ok(Value::Undefined)
}

// ---- %AsyncFromSyncIteratorPrototype% (§27.1.6.2) ----

fn sync_record(vm: &Vm, this: &Value) -> Option<(Value, Value)> {
    let Value::Object(o) = this else { return None };
    match &vm.heap.obj(*o).kind {
        Kind::IterRecord(r) => Some((r.iterator.clone(), r.next.clone())),
        _ => None,
    }
}

/// AsyncFromSyncIteratorContinuation (§27.1.6.4).
fn continuation(
    vm: &mut Vm,
    result: Value,
    cap: (Value, Value, Value),
    sync_it: Value,
    close: bool,
) -> JsResult<Value> {
    let (p, res, rej) = cap;
    let r = (|| -> JsResult<()> {
        let Value::Object(ro) = result else { return vm.type_error("Iterator result is not an object") };
        let done = {
            let b = vm.get(ro, &PropKey::lit("done"))?;
            vm.to_boolean(&b)
        };
        let value = vm.get(ro, &PropKey::lit("value"))?;
        let pc = Value::Object(vm.realm.promise_ctor);
        let wrapper = match vm.promise_resolve(&pc, value) {
            Ok(w) => w,
            Err(e) => {
                if !done && close {
                    if let Ok(Some(f)) = vm.get_method(&sync_it, &PropKey::lit("return")) {
                        let _ = vm.call(&f, sync_it.clone(), Vec::new());
                    }
                }
                return Err(e);
            }
        };
        let on_ful = native_with(
            vm,
            "",
            1,
            |vm, c| {
                let done = {
                    let b = slot0(vm, c.callee);
                    vm.to_boolean(&b)
                };
                Ok(vm.iter_result(c.arg(0), done))
            },
            false,
            vec![Value::Bool(done)],
        );
        let on_rej = if done || !close {
            Value::Undefined
        } else {
            Value::Object(native_with(
                vm,
                "",
                1,
                |vm, c| {
                    let it = slot0(vm, c.callee);
                    if let Ok(Some(f)) = vm.get_method(&it, &PropKey::lit("return")) {
                        let _ = vm.call(&f, it, Vec::new());
                    }
                    Err(c.arg(0))
                },
                false,
                vec![sync_it.clone()],
            ))
        };
        if let Value::Object(w) = wrapper {
            vm.perform_then(w, Value::Object(on_ful), on_rej, Some((p.clone(), res.clone(), rej.clone())));
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

fn afs_method(vm: &mut Vm, c: NativeCall, which: Resume) -> JsResult<Value> {
    let pc = Value::Object(vm.realm.promise_ctor);
    let cap = vm.new_promise_capability(&pc)?;
    let Some((it, next)) = sync_record(vm, &c.this) else {
        let e = vm.error(ErrorKind::Type, "not an async-from-sync iterator");
        vm.call(&cap.2, Value::Undefined, vec![e])?;
        return Ok(cap.0);
    };
    let args = if c.args.is_empty() { Vec::new() } else { vec![c.arg(0)] };
    let step = (|| -> JsResult<Option<(Value, bool)>> {
        match which {
            Resume::Next => Ok(Some((vm.call(&next, it.clone(), args)?, true))),
            Resume::Return => match vm.get_method(&it, &PropKey::lit("return"))? {
                None => Ok(None),
                Some(f) => Ok(Some((vm.call(&f, it.clone(), args)?, false))),
            },
            Resume::Throw => match vm.get_method(&it, &PropKey::lit("throw"))? {
                None => {
                    // Close the sync iterator, then reject with a TypeError (§27.1.6.2.3 step 7).
                    if let Some(f) = vm.get_method(&it, &PropKey::lit("return"))? {
                        let r = vm.call(&f, it.clone(), Vec::new())?;
                        if !matches!(r, Value::Object(_)) {
                            return vm.type_error("Iterator result is not an object");
                        }
                    }
                    vm.type_error("The iterator does not provide a 'throw' method")
                }
                Some(f) => Ok(Some((vm.call(&f, it.clone(), args)?, true))),
            },
        }
    })();
    match step {
        Ok(None) => {
            let r = vm.iter_result(c.arg(0), true);
            vm.call(&cap.1, Value::Undefined, vec![r])?;
            Ok(cap.0)
        }
        Ok(Some((result, close))) => continuation(vm, result, cap, it, close),
        Err(e) => {
            if vm.terminated {
                return Err(e);
            }
            vm.call(&cap.2, Value::Undefined, vec![e])?;
            Ok(cap.0)
        }
    }
}

pub(super) fn install(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let fp = vm.realm.function_proto;
    let aip = vm.new_object_with_proto(Some(op));
    vm.realm.async_iterator_proto = aip;
    let ak = vm.realm.sym_async_iterator.clone();
    method_sym(vm, aip, ak, "[Symbol.asyncIterator]", 0, |_, c| Ok(c.this));
    let afs = vm.new_object_with_proto(Some(aip));
    vm.realm.async_from_sync_iterator_proto = afs;
    method(vm, afs, "next", 1, |vm, c| afs_method(vm, c, Resume::Next));
    method(vm, afs, "return", 1, |vm, c| afs_method(vm, c, Resume::Return));
    method(vm, afs, "throw", 1, |vm, c| afs_method(vm, c, Resume::Throw));
    let agfp = vm.new_object_with_proto(Some(fp));
    let agp = vm.new_object_with_proto(Some(aip));
    vm.realm.async_generator_function_proto = agfp;
    vm.realm.async_generator_proto = agp;
    vm.heap
        .obj_mut(agfp)
        .props
        .insert(PropKey::lit("prototype"), Prop::data(Value::Object(agp), false, false, true));
    vm.heap
        .obj_mut(agp)
        .props
        .insert(PropKey::lit("constructor"), Prop::data(Value::Object(agfp), false, false, true));
    realm::to_string_tag(vm, agfp, "AsyncGeneratorFunction");
    realm::to_string_tag(vm, agp, "AsyncGenerator");
    method(vm, agp, "next", 1, |vm, c| vm.ag_enqueue(&c.this, Resume::Next, c.arg(0)));
    method(vm, agp, "return", 1, |vm, c| vm.ag_enqueue(&c.this, Resume::Return, c.arg(0)));
    method(vm, agp, "throw", 1, |vm, c| vm.ag_enqueue(&c.this, Resume::Throw, c.arg(0)));
}

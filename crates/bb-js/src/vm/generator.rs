//! Generators (§27.5): a generator's frame is parked in the generator object between
//! resumptions and run as the base frame of a nested run when resumed.

use super::heap::*;
use super::realm::{self, method};
use super::value::{PropKey, Value};
use super::{JsResult, Resume, Vm};

impl Vm {
    /// `GeneratorStart` (§27.5.3.1): after FunctionDeclarationInstantiation, park the frame
    /// and return the generator object to the caller.
    pub(super) fn generator_start(&mut self, base: usize) -> JsResult<Option<Value>> {
        let mut frame = self.frames.pop().expect("generator frame");
        let proto = match frame.callee {
            Some(f) => match self.get(f, &PropKey::named("prototype")) {
                Ok(Value::Object(p)) => p,
                Ok(_) => self.realm.generator_proto,
                Err(e) => {
                    self.frames.push(frame);
                    return Err(e);
                }
            },
            None => self.realm.generator_proto,
        };
        let g = self.heap.alloc(Object::new(Some(proto), Kind::Generator(Box::new(GenState::Executing))));
        frame.generator = Some(g);
        if let Kind::Generator(st) = &mut self.heap.obj_mut(g).kind {
            **st = GenState::SuspendedStart(Box::new(frame));
        }
        if self.frames.len() <= base {
            return Ok(Some(Value::Object(g)));
        }
        self.push(Value::Object(g));
        Ok(None)
    }

    /// Suspend the running generator frame (always its run's base), yielding `v`.
    /// `again`: re-run the current instruction on resumption (`yield*`).
    pub(super) fn suspend(&mut self, v: Value, base: usize, again: bool) -> JsResult<Option<Value>> {
        let mut frame = self.frames.pop().expect("generator frame");
        if again {
            frame.pc -= 1;
        }
        let Some(g) = frame.generator else {
            return self.syntax_error("yield outside a generator");
        };
        if let Kind::Generator(st) = &mut self.heap.obj_mut(g).kind {
            **st = GenState::SuspendedYield(Box::new(frame));
        }
        if self.frames.len() <= base {
            return Ok(Some(v));
        }
        // Not reachable: generator frames only run as the base of a resumption.
        self.push(v);
        Ok(None)
    }

    pub(super) fn generator_completed(&mut self, g: ObjRef) {
        if let Kind::Generator(st) = &mut self.heap.obj_mut(g).kind {
            if matches!(**st, GenState::Executing) {
                **st = GenState::Completed;
            }
        }
    }

    /// One step of `yield*` (§14.4.14 steps 7.a–c) with `[rec received]` on the stack.
    pub(super) fn yield_star(&mut self, done_at: u32, ret_at: u32, base: usize) -> JsResult<Option<Value>> {
        let received = self.pop();
        let Value::Object(rec) = self.peek(0) else { return Ok(None) };
        let mode = std::mem::take(&mut self.top().resume);
        let (it, next) = match &self.heap.obj(rec).kind {
            Kind::IterRecord(r) => (r.iterator.clone(), r.next.clone()),
            _ => return self.type_error("yield* on a non-iterator"),
        };
        let inner = match mode {
            Resume::Next => self.call(&next, it.clone(), vec![received])?,
            Resume::Throw => match self.get_method(&it, &PropKey::named("throw"))? {
                Some(t) => self.call(&t, it.clone(), vec![received])?,
                None => {
                    // §14.4.14 step 7.b.iii: close the delegate, then a TypeError.
                    self.iterator_close(rec)?;
                    return self.type_error("The iterator does not provide a 'throw' method");
                }
            },
            Resume::Return => match self.get_method(&it, &PropKey::named("return"))? {
                None => {
                    self.pop();
                    self.push(received);
                    self.jump(ret_at);
                    return Ok(None);
                }
                Some(r) => {
                    let res = self.call(&r, it.clone(), vec![received])?;
                    let Value::Object(ro) = res else {
                        return self.type_error("Iterator result is not an object");
                    };
                    if self.get(ro, &PropKey::named("done"))?.truthy() {
                        let v = self.get(ro, &PropKey::named("value"))?;
                        self.pop();
                        self.push(v);
                        self.jump(ret_at);
                        return Ok(None);
                    }
                    res
                }
            },
        };
        let Value::Object(io) = inner else {
            return self.type_error("Iterator result is not an object");
        };
        if self.get(io, &PropKey::named("done"))?.truthy() {
            let v = self.get(io, &PropKey::named("value"))?;
            self.pop();
            self.push(v);
            self.jump(done_at);
            return Ok(None);
        }
        // Pass the inner result through unchanged (§27.5.3.7 GeneratorYield).
        self.yield_raw = true;
        self.suspend(inner, base, true)
    }

    /// GeneratorResume / GeneratorResumeAbrupt (§27.5.3.3–4).
    fn generator_resume(&mut self, this: &Value, mode: Resume, v: Value) -> JsResult<Value> {
        let Value::Object(g) = this else { return self.type_error("not a generator") };
        let g = *g;
        let state = match &mut self.heap.obj_mut(g).kind {
            Kind::Generator(st) => std::mem::replace(&mut **st, GenState::Executing),
            _ => return self.type_error("not a generator"),
        };
        let set = |vm: &mut Vm, s: GenState| {
            if let Kind::Generator(st) = &mut vm.heap.obj_mut(g).kind {
                **st = s;
            }
        };
        let mut frame = match state {
            GenState::Executing => {
                set(self, GenState::Executing);
                return self.type_error("Generator is already running");
            }
            GenState::Completed => {
                set(self, GenState::Completed);
                return match mode {
                    Resume::Next => Ok(self.iter_result(Value::Undefined, true)),
                    Resume::Return => Ok(self.iter_result(v, true)),
                    Resume::Throw => Err(v),
                };
            }
            GenState::SuspendedStart(f) => match mode {
                Resume::Next => f,
                Resume::Return => {
                    set(self, GenState::Completed);
                    return Ok(self.iter_result(v, true));
                }
                Resume::Throw => {
                    set(self, GenState::Completed);
                    return Err(v);
                }
            },
            GenState::SuspendedYield(mut f) => {
                f.stack.push(v);
                f.resume = mode;
                f
            }
        };
        frame.generator = Some(g);
        self.yield_raw = false;
        let r = self.execute(*frame);
        let suspended = matches!(&self.heap.obj(g).kind, Kind::Generator(st) if matches!(**st, GenState::SuspendedYield(_)));
        if !suspended {
            set(self, GenState::Completed);
        }
        let v = r?;
        if suspended && self.yield_raw {
            self.yield_raw = false;
            return Ok(v);
        }
        Ok(self.iter_result(v, !suspended))
    }
}

/// %GeneratorFunction.prototype% and %GeneratorPrototype% (§27.3.3, §27.5.1).
pub(super) fn install(vm: &mut Vm) {
    let fp = vm.realm.function_proto;
    let ip = vm.realm.iterator_proto;
    let gfp = vm.new_object_with_proto(Some(fp));
    let gp = vm.new_object_with_proto(Some(ip));
    vm.realm.generator_function_proto = gfp;
    vm.realm.generator_proto = gp;
    vm.heap
        .obj_mut(gfp)
        .props
        .insert(PropKey::named("prototype"), Prop::data(Value::Object(gp), false, false, true));
    vm.heap
        .obj_mut(gp)
        .props
        .insert(PropKey::named("constructor"), Prop::data(Value::Object(gfp), false, false, true));
    realm::to_string_tag(vm, gfp, "GeneratorFunction");
    realm::to_string_tag(vm, gp, "Generator");
    method(vm, gp, "next", 1, |vm, c| vm.generator_resume(&c.this, Resume::Next, c.arg(0)));
    method(vm, gp, "return", 1, |vm, c| vm.generator_resume(&c.this, Resume::Return, c.arg(0)));
    method(vm, gp, "throw", 1, |vm, c| vm.generator_resume(&c.this, Resume::Throw, c.arg(0)));
}

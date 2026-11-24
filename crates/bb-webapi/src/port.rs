//! MessageChannel and MessagePort (HTML §9.4). Two entangled ports are two channels: each
//! port holds its own receiving end and its peer's sending end, so transferring a port to a
//! worker (HTML §9.4.3 transfer steps) just moves those ends to that thread. A shared slot
//! names the thread a port lives on (its waker and pending-message counter), so its peer's
//! messages wake the right event loop.

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};

use bb_js::vm::clone::Serialized;
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::host::{host, this_of, Host};
use crate::idl::{attr, op};

type R = JsResult<Value>;

/// Where a port lives: its thread's waker and pending-message counter.
#[derive(Default)]
pub struct Home {
    pub wake: Option<crate::worker::Waker>,
    pub pending: Option<Arc<AtomicUsize>>,
}

type HomeSlot = Arc<Mutex<Home>>;

pub enum PortMsg {
    Message(Serialized),
    /// The entangled port was closed (or went away).
    Closed,
}

/// A port's channel ends, which travel when it is transferred.
pub struct PortEnds {
    rx: Receiver<PortMsg>,
    peer: Option<Sender<PortMsg>>,
    /// This port's home (the peer's sender wakes it), and the peer's.
    home: HomeSlot,
    peer_home: HomeSlot,
}

/// A MessagePort's state in the Host of the thread it lives on.
pub struct PortState {
    pub obj: ObjRef,
    ends: Option<PortEnds>,
    /// Arrived messages (HTML §9.4.4 the port message queue).
    queue: VecDeque<Serialized>,
    /// The port message queue is enabled (start(), or onmessage was set).
    enabled: bool,
    detached: bool,
}

/// Posting to a port's peer: send, count it at the peer's home, wake that thread.
fn send(ends: &PortEnds, m: PortMsg) {
    let Some(tx) = &ends.peer else { return };
    let home = ends.peer_home.lock().ok();
    if let Some(p) = home.as_ref().and_then(|h| h.pending.clone()) {
        p.fetch_add(1, Ordering::SeqCst);
        if tx.send(m).is_err() {
            p.fetch_sub(1, Ordering::SeqCst);
        }
    } else {
        let _ = tx.send(m);
    }
    if let Some(w) = home.as_ref().and_then(|h| h.wake.clone()) {
        drop(home);
        w();
    }
}

/// This thread's home: the embedder's waker and the Host's pending counter.
fn home_of(h: &Host) -> Home {
    Home { wake: h.embedder.waker(), pending: Some(h.workers.borrow().pending.clone()) }
}

fn new_port(vm: &mut Vm, h: &Host, ends: PortEnds) -> ObjRef {
    if let Ok(mut home) = ends.home.lock() {
        *home = home_of(h);
    }
    let id = h.ports.borrow().len() as u64;
    let o = vm.new_host_object(Some(h.proto(C::MessagePort)), C::MessagePort as u32, id, false);
    h.ports.borrow_mut().push(PortState {
        obj: o,
        ends: Some(ends),
        queue: VecDeque::new(),
        enabled: false,
        detached: false,
    });
    o
}

pub fn message_channel_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("MessageChannel requires 'new'") };
    let h = host(vm)?;
    let (tx1, rx1) = channel::<PortMsg>();
    let (tx2, rx2) = channel::<PortMsg>();
    let (home1, home2): (HomeSlot, HomeSlot) = (Arc::default(), Arc::default());
    let p1 = new_port(
        vm,
        &h,
        PortEnds { rx: rx1, peer: Some(tx2), home: home1.clone(), peer_home: home2.clone() },
    );
    let p2 = new_port(vm, &h, PortEnds { rx: rx2, peer: Some(tx1), home: home2, peer_home: home1 });
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => p,
        _ => h.proto(C::MessageChannel),
    };
    let o = vm.new_host_object(Some(proto), C::MessageChannel as u32, 0, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = vec![Value::Object(p1), Value::Object(p2)];
    }
    Ok(Value::Object(o))
}

pub fn setup_message_channel(vm: &mut Vm, proto: ObjRef) {
    for (name, i) in [("port1", 0usize), ("port2", 1)] {
        let f: bb_js::vm::heap::NativeFn =
            if i == 0 { |vm, c| channel_port(vm, &c, 0) } else { |vm, c| channel_port(vm, &c, 1) };
        attr(vm, proto, name, f, None);
    }
}

fn channel_port(vm: &mut Vm, c: &NativeCall, i: usize) -> R {
    this_of(vm, &c.this, C::MessageChannel)?;
    let Value::Object(o) = c.this else { return Ok(Value::Undefined) };
    Ok(vm.host_obj(o).and_then(|h| h.slots.get(i).cloned()).unwrap_or(Value::Undefined))
}

fn port_this(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, usize)> {
    let (_, id) = this_of(vm, &c.this, C::MessagePort)?;
    Ok((host(vm)?, id as usize))
}

pub fn setup_message_port(vm: &mut Vm, proto: ObjRef) {
    // HTML §9.4.4 postMessage(message, transfer | options).
    op(vm, proto, "postMessage", 1, |vm, c| {
        if c.args.is_empty() {
            return vm.type_error("postMessage needs a message");
        }
        let (h, id) = port_this(vm, &c)?;
        let transfer = crate::worker::transfer_arg(vm, &c.arg(1))?;
        // Transferring the port itself (or its peer, which is only checked by identity here).
        if transfer.iter().any(|&t| Some(t) == h.ports.borrow().get(id).map(|p| p.obj)) {
            return crate::exception::throw(
                vm,
                "DataCloneError",
                "a port can't be transferred through itself",
            );
        }
        let s = vm.structured_serialize(&c.arg(0), &transfer)?;
        if let Some(ends) = h.ports.borrow().get(id).and_then(|p| p.ends.as_ref()) {
            send(ends, PortMsg::Message(s));
        }
        Ok(Value::Undefined)
    });
    op(vm, proto, "start", 0, |vm, c| {
        let (h, id) = port_this(vm, &c)?;
        if let Some(p) = h.ports.borrow_mut().get_mut(id) {
            p.enabled = true;
        }
        Ok(Value::Undefined)
    });
    // HTML §9.4.4 close(): disentangle.
    op(vm, proto, "close", 0, |vm, c| {
        let (h, id) = port_this(vm, &c)?;
        if let Some(p) = h.ports.borrow_mut().get_mut(id) {
            if let Some(ends) = p.ends.take() {
                send(&ends, PortMsg::Closed);
            }
            p.detached = true;
        }
        Ok(Value::Undefined)
    });
    // Setting onmessage enables the port message queue (HTML §9.4.4).
    attr(
        vm,
        proto,
        "onmessage",
        |vm, c| {
            let (h, _) = port_this(vm, &c)?;
            let Value::Object(o) = c.this else { return Ok(Value::Null) };
            crate::events::handler_value(vm, &h, o, "onmessage")
        },
        Some(|vm, c| {
            let (h, id) = port_this(vm, &c)?;
            let Value::Object(o) = c.this else { return Ok(Value::Undefined) };
            crate::events::set_handler_value(vm, &h, o, "onmessage", c.arg(0));
            if let Some(p) = h.ports.borrow_mut().get_mut(id) {
                p.enabled = true;
            }
            Ok(Value::Undefined)
        }),
    );
    crate::events::setup_named_handlers(vm, proto, &["onmessageerror", "onclose"]);
}

/// Deliver arrived port messages (the event loop calls this); returns whether any.
pub fn poll(vm: &mut Vm, h: &Host) -> bool {
    // Move what arrived into the ports' queues (it no longer counts as pending).
    let mut closed = Vec::new();
    {
        let pending = h.workers.borrow().pending.clone();
        let mut ports = h.ports.borrow_mut();
        for p in ports.iter_mut() {
            let Some(ends) = &p.ends else { continue };
            loop {
                match ends.rx.try_recv() {
                    Ok(PortMsg::Message(s)) => {
                        pending.fetch_sub(1, Ordering::SeqCst);
                        p.queue.push_back(s);
                    }
                    Ok(PortMsg::Closed) => {
                        pending.fetch_sub(1, Ordering::SeqCst);
                        closed.push(p.obj);
                        break;
                    }
                    Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                }
            }
        }
    }
    let mut ran = false;
    loop {
        let next = {
            let mut ports = h.ports.borrow_mut();
            ports
                .iter_mut()
                .filter(|p| p.enabled && !p.detached)
                .find_map(|p| p.queue.pop_front().map(|s| (p.obj, s)))
        };
        let Some((o, s)) = next else { break };
        ran = true;
        let r = match vm.structured_deserialize_with_transfer(s) {
            Ok((data, ports)) => {
                crate::worker::fire_message_with_ports(vm, h, Value::Object(o), "message", data, ports)
            }
            Err(_) => crate::worker::fire_message_with_ports(
                vm,
                h,
                Value::Object(o),
                "messageerror",
                Value::Null,
                Vec::new(),
            ),
        };
        if let Err(e) = r {
            if vm.terminated {
                return ran;
            }
            crate::events::report_exception(vm, e);
        }
        vm.run_jobs();
    }
    for o in closed {
        let _ = crate::events::fire(vm, h, Value::Object(o), "close", false, false);
        ran = true;
    }
    ran
}

/// Whether a started port has messages waiting.
pub fn has_messages(h: &Host) -> bool {
    h.ports.borrow().iter().any(|p| p.enabled && !p.detached && !p.queue.is_empty())
}

// ---- transfer (HTML §9.4.3) ----

/// A MessagePort in transit: its ends and the messages already queued.
struct Transferred {
    ends: PortEnds,
    queue: VecDeque<Serialized>,
}

pub fn transferable(vm: &Vm, o: ObjRef) -> bool {
    let Some((c, id)) = vm.host_data(&Value::Object(o)) else { return false };
    if c != C::MessagePort as u32 {
        return false;
    }
    host_of(vm)
        .is_some_and(|h| h.ports.borrow().get(id as usize).is_some_and(|p| !p.detached && p.ends.is_some()))
}

fn host_of(vm: &Vm) -> Option<Rc<Host>> {
    vm.host::<Host>()
}

/// The transfer steps: the port is detached here and its ends move.
pub fn transfer(vm: &mut Vm, o: ObjRef) -> JsResult<Box<dyn std::any::Any + Send>> {
    let h = host(vm)?;
    let Some((_, id)) = vm.host_data(&Value::Object(o)) else { return vm.type_error("not a port") };
    let mut ports = h.ports.borrow_mut();
    let Some(p) = ports.get_mut(id as usize) else { return vm.type_error("not a port") };
    p.detached = true;
    // Messages that arrived but weren't delivered go along.
    let mut queue = std::mem::take(&mut p.queue);
    let Some(ends) = p.ends.take() else { return vm.type_error("the port was already transferred") };
    let pending = h.workers.borrow().pending.clone();
    while let Ok(m) = ends.rx.try_recv() {
        pending.fetch_sub(1, Ordering::SeqCst);
        if let PortMsg::Message(s) = m {
            queue.push_back(s);
        }
    }
    Ok(Box::new(Transferred { ends, queue }))
}

/// The receiving steps: a new MessagePort here with the moved ends.
pub fn receive(vm: &mut Vm, b: Box<dyn std::any::Any + Send>) -> JsResult<Value> {
    let h = host(vm)?;
    let Ok(t) = b.downcast::<Transferred>() else { return vm.type_error("not a transferred port") };
    let t = *t;
    let o = new_port(vm, &h, t.ends);
    if let Some(p) = h.ports.borrow_mut().last_mut() {
        p.queue = t.queue;
    }
    Ok(Value::Object(o))
}

pub fn trace(h: &Host, out: &mut Vec<ObjRef>) {
    out.extend(h.ports.borrow().iter().filter(|p| !p.detached).map(|p| p.obj));
}

// ---- BroadcastChannel (HTML §9.5) ----

struct Registered {
    id: u64,
    origin: String,
    name: String,
    tx: Sender<(Serialized, String)>,
    home: HomeSlot,
}

/// Every open BroadcastChannel in the process (all agents of one user agent).
static REGISTRY: Mutex<Vec<Registered>> = Mutex::new(Vec::new());
static NEXT_BC: AtomicUsize = AtomicUsize::new(1);

/// A BroadcastChannel's state in its Host.
pub struct BroadcastState {
    pub obj: ObjRef,
    id: u64,
    rx: Receiver<(Serialized, String)>,
    pub closed: bool,
}

impl Drop for BroadcastState {
    fn drop(&mut self) {
        unregister(self.id);
    }
}

fn unregister(id: u64) {
    if let Ok(mut r) = REGISTRY.lock() {
        r.retain(|e| e.id != id);
    }
}

pub fn broadcast_channel_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("BroadcastChannel requires 'new'") };
    if c.args.is_empty() {
        return vm.type_error("BroadcastChannel needs a name");
    }
    let name = crate::host::rstr(vm, &c.arg(0))?;
    let h = host(vm)?;
    let origin = h.url.borrow().origin().serialize();
    let (tx, rx) = channel();
    let id = NEXT_BC.fetch_add(1, Ordering::SeqCst) as u64;
    let home: HomeSlot = Arc::new(Mutex::new(home_of(&h)));
    if let Ok(mut r) = REGISTRY.lock() {
        r.push(Registered { id, origin, name: name.clone(), tx, home });
    }
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => p,
        _ => h.proto(C::BroadcastChannel),
    };
    let idx = h.broadcasts.borrow().len() as u64;
    let o = vm.new_host_object(Some(proto), C::BroadcastChannel as u32, idx, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = vec![crate::host::js(&name)];
    }
    h.broadcasts.borrow_mut().push(BroadcastState { obj: o, id, rx, closed: false });
    Ok(Value::Object(o))
}

fn bc_this(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, usize)> {
    let (_, id) = this_of(vm, &c.this, C::BroadcastChannel)?;
    Ok((host(vm)?, id as usize))
}

pub fn setup_broadcast_channel(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "name",
        |vm, c| {
            bc_this(vm, &c)?;
            let Value::Object(o) = c.this else { return Ok(Value::Undefined) };
            Ok(vm.host_obj(o).and_then(|h| h.slots.first().cloned()).unwrap_or(Value::Undefined))
        },
        None,
    );
    // HTML §9.5 postMessage(message): to every other channel of this name and origin.
    op(vm, proto, "postMessage", 1, |vm, c| {
        if c.args.is_empty() {
            return vm.type_error("postMessage needs a message");
        }
        let (h, idx) = bc_this(vm, &c)?;
        let (closed, me) = h.broadcasts.borrow().get(idx).map_or((true, 0), |b| (b.closed, b.id));
        if closed {
            return crate::exception::throw(vm, "InvalidStateError", "the channel is closed");
        }
        let origin = h.url.borrow().origin().serialize();
        let Ok(reg) = REGISTRY.lock() else { return Ok(Value::Undefined) };
        let Some(mine) = reg.iter().find(|e| e.id == me) else { return Ok(Value::Undefined) };
        let name = mine.name.clone();
        let targets: Vec<(Sender<(Serialized, String)>, HomeSlot)> = reg
            .iter()
            .filter(|e| e.id != me && e.name == name && e.origin == origin)
            .map(|e| (e.tx.clone(), e.home.clone()))
            .collect();
        drop(reg);
        for (tx, home) in targets {
            // Each receiver gets its own serialization (HTML §9.5 step 6).
            let s = vm.structured_serialize(&c.arg(0), &[])?;
            let (wake, pending) = match home.lock() {
                Ok(h) => (h.wake.clone(), h.pending.clone()),
                Err(_) => (None, None),
            };
            if let Some(p) = &pending {
                p.fetch_add(1, Ordering::SeqCst);
            }
            if tx.send((s, origin.clone())).is_err() {
                if let Some(p) = &pending {
                    p.fetch_sub(1, Ordering::SeqCst);
                }
            }
            if let Some(w) = wake {
                w();
            }
        }
        Ok(Value::Undefined)
    });
    op(vm, proto, "close", 0, |vm, c| {
        let (h, idx) = bc_this(vm, &c)?;
        if let Some(b) = h.broadcasts.borrow_mut().get_mut(idx) {
            b.closed = true;
            unregister(b.id);
        }
        Ok(Value::Undefined)
    });
    crate::events::setup_named_handlers(vm, proto, &["onmessage", "onmessageerror"]);
}

/// Deliver broadcast messages that arrived; returns whether any.
pub fn poll_broadcasts(vm: &mut Vm, h: &Host) -> bool {
    let mut got = Vec::new();
    {
        let pending = h.workers.borrow().pending.clone();
        for b in h.broadcasts.borrow().iter() {
            while let Ok(m) = b.rx.try_recv() {
                pending.fetch_sub(1, Ordering::SeqCst);
                if !b.closed {
                    got.push((b.obj, m));
                }
            }
        }
    }
    let any = !got.is_empty();
    for (o, (s, origin)) in got {
        let r = match vm.structured_deserialize(s) {
            Ok(data) => crate::worker::fire_message_from(vm, h, Value::Object(o), "message", data, &origin),
            Err(_) => crate::worker::fire_message_from(
                vm,
                h,
                Value::Object(o),
                "messageerror",
                Value::Null,
                &origin,
            ),
        };
        if let Err(e) = r {
            if vm.terminated {
                return any;
            }
            crate::events::report_exception(vm, e);
        }
        vm.run_jobs();
    }
    any
}

pub fn trace_broadcasts(h: &Host, out: &mut Vec<ObjRef>) {
    out.extend(h.broadcasts.borrow().iter().filter(|b| !b.closed).map(|b| b.obj));
}

//! Dedicated workers (HTML §10.2): `new Worker(url)` runs a classic script on its own thread
//! with its own VM and a DedicatedWorkerGlobalScope; messages cross as structured clones
//! (HTML §2.7). Also MessageEvent, `window.postMessage` (HTML §9.3.3) and
//! `structuredClone` (HTML §2.7.10).
//!
//! The parent learns of a worker's messages through a channel it polls in its event loop;
//! the embedder's waker wakes that loop when one arrives.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bb_js::vm::clone::{HostSer, Serialized};
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};
use bb_url::Url;

use crate::class::C;
use crate::exception::throw;
use crate::host::{dict_get, host, js, rstr, this_of, Embedder, Host};
use crate::idl::{attr, op};
use crate::net::{HttpRequest, HttpResponse};

type R = JsResult<Value>;

/// Network access a worker thread can use (the embedder's client, which is `Send`).
pub trait WorkerNet: Send + Sync {
    fn fetch(&self, req: HttpRequest) -> Result<HttpResponse, String>;
}

/// Worker threads alive in the process; past [`MAX_WORKERS`] a new Worker only gets an
/// error event (HTML lets user agents limit workers), so a page can't spawn without end.
static LIVE_WORKERS: AtomicUsize = AtomicUsize::new(0);
const MAX_WORKERS: usize = 64;

/// Count a new worker thread (dedicated or shared); false when there are too many.
pub(crate) fn reserve_thread() -> bool {
    if LIVE_WORKERS.fetch_add(1, Ordering::SeqCst) >= MAX_WORKERS {
        LIVE_WORKERS.fetch_sub(1, Ordering::SeqCst);
        return false;
    }
    true
}

/// A worker thread ended (or never started).
pub(crate) fn release_thread() {
    LIVE_WORKERS.fetch_sub(1, Ordering::SeqCst);
}

/// Wakes an event loop from another thread (the embedder's, or a worker's own).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

pub enum ToWorker {
    Message(Serialized),
    /// Something for the worker's loop to look at (its own nested workers' messages).
    Wake,
    Terminate,
}

pub enum FromWorker {
    Message(Serialized),
    /// An exception the worker didn't handle (HTML §10.2.4 "runtime script errors").
    Error(String),
    /// The script couldn't be fetched (HTML §10.2.4 step 12 "error" event).
    LoadFailed(String),
    /// Lines the worker wrote to its console (shown in the parent's).
    Console(Vec<String>),
    Closed,
}

/// The parent's side of a worker.
pub struct WorkerHandle {
    pub obj: ObjRef,
    tx: Sender<ToWorker>,
    rx: Receiver<FromWorker>,
    pub alive: bool,
    /// Stops the worker's running script (terminate(), or the parent going away).
    interrupt: Arc<AtomicBool>,
}

impl Drop for WorkerHandle {
    /// HTML §10.2.4: a worker whose owner is gone is terminated.
    fn drop(&mut self) {
        self.interrupt.store(true, Ordering::SeqCst);
        let _ = self.tx.send(ToWorker::Terminate);
    }
}

/// A worker's way back to its parent.
#[derive(Clone)]
pub struct WorkerLink {
    tx: Sender<FromWorker>,
    wake: Option<Waker>,
    /// The parent's count of messages waiting (its `has_tasks` reads it).
    pending: Arc<AtomicUsize>,
}

impl WorkerLink {
    pub(crate) fn new(tx: Sender<FromWorker>, wake: Option<Waker>, pending: Arc<AtomicUsize>) -> Self {
        WorkerLink { tx, wake, pending }
    }

    /// Send to the parent; false when it is gone.
    pub(crate) fn send(&self, m: FromWorker) -> bool {
        self.pending.fetch_add(1, Ordering::SeqCst);
        if self.tx.send(m).is_err() {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        if let Some(w) = &self.wake {
            w();
        }
        true
    }
}

/// The worker-related state of a Host (a window's or a worker's).
#[derive(Default)]
pub struct WorkerState {
    pub workers: Vec<WorkerHandle>,
    /// This window's SharedWorker objects.
    pub shared: Vec<crate::shared_worker::SharedHandle>,
    pub pending: Arc<AtomicUsize>,
    /// Set in a worker's own Host: its parent.
    pub link: Option<WorkerLink>,
    /// The worker's name (WorkerGlobalScope.name).
    pub name: String,
    /// A module worker (its global's importScripts throws).
    pub module: bool,
}

impl WorkerState {
    pub fn trace(&self, out: &mut Vec<ObjRef>) {
        out.extend(self.workers.iter().filter(|w| w.alive).map(|w| w.obj));
        out.extend(self.shared.iter().map(|w| w.obj));
    }

    pub fn has_messages(&self) -> bool {
        self.pending.load(Ordering::SeqCst) > 0
    }
}

// ---- structured clone hooks ----

/// Platform objects that serialize: Blob, File (File API §3.1, §4.1) and DOMException.
pub fn host_serialize(vm: &mut Vm, o: ObjRef) -> Option<JsResult<HostSer>> {
    let (class, _) = vm.host_data(&Value::Object(o))?;
    if let Some((bytes, ty, name)) = crate::blob::blob_data(vm, &Value::Object(o)) {
        let modified = vm.host_obj(o).and_then(|h| h.slots.get(3).cloned());
        let numbers = match modified {
            Some(Value::Number(n)) => vec![n],
            _ => Vec::new(),
        };
        let mut strings = vec![ty];
        strings.extend(name);
        return Some(Ok(HostSer { class, bytes, strings, numbers }));
    }
    if let Some(g) = crate::geometry::serialize(vm, o, class) {
        return Some(Ok(g));
    }
    if class == C::DOMException as u32 {
        let slots = vm.host_obj(o).map(|h| h.slots.clone()).unwrap_or_default();
        let s = |v: Option<&Value>| match v {
            Some(Value::String(s)) => bb_js::vm::to_rust(s),
            _ => String::new(),
        };
        return Some(Ok(HostSer {
            class,
            strings: vec![s(slots.first()), s(slots.get(1))],
            ..Default::default()
        }));
    }
    None
}

pub fn host_deserialize(vm: &mut Vm, s: &HostSer) -> JsResult<Value> {
    let h = host(vm)?;
    if s.class == C::Blob as u32 || s.class == C::File as u32 {
        let ty = s.strings.first().cloned().unwrap_or_default();
        let file = (s.class == C::File as u32).then(|| {
            (s.strings.get(1).cloned().unwrap_or_default(), s.numbers.first().copied().unwrap_or(0.0))
        });
        let o = crate::blob::new_blob(
            vm,
            &h,
            s.bytes.clone(),
            &ty,
            file.as_ref().map(|(n, m)| (n.as_str(), *m)),
            None,
        );
        return Ok(Value::Object(o));
    }
    if s.class == C::DOMException as u32 {
        let name = s.strings.first().cloned().unwrap_or_default();
        let msg = s.strings.get(1).cloned().unwrap_or_default();
        return Ok(crate::exception::make(vm, &name, &msg));
    }
    if let Some(v) = crate::geometry::deserialize(vm, &h, s) {
        return Ok(v);
    }
    vm.type_error("that platform object can't be recreated here")
}

pub fn clone_error(vm: &mut Vm, msg: &str) -> Value {
    crate::exception::make(vm, "DataCloneError", msg)
}

/// A postMessage's second argument: a transfer list, or StructuredSerializeOptions.
pub fn transfer_arg(vm: &mut Vm, v: &Value) -> JsResult<Vec<ObjRef>> {
    let dict = matches!(v, Value::Object(_)) && !is_sequence(vm, v);
    transfer_list(vm, v, dict)
}

/// A transfer list from `transfer` (a sequence of ArrayBuffers) or an options dictionary's
/// `transfer` member.
fn transfer_list(vm: &mut Vm, v: &Value, dict: bool) -> JsResult<Vec<ObjRef>> {
    let seq = if dict { dict_get(vm, v, "transfer")? } else { v.clone() };
    if seq.is_nullish() {
        return Ok(Vec::new());
    }
    let Value::Object(o) = &seq else { return vm.type_error("the transfer list must be a sequence") };
    let key = PropKey::Sym(vm.realm.sym_iterator.clone());
    let f = vm.get(*o, &key)?;
    let items = crate::url::iter_values(vm, &seq, &f)?;
    let mut out = Vec::with_capacity(items.len());
    for i in items {
        match i {
            Value::Object(o) => out.push(o),
            _ => return vm.type_error("the transfer list holds a non-object"),
        }
    }
    Ok(out)
}

/// `structuredClone(value, { transfer })` (HTML §2.7.10).
pub fn structured_clone(vm: &mut Vm, c: NativeCall) -> R {
    if c.args.is_empty() {
        return vm.type_error("structuredClone needs a value");
    }
    let opts = c.arg(1);
    if !matches!(opts, Value::Undefined | Value::Null | Value::Object(_)) {
        return vm.type_error("the options must be an object");
    }
    let transfer = transfer_list(vm, &opts, true)?;
    let s = vm.structured_serialize(&c.arg(0), &transfer)?;
    vm.structured_deserialize(s)
}

// ---- MessageEvent (HTML §9.2) ----

pub fn setup_message_event(vm: &mut Vm, proto: ObjRef) {
    for name in ["data", "origin", "lastEventId", "source", "ports"] {
        crate::idl::attr_with(
            vm,
            proto,
            name,
            |vm, c| {
                this_of(vm, &c.this, C::MessageEvent)?;
                let Value::Object(e) = c.this else { return Ok(Value::Null) };
                let name = match crate::idl::callee_slots(vm, c.callee).first() {
                    Some(Value::String(s)) => bb_js::vm::to_rust(s),
                    _ => return Ok(Value::Null),
                };
                Ok(crate::events::get_field(vm, e, &name).unwrap_or(Value::Null))
            },
            None,
            vec![js(name)],
        );
    }
}

/// A trusted MessageEvent `ty` carrying `data` (with `origin`), at `target`.
fn fire_message(
    vm: &mut Vm,
    h: &Host,
    target: Value,
    ty: &str,
    data: Value,
    origin: &str,
    source: Value,
) -> JsResult<()> {
    fire_message_full(vm, h, target, ty, data, origin, source, Vec::new())
}

/// A trusted MessageEvent `ty` carrying `data` sent from `origin`, at `target`.
pub fn fire_message_from(
    vm: &mut Vm,
    h: &Host,
    target: Value,
    ty: &str,
    data: Value,
    origin: &str,
) -> JsResult<()> {
    fire_message_full(vm, h, target, ty, data, origin, Value::Null, Vec::new())
}

/// A trusted MessageEvent `ty` carrying `data` and the transferred `ports`, at `target`.
pub fn fire_message_with_ports(
    vm: &mut Vm,
    h: &Host,
    target: Value,
    ty: &str,
    data: Value,
    ports: Vec<Value>,
) -> JsResult<()> {
    fire_message_full(vm, h, target, ty, data, "", Value::Null, ports)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fire_message_full(
    vm: &mut Vm,
    h: &Host,
    target: Value,
    ty: &str,
    data: Value,
    origin: &str,
    source: Value,
    ports: Vec<Value>,
) -> JsResult<()> {
    let e = crate::events::trusted_event(vm, h, C::MessageEvent, ty, false, false);
    crate::events::set_field(vm, e, "data", data);
    crate::events::set_field(vm, e, "origin", js(origin));
    crate::events::set_field(vm, e, "lastEventId", js(""));
    crate::events::set_field(vm, e, "source", source);
    let ports = vm.new_array(ports);
    crate::events::set_field(vm, e, "ports", Value::Object(ports));
    crate::events::dispatch(vm, h, e, target)?;
    Ok(())
}

// ---- window.postMessage (HTML §9.3.3) ----

pub fn window_post_message(vm: &mut Vm, c: NativeCall) -> R {
    // The function is the target window's; the code calling it is the sender's.
    let target = host(vm)?;
    let source = vm.realm_host::<Host>(vm.caller_realm).unwrap_or_else(|| target.clone());
    post_message(vm, &source, &target, &c.args)
}

/// HTML §9.3.3 "window post message steps": `source`'s code posts `args` to `target`.
pub fn post_message(vm: &mut Vm, source: &Rc<Host>, target: &Rc<Host>, args: &[Value]) -> R {
    if args.is_empty() {
        return vm.type_error("postMessage needs a message");
    }
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Undefined);
    // postMessage(message, options) or postMessage(message, targetOrigin, transfer).
    let opt = arg(1);
    let (target_origin, transfer) = if matches!(opt, Value::Object(_)) && !is_sequence(vm, &opt) {
        let t = dict_get(vm, &opt, "targetOrigin")?;
        let t = if t.is_undefined() { "/".to_string() } else { rstr(vm, &t)? };
        (t, transfer_list(vm, &opt, true)?)
    } else if opt.is_undefined() {
        ("/".to_string(), Vec::new())
    } else {
        (rstr(vm, &opt)?, transfer_list(vm, &arg(2), false)?)
    };
    // Steps 2–4: the target origin ("/" is the sender's own).
    let own = crate::frames::origin(source);
    let target_now = crate::frames::origin(target);
    let wanted = match target_origin.as_str() {
        "*" => None,
        "/" => Some(own.clone()),
        t => match Url::parse(t) {
            Ok(u) => Some(crate::frames::FrameOrigin::of_url(&u)),
            Err(_) => return throw(vm, "SyntaxError", &format!("'{target_origin}' is not a valid origin")),
        },
    };
    let s = vm.structured_serialize(&arg(0), &transfer)?;
    if wanted.is_some_and(|w| !w.same(&target_now)) {
        // Serialized anyway (errors still throw), then dropped.
        return Ok(Value::Undefined);
    }
    let nav = source.frames.borrow().nav.clone();
    let w = target.window.get().map_or(Value::Undefined, Value::Object);
    target.tasks.borrow_mut().push_back(crate::host::Task::Message(
        w,
        Box::new(s),
        own.serialize(),
        Some(nav),
    ));
    Ok(Value::Undefined)
}

fn is_sequence(vm: &mut Vm, v: &Value) -> bool {
    let Value::Object(o) = v else { return false };
    let key = PropKey::Sym(vm.realm.sym_iterator.clone());
    vm.get(*o, &key).is_ok_and(|f| vm.is_callable(&f))
}

/// Deliver a posted message (the task window.postMessage queued).
pub fn deliver_window_message(
    vm: &mut Vm,
    h: &Rc<Host>,
    target: Value,
    s: Serialized,
    origin: &str,
    from: Option<Rc<crate::frames::Nav>>,
) -> JsResult<()> {
    // MessageEvent.source: the sender's WindowProxy as this window sees it.
    let source = match from.and_then(|n| n.host()) {
        Some(src) => crate::frames::proxy_for(vm, h, &src)?,
        None => Value::Null,
    };
    match vm.structured_deserialize_with_transfer(s) {
        Ok((data, ports)) => fire_message_full(vm, h, target, "message", data, origin, source, ports),
        Err(_) => fire_message(vm, h, target, "messageerror", Value::Null, origin, source),
    }
}

// ---- Worker (the parent's object, HTML §10.2.6) ----

pub fn worker_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("Worker requires 'new'") };
    if c.args.is_empty() {
        return vm.type_error("Worker needs a script URL");
    }
    let href = rstr(vm, &c.arg(0))?;
    let opts = c.arg(1);
    let name = match dict_get(vm, &opts, "name")? {
        Value::Undefined => String::new(),
        v => rstr(vm, &v)?,
    };
    let module = match dict_get(vm, &opts, "type")? {
        Value::Undefined => false,
        v => match rstr(vm, &v)?.as_str() {
            "classic" => false,
            "module" => true,
            other => return vm.type_error(&format!("'{other}' is not a WorkerType")),
        },
    };
    let h = host(vm)?;
    let base = crate::host::base_url(&h, 0);
    let Ok(url) = Url::parse_with_base(&href, Some(&base)) else {
        return throw(vm, "SyntaxError", &format!("'{href}' is not a valid URL"));
    };
    // The script must be same origin (data: and blob: URLs too, the latter by their creator):
    // fetching it fails otherwise, with an error event (HTML §10.2.4 step 12).
    let own = h.url.borrow().origin();
    let same = url.scheme() == "data" || url.scheme() == "blob" || url.origin().same_origin(&own);
    // A blob URL's script is read now (its store is this thread's).
    let source = if url.scheme() == "blob" {
        let req = HttpRequest { method: "GET".into(), url: url.clone(), headers: Vec::new(), body: None };
        match crate::blob::blob_fetch(vm, &h, &req) {
            Some(Ok(r)) => Some(Ok(String::from_utf8_lossy(&r.body).into_owned())),
            Some(Err(e)) => Some(Err(e)),
            None => None,
        }
    } else {
        None
    };
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => p,
        _ => h.proto(C::Worker),
    };
    let id = h.workers.borrow().workers.len() as u64;
    let o = vm.new_host_object(Some(proto), C::Worker as u32, id, false);
    let (to_tx, to_rx) = channel::<ToWorker>();
    let (from_tx, from_rx) = channel::<FromWorker>();
    let link =
        WorkerLink { tx: from_tx, wake: h.embedder.waker(), pending: h.workers.borrow().pending.clone() };
    let net = h.embedder.worker_net();
    let ua = h.embedder.user_agent();
    let origin_ms = h.embedder.time_origin_ms() + h.embedder.now_ms();
    let self_tx = to_tx.clone();
    let interrupt = Arc::new(AtomicBool::new(false));
    let start = WorkerStart {
        url,
        source,
        name,
        module,
        net,
        ua,
        origin_ms,
        rx: to_rx,
        self_tx,
        link: link.clone(),
        interrupt: interrupt.clone(),
    };
    let spawned = same && reserve_thread() && {
        let r = std::thread::Builder::new().name("bb-worker".into()).stack_size(64 << 20).spawn(move || {
            run_worker(start);
            release_thread();
        });
        if r.is_err() {
            release_thread();
        }
        r.is_ok()
    };
    h.workers.borrow_mut().workers.push(WorkerHandle {
        obj: o,
        tx: to_tx,
        rx: from_rx,
        alive: true,
        interrupt,
    });
    if !spawned {
        let why = if same { "too many workers" } else { "a worker's script must be same origin" };
        link.send(FromWorker::LoadFailed(why.into()));
    }
    Ok(Value::Object(o))
}

fn worker_this(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, usize)> {
    let (_, id) = this_of(vm, &c.this, C::Worker)?;
    Ok((host(vm)?, id as usize))
}

pub fn setup_worker(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "postMessage", 1, |vm, c| {
        if c.args.is_empty() {
            return vm.type_error("postMessage needs a message");
        }
        let dict = matches!(c.arg(1), Value::Object(_)) && !is_sequence(vm, &c.arg(1));
        let transfer = transfer_list(vm, &c.arg(1), dict)?;
        let (h, id) = worker_this(vm, &c)?;
        let s = vm.structured_serialize(&c.arg(0), &transfer)?;
        if let Some(w) = h.workers.borrow().workers.get(id).filter(|w| w.alive) {
            let _ = w.tx.send(ToWorker::Message(s));
        }
        Ok(Value::Undefined)
    });
    op(vm, proto, "terminate", 0, |vm, c| {
        let (h, id) = worker_this(vm, &c)?;
        if let Some(w) = h.workers.borrow_mut().workers.get_mut(id) {
            // HTML §10.2.4 "terminate a worker": abort the running script.
            w.interrupt.store(true, Ordering::SeqCst);
            let _ = w.tx.send(ToWorker::Terminate);
            w.alive = false;
        }
        Ok(Value::Undefined)
    });
    crate::events::setup_named_handlers(vm, proto, &["onmessage", "onmessageerror", "onerror"]);
}

/// Deliver what workers sent (the parent's event loop calls this); returns whether any.
pub fn poll(vm: &mut Vm, h: &Host) -> bool {
    let mut got: Vec<(ObjRef, FromWorker)> = Vec::new();
    {
        let mut st = h.workers.borrow_mut();
        let pending = st.pending.clone();
        for w in st.workers.iter_mut() {
            loop {
                match w.rx.try_recv() {
                    Ok(m) => {
                        pending.fetch_sub(1, Ordering::SeqCst);
                        if w.alive {
                            got.push((w.obj, m));
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        w.alive = false;
                        break;
                    }
                }
            }
        }
    }
    let any = !got.is_empty();
    for (o, m) in got {
        let r = match m {
            FromWorker::Message(s) => match vm.structured_deserialize_with_transfer(s) {
                Ok((data, ports)) => fire_message_with_ports(vm, h, Value::Object(o), "message", data, ports),
                Err(_) => fire_message(vm, h, Value::Object(o), "messageerror", Value::Null, "", Value::Null),
            },
            FromWorker::Error(msg) => {
                let e = crate::events::trusted_event(vm, h, C::ErrorEvent, "error", false, true);
                crate::events::set_field(vm, e, "message", js(&msg));
                crate::events::set_field(vm, e, "filename", js(""));
                crate::events::set_field(vm, e, "lineno", Value::Number(0.0));
                crate::events::set_field(vm, e, "colno", Value::Number(0.0));
                crate::events::set_field(vm, e, "error", Value::Null);
                match crate::events::dispatch(vm, h, e, Value::Object(o)) {
                    Ok(true) => {
                        h.console.borrow_mut().push(format!("{msg} (in a worker)"));
                        Ok(())
                    }
                    Ok(false) => Ok(()),
                    Err(e) => Err(e),
                }
            }
            FromWorker::LoadFailed(why) => {
                log::info!("a worker's script couldn't load: {why}");
                crate::events::fire(vm, h, Value::Object(o), "error", false, false).map(|_| ())
            }
            FromWorker::Console(lines) => {
                h.console.borrow_mut().extend(lines.into_iter().map(|l| format!("[worker] {l}")));
                Ok(())
            }
            FromWorker::Closed => {
                if let Some(w) = h.workers.borrow_mut().workers.iter_mut().find(|w| w.obj == o) {
                    w.alive = false;
                }
                Ok(())
            }
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

/// An exception the worker's own handlers didn't take: it goes to the Worker object.
pub fn unhandled_error(h: &Host, msg: &str) {
    if let Some(l) = &h.workers.borrow().link {
        l.send(FromWorker::Error(format!("Uncaught {msg}")));
    }
}

// ---- the worker's side ----

struct WorkerStart {
    url: Url,
    source: Option<Result<String, String>>,
    name: String,
    module: bool,
    net: Option<Arc<dyn WorkerNet>>,
    ua: String,
    origin_ms: f64,
    rx: Receiver<ToWorker>,
    self_tx: Sender<ToWorker>,
    link: WorkerLink,
    interrupt: Arc<AtomicBool>,
}

/// A worker thread's embedder: its clock, the network through the parent's client, and a
/// waker that posts to its own queue.
pub(crate) struct WorkerEmbedder {
    net: Option<Arc<dyn WorkerNet>>,
    start: Instant,
    origin_ms: f64,
    ua: String,
    wake: Waker,
}

impl WorkerEmbedder {
    pub(crate) fn new(net: Option<Arc<dyn WorkerNet>>, origin_ms: f64, ua: String, wake: Waker) -> Self {
        WorkerEmbedder { net, start: Instant::now(), origin_ms, ua, wake }
    }
}

impl Embedder for WorkerEmbedder {
    fn fetch_script(&self, url: &Url) -> Result<String, String> {
        let req = HttpRequest { method: "GET".into(), url: url.clone(), headers: Vec::new(), body: None };
        let r = self.fetch_sync(req)?;
        if !(200..300).contains(&r.status) {
            return Err(format!("HTTP {}", r.status));
        }
        Ok(String::from_utf8_lossy(&r.body).into_owned())
    }
    fn now_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }
    fn time_origin_ms(&self) -> f64 {
        self.origin_ms
    }
    fn user_agent(&self) -> String {
        self.ua.clone()
    }
    fn fetch_sync(&self, req: HttpRequest) -> Result<HttpResponse, String> {
        match &self.net {
            Some(n) => n.fetch(req),
            None => Err("no network in this worker".into()),
        }
    }
    fn worker_net(&self) -> Option<Arc<dyn WorkerNet>> {
        self.net.clone()
    }
    fn waker(&self) -> Option<Waker> {
        Some(self.wake.clone())
    }
}

fn run_worker(s: WorkerStart) {
    let self_tx = std::sync::Mutex::new(s.self_tx);
    let wake: Waker = Arc::new(move || {
        if let Ok(tx) = self_tx.lock() {
            let _ = tx.send(ToWorker::Wake);
        }
    });
    let emb = Rc::new(WorkerEmbedder::new(s.net, s.origin_ms, s.ua, wake));
    let mut w = crate::Window::new_worker(s.url.clone(), emb.clone(), &s.name);
    w.vm.interrupt = Some(s.interrupt.clone());
    w.host.workers.borrow_mut().link = Some(s.link.clone());
    // HTML §10.2.4 "run a worker" step 12: fetch the script.
    let src = match s.source {
        Some(r) => r,
        None => emb.fetch_script(&s.url),
    };
    let src = match src {
        Ok(t) => t,
        Err(e) => {
            s.link.send(FromWorker::LoadFailed(e));
            return;
        }
    };
    if s.module {
        // A module worker: its script is a module (with static imports through the loader).
        w.host.workers.borrow_mut().module = true;
        w.run_module_script(&src, &s.url.href());
    } else {
        w.run_script(&src);
    }
    loop {
        if s.interrupt.load(Ordering::SeqCst) {
            break;
        }
        w.run_tasks();
        let lines = w.take_console();
        if !lines.is_empty() {
            s.link.send(FromWorker::Console(lines));
        }
        if w.host.closing.get() {
            break;
        }
        let wait = if w.has_tasks() {
            Some(Duration::ZERO)
        } else {
            w.next_timer_due().map(|due| Duration::from_secs_f64(((due - emb.now_ms()).max(0.0)) / 1000.0))
        };
        let msg = match wait {
            None => s.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(d) => s.rx.recv_timeout(d),
        };
        match msg {
            Ok(ToWorker::Message(m)) => w.deliver_message(m),
            Ok(ToWorker::Wake) | Err(RecvTimeoutError::Timeout) => {}
            Ok(ToWorker::Terminate) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    s.link.send(FromWorker::Closed);
}

/// Install the DedicatedWorkerGlobalScope members (HTML §10.2.1.2) on global `g`.
pub fn setup_worker_global(vm: &mut Vm, g: ObjRef) {
    op(vm, g, "postMessage", 1, |vm, c| {
        if c.args.is_empty() {
            return vm.type_error("postMessage needs a message");
        }
        let dict = matches!(c.arg(1), Value::Object(_)) && !is_sequence(vm, &c.arg(1));
        let transfer = transfer_list(vm, &c.arg(1), dict)?;
        let s = vm.structured_serialize(&c.arg(0), &transfer)?;
        let h = host(vm)?;
        if let Some(l) = &h.workers.borrow().link {
            l.send(FromWorker::Message(s));
        }
        Ok(Value::Undefined)
    });
    crate::events::setup_named_handlers(vm, g, &["onmessage", "onmessageerror", "onerror"]);
    setup_global_common(vm, g);
}

/// Install the SharedWorkerGlobalScope members (HTML §10.2.1.3) on global `g`.
pub fn setup_shared_worker_global(vm: &mut Vm, g: ObjRef) {
    crate::events::setup_named_handlers(vm, g, &["onconnect", "onerror"]);
    setup_global_common(vm, g);
}

/// WorkerGlobalScope's members (HTML §10.2.1.1, §10.3).
fn setup_global_common(vm: &mut Vm, g: ObjRef) {
    attr(
        vm,
        g,
        "self",
        |vm, _| {
            let h = host(vm)?;
            Ok(h.window.get().map_or(Value::Undefined, Value::Object))
        },
        None,
    );
    // [Replaceable] readonly attribute name: setting it makes an own data property.
    attr(
        vm,
        g,
        "name",
        |vm, _| {
            let h = host(vm)?;
            let n = h.workers.borrow().name.clone();
            Ok(js(&n))
        },
        Some(|vm, c| {
            if let Value::Object(o) = c.this {
                vm.create_data_property(o, PropKey::lit("name"), c.arg(0))?;
            }
            Ok(Value::Undefined)
        }),
    );
    // HTML §10.2.3 close(): discard the queued tasks, and stop.
    op(vm, g, "close", 0, |vm, _| {
        let h = host(vm)?;
        h.closing.set(true);
        h.tasks.borrow_mut().clear();
        h.timers.borrow_mut().clear();
        Ok(Value::Undefined)
    });
    // HTML §10.3.4 importScripts(...urls): fetch and run each, in order.
    op(vm, g, "importScripts", 0, |vm, c| {
        let h = host(vm)?;
        // HTML §10.3.4 step 1: module workers can't importScripts.
        if h.workers.borrow().module {
            return vm.type_error("importScripts() can't be used in a module worker");
        }
        let base = h.url.borrow().clone();
        let mut urls = Vec::with_capacity(c.args.len());
        for a in &c.args {
            let s = rstr(vm, a)?;
            match Url::parse_with_base(&s, Some(&base)) {
                Ok(u) => urls.push(u),
                Err(_) => return throw(vm, "SyntaxError", &format!("'{s}' is not a valid URL")),
            }
        }
        for u in urls {
            let src = match h.embedder.fetch_script(&u) {
                Ok(s) => s,
                Err(e) => return throw(vm, "NetworkError", &format!("couldn't load {}: {e}", u.href())),
            };
            vm.run_script_at(&src, Some(&u.href()))?;
        }
        Ok(Value::Undefined)
    });
    install_worker_location(vm, g);
    // bb-js's shell `print` isn't a web API.
    vm.heap.obj_mut(g).props.remove(&PropKey::lit("print"));
    install_worker_navigator(vm, g);
    crate::window::setup_global_scope(vm, g);
}

fn install_worker_location(vm: &mut Vm, g: ObjRef) {
    let Ok(h) = host(vm) else { return };
    let loc = vm.new_host_object(Some(h.proto(C::WorkerLocation)), C::WorkerLocation as u32, 0, false);
    let url = h.url.borrow().clone();
    // WorkerLocation (HTML §10.3.2): the worker's URL never changes, so its parts are fixed.
    let parts: [(&str, String); 9] = [
        ("href", url.href()),
        ("origin", url.origin().serialize()),
        ("protocol", format!("{}:", url.scheme())),
        ("host", url.host_with_port()),
        ("hostname", url.hostname()),
        ("port", url.port().map(|p| p.to_string()).unwrap_or_default()),
        ("pathname", url.pathname()),
        ("search", url.search()),
        ("hash", url.hash()),
    ];
    for (k, v) in parts {
        crate::idl::constant(vm, loc, k, js(&v));
    }
    let href = js(&url.href());
    let f = bb_js::vm::realm::native_with(
        vm,
        "toString",
        0,
        |vm, c| Ok(crate::idl::callee_slots(vm, c.callee).first().cloned().unwrap_or(Value::Undefined)),
        false,
        vec![href],
    );
    vm.heap
        .obj_mut(loc)
        .props
        .insert(PropKey::lit("toString"), bb_js::vm::heap::Prop::method(Value::Object(f)));
    crate::idl::constant(vm, g, "location", Value::Object(loc));
}

fn install_worker_navigator(vm: &mut Vm, g: ObjRef) {
    let Ok(h) = host(vm) else { return };
    let nav = vm.new_host_object(Some(h.proto(C::WorkerNavigator)), C::WorkerNavigator as u32, 0, false);
    let ua = h.embedder.user_agent();
    let app_version = ua.strip_prefix("Mozilla/").unwrap_or(&ua).to_string();
    for (k, v) in [
        ("userAgent", ua.as_str()),
        ("appVersion", app_version.as_str()),
        ("appName", "Netscape"),
        ("appCodeName", "Mozilla"),
        ("product", "Gecko"),
        ("language", "en-US"),
        ("platform", crate::window::platform()),
    ] {
        crate::idl::constant(vm, nav, k, js(v));
    }
    let langs = vm.new_array(vec![js("en-US"), js("en")]);
    crate::idl::constant(vm, nav, "languages", Value::Object(langs));
    crate::idl::constant(vm, nav, "onLine", Value::Bool(true));
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    crate::idl::constant(vm, nav, "hardwareConcurrency", Value::Number(cores as f64));
    crate::idl::constant(vm, g, "navigator", Value::Object(nav));
}

impl crate::Window {
    /// Deliver a message from the parent (a task on the worker's port message queue).
    pub fn deliver_message(&mut self, s: Serialized) {
        let h = self.host.clone();
        let g = h.window.get().map_or(Value::Undefined, Value::Object);
        self.vm.terminated = false;
        self.vm.fuel = Some(crate::SCRIPT_FUEL);
        let r = match self.vm.structured_deserialize_with_transfer(s) {
            Ok((data, ports)) => fire_message_with_ports(&mut self.vm, &h, g, "message", data, ports),
            Err(_) => fire_message(&mut self.vm, &h, g, "messageerror", Value::Null, "", Value::Null),
        };
        if let Err(e) = r {
            if !self.vm.terminated {
                crate::events::report_exception(&mut self.vm, e);
            }
        }
        self.vm.terminated = false;
        self.vm.run_jobs();
    }
}

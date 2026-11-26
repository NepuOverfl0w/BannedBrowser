//! Shared workers (HTML §10.2.6.4 SharedWorker, §10.2.1.3 SharedWorkerGlobalScope). The
//! shared worker manager is process-wide: a worker is found again by its constructor's
//! origin, its constructor URL and its name, so every window of the process that asks for
//! the same one connects to the same thread. Each `new SharedWorker()` makes an entangled
//! pair of MessagePorts: the outside port is `worker.port`, the inside port travels to the
//! worker thread and arrives with a `connect` event.
//!
//! A shared worker lives while one of the windows that connected to it does (each
//! connection holds a token the window keeps; HTML §10.2.3 "closing orphan workers"), or
//! until it calls `close()`.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};
use bb_url::Url;

use crate::class::C;
use crate::exception::throw;
use crate::host::{dict_get, host, js, rstr, this_of, Embedder, Host, Task};
use crate::idl::attr;
use crate::worker::{FromWorker, WorkerEmbedder, WorkerLink, WorkerNet};

type R = JsResult<Value>;

/// How often a shared worker with nothing to do checks that a window still holds it.
const OWNER_CHECK: Duration = Duration::from_millis(250);

/// A connection on its way to a shared worker: the inside port, the way back to the
/// SharedWorker object's window, and that window's token.
pub(crate) struct Connect {
    port: Box<dyn std::any::Any + Send>,
    link: WorkerLink,
    owner: Weak<()>,
}

pub(crate) enum ToShared {
    Connect(Connect),
    /// Something for the worker's loop to look at.
    Wake,
}

/// A running shared worker, as the manager knows it.
struct Entry {
    origin: String,
    url: String,
    name: String,
    module: bool,
    credentials: String,
    tx: Sender<ToShared>,
    /// Set once the worker stops (closing, or every window left).
    closing: Arc<AtomicBool>,
}

/// The shared worker manager's list of SharedWorkerGlobalScopes.
static MANAGER: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// A SharedWorker object's side, in its window's Host.
pub struct SharedHandle {
    pub obj: ObjRef,
    rx: Receiver<FromWorker>,
    /// While the window holds this, the worker has an owner.
    _owner: Arc<()>,
    /// The worker's closing flag (set when it stops; set from the start when it never ran).
    closing: Arc<AtomicBool>,
}

impl SharedHandle {
    pub fn running(&self) -> bool {
        !self.closing.load(Ordering::SeqCst)
    }
}

/// `new SharedWorker(scriptURL, options)` (HTML §10.2.6.4).
pub fn shared_worker_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("SharedWorker requires 'new'") };
    if c.args.is_empty() {
        return vm.type_error("SharedWorker needs a script URL");
    }
    let href = rstr(vm, &c.arg(0))?;
    // (DOMString or SharedWorkerOptions): a string is the name.
    let opts = c.arg(1);
    let (name, module, credentials) = match &opts {
        Value::Undefined | Value::Null | Value::Object(_) => {
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
            let credentials = match dict_get(vm, &opts, "credentials")? {
                Value::Undefined => "same-origin".to_string(),
                v => match rstr(vm, &v)?.as_str() {
                    c @ ("omit" | "same-origin" | "include") => c.to_string(),
                    other => return vm.type_error(&format!("'{other}' is not a RequestCredentials")),
                },
            };
            (name, module, credentials)
        }
        v => (rstr(vm, v)?, false, "same-origin".to_string()),
    };
    let h = host(vm)?;
    let base = crate::host::base_url(&h, 0);
    let Ok(url) = Url::parse_with_base(&href, Some(&base)) else {
        return throw(vm, "SyntaxError", &format!("'{href}' is not a valid URL"));
    };
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => p,
        _ => h.proto(C::SharedWorker),
    };
    let id = h.workers.borrow().shared.len() as u64;
    let o = vm.new_host_object(Some(proto), C::SharedWorker as u32, id, false);
    let (port, inside) = crate::port::pair_for_transfer(vm, &h);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = vec![Value::Object(port)];
    }
    let (from_tx, from_rx) = channel::<FromWorker>();
    let link = WorkerLink::new(from_tx, h.embedder.waker(), h.workers.borrow().pending.clone());
    let owner = Arc::new(());
    let error = |h: &Host| {
        h.tasks.borrow_mut().push_back(Task::Event(Value::Object(o), "error".into(), false, false))
    };
    let handle =
        |closing: Arc<AtomicBool>| SharedHandle { obj: o, rx: from_rx, _owner: owner.clone(), closing };
    let never = || Arc::new(AtomicBool::new(true));
    // The script must be same origin (data: and blob: URLs too, the latter by their creator):
    // fetching it fails otherwise.
    let own = h.url.borrow().origin();
    if !(url.scheme() == "data" || url.scheme() == "blob" || url.origin().same_origin(&own)) {
        h.workers.borrow_mut().shared.push(handle(never()));
        error(&h);
        return Ok(Value::Object(o));
    }
    let origin = own.serialize();
    let connect = Connect { port: inside, link, owner: Arc::downgrade(&owner) };
    let Ok(mut mgr) = MANAGER.lock() else {
        h.workers.borrow_mut().shared.push(handle(never()));
        error(&h);
        return Ok(Value::Object(o));
    };
    mgr.retain(|e| !e.closing.load(Ordering::SeqCst));
    let found = mgr.iter().position(|e| e.origin == origin && e.url == url.href() && e.name == name);
    let connect = match found {
        // Another type or credentials mode than the running worker's: an error event, and
        // no connection.
        Some(i) if mgr[i].module != module || mgr[i].credentials != credentials => {
            drop(mgr);
            h.workers.borrow_mut().shared.push(handle(never()));
            error(&h);
            return Ok(Value::Object(o));
        }
        Some(i) => {
            let closing = mgr[i].closing.clone();
            match mgr[i].tx.send(ToShared::Connect(connect)) {
                Ok(()) => {
                    drop(mgr);
                    h.workers.borrow_mut().shared.push(handle(closing));
                    return Ok(Value::Object(o));
                }
                // Its thread is gone: start another.
                Err(std::sync::mpsc::SendError(m)) => {
                    mgr.remove(i);
                    match m {
                        ToShared::Connect(c) => c,
                        ToShared::Wake => return Ok(Value::Object(o)),
                    }
                }
            }
        }
        None => connect,
    };
    // A new SharedWorkerGlobalScope (HTML §10.2.4 "run a worker" with is shared true).
    let source = if url.scheme() == "blob" {
        let req = crate::net::HttpRequest {
            method: "GET".into(),
            url: url.clone(),
            headers: Vec::new(),
            body: None,
        };
        match crate::blob::blob_fetch(vm, &h, &req) {
            Some(Ok(r)) => Some(Ok(String::from_utf8_lossy(&r.body).into_owned())),
            Some(Err(e)) => Some(Err(e)),
            None => None,
        }
    } else {
        None
    };
    let (tx, rx) = channel::<ToShared>();
    let closing = Arc::new(AtomicBool::new(false));
    let start = SharedStart {
        url: url.clone(),
        source,
        name: name.clone(),
        module,
        net: h.embedder.worker_net(),
        ua: h.embedder.user_agent(),
        origin_ms: h.embedder.time_origin_ms() + h.embedder.now_ms(),
        rx,
        self_tx: tx.clone(),
        closing: closing.clone(),
        first: connect,
    };
    if !crate::worker::reserve_thread() {
        drop(mgr);
        h.workers.borrow_mut().shared.push(handle(never()));
        error(&h);
        return Ok(Value::Object(o));
    }
    let spawned =
        std::thread::Builder::new().name("bb-shared-worker".into()).stack_size(64 << 20).spawn(move || {
            run_shared(start);
            crate::worker::release_thread();
        });
    if spawned.is_err() {
        crate::worker::release_thread();
        drop(mgr);
        h.workers.borrow_mut().shared.push(handle(never()));
        error(&h);
        return Ok(Value::Object(o));
    }
    mgr.push(Entry { origin, url: url.href(), name, module, credentials, tx, closing: closing.clone() });
    drop(mgr);
    h.workers.borrow_mut().shared.push(handle(closing));
    // "run a worker": the outside port's message queue is enabled.
    crate::port::enable(&h, port);
    Ok(Value::Object(o))
}

fn shared_this(vm: &mut Vm, c: &NativeCall) -> JsResult<ObjRef> {
    this_of(vm, &c.this, C::SharedWorker)?;
    match c.this {
        Value::Object(o) => Ok(o),
        _ => vm.type_error("not a SharedWorker"),
    }
}

pub fn setup_shared_worker(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "port",
        |vm, c| {
            let o = shared_this(vm, &c)?;
            Ok(vm.host_obj(o).and_then(|ho| ho.slots.first().cloned()).unwrap_or(Value::Null))
        },
        None,
    );
    crate::events::setup_named_handlers(vm, proto, &["onerror"]);
}

/// What shared workers sent this window (load failures, console lines); whether any.
pub fn poll(vm: &mut Vm, h: &Host) -> bool {
    let mut got: Vec<(ObjRef, FromWorker)> = Vec::new();
    {
        let st = h.workers.borrow();
        for sh in &st.shared {
            while let Ok(m) = sh.rx.try_recv() {
                crate::port::uncount(&st.pending);
                got.push((sh.obj, m));
            }
        }
    }
    let any = !got.is_empty();
    for (o, m) in got {
        let r = match m {
            FromWorker::LoadFailed(why) => {
                log::info!("a shared worker's script couldn't load: {why}");
                crate::events::fire(vm, h, Value::Object(o), "error", false, false).map(|_| ())
            }
            FromWorker::Console(lines) => {
                h.console.borrow_mut().extend(lines.into_iter().map(|l| format!("[shared worker] {l}")));
                Ok(())
            }
            _ => Ok(()),
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

// ---- the worker's side ----

struct SharedStart {
    url: Url,
    source: Option<Result<String, String>>,
    name: String,
    module: bool,
    net: Option<Arc<dyn WorkerNet>>,
    ua: String,
    origin_ms: f64,
    rx: Receiver<ToShared>,
    self_tx: Sender<ToShared>,
    closing: Arc<AtomicBool>,
    first: Connect,
}

/// Leave the manager's list (under its lock, so no connection is sent to a worker that is
/// going away): the connections that came in meanwhile, if any, instead.
fn leave_manager(closing: &AtomicBool, rx: &Receiver<ToShared>) -> Vec<Connect> {
    let Ok(mut mgr) = MANAGER.lock() else {
        closing.store(true, Ordering::SeqCst);
        return Vec::new();
    };
    let mut late = Vec::new();
    while let Ok(m) = rx.try_recv() {
        if let ToShared::Connect(c) = m {
            late.push(c);
        }
    }
    if late.is_empty() {
        closing.store(true, Ordering::SeqCst);
        mgr.retain(|e| !std::ptr::eq(Arc::as_ptr(&e.closing), closing));
    }
    late
}

fn run_shared(s: SharedStart) {
    let self_tx = Mutex::new(s.self_tx);
    let wake: crate::worker::Waker = Arc::new(move || {
        if let Ok(tx) = self_tx.lock() {
            let _ = tx.send(ToShared::Wake);
        }
    });
    let emb = Rc::new(WorkerEmbedder::new(s.net, s.origin_ms, s.ua, wake));
    let mut w = crate::Window::new_shared_worker(s.url.clone(), emb.clone(), &s.name);
    let mut links: Vec<WorkerLink> = vec![s.first.link.clone()];
    // HTML §10.2.4 "run a worker" step 12: fetch the script.
    let src = match s.source {
        Some(r) => r,
        None => emb.fetch_script(&s.url),
    };
    let src = match src {
        Ok(t) => t,
        Err(e) => {
            // Every window waiting for this worker gets the error.
            loop {
                for l in &links {
                    let _ = l.send(FromWorker::LoadFailed(e.clone()));
                }
                let late = leave_manager(&s.closing, &s.rx);
                if late.is_empty() {
                    return;
                }
                links = late.into_iter().map(|c| c.link).collect();
            }
        }
    };
    if s.module {
        w.host.workers.borrow_mut().module = true;
        w.run_module_script(&src, &s.url.href());
    } else {
        w.run_script(&src);
    }
    let mut owners: Vec<Weak<()>> = Vec::new();
    links.clear();
    connect(&mut w, s.first, &mut owners, &mut links);
    loop {
        w.run_tasks();
        let lines = w.take_console();
        if !lines.is_empty() {
            links.retain(|l| l.send(FromWorker::Console(lines.clone())));
        }
        if w.host.closing.get() {
            let late = leave_manager(&s.closing, &s.rx);
            // Connections that raced with close() find no worker (HTML: the closing flag
            // keeps them from matching); their windows' ports just stay quiet.
            drop(late);
            s.closing.store(true, Ordering::SeqCst);
            break;
        }
        let wait = if w.has_tasks() {
            Duration::ZERO
        } else {
            w.next_timer_due()
                .map(|due| Duration::from_secs_f64(((due - emb.now_ms()).max(0.0)) / 1000.0))
                .map_or(OWNER_CHECK, |d| d.min(OWNER_CHECK))
        };
        match s.rx.recv_timeout(wait) {
            Ok(ToShared::Connect(c)) => connect(&mut w, c, &mut owners, &mut links),
            Ok(ToShared::Wake) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // HTML §10.2.3: a worker no window holds closes.
        owners.retain(|o| o.strong_count() > 0);
        if owners.is_empty() {
            let late = leave_manager(&s.closing, &s.rx);
            if late.is_empty() {
                break;
            }
            for c in late {
                connect(&mut w, c, &mut owners, &mut links);
            }
        }
    }
}

/// A connection arrives: its inside port, in a `connect` MessageEvent (data "", ports and
/// source the port), as "run a worker" and the SharedWorker constructor queue it.
fn connect(w: &mut crate::Window, c: Connect, owners: &mut Vec<Weak<()>>, links: &mut Vec<WorkerLink>) {
    owners.push(c.owner);
    links.push(c.link);
    let h = w.host.clone();
    let vm = &mut w.vm;
    vm.terminated = false;
    vm.fuel = Some(crate::SCRIPT_FUEL);
    let r = crate::port::receive(vm, c.port).and_then(|port| {
        let g = h.window.get().map_or(Value::Undefined, Value::Object);
        crate::worker::fire_message_full(vm, &h, g, "connect", js(""), "", port.clone(), vec![port])
    });
    if let Err(e) = r {
        if !vm.terminated {
            crate::events::report_exception(vm, e);
        }
    }
    vm.terminated = false;
    vm.run_jobs();
}

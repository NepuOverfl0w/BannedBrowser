//! `bb-webapi` — Web APIs for BannedBrowser's JavaScript engine (milestone E).
//!
//! A [`Window`] is one browsing context's global: a bb-js VM whose global object is a
//! Window, with the DOM Standard's interfaces over bb-dom documents (`dom.rs`, `tree.rs`,
//! `collections.rs`, `attr.rs`), HTML's element interfaces (`html.rs`, `forms.rs`), events
//! (`events.rs`), timers and the other Window members (`window.rs`), and the parser
//! running scripts as it goes (`loader.rs`). The embedder supplies fetching and the clock.
//!
//! Event loop (HTML §8.1.7): the embedder calls [`Window::run_tasks`] when a timer is due
//! or work was queued; each task is followed by a microtask checkpoint.

pub mod activation;
pub mod attr;
pub mod blob;
pub mod canvas;
pub mod class;
pub mod collections;
pub mod css;
pub mod dom;
pub mod events;
pub mod exception;
pub mod forms;
pub mod frames;
pub mod geometry;
pub mod host;
pub mod html;
pub mod idl;
pub mod loader;
pub mod mutation;
pub mod net;
pub mod port;
pub mod range;
pub mod selection;
pub mod storage;
pub mod traversal;
pub mod tree;
pub mod url;
pub mod window;
pub mod worker;
pub mod xml;

use std::rc::Rc;

use bb_dom::Document;
use bb_js::{Value, Vm};
use bb_url::Url;

pub use host::{BoxInfo, Embedder, Host, NodeRef, ReadyState};

/// Instructions one script (or task) may run before it is stopped, so a runaway page can't
/// hang its tab.
pub const SCRIPT_FUEL: u64 = 2_000_000_000;

pub struct Window {
    pub vm: Vm,
    pub host: Rc<Host>,
}

fn report(vm: &mut Vm, e: Value) {
    events::report_exception(vm, e);
}

impl Window {
    /// A new window whose document (empty, at `url`) is document 0.
    pub fn new(url: Url, embedder: Rc<dyn Embedder>) -> Window {
        Self::build(url, embedder, None)
    }

    /// A dedicated worker's global scope (HTML §10.2.4 "run a worker"), named `name`.
    pub fn new_worker(url: Url, embedder: Rc<dyn Embedder>, name: &str) -> Window {
        Self::build(url, embedder, Some(name))
    }

    fn build(url: Url, embedder: Rc<dyn Embedder>, worker: Option<&str>) -> Window {
        let mut doc = Document::new();
        doc.url = url.href();
        let host = Rc::new(Host::new(doc, url, embedder));
        if let Some(n) = worker {
            host.workers.borrow_mut().name = n.to_string();
            host.ready_state.set(ReadyState::Complete);
        }
        let mut vm = Vm::new();
        // A worker may block (Atomics.wait); a window may not.
        vm.can_block = worker.is_some();
        vm.host = Some(host.clone());
        vm.host_exotic = Some(Rc::new(collections::Exotic));
        frames::init_top(&host);
        // Every window of the page holds objects for the collector.
        let family = host.frames.borrow().family.clone();
        vm.host_trace = Some(Box::new(move |out| {
            for w in family.borrow().iter() {
                if let Some(h) = w.upgrade() {
                    h.trace(out);
                }
            }
        }));
        vm.host_report = Some(report);
        vm.host_serialize = Some(worker::host_serialize);
        vm.host_deserialize = Some(worker::host_deserialize);
        vm.host_clone_error = Some(worker::clone_error);
        vm.host_transferable = Some(port::transferable);
        vm.host_transfer = Some(port::transfer);
        vm.host_receive = Some(port::receive);
        host.window.set(Some(vm.realm.global));
        class::install_scope(&mut vm, &host, worker.is_some());
        Window { vm, host }
    }

    /// Parse `html` as the document, running its scripts, up to DOMContentLoaded.
    pub fn load_html(&mut self, html: &str) {
        let h = self.host.clone();
        loader::load(&mut self.vm, &h, html);
    }

    /// Fire the load event (after the embedder loaded the page's subresources).
    pub fn fire_load(&mut self) {
        let h = self.host.clone();
        loader::fire_load(&mut self.vm, &h);
    }

    /// Run a classic script in the window (e.g. from a `javascript:` URL or a test).
    pub fn run_script(&mut self, src: &str) {
        let key = self.host.url.borrow().href();
        loader::run_source(&mut self.vm, src, &key, false);
        let h = self.host.clone();
        loader::register_handlers(&mut self.vm, &h);
    }

    /// When the next timer is due (embedder clock, ms), if any, in any of the page's windows.
    pub fn next_timer_due(&self) -> Option<f64> {
        frames::family(&self.host)
            .iter()
            .flat_map(|h| h.timers.borrow().iter().map(|t| t.due).collect::<Vec<_>>())
            .reduce(f64::min)
    }

    /// Whether a worker this global (or one of its frames) started is still running.
    pub fn has_live_workers(&self) -> bool {
        frames::family(&self.host).iter().any(|h| h.workers.borrow().workers.iter().any(|w| w.alive))
    }

    /// Whether tasks are waiting (queued tasks or animation frame callbacks) in any window.
    pub fn has_tasks(&self) -> bool {
        self.has_queued_tasks()
            || frames::family(&self.host).iter().any(|h| !h.frame_callbacks.borrow().is_empty())
    }

    /// Whether tasks, messages or finished requests are waiting (animation frames aside, which
    /// wait for the next frame).
    pub fn has_queued_tasks(&self) -> bool {
        frames::family(&self.host).iter().any(|h| {
            !h.tasks.borrow().is_empty()
                || !h.pending.borrow().is_empty()
                || h.workers.borrow().has_messages()
                || port::has_messages(h)
        })
    }

    /// Whether animation frame callbacks are waiting in any window.
    pub fn has_animation_frames(&self) -> bool {
        frames::family(&self.host).iter().any(|h| !h.frame_callbacks.borrow().is_empty())
    }

    /// Run queued tasks and the timers due now, in every window of the page (each in its
    /// realm); returns whether anything ran.
    pub fn run_tasks(&mut self) -> bool {
        let mut ran = false;
        for h in frames::family(&self.host) {
            let realm = h.frames.borrow().realm;
            if Rc::ptr_eq(&h, &self.host) {
                ran |= run_host_tasks(&mut self.vm, &h);
            } else {
                ran |= self.vm.try_in_realm(realm, |vm| run_host_tasks(vm, &h)).unwrap_or(false);
            }
        }
        ran
    }

    /// Run the animation frame callbacks (HTML §8.1.7.3 "run the animation frame callbacks")
    /// of every window.
    pub fn run_animation_frames(&mut self) -> bool {
        let mut ran = false;
        for h in frames::family(&self.host) {
            let cbs = std::mem::take(&mut *h.frame_callbacks.borrow_mut());
            if cbs.is_empty() {
                continue;
            }
            ran = true;
            let realm = h.frames.borrow().realm;
            let now = h.embedder.now_ms();
            for (_, f) in cbs {
                self.vm.try_in_realm(realm, |vm| {
                    begin_task(vm);
                    let r = vm.call(&f, Value::Undefined, vec![Value::Number(now)]).map(|_| ());
                    end_task(vm, &h, r);
                });
            }
        }
        ran
    }

    fn begin_task(&mut self) {
        begin_task(&mut self.vm);
    }

    fn end_task(&mut self, r: Result<(), Value>) {
        let h = self.host.clone();
        end_task(&mut self.vm, &h, r);
    }

    /// Run the frame navigations waiting in the page's windows, and those they start, until
    /// none are left: the page's load waits for its frames (HTML §13.2.7 "the end").
    pub fn settle_frames(&mut self) {
        for _ in 0..=8 {
            let mut any = false;
            for h in frames::family(&self.host) {
                let navs: Vec<(bb_dom::NodeId, frames::NavTarget)> = {
                    let mut tasks = h.tasks.borrow_mut();
                    let mut out = Vec::new();
                    let mut rest = std::collections::VecDeque::new();
                    for t in tasks.drain(..) {
                        match t {
                            host::Task::Navigate(c, n) => out.push((c, n)),
                            other => rest.push_back(other),
                        }
                    }
                    *tasks = rest;
                    out
                };
                if navs.is_empty() {
                    continue;
                }
                any = true;
                let realm = h.frames.borrow().realm;
                for (c, t) in navs {
                    self.vm.try_in_realm(realm, |vm| {
                        begin_task(vm);
                        let r = frames::navigate_now(vm, &h, c, t);
                        end_task(vm, &h, r);
                    });
                }
            }
            if !any {
                break;
            }
        }
    }

    /// Show a document the embedder fetched in the frame of `container` in `parent`'s
    /// document (a link or form targeting the frame, or session history): see
    /// `frames::install`. Returns the frame's new window.
    pub fn load_frame(
        &mut self,
        parent: &Rc<Host>,
        container: bb_dom::NodeId,
        url: Url,
        content_type: &str,
        body: Vec<u8>,
    ) -> Option<Rc<Host>> {
        let realm = parent.frames.borrow().realm;
        let origin = frames::origin_for(parent, &url);
        self.vm
            .try_in_realm(realm, |vm| {
                begin_task(vm);
                let r = frames::install(vm, parent, container, url, content_type, body, origin);
                end_task(vm, parent, Ok(()));
                r
            })
            .flatten()
    }

    /// Fire the submit event at form `form` of the window's document, with its submitter;
    /// false if a listener canceled it (HTML §4.10.21.3 "submit" step 6.3).
    pub fn fire_submit(&mut self, form: bb_dom::NodeId, submitter: Option<bb_dom::NodeId>) -> bool {
        let h = self.host.clone();
        self.begin_task();
        let r = events::fire_submit(
            &mut self.vm,
            &h,
            NodeRef { doc: 0, id: form },
            submitter.map(|id| NodeRef { doc: 0, id }),
        );
        let proceed = *r.as_ref().unwrap_or(&true);
        self.end_task(r.map(|_| ()));
        proceed
    }

    /// The form a script asked to submit (`form.submit()`, or `requestSubmit()` not canceled).
    pub fn take_pending_submit(&self) -> Option<bb_dom::NodeId> {
        self.host.pending_submit.take().filter(|n| n.doc == 0).map(|n| n.id)
    }

    /// Dispatch a trusted event at node `target` of the window's document (or at the window
    /// when `target` is None); `fields` sets the event's init fields. Returns false if a
    /// listener canceled it (the embedder then skips the default action).
    pub fn dispatch_ui(
        &mut self,
        class: class::C,
        ty: &str,
        target: Option<bb_dom::NodeId>,
        bubbles: bool,
        cancelable: bool,
        fields: &[(&str, Value)],
    ) -> bool {
        let h = self.host.clone();
        self.begin_task();
        let e = events::trusted_event(&mut self.vm, &h, class, ty, bubbles, cancelable);
        for (k, v) in fields {
            events::set_field(&mut self.vm, e, k, v.clone());
        }
        let t = match target {
            Some(n) => host::wrap(&mut self.vm, &h, NodeRef { doc: 0, id: n }),
            None => Ok(h.window.get().map_or(Value::Undefined, Value::Object)),
        };
        let r = match t {
            Ok(t) => events::dispatch(&mut self.vm, &h, e, t),
            Err(e) => Err(e),
        };
        let proceed = *r.as_ref().unwrap_or(&true);
        self.end_task(r.map(|_| ()));
        proceed
    }

    /// Whether scripts changed the DOM since the last call (the embedder restyles).
    pub fn take_dirty(&self) -> bool {
        self.host.dirty.replace(false)
    }

    /// The window's document (document 0), for the embedder to read.
    pub fn document(&self) -> std::cell::Ref<'_, Document> {
        std::cell::Ref::map(self.host.docs.borrow(), |d| &d[0])
    }

    /// Swap the window's document with `doc` (the embedder lends its document for layout).
    pub fn swap_document(&self, doc: &mut Document) {
        std::mem::swap(&mut self.host.docs.borrow_mut()[0], doc);
    }

    /// Lines written to the console (and uncaught errors), taken.
    pub fn take_console(&self) -> Vec<String> {
        std::mem::take(&mut *self.host.console.borrow_mut())
    }
}

fn begin_task(vm: &mut Vm) {
    vm.terminated = false;
    vm.fuel = Some(SCRIPT_FUEL);
}

fn end_task(vm: &mut Vm, h: &Rc<Host>, r: Result<(), Value>) {
    if let Err(e) = r {
        if vm.terminated {
            log::warn!("a task ran too long and was stopped");
        } else {
            events::report_exception(vm, e);
        }
    }
    vm.terminated = false;
    vm.run_jobs();
    vm.terminated = false;
    loader::register_handlers(vm, h);
    // A frame's own navigation (location.href = …) is its parent's to do.
    if frames::parent_of(h).is_some() {
        let to = h.pending_navigation.borrow_mut().take();
        if let Some(url) = to.and_then(|t| bb_url::Url::parse(&t).ok()) {
            frames::navigate(h, url);
        }
    }
}

/// One window's queued tasks, messages, finished requests and due timers (HTML §8.1.7.3).
fn run_host_tasks(vm: &mut Vm, h: &Rc<Host>) -> bool {
    let mut ran = false;
    loop {
        if h.frames.borrow().destroyed {
            return ran;
        }
        let task = h.tasks.borrow_mut().pop_front();
        let Some(task) = task else { break };
        ran = true;
        begin_task(vm);
        let r = match task {
            host::Task::Call(f, this, args) => vm.call(&f, this, args).map(|_| ()),
            host::Task::Event(t, ty, bubbles, cancelable) => {
                events::fire(vm, h, t, &ty, bubbles, cancelable).map(|_| ())
            }
            host::Task::Script(s) => {
                loader::run_script_task(vm, h, s);
                Ok(())
            }
            host::Task::Fetch(id, req) => {
                let r = net::fetch_now(vm, h, *req);
                net::complete(vm, h, id, r)
            }
            host::Task::Read(t) => blob::run_read_task(vm, h, *t),
            host::Task::Message(t, s, origin, from) => {
                worker::deliver_window_message(vm, h, t, *s, &origin, from)
            }
            host::Task::Navigate(container, target) => frames::navigate_now(vm, h, container, target),
        };
        end_task(vm, h, r);
    }
    // Messages and errors from this global's workers, and messages to its ports.
    if worker::poll(vm, h) {
        ran = true;
    }
    if port::poll(vm, h) {
        ran = true;
    }
    if port::poll_broadcasts(vm, h) {
        ran = true;
    }
    // Background requests that finished.
    for (id, r) in h.embedder.poll_fetches() {
        ran = true;
        begin_task(vm);
        let r = net::complete(vm, h, id, r);
        end_task(vm, h, r);
    }
    // Timers in due order (HTML §8.6 step 12: by due time, then by when they were set).
    loop {
        let now = h.embedder.now_ms();
        let next = {
            let timers = h.timers.borrow();
            timers
                .iter()
                .filter(|t| t.due <= now)
                .min_by(|a, b| a.due.total_cmp(&b.due).then(a.seq.cmp(&b.seq)))
                .map(|t| t.id)
        };
        let Some(id) = next else { break };
        let (cb, args) = {
            let mut timers = h.timers.borrow_mut();
            let Some(i) = timers.iter().position(|t| t.id == id) else { break };
            let t = &mut timers[i];
            let cb = (t.callback.clone(), t.args.clone());
            match t.interval {
                Some(iv) => {
                    t.due = now + iv.max(1.0);
                    let seq = h.timer_seq.get();
                    h.timer_seq.set(seq + 1);
                    t.seq = seq;
                }
                None => {
                    timers.remove(i);
                }
            }
            cb
        };
        ran = true;
        begin_task(vm);
        let r = match cb {
            Value::String(s) => {
                let src = bb_js::vm::to_rust(&s);
                let key = h.url.borrow().href();
                vm.run_script_at(&src, Some(&key)).map(|_| ())
            }
            f => vm.call(&f, Value::Undefined, args).map(|_| ()),
        };
        end_task(vm, h, r);
    }
    ran
}

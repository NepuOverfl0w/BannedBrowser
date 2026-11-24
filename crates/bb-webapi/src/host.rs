//! The state the Web API natives share (`Vm::host`), and the helpers they use to reach it.
//!
//! Natives never hold a `RefCell` borrow across a call back into JavaScript (argument
//! conversions included): they convert their arguments first, then borrow, then release.
//! A borrow that fails anyway (a host bug) throws instead of panicking.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use bb_dom::{Document, NodeId};
use bb_html::TreeBuilder;
use bb_js::vm::heap::ObjRef;
use bb_js::vm::{to_rust, PropKey};
use bb_js::{JsResult, Value, Vm};
use bb_url::Url;

use crate::class::{self, C};

/// A node: which document's arena, and its index there.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeRef {
    pub doc: u32,
    pub id: NodeId,
}

impl NodeRef {
    pub fn pack(self) -> u64 {
        (self.doc as u64) << 32 | self.id.0 as u64
    }
    pub fn unpack(v: u64) -> NodeRef {
        NodeRef { doc: (v >> 32) as u32, id: NodeId(v as u32) }
    }
    pub fn with(self, id: NodeId) -> NodeRef {
        NodeRef { doc: self.doc, id }
    }
}

/// What the embedder (the browser's page, or a test runner) provides.
pub trait Embedder {
    /// Fetch a classic script's source (HTML §4.12.1.1 "fetch a classic script").
    fn fetch_script(&self, url: &Url) -> Result<String, String>;
    /// Milliseconds since the time origin (HTML §8.1.7.1 DOMHighResTimeStamp); monotonic.
    fn now_ms(&self) -> f64;
    /// The time origin, in milliseconds since the epoch (performance.timeOrigin).
    fn time_origin_ms(&self) -> f64 {
        0.0
    }
    /// The viewport size in CSS pixels (innerWidth, innerHeight).
    fn viewport(&self) -> (f64, f64) {
        (1024.0, 768.0)
    }
    /// navigator.userAgent.
    fn user_agent(&self) -> String {
        "Mozilla/5.0".to_string()
    }
    /// Do an HTTP request now, blocking (synchronous XMLHttpRequest, and requests when
    /// `start_fetch` declines).
    fn fetch_sync(&self, _req: crate::net::HttpRequest) -> Result<crate::net::HttpResponse, String> {
        Err("no network".to_string())
    }
    /// Start an HTTP request in the background; its result comes back from `poll_fetches`
    /// with this id. Returns false if the embedder can't (the request is then done with
    /// `fetch_sync` in a task).
    fn start_fetch(&self, _id: u64, _req: crate::net::HttpRequest) -> bool {
        false
    }
    /// Background requests that finished since the last call.
    fn poll_fetches(&self) -> Vec<(u64, Result<crate::net::HttpResponse, String>)> {
        Vec::new()
    }
    /// Whether the embedder keeps localStorage (else it lives with the window).
    fn has_storage(&self) -> bool {
        false
    }
    /// The page's origin's localStorage area.
    fn storage_area(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    fn set_storage_area(&self, _area: Vec<(String, String)>) {}
    /// The page's cookies as document.cookie sees them.
    fn cookie(&self) -> String {
        String::new()
    }
    /// Store a cookie set through document.cookie.
    fn set_cookie(&self, _cookie: &str) {}
    /// An element's computed values (CSSOM §9 getComputedStyle), as (property, CSS text).
    fn computed_style(&self, _doc: &Document, _node: NodeId) -> Vec<(String, String)> {
        Vec::new()
    }
    /// An element's boxes (CSSOM View §6), if it has a layout box.
    fn box_info(&self, _doc: &Document, _node: NodeId) -> Option<BoxInfo> {
        None
    }
    /// The element at a viewport point (CSSOM View §5 elementFromPoint).
    fn element_at(&self, _doc: &Document, _x: f64, _y: f64) -> Option<NodeId> {
        None
    }
}

/// Layout facts about an element's box, in CSS px.
#[derive(Clone, Copy, Debug, Default)]
pub struct BoxInfo {
    /// The border box (x, y, width, height) in document coordinates.
    pub border: [f64; 4],
    /// The padding box relative to the border box: (left border, top border, width, height).
    pub client: [f64; 4],
    /// The scrollable overflow size (width, height).
    pub scroll: (f64, f64),
}

/// An event listener (DOM §2.7).
pub struct Listener {
    pub ty: String,
    pub callback: Value,
    pub capture: bool,
    pub once: bool,
    pub passive: bool,
    /// Set when removed, so a dispatch already holding the list skips it (DOM §2.10 step 2.5).
    pub removed: Rc<Cell<bool>>,
    /// The listener standing for an event handler (HTML §8.1.8.1): its callback is looked
    /// up in `Host::handlers` when it runs.
    pub handler: bool,
    /// The AbortSignal that removes it (DOM §2.7 addEventListener `signal`).
    pub signal: Option<ObjRef>,
}

/// A timer (HTML §8.6): when it is due, and its callback.
pub struct Timer {
    pub id: u32,
    pub due: f64,
    /// Set for setInterval: the repeat period.
    pub interval: Option<f64>,
    pub callback: Value,
    pub args: Vec<Value>,
    /// Insertion order, for timers due at the same time.
    pub seq: u64,
}

/// A queued task (HTML §8.1.7.1).
pub enum Task {
    /// Call a function.
    Call(Value, Value, Vec<Value>),
    /// Fire a simple event named `.1` at target `.0` (bubbles, cancelable).
    Event(Value, String, bool, bool),
    /// A request the embedder can only do synchronously: do it now (`fetch_sync`).
    Fetch(u64, Box<crate::net::HttpRequest>),
    /// Fetch and run a script element a script inserted (HTML §4.12.1.1: such external
    /// scripts are async).
    Script(NodeRef),
    /// A step of a FileReader read (File API).
    Read(Box<crate::blob::ReadTask>),
}

/// What a live (or static) collection object lists.
#[derive(Clone, Debug)]
pub enum CollKind {
    /// Node.childNodes.
    ChildNodes(NodeRef),
    /// ParentNode.children.
    Children(NodeRef),
    /// getElementsByTagName (qualified name; "*" for all).
    TagName(NodeRef, String),
    /// getElementsByTagNameNS (namespace or None for any ("*"), local name or "*").
    TagNameNs(NodeRef, Option<Option<String>>, String),
    /// getElementsByClassName.
    ClassName(NodeRef, Vec<String>),
    /// querySelectorAll's static NodeList.
    Static(Vec<NodeRef>),
    /// Element.attributes (NamedNodeMap).
    Attributes(NodeRef),
    /// Element.classList (DOMTokenList over `class`).
    ClassList(NodeRef),
    /// HTMLElement.dataset.
    Dataset(NodeRef),
    /// Element.style (the declarations of its style attribute).
    InlineStyle(NodeRef),
    /// getComputedStyle's read-only declarations: (element, its computed values).
    Computed(NodeRef, Vec<(String, String)>),
    /// HTMLSelectElement.options (its list of options).
    Options(NodeRef),
    /// HTMLFormElement.elements (its listed controls).
    FormElements(NodeRef),
    /// The Window's named objects with this name (HTML §7.2.2.3).
    WindowNamed(String),
}

/// The document's readiness (HTML §3.1.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadyState {
    Loading,
    Interactive,
    Complete,
}

impl ReadyState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadyState::Loading => "loading",
            ReadyState::Interactive => "interactive",
            ReadyState::Complete => "complete",
        }
    }
}

/// An attribute node (DOM §4.9.2): its element (None when detached) and identity.
#[derive(Clone, Debug)]
pub struct AttrRec {
    pub owner: Option<NodeRef>,
    pub ns: bb_dom::Namespace,
    pub prefix: Option<String>,
    pub local: String,
    /// The value while detached (attached ones read their element).
    pub value: String,
    /// The document it belongs to.
    pub doc: u32,
}

pub struct Host {
    /// Documents by index; 0 is the window's.
    pub docs: RefCell<Vec<Document>>,
    /// Content types of the documents ("text/html", "application/xml", …).
    pub doc_types: RefCell<Vec<String>>,
    /// Platform object wrappers of nodes, by packed `NodeRef`.
    pub wrappers: RefCell<HashMap<u64, ObjRef>>,
    /// Interface prototype and constructor objects, by class.
    pub protos: RefCell<Vec<ObjRef>>,
    pub ctors: RefCell<Vec<ObjRef>>,
    /// Collections by id, and the cached ones ([SameObject]) by (kind tag, node).
    pub collections: RefCell<Vec<CollKind>>,
    pub coll_cache: RefCell<HashMap<(u8, u64), ObjRef>>,
    /// Attribute nodes by id, and the id of each attached one by (element, ns, local name).
    pub attrs: RefCell<Vec<AttrRec>>,
    pub attr_ids: RefCell<HashMap<(u64, String, String), u64>>,
    /// Event listeners by target object.
    pub listeners: RefCell<HashMap<ObjRef, Vec<Listener>>>,
    /// Event handler attributes' compiled functions (target, "onclick" …).
    pub handlers: RefCell<HashMap<(ObjRef, String), Value>>,
    pub timers: RefCell<Vec<Timer>>,
    pub next_timer: Cell<u32>,
    pub timer_seq: Cell<u64>,
    pub tasks: RefCell<VecDeque<Task>>,
    /// requestAnimationFrame callbacks (id, callback).
    pub frame_callbacks: RefCell<Vec<(u32, Value)>>,
    /// The HTML parser while the document is being parsed.
    pub parser: RefCell<Option<Box<TreeBuilder>>>,
    /// Ends of the text `document.write` calls inserted (innermost last).
    pub write_ends: RefCell<Vec<usize>>,
    /// Scripts that have "already started" (HTML §4.12.1), by packed node.
    pub started_scripts: RefCell<HashSet<u64>>,
    /// `defer` scripts waiting for the end of parsing.
    pub deferred: RefCell<Vec<NodeRef>>,
    /// document.currentScript.
    pub current_script: RefCell<Value>,
    pub url: RefCell<Url>,
    pub ready_state: Cell<ReadyState>,
    /// Text written with console.*.
    pub console: RefCell<Vec<String>>,
    pub embedder: Rc<dyn Embedder>,
    /// Set by any DOM mutation: the embedder restyles and lays out again.
    pub dirty: Cell<bool>,
    /// The global object (Window).
    pub window: Cell<Option<ObjRef>>,
    /// Exceptions reported (HTML §8.1.4.6 "report an exception"), as messages.
    pub errors: RefCell<Vec<String>>,
    /// A form `submit()` asked for: the embedder submits it (HTML §4.10.21.3).
    pub pending_submit: Cell<Option<NodeRef>>,
    /// A navigation scripts asked for (location.href = …).
    pub pending_navigation: RefCell<Option<String>>,
    /// Shared objects made once (e.g. the isTrusted getter), by name.
    pub misc: RefCell<HashMap<&'static str, ObjRef>>,
    /// Set while an error is being reported, so an error in an error handler isn't again.
    pub reporting: Cell<bool>,
    pub strings: RefCell<HashMap<&'static str, String>>,
    /// Requests in flight, by id.
    pub pending: RefCell<HashMap<u64, crate::net::Pending>>,
    pub next_fetch: Cell<u64>,
    /// XMLHttpRequest states, by the objects' payloads.
    pub xhrs: RefCell<Vec<crate::net::XhrState>>,
    /// sessionStorage, and localStorage when the embedder keeps none.
    pub session: RefCell<Vec<(String, String)>>,
    pub local_mem: RefCell<Vec<(String, String)>>,
    /// Nodes of document 0 up to this index have had their `on…` attributes registered.
    pub scanned: Cell<u32>,
    /// Mutation observers and the nodes they observe (DOM §4.3).
    pub mo: RefCell<crate::mutation::State>,
    /// Ranges' boundary points, by the objects' payloads (DOM §5).
    pub ranges: RefCell<Vec<crate::range::RangeData>>,
    /// NodeIterators' and TreeWalkers' states (DOM §6).
    pub traversers: RefCell<Vec<crate::traversal::Traverser>>,
    /// The window's document's selection.
    pub selection: RefCell<crate::selection::State>,
    /// Documents made by createDocument: their interface is XMLDocument (DOM §4.5.1).
    pub xml_documents: RefCell<HashSet<u32>>,
    /// FileReader states, by the objects' payloads.
    pub readers: RefCell<Vec<crate::blob::ReaderState>>,
    /// The blob URL store (File API §8.3): URL → Blob.
    pub blob_urls: RefCell<HashMap<String, Value>>,
}

impl Host {
    pub fn new(doc: Document, url: Url, embedder: Rc<dyn Embedder>) -> Host {
        Host {
            docs: RefCell::new(vec![doc]),
            doc_types: RefCell::new(vec!["text/html".to_string()]),
            wrappers: RefCell::default(),
            protos: RefCell::default(),
            ctors: RefCell::default(),
            collections: RefCell::default(),
            coll_cache: RefCell::default(),
            attrs: RefCell::default(),
            attr_ids: RefCell::default(),
            listeners: RefCell::default(),
            handlers: RefCell::default(),
            timers: RefCell::default(),
            next_timer: Cell::new(1),
            timer_seq: Cell::new(0),
            tasks: RefCell::default(),
            frame_callbacks: RefCell::default(),
            parser: RefCell::new(None),
            write_ends: RefCell::default(),
            started_scripts: RefCell::default(),
            deferred: RefCell::default(),
            current_script: RefCell::new(Value::Null),
            url: RefCell::new(url),
            ready_state: Cell::new(ReadyState::Loading),
            console: RefCell::default(),
            embedder,
            dirty: Cell::new(false),
            window: Cell::new(None),
            errors: RefCell::default(),
            pending_submit: Cell::new(None),
            pending_navigation: RefCell::new(None),
            misc: RefCell::default(),
            reporting: Cell::new(false),
            strings: RefCell::default(),
            pending: RefCell::default(),
            next_fetch: Cell::new(1),
            xhrs: RefCell::default(),
            session: RefCell::default(),
            local_mem: RefCell::default(),
            scanned: Cell::new(0),
            mo: RefCell::default(),
            ranges: RefCell::default(),
            traversers: RefCell::default(),
            selection: RefCell::default(),
            xml_documents: RefCell::default(),
            readers: RefCell::default(),
            blob_urls: RefCell::default(),
        }
    }

    /// Every object and value the host holds, for the collector.
    pub fn trace(&self, out: &mut Vec<ObjRef>) {
        let push = |v: &Value, out: &mut Vec<ObjRef>| {
            if let Value::Object(o) = v {
                out.push(*o);
            }
        };
        out.extend(self.wrappers.borrow().values());
        out.extend(self.protos.borrow().iter());
        out.extend(self.ctors.borrow().iter());
        out.extend(self.coll_cache.borrow().values());
        out.extend(self.misc.borrow().values());
        for (k, ls) in self.listeners.borrow().iter() {
            out.push(*k);
            for l in ls {
                push(&l.callback, out);
                if let Some(s) = l.signal {
                    out.push(s);
                }
            }
        }
        for ((k, _), v) in self.handlers.borrow().iter() {
            out.push(*k);
            push(v, out);
        }
        for t in self.timers.borrow().iter() {
            push(&t.callback, out);
            for a in &t.args {
                push(a, out);
            }
        }
        for t in self.tasks.borrow().iter() {
            match t {
                Task::Call(f, this, args) => {
                    push(f, out);
                    push(this, out);
                    for a in args {
                        push(a, out);
                    }
                }
                Task::Event(t, ..) => push(t, out),
                Task::Read(r) => {
                    out.push(r.reader);
                    out.push(r.blob);
                }
                Task::Fetch(..) | Task::Script(..) => {}
            }
        }
        for (_, f) in self.frame_callbacks.borrow().iter() {
            push(f, out);
        }
        self.mo.borrow().trace(out);
        self.selection.borrow().trace(out);
        for v in self.blob_urls.borrow().values() {
            push(v, out);
        }
        push(&self.current_script.borrow(), out);
        if let Some(w) = self.window.get() {
            out.push(w);
        }
        for p in self.pending.borrow().values() {
            match p {
                crate::net::Pending::Fetch { resolve, reject, signal } => {
                    push(resolve, out);
                    push(reject, out);
                    push(signal, out);
                }
                crate::net::Pending::Xhr(o) => out.push(*o),
            }
        }
    }

    /// The prototype of interface `c`.
    pub fn proto(&self, c: C) -> ObjRef {
        self.protos.borrow()[c as usize]
    }

    pub fn ctor(&self, c: C) -> ObjRef {
        self.ctors.borrow()[c as usize]
    }

    /// A string kept for the page's lifetime (window.name, …).
    pub fn misc_str(&self, k: &'static str) -> String {
        self.strings.borrow().get(k).cloned().unwrap_or_default()
    }

    pub fn set_misc_str(&self, k: &'static str, v: &str) {
        self.strings.borrow_mut().insert(k, v.to_string());
    }

    pub fn mark_dirty(&self) {
        self.dirty.set(true);
    }
}

/// The document base URL of document `doc` (HTML §2.4.1): the first `<base href>`'s URL
/// resolved against the document's URL, else the document's URL.
pub fn base_url(h: &Host, doc: u32) -> Url {
    let doc_url = {
        let docs = h.docs.borrow();
        docs.get(doc as usize).and_then(|d| Url::parse(&d.url).ok())
    }
    .unwrap_or_else(|| h.url.borrow().clone());
    let href = {
        let docs = h.docs.borrow();
        docs.get(doc as usize).and_then(|d| {
            d.descendants(bb_dom::Document::ROOT)
                .find(|&n| d.is_html(n, "base") && d.attr(n, "href").is_some())
                .and_then(|n| d.attr(n, "href").map(str::to_string))
        })
    };
    match href {
        Some(href) => Url::parse_with_base(&href, Some(&doc_url)).unwrap_or(doc_url),
        None => doc_url,
    }
}

// ---- reaching the host from natives ----

pub fn host(vm: &mut Vm) -> JsResult<Rc<Host>> {
    match vm.host::<Host>() {
        Some(h) => Ok(h),
        None => vm.type_error("no Web API host"),
    }
}

/// Read document `d`.
pub fn with_doc<R>(vm: &mut Vm, h: &Host, d: u32, f: impl FnOnce(&Document) -> R) -> JsResult<R> {
    let docs = match h.docs.try_borrow() {
        Ok(d) => d,
        Err(_) => return crate::exception::throw(vm, "InvalidStateError", "the document is busy"),
    };
    match docs.get(d as usize) {
        Some(doc) => Ok(f(doc)),
        None => vm.type_error("no such document"),
    }
}

/// Change document `d` (marks the page dirty).
pub fn with_doc_mut<R>(vm: &mut Vm, h: &Host, d: u32, f: impl FnOnce(&mut Document) -> R) -> JsResult<R> {
    let mut docs = match h.docs.try_borrow_mut() {
        Ok(d) => d,
        Err(_) => return crate::exception::throw(vm, "InvalidStateError", "the document is busy"),
    };
    h.dirty.set(true);
    match docs.get_mut(d as usize) {
        Some(doc) => Ok(f(doc)),
        None => vm.type_error("no such document"),
    }
}

// ---- wrappers ----

/// The platform object for node `n` (created on first use; the same object afterwards).
pub fn wrap(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<Value> {
    if let Some(o) = h.wrappers.borrow().get(&n.pack()) {
        return Ok(Value::Object(*o));
    }
    let mut c = with_doc(vm, h, n.doc, |d| class::node_class(d, n.id))?;
    if c == C::Document {
        // HTML documents are HTMLDocument, createDocument's XMLDocument, others Document.
        c = if h.doc_types.borrow().get(n.doc as usize).map(String::as_str) == Some("text/html") {
            C::HTMLDocument
        } else if h.xml_documents.borrow().contains(&n.doc) {
            C::XMLDocument
        } else {
            C::Document
        };
    }
    let proto = h.proto(c);
    let o = vm.new_host_object(Some(proto), c as u32, n.pack(), false);
    h.wrappers.borrow_mut().insert(n.pack(), o);
    Ok(Value::Object(o))
}

pub fn wrap_opt(vm: &mut Vm, h: &Host, n: Option<NodeRef>) -> JsResult<Value> {
    match n {
        Some(n) => wrap(vm, h, n),
        None => Ok(Value::Null),
    }
}

/// The node a value wraps, if it is a Node.
pub fn node_of(vm: &Vm, v: &Value) -> Option<NodeRef> {
    let (c, id) = vm.host_data(v)?;
    // Attr implements Node, but its payload is an attribute record, not a tree node.
    (class::implements(c, C::Node) && !class::implements(c, C::Attr)).then(|| NodeRef::unpack(id))
}

/// `this` as an object implementing `iface`, or TypeError ("Illegal invocation", WebIDL §3.7.6).
pub fn this_of(vm: &mut Vm, this: &Value, iface: C) -> JsResult<(u32, u64)> {
    match vm.host_data(this) {
        Some((c, id)) if class::implements(c, iface) => Ok((c, id)),
        _ => vm.type_error(&format!("Illegal invocation: not a {}", class::name(iface))),
    }
}

pub fn this_node(vm: &mut Vm, this: &Value, iface: C) -> JsResult<(Rc<Host>, NodeRef)> {
    let (c, id) = this_of(vm, this, iface)?;
    if class::implements(c, C::Attr) {
        return crate::exception::throw(vm, "HierarchyRequestError", "attributes have no children");
    }
    let h = host(vm)?;
    let n = NodeRef::unpack(id);
    // Payloads are made by this crate; check anyway, since the arena indexes directly.
    let ok =
        h.docs.try_borrow().is_ok_and(|d| d.get(n.doc as usize).is_some_and(|d| (n.id.0 as usize) < d.len()));
    if !ok {
        return vm.type_error("Illegal invocation: a stale node");
    }
    Ok((h, n))
}

/// A Node argument (WebIDL conversion to an interface type).
pub fn arg_node(vm: &mut Vm, v: &Value, what: &str) -> JsResult<NodeRef> {
    match node_of(vm, v) {
        Some(n) => Ok(n),
        None => vm.type_error(&format!("{what} is not a Node")),
    }
}

// ---- conversions ----

/// ToString as a Rust string (DOMString; lone surrogates become U+FFFD).
pub fn rstr(vm: &mut Vm, v: &Value) -> JsResult<String> {
    let s = vm.to_string(v)?;
    Ok(to_rust(&s))
}

/// A `DOMString?` argument: null and undefined are None (WebIDL §3.2.21 nullable types).
pub fn rstr_opt(vm: &mut Vm, v: &Value, _undef_null: bool) -> JsResult<Option<String>> {
    match v {
        Value::Null | Value::Undefined => Ok(None),
        _ => rstr(vm, v).map(Some),
    }
}

/// [LegacyNullToEmptyString] DOMString.
pub fn rstr_null_empty(vm: &mut Vm, v: &Value) -> JsResult<String> {
    match v {
        Value::Null => Ok(String::new()),
        _ => rstr(vm, v),
    }
}

pub fn js(s: &str) -> Value {
    Value::str(s)
}

/// ToNumber then WebIDL `unsigned long` (modulo 2^32).
pub fn to_ulong(vm: &mut Vm, v: &Value) -> JsResult<u32> {
    vm.to_uint32(v)
}

/// A property of an options dictionary (undefined when the dictionary is undefined/null).
pub fn dict_get(vm: &mut Vm, d: &Value, name: &'static str) -> JsResult<Value> {
    match d {
        Value::Object(o) => vm.get(*o, &PropKey::lit(name)),
        Value::Undefined | Value::Null => Ok(Value::Undefined),
        _ => vm.type_error("the options must be an object"),
    }
}

/// UTF-16 length of a Rust string.
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// The byte offset of UTF-16 offset `n` in `s` (clamped; inside a pair, after it).
pub fn utf16_to_byte(s: &str, n: usize) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units >= n {
            return i;
        }
        units += c.len_utf16();
    }
    s.len()
}

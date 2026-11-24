//! Child navigables: `<iframe>` and `<frame>` documents with their own Window (HTML §4.8.5,
//! §7.3), and how windows of one page reach each other (HTML §7.2.3 WindowProxy, §7.2.1.3
//! cross-origin objects).
//!
//! Every window of a page lives in the page's one VM, each in its own realm with its own
//! [`Host`]. A connected iframe gets a child host at once (an about:blank document), and its
//! `src`/`srcdoc` loads in a task, which replaces that host with a new one (a new realm) for
//! the new document. Code reaches another window only through a WindowProxy: when the two
//! are same origin that is the other realm's global object itself; otherwise it is a
//! cross-origin wrapper made in the accessing realm, which offers only the members HTML
//! §7.2.1.3.1 "CrossOriginProperties" lists and throws a SecurityError for anything else.
//! Same origin is an equivalence relation, so a getter may decide by its own realm's origin:
//! code can only call a realm's functions if it is same origin with that realm.
//!
//! Not done yet: WindowProxy identity across navigations (after a frame navigates, its
//! `contentWindow` is the new document's global), `document.domain`, sandboxing, frames in
//! documents other than the window's, and cross-frame node adoption (nodes of one frame's
//! document can't be inserted into another's: they belong to the other realm's host).

use std::any::Any;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

use bb_dom::{Document, NodeId};
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::object::PropDesc;
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};
use bb_url::{Origin, Url};

use crate::exception::throw;
use crate::host::*;
use crate::idl::{attr, op};

type R = JsResult<Value>;

/// Classes of the cross-origin wrappers (no interfaces of their own).
pub const CROSS_ORIGIN_WINDOW: u32 = 0xffff_0003;
pub const CROSS_ORIGIN_LOCATION: u32 = 0xffff_0004;

/// Frames nest at most this deep, and a page has at most this many windows.
const MAX_DEPTH: u32 = 8;
const MAX_WINDOWS: usize = 128;

/// An origin with identity for opaque ones (HTML §7.1.1: an opaque origin is only same
/// origin with itself).
#[derive(Clone, Debug)]
pub enum FrameOrigin {
    Tuple(Origin),
    Opaque(u64),
}

static NEXT_OPAQUE: AtomicU64 = AtomicU64::new(1);

impl FrameOrigin {
    pub fn opaque() -> FrameOrigin {
        FrameOrigin::Opaque(NEXT_OPAQUE.fetch_add(1, Ordering::Relaxed))
    }

    pub fn of_url(u: &Url) -> FrameOrigin {
        match u.origin() {
            o @ Origin::Tuple(..) => FrameOrigin::Tuple(o),
            Origin::Opaque => FrameOrigin::opaque(),
        }
    }

    /// HTML §7.1.1 "same origin".
    pub fn same(&self, o: &FrameOrigin) -> bool {
        match (self, o) {
            (FrameOrigin::Tuple(a), FrameOrigin::Tuple(b)) => a == b,
            (FrameOrigin::Opaque(a), FrameOrigin::Opaque(b)) => a == b,
            _ => false,
        }
    }

    pub fn serialize(&self) -> String {
        match self {
            FrameOrigin::Tuple(o) => o.serialize(),
            FrameOrigin::Opaque(_) => "null".to_string(),
        }
    }
}

/// A navigable (HTML §7.3.1): the window currently shown in it.
pub struct Nav {
    pub active: RefCell<Weak<Host>>,
}

impl Nav {
    fn new() -> Rc<Nav> {
        Rc::new(Nav { active: RefCell::new(Weak::new()) })
    }
    pub fn host(&self) -> Option<Rc<Host>> {
        self.active.borrow().upgrade().filter(|h| !h.frames.borrow().destroyed)
    }
}

/// A child navigable of a window's document: its container element and its window.
pub struct Child {
    pub container: NodeId,
    pub nav: Rc<Nav>,
    pub host: Rc<Host>,
}

/// A wrapper for another origin's window made in this realm, and its Location wrapper.
pub struct Remote {
    nav: Weak<Nav>,
    window: ObjRef,
    location: Option<ObjRef>,
}

/// A window's place among the page's windows.
pub struct FrameState {
    /// Its realm in the page's VM.
    pub realm: u32,
    pub origin: FrameOrigin,
    /// The navigable it is shown in.
    pub nav: Rc<Nav>,
    /// The parent window and the container element in its document (None for the top).
    pub parent: Option<(Weak<Host>, NodeId)>,
    pub children: Vec<Child>,
    /// Every window of the page (for the collector and the event loop).
    pub family: Rc<RefCell<Vec<Weak<Host>>>>,
    pub depth: u32,
    /// The frame went away (its container was removed, or it navigated).
    pub destroyed: bool,
    pub remotes: Vec<Remote>,
    /// HTML §8.4.3: the ignore-destructive-writes counter.
    pub ignore_destructive: u32,
    /// The parser is a script-created one (document.open).
    pub script_parser: bool,
    /// Whether the embedder gives this page nested browsing contexts.
    pub nested: bool,
}

impl Default for FrameState {
    fn default() -> Self {
        FrameState {
            realm: 0,
            origin: FrameOrigin::opaque(),
            nav: Nav::new(),
            parent: None,
            children: Vec::new(),
            family: Rc::default(),
            depth: 0,
            destroyed: false,
            remotes: Vec::new(),
            ignore_destructive: 0,
            script_parser: false,
            nested: false,
        }
    }
}

impl FrameState {
    pub fn trace(&self, out: &mut Vec<ObjRef>) {
        for r in &self.remotes {
            out.push(r.window);
            out.extend(r.location);
        }
    }
}

/// Set up a page's top window (or a worker's global): its own family, navigable and origin.
pub fn init_top(h: &Rc<Host>) {
    let mut f = h.frames.borrow_mut();
    f.origin = FrameOrigin::of_url(&h.url.borrow());
    *f.nav.active.borrow_mut() = Rc::downgrade(h);
    f.family.borrow_mut().push(Rc::downgrade(h));
    f.nested = h.embedder.nested_browsing();
}

/// The windows of `h`'s page that are still there, top first.
pub fn family(h: &Host) -> Vec<Rc<Host>> {
    let fam = h.frames.borrow().family.clone();
    let mut out = Vec::new();
    fam.borrow_mut().retain(|w| match w.upgrade() {
        Some(x) => {
            if !x.frames.borrow().destroyed {
                out.push(x);
            }
            true
        }
        None => false,
    });
    out
}

pub fn origin(h: &Host) -> FrameOrigin {
    h.frames.borrow().origin.clone()
}

pub fn same_origin(a: &Host, b: &Host) -> bool {
    a.frames.borrow().origin.same(&b.frames.borrow().origin)
}

pub fn parent_of(h: &Host) -> Option<(Rc<Host>, NodeId)> {
    let f = h.frames.borrow();
    let (p, n) = f.parent.as_ref()?;
    let p = p.upgrade()?;
    Some((p, *n))
}

fn top_of(h: &Rc<Host>) -> Rc<Host> {
    let mut cur = h.clone();
    while let Some((p, _)) = parent_of(&cur) {
        cur = p;
    }
    cur
}

fn connected(d: &Document, x: NodeId) -> bool {
    std::iter::once(x).chain(d.ancestors(x)).last() == Some(Document::ROOT)
}

/// The child windows in tree order of their containers (HTML §7.3.1.1 "document-tree child
/// navigables").
pub fn children(h: &Host) -> Vec<(NodeId, Rc<Host>)> {
    let kids: Vec<(NodeId, Rc<Host>)> =
        h.frames.borrow().children.iter().filter_map(|c| Some((c.container, c.nav.host()?))).collect();
    if kids.len() < 2 {
        return kids;
    }
    let Ok(docs) = h.docs.try_borrow() else { return kids };
    let order: Vec<NodeId> =
        docs[0].descendants(Document::ROOT).filter(|x| kids.iter().any(|k| k.0 == *x)).collect();
    let mut kids = kids;
    kids.sort_by_key(|k| order.iter().position(|x| *x == k.0).unwrap_or(usize::MAX));
    kids
}

/// The WindowProxy of `target` as code of `acc`'s realm (the current one) sees it: the
/// global object if same origin, else the cross-origin wrapper.
pub fn proxy_for(vm: &mut Vm, acc: &Rc<Host>, target: &Rc<Host>) -> R {
    if target.frames.borrow().destroyed {
        return Ok(Value::Null);
    }
    if same_origin(acc, target) {
        return Ok(target.window.get().map_or(Value::Null, Value::Object));
    }
    let nav = target.frames.borrow().nav.clone();
    remote_window(vm, acc, &nav)
}

fn remote_index(acc: &Host, nav: &Rc<Nav>) -> Option<usize> {
    acc.frames.borrow().remotes.iter().position(|r| r.nav.upgrade().is_some_and(|n| Rc::ptr_eq(&n, nav)))
}

/// The cross-origin wrapper for `nav` in the current realm (`acc`'s), made once.
fn remote_window(vm: &mut Vm, acc: &Rc<Host>, nav: &Rc<Nav>) -> R {
    if let Some(i) = remote_index(acc, nav) {
        return Ok(Value::Object(acc.frames.borrow().remotes[i].window));
    }
    let id = acc.frames.borrow().remotes.len() as u64;
    let o = vm.new_host_object(None, CROSS_ORIGIN_WINDOW, id, true);
    for name in ["window", "self", "frames"] {
        attr(vm, o, name, |_, c| Ok(c.this.clone()), None);
    }
    attr(vm, o, "closed", |vm, c| Ok(Value::Bool(remote_target(vm, &c.this)?.1.is_none())), None);
    attr(
        vm,
        o,
        "length",
        |vm, c| {
            let n = remote_target(vm, &c.this)?.1.map_or(0, |t| children(&t).len());
            Ok(Value::Number(n as f64))
        },
        None,
    );
    attr(
        vm,
        o,
        "parent",
        |vm, c| {
            let (acc, t) = remote_target(vm, &c.this)?;
            let Some(t) = t else { return Ok(Value::Null) };
            match parent_of(&t) {
                Some((p, _)) => proxy_for(vm, &acc, &p),
                None => Ok(c.this.clone()),
            }
        },
        None,
    );
    attr(
        vm,
        o,
        "top",
        |vm, c| {
            let (acc, t) = remote_target(vm, &c.this)?;
            let Some(t) = t else { return Ok(Value::Null) };
            let top = top_of(&t);
            proxy_for(vm, &acc, &top)
        },
        None,
    );
    attr(vm, o, "opener", |_, _| Ok(Value::Null), None);
    attr(vm, o, "location", remote_location_get, Some(remote_location_set));
    op(vm, o, "postMessage", 1, |vm, c| {
        let (acc, t) = remote_target(vm, &c.this)?;
        let source = vm.realm_host::<Host>(vm.caller_realm).unwrap_or(acc);
        match t {
            Some(t) => crate::worker::post_message(vm, &source, &t, &c.args),
            None => Ok(Value::Undefined),
        }
    });
    for name in ["close", "focus", "blur"] {
        op(vm, o, name, 0, |vm, c| {
            remote_target(vm, &c.this)?;
            Ok(Value::Undefined)
        });
    }
    acc.frames.borrow_mut().remotes.push(Remote { nav: Rc::downgrade(nav), window: o, location: None });
    Ok(Value::Object(o))
}

/// A cross-origin wrapper's realm host and its window's host (None once it is gone).
fn remote_target(vm: &mut Vm, this: &Value) -> JsResult<(Rc<Host>, Option<Rc<Host>>)> {
    let Some((class, id)) = vm.host_data(this) else { return vm.type_error("Illegal invocation") };
    if class != CROSS_ORIGIN_WINDOW && class != CROSS_ORIGIN_LOCATION {
        return vm.type_error("Illegal invocation");
    }
    let acc = host(vm)?;
    let nav = acc.frames.borrow().remotes.get(id as usize).and_then(|r| r.nav.upgrade());
    Ok((acc, nav.and_then(|n| n.host())))
}

fn remote_location_get(vm: &mut Vm, c: NativeCall) -> R {
    let (acc, _) = remote_target(vm, &c.this)?;
    let Some((_, id)) = vm.host_data(&c.this) else { return Ok(Value::Undefined) };
    let id = id as usize;
    if let Some(l) = acc.frames.borrow().remotes.get(id).and_then(|r| r.location) {
        return Ok(Value::Object(l));
    }
    let o = vm.new_host_object(None, CROSS_ORIGIN_LOCATION, id as u64, true);
    attr(
        vm,
        o,
        "href",
        |vm, _| throw(vm, "SecurityError", "a cross-origin location can't be read"),
        Some(|vm, c| {
            let (acc, t) = remote_target(vm, &c.this)?;
            let href = rstr(vm, &c.arg(0))?;
            navigate_from(vm, &acc, t, &href)
        }),
    );
    op(vm, o, "replace", 1, |vm, c| {
        let (acc, t) = remote_target(vm, &c.this)?;
        let href = rstr(vm, &c.arg(0))?;
        navigate_from(vm, &acc, t, &href)
    });
    if let Some(r) = acc.frames.borrow_mut().remotes.get_mut(id) {
        r.location = Some(o);
    }
    Ok(Value::Object(o))
}

fn remote_location_set(vm: &mut Vm, c: NativeCall) -> R {
    let (acc, t) = remote_target(vm, &c.this)?;
    let href = rstr(vm, &c.arg(0))?;
    navigate_from(vm, &acc, t, &href)
}

/// HTML §7.4.2.2 "navigate" `target` to `href` (parsed against `acc`'s document), asked by
/// `acc` through a cross-origin Location. Only the window's own descendants may be
/// navigated this way (no top-level or sibling navigation by another origin).
fn navigate_from(vm: &mut Vm, acc: &Rc<Host>, target: Option<Rc<Host>>, href: &str) -> R {
    let Some(t) = target else { return Ok(Value::Undefined) };
    let base = base_url(acc, 0);
    let Ok(url) = Url::parse_with_base(href, Some(&base)) else {
        return throw(vm, "SyntaxError", &format!("'{href}' is not a valid URL"));
    };
    let mut cur = parent_of(&t);
    let mut descendant = false;
    while let Some((p, _)) = cur {
        if Rc::ptr_eq(&p, acc) {
            descendant = true;
            break;
        }
        cur = parent_of(&p);
    }
    if !descendant {
        acc.console.borrow_mut().push(format!(
            "navigation of a cross-origin window that isn't a descendant was blocked ({})",
            url.href()
        ));
        return Ok(Value::Undefined);
    }
    navigate(&t, url);
    Ok(Value::Undefined)
}

/// Navigate window `t` to `url`: a frame's parent loads it in a task; the top window's
/// navigation is the embedder's.
pub fn navigate(t: &Rc<Host>, url: Url) {
    match parent_of(t) {
        Some((p, container)) => {
            p.tasks.borrow_mut().push_back(Task::Navigate(container, NavTarget::Url(url)))
        }
        None => *t.pending_navigation.borrow_mut() = Some(url.href()),
    }
}

/// What a frame navigates to.
#[derive(Clone, Debug)]
pub enum NavTarget {
    Url(Url),
    Srcdoc(String),
}

// ---- the cross-origin wrappers' exotic internal methods (HTML §7.2.3) ----

/// CrossOriginProperties (HTML §7.2.1.3.1) of a WindowProxy or Location.
fn allowed(class: u32, name: &str) -> bool {
    if class == CROSS_ORIGIN_LOCATION {
        return matches!(name, "href" | "replace");
    }
    matches!(
        name,
        "window"
            | "self"
            | "location"
            | "close"
            | "closed"
            | "focus"
            | "blur"
            | "frames"
            | "length"
            | "top"
            | "opener"
            | "parent"
            | "postMessage"
    )
}

/// [[GetOwnProperty]]: child windows by index, the allowed members (ordinary own
/// properties), `then` and the well-known symbols as undefined; anything else throws.
pub fn exotic_get_own(vm: &mut Vm, this: ObjRef, class: u32, k: &PropKey) -> JsResult<Option<PropDesc>> {
    match k {
        PropKey::Index(i) if class == CROSS_ORIGIN_WINDOW => {
            let (acc, t) = remote_target(vm, &Value::Object(this))?;
            let kid = t.and_then(|t| children(&t).into_iter().nth(*i as usize));
            match kid {
                Some((_, kid)) => {
                    let v = proxy_for(vm, &acc, &kid)?;
                    Ok(Some(PropDesc::data(v, false, false, true)))
                }
                None => throw(vm, "SecurityError", "cross-origin access was blocked"),
            }
        }
        PropKey::Sym(_) => Ok(None),
        _ => {
            let name = k.display();
            if name == "then" || allowed(class, &name) {
                Ok(None)
            } else {
                throw(
                    vm,
                    "SecurityError",
                    &format!("access to '{name}' of a cross-origin object was blocked"),
                )
            }
        }
    }
}

pub fn exotic_keys(vm: &mut Vm, this: ObjRef, class: u32) -> JsResult<Vec<PropKey>> {
    if class != CROSS_ORIGIN_WINDOW {
        return Ok(Vec::new());
    }
    let (_, t) = remote_target(vm, &Value::Object(this))?;
    let n = t.map_or(0, |t| children(&t).len());
    Ok((0..n as u32).map(PropKey::Index).collect())
}

/// [[Set]]: only through the `location` (and Location `href`) setters.
pub fn exotic_set(vm: &mut Vm, class: u32, k: &PropKey) -> JsResult<Option<bool>> {
    let name = k.display();
    let ok = match class {
        CROSS_ORIGIN_LOCATION => name == "href",
        _ => name == "location",
    };
    if ok && !matches!(k, PropKey::Sym(_) | PropKey::Index(_)) {
        return Ok(None);
    }
    throw(vm, "SecurityError", &format!("setting '{name}' on a cross-origin object was blocked"))
}

pub fn exotic_refuse(vm: &mut Vm) -> JsResult<Option<bool>> {
    throw(vm, "SecurityError", "cross-origin objects can't be changed")
}

// ---- the Window members about frames (HTML §7.2.2) ----

pub fn parent_getter(vm: &mut Vm, _c: NativeCall) -> R {
    let h = host(vm)?;
    match parent_of(&h) {
        Some((p, _)) => proxy_for(vm, &h, &p),
        None => Ok(h.window.get().map_or(Value::Undefined, Value::Object)),
    }
}

pub fn top_getter(vm: &mut Vm, _c: NativeCall) -> R {
    let h = host(vm)?;
    let t = top_of(&h);
    proxy_for(vm, &h, &t)
}

pub fn length_getter(vm: &mut Vm, _c: NativeCall) -> R {
    let h = host(vm)?;
    Ok(Value::Number(children(&h).len() as f64))
}

/// HTML §7.2.2.1 frameElement: the container, if same origin.
pub fn frame_element_getter(vm: &mut Vm, _c: NativeCall) -> R {
    let h = host(vm)?;
    match parent_of(&h) {
        Some((p, container)) if same_origin(&h, &p) => {
            let realm = p.frames.borrow().realm;
            vm.try_in_realm(realm, |vm| wrap(vm, &p, NodeRef { doc: 0, id: container }))
                .unwrap_or(Ok(Value::Null))
        }
        _ => Ok(Value::Null),
    }
}

/// The closed attribute: the frame went away.
pub fn closed_getter(vm: &mut Vm, _c: NativeCall) -> R {
    let h = host(vm)?;
    let gone = h.frames.borrow().destroyed;
    Ok(Value::Bool(gone))
}

/// A child window by index (the WindowProxy's indexed properties, served by the
/// WindowProperties object here).
pub fn child_by_index(vm: &mut Vm, i: u32) -> JsResult<Option<Value>> {
    let h = host(vm)?;
    let kid = children(&h).into_iter().nth(i as usize);
    match kid {
        Some((_, k)) => proxy_for(vm, &h, &k).map(Some),
        None => Ok(None),
    }
}

/// A child window by its name (HTML §7.2.2.3 "document-tree child navigable target name
/// property set").
pub fn child_by_name(vm: &mut Vm, name: &str) -> JsResult<Option<Value>> {
    if name.is_empty() {
        return Ok(None);
    }
    let h = host(vm)?;
    let kid = children(&h).into_iter().find(|(_, k)| k.misc_str("window.name") == name);
    match kid {
        Some((_, k)) => proxy_for(vm, &h, &k).map(Some),
        None => Ok(None),
    }
}

pub fn child_count(h: &Host) -> usize {
    children(h).len()
}

pub fn child_names(h: &Host) -> Vec<String> {
    children(h).into_iter().map(|(_, k)| k.misc_str("window.name")).filter(|n| !n.is_empty()).collect()
}

/// Swap a frame window's document with `doc` (an embedder lends documents to the windows
/// while scripts run, and takes them back for layout).
pub fn swap_document(h: &Host, doc: &mut Document) {
    if let Ok(mut docs) = h.docs.try_borrow_mut() {
        std::mem::swap(&mut docs[0], doc);
    }
}

/// The container element's window, if it has one.
pub fn child_of(h: &Host, container: NodeId) -> Option<Rc<Host>> {
    h.frames.borrow().children.iter().find(|c| c.container == container).and_then(|c| c.nav.host())
}

/// HTMLIFrameElement / HTMLFrameElement contentWindow.
pub fn content_window(vm: &mut Vm, c: NativeCall) -> R {
    let (h, n) = this_node(vm, &c.this, crate::class::C::HTMLElement)?;
    if n.doc != 0 {
        return Ok(Value::Null);
    }
    match child_of(&h, n.id) {
        Some(k) => proxy_for(vm, &h, &k),
        None => Ok(Value::Null),
    }
}

/// contentDocument: the child's document, if same origin (HTML §4.8.5).
pub fn content_document(vm: &mut Vm, c: NativeCall) -> R {
    let (h, n) = this_node(vm, &c.this, crate::class::C::HTMLElement)?;
    if n.doc != 0 {
        return Ok(Value::Null);
    }
    match child_of(&h, n.id) {
        Some(k) if same_origin(&h, &k) => {
            let realm = k.frames.borrow().realm;
            vm.try_in_realm(realm, |vm| wrap(vm, &k, NodeRef { doc: 0, id: Document::ROOT }))
                .unwrap_or(Ok(Value::Null))
        }
        _ => Ok(Value::Null),
    }
}

// ---- creating, loading and destroying frames ----

/// An embedder for a frame's window when the page's embedder has none of its own: it
/// fetches and keeps time like the page, but has no layout, keeps localStorage in memory and
/// sees no cookies unless the frame is same origin with the page (so another site's frame
/// never reads the page's storage).
struct FrameEmbedder {
    page: Rc<dyn Embedder>,
    same_as_page: bool,
}

impl Embedder for FrameEmbedder {
    fn fetch_script(&self, url: &Url) -> Result<String, String> {
        self.page.fetch_script(url)
    }
    fn now_ms(&self) -> f64 {
        self.page.now_ms()
    }
    fn time_origin_ms(&self) -> f64 {
        self.page.time_origin_ms()
    }
    fn viewport(&self) -> (f64, f64) {
        (300.0, 150.0)
    }
    fn user_agent(&self) -> String {
        self.page.user_agent()
    }
    fn fetch_sync(&self, req: crate::net::HttpRequest) -> Result<crate::net::HttpResponse, String> {
        self.page.fetch_sync(req)
    }
    fn worker_net(&self) -> Option<std::sync::Arc<dyn crate::worker::WorkerNet>> {
        self.page.worker_net()
    }
    fn image_bitmap(&self, url: &Url) -> Option<(u32, u32, Vec<u8>)> {
        self.page.image_bitmap(url)
    }
    fn waker(&self) -> Option<crate::worker::Waker> {
        self.page.waker()
    }
    fn has_storage(&self) -> bool {
        self.same_as_page && self.page.has_storage()
    }
    fn storage_area(&self) -> Vec<(String, String)> {
        if self.same_as_page {
            self.page.storage_area()
        } else {
            Vec::new()
        }
    }
    fn set_storage_area(&self, area: Vec<(String, String)>) {
        if self.same_as_page {
            self.page.set_storage_area(area)
        }
    }
    fn cookie(&self) -> String {
        if self.same_as_page {
            self.page.cookie()
        } else {
            String::new()
        }
    }
    fn set_cookie(&self, cookie: &str) {
        if self.same_as_page {
            self.page.set_cookie(cookie)
        }
    }
    fn nested_browsing(&self) -> bool {
        self.page.nested_browsing()
    }
}

/// A new window for `container` in `parent`'s document, shown in `nav`, with a new realm.
fn create_host(
    vm: &mut Vm,
    parent: &Rc<Host>,
    container: NodeId,
    nav: Rc<Nav>,
    url: Url,
    origin: FrameOrigin,
) -> Option<Rc<Host>> {
    let (family, depth) = {
        let f = parent.frames.borrow();
        (f.family.clone(), f.depth + 1)
    };
    if depth > MAX_DEPTH || family.borrow().iter().filter(|w| w.strong_count() > 0).count() >= MAX_WINDOWS {
        log::debug!("frame not created: too many or too deeply nested frames");
        return None;
    }
    let top = top_of(parent);
    let embedder = parent.embedder.frame_embedder(&url).unwrap_or_else(|| {
        Rc::new(FrameEmbedder {
            page: top.embedder.clone(),
            same_as_page: origin.same(&top.frames.borrow().origin),
        })
    });
    let mut doc = Document::new();
    doc.url = url.href();
    let child = Rc::new(Host::new(doc, url, embedder));
    let realm = vm.create_realm();
    {
        let mut f = child.frames.borrow_mut();
        f.realm = realm;
        f.origin = origin;
        f.nav = nav.clone();
        f.parent = Some((Rc::downgrade(parent), container));
        f.family = family.clone();
        f.depth = depth;
        f.nested = true;
    }
    *nav.active.borrow_mut() = Rc::downgrade(&child);
    family.borrow_mut().push(Rc::downgrade(&child));
    let name =
        with_doc_quiet(parent, |d| d.attr(container, "name").unwrap_or("").to_string()).unwrap_or_default();
    child.set_misc_str("window.name", &name);
    let any: Rc<dyn Any> = child.clone();
    let weak = Rc::downgrade(&any);
    vm.try_in_realm(realm, |vm| {
        vm.set_realm_host(weak);
        child.window.set(Some(vm.realm.global));
        crate::class::install_scope(vm, &child, false);
    });
    Some(child)
}

fn with_doc_quiet<T>(h: &Host, f: impl FnOnce(&Document) -> T) -> Option<T> {
    h.docs.try_borrow().ok().map(|d| f(&d[0]))
}

/// HTML §4.8.5 iframe "post-connection steps" (and the frame element's insertion steps):
/// create the child navigable with its initial about:blank document, then process the
/// container's attributes.
pub fn container_connected(vm: &mut Vm, h: &Rc<Host>, container: NodeId) -> JsResult<()> {
    {
        let f = h.frames.borrow();
        if !f.nested || f.destroyed || f.children.iter().any(|c| c.container == container) {
            return Ok(());
        }
    }
    let url = Url::parse("about:blank").expect("about:blank parses");
    let nav = Nav::new();
    let Some(child) = create_host(vm, h, container, nav.clone(), url, origin(h)) else { return Ok(()) };
    h.frames.borrow_mut().children.push(Child { container, nav, host: child.clone() });
    let realm = child.frames.borrow().realm;
    vm.try_in_realm(realm, |vm| {
        crate::loader::load(vm, &child, "");
        child.ready_state.set(ReadyState::Complete);
    });
    process_attributes(vm, h, container, true)
}

/// HTML §4.8.5 "process the iframe attributes".
pub fn process_attributes(vm: &mut Vm, h: &Rc<Host>, container: NodeId, initial: bool) -> JsResult<()> {
    let info = with_doc(vm, h, 0, |d| {
        let iframe = d.is_html(container, "iframe");
        let srcdoc = if iframe { d.attr(container, "srcdoc").map(str::to_string) } else { None };
        (srcdoc, d.attr(container, "src").map(str::to_string))
    })?;
    let target = match info {
        (Some(s), _) => Some(NavTarget::Srcdoc(s)),
        (None, Some(src)) if !src.trim().is_empty() => {
            let base = base_url(h, 0);
            match Url::parse_with_base(src.trim(), Some(&base)) {
                Ok(u) if u.href() != "about:blank" => Some(NavTarget::Url(u)),
                _ => None,
            }
        }
        _ => None,
    };
    match target {
        Some(t) => h.tasks.borrow_mut().push_back(Task::Navigate(container, t)),
        // "If url matches about:blank and initialInsertion is true, run the iframe load
        // event steps"; later changes navigate to about:blank.
        None if initial => load_event(vm, h, container),
        None => h.tasks.borrow_mut().push_back(Task::Navigate(
            container,
            NavTarget::Url(Url::parse("about:blank").expect("about:blank parses")),
        )),
    }
    Ok(())
}

/// HTML §4.8.5 "iframe load event steps": a load event at the container.
fn load_event(vm: &mut Vm, h: &Host, container: NodeId) {
    if let Ok(t) = wrap(vm, h, NodeRef { doc: 0, id: container }) {
        if let Err(e) = crate::events::fire(vm, h, t, "load", false, false) {
            crate::events::report_exception(vm, e);
        }
    }
}

/// The src or srcdoc attribute of a connected container changed.
pub fn attribute_changed(vm: &mut Vm, h: &Rc<Host>, n: NodeRef, local: &str) -> JsResult<()> {
    if n.doc != 0 || !matches!(local, "src" | "srcdoc") {
        return Ok(());
    }
    let ok = with_doc(vm, h, 0, |d| {
        (d.is_html(n.id, "iframe") || (local == "src" && d.is_html(n.id, "frame"))) && connected(d, n.id)
    })?;
    if ok && child_of(h, n.id).is_some() {
        process_attributes(vm, h, n.id, false)?;
    }
    Ok(())
}

/// Nodes were inserted into the window's document: containers among them that are now
/// connected get their frames.
pub fn nodes_inserted(vm: &mut Vm, h: &Rc<Host>, roots: &[NodeId]) -> JsResult<()> {
    if !h.frames.borrow().nested {
        return Ok(());
    }
    let found = with_doc(vm, h, 0, |d| {
        let mut out = Vec::new();
        for &r in roots {
            if !connected(d, r) {
                continue;
            }
            for x in d.descendants(r) {
                if d.is_html(x, "iframe") || d.is_html(x, "frame") {
                    out.push(x);
                }
            }
        }
        out
    })?;
    for c in found {
        container_connected(vm, h, c)?;
    }
    Ok(())
}

/// Nodes left the window's document: frames whose containers are no longer connected are
/// destroyed (HTML §4.8.5 iframe "removing steps").
pub fn nodes_removed(vm: &mut Vm, h: &Host) {
    if h.frames.borrow().children.is_empty() {
        return;
    }
    let gone: Vec<NodeId> = {
        let Ok(docs) = h.docs.try_borrow() else { return };
        let d = &docs[0];
        h.frames.borrow().children.iter().map(|c| c.container).filter(|&c| !connected(d, c)).collect()
    };
    for c in gone {
        let child = {
            let mut f = h.frames.borrow_mut();
            let i = f.children.iter().position(|x| x.container == c);
            i.map(|i| f.children.remove(i))
        };
        if let Some(child) = child {
            destroy(vm, &child.host);
        }
    }
}

/// HTML §7.3.1.4 "destroy a child navigable": the window and its descendants go away; their
/// realms are dropped, so their code no longer runs.
pub fn destroy(vm: &mut Vm, h: &Host) {
    let kids = std::mem::take(&mut h.frames.borrow_mut().children);
    for k in kids {
        destroy(vm, &k.host);
    }
    let realm = {
        let mut f = h.frames.borrow_mut();
        f.destroyed = true;
        f.realm
    };
    vm.drop_realm(realm);
    h.timers.borrow_mut().clear();
    h.tasks.borrow_mut().clear();
    h.frame_callbacks.borrow_mut().clear();
    *h.workers.borrow_mut() = Default::default();
}

/// Run a frame navigation task: fetch the document, make a new window for it in place of
/// the frame's current one, parse it (running its scripts), and fire the load events.
pub fn navigate_now(vm: &mut Vm, h: &Rc<Host>, container: NodeId, target: NavTarget) -> JsResult<()> {
    if child_of(h, container).is_none() {
        return Ok(());
    }
    let (url, content_type, body, origin) = load(vm, h, target);
    install(vm, h, container, url, &content_type, body, origin);
    Ok(())
}

/// Show a fetched document in the frame of `container` (the embedder's own frame
/// navigations use this too): a new window replaces the frame's current one, the document is
/// parsed with its scripts, and the load events fire. Returns the new window.
pub fn install(
    vm: &mut Vm,
    h: &Rc<Host>,
    container: NodeId,
    url: Url,
    content_type: &str,
    body: Vec<u8>,
    origin: FrameOrigin,
) -> Option<Rc<Host>> {
    let nav = h.frames.borrow().children.iter().find(|c| c.container == container).map(|c| c.nav.clone())?;
    let content_type = content_type.to_ascii_lowercase();
    let child = create_host(vm, h, container, nav, url, origin)?;
    let old = {
        let mut f = h.frames.borrow_mut();
        match f.children.iter_mut().find(|c| c.container == container) {
            Some(c) => Some(std::mem::replace(&mut c.host, child.clone())),
            None => None,
        }
    };
    if let Some(old) = old {
        destroy(vm, &old);
    }
    let realm = child.frames.borrow().realm;
    vm.try_in_realm(realm, |vm| {
        let essence = content_type.split(';').next().unwrap_or("").trim().to_string();
        if essence.ends_with("+xml") || essence.ends_with("/xml") {
            let text = String::from_utf8_lossy(&body).into_owned();
            let mut doc = crate::xml::xml_or_error(&text);
            doc.url = child.url.borrow().href();
            if let Ok(mut docs) = child.docs.try_borrow_mut() {
                docs[0] = doc;
            }
            if let Ok(mut t) = child.doc_types.try_borrow_mut() {
                t[0] = essence.clone();
            }
            child.ready_state.set(ReadyState::Interactive);
        } else {
            let html = decode(&body, &content_type);
            let html = if content_type.starts_with("text/html") || content_type.is_empty() {
                html
            } else if content_type.starts_with("text/") {
                format!("<pre>{}</pre>", escape(&html))
            } else {
                String::new()
            };
            crate::loader::load(vm, &child, &html);
        }
        crate::loader::fire_load(vm, &child);
    });
    load_event(vm, h, container);
    Some(child)
}

/// The origin of a document the embedder fetched for a frame of `h` at `url`.
pub fn origin_for(h: &Host, url: &Url) -> FrameOrigin {
    match url.scheme() {
        "about" | "blob" => origin(h),
        _ => FrameOrigin::of_url(url),
    }
}

/// Fetch a frame's document: (URL, content type, body, origin).
fn load(vm: &mut Vm, h: &Host, target: NavTarget) -> (Url, String, Vec<u8>, FrameOrigin) {
    let html = "text/html".to_string();
    match target {
        NavTarget::Srcdoc(s) => {
            (Url::parse("about:srcdoc").expect("about:srcdoc parses"), html, s.into_bytes(), origin(h))
        }
        NavTarget::Url(u) if u.scheme() == "about" => (u, html, Vec::new(), origin(h)),
        NavTarget::Url(u) if u.scheme() == "javascript" => {
            // javascript: URLs in frames aren't run yet; the frame shows an empty document.
            log::debug!("javascript: URL in a frame not run");
            (Url::parse("about:blank").expect("about:blank parses"), html, Vec::new(), origin(h))
        }
        NavTarget::Url(u) if u.scheme() == "data" => match data_url(&u) {
            Some((ty, body)) => (u, ty, body, FrameOrigin::opaque()),
            None => (u, html, Vec::new(), FrameOrigin::opaque()),
        },
        NavTarget::Url(u) => {
            let req = crate::net::HttpRequest {
                method: "GET".into(),
                url: u.clone(),
                headers: Vec::new(),
                body: None,
            };
            match crate::net::fetch_now(vm, h, req) {
                Ok(r) => {
                    let ty = r
                        .headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                        .map(|(_, v)| v.to_ascii_lowercase())
                        .unwrap_or_else(|| html.clone());
                    let origin =
                        if r.url.scheme() == "blob" { origin(h) } else { FrameOrigin::of_url(&r.url) };
                    (r.url, ty, r.body, origin)
                }
                Err(e) => {
                    log::debug!("frame {}: {e}", u.href());
                    (u, html, Vec::new(), FrameOrigin::opaque())
                }
            }
        }
    }
}

/// Fetch §4.2 "data: URL processor": (MIME type, body).
fn data_url(u: &Url) -> Option<(String, Vec<u8>)> {
    let href = u.href();
    let rest = href.strip_prefix("data:")?;
    let (mime, data) = rest.split_once(',')?;
    let mime = mime.trim();
    let (mime, b64) = match mime.strip_suffix(";base64").or_else(|| mime.strip_suffix(";BASE64")) {
        Some(m) => (m, true),
        None => (mime, false),
    };
    let bytes = bb_url::percent::percent_decode(data.as_bytes());
    let bytes = if b64 { crate::window::base64_decode(&String::from_utf8_lossy(&bytes))? } else { bytes };
    let mime = if mime.is_empty() { "text/plain".to_string() } else { mime.to_ascii_lowercase() };
    Some((mime, bytes))
}

/// Decode a document's bytes: the BOM, else the Content-Type charset, else UTF-8.
fn decode(body: &[u8], content_type: &str) -> String {
    if let Some((enc, _)) = encoding_rs::Encoding::for_bom(body) {
        return enc.decode_with_bom_removal(body).0.into_owned();
    }
    let label = content_type.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim().eq_ignore_ascii_case("charset").then(|| v.trim().trim_matches('"').to_string())
    });
    let enc =
        label.and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes())).unwrap_or(encoding_rs::UTF_8);
    enc.decode_without_bom_handling(body).0.into_owned()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;")
}

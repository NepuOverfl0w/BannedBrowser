//! Loading a document with scripts (HTML §13.2 parsing with the scripting flag on, §4.12.1
//! script elements, §8.4.3 document.write, §13.2.7 "the end").
//!
//! The parser owns the document while it pumps; between pumps (when a `</script>` paused
//! it) the document is back in `Host::docs[0]` for scripts to use. External scripts are
//! fetched synchronously through the embedder; `defer` and module scripts wait for the end
//! of parsing; `async` ones run as soon as they are fetched (here: at once).

use bb_dom::{Document, NodeId};
use bb_html::TreeBuilder;
use bb_js::vm::heap::NativeCall;
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::host::*;

/// Run the parser on document 0 until a script element closes (or until the tokenizer
/// passes `end`, for document.write).
fn pump(vm: &mut Vm, h: &Host, end: Option<usize>) -> Option<NodeId> {
    let (r, before) = {
        let mut p = h.parser.try_borrow_mut().ok()?;
        let tb = p.as_mut()?;
        let mut docs = h.docs.try_borrow_mut().ok()?;
        let before = docs[0].len();
        std::mem::swap(&mut tb.doc, &mut docs[0]);
        let r = match end {
            Some(e) => tb.pump_until(e),
            None => tb.pump(),
        };
        std::mem::swap(&mut tb.doc, &mut docs[0]);
        h.dirty.set(true);
        (r, before)
    };
    parsed_records(vm, h, before);
    parsed_frames(vm, h, before);
    r
}

/// Frames for the iframes and frames the parser inserted (HTML §4.8.5).
fn parsed_frames(vm: &mut Vm, h: &Host, before: usize) {
    if !h.frames.borrow().nested {
        return;
    }
    let found: Vec<NodeId> = {
        let Ok(docs) = h.docs.try_borrow() else { return };
        let d = &docs[0];
        (before..d.len())
            .map(|i| NodeId(i as u32))
            .filter(|&x| d.is_html(x, "iframe") || d.is_html(x, "frame"))
            .collect()
    };
    if found.is_empty() {
        return;
    }
    let Ok(hr) = host(vm) else { return };
    if !std::ptr::eq(&*hr, h) {
        return;
    }
    if let Err(e) = crate::frames::nodes_inserted(vm, &hr, &found) {
        crate::events::report_exception(vm, e);
    }
}

/// Mutation records for the nodes the parser inserted (HTML §13.2.6.1 "insert an element
/// at the adjusted insertion location" inserts each node as it is created, so each gets a
/// childList record on its parent). New nodes are the ones past `before` in the arena.
fn parsed_records(vm: &mut Vm, h: &Host, before: usize) {
    if !h.mo.borrow().any() {
        return;
    }
    let recs: Vec<(NodeId, NodeId, Option<NodeId>, Option<NodeId>)> = {
        let Ok(docs) = h.docs.try_borrow() else { return };
        let d = &docs[0];
        (before..d.len())
            .filter_map(|i| {
                let id = NodeId(i as u32);
                let p = d.parent(id)?;
                // Siblings as they were when it went in: earlier nodes before it; after it,
                // only nodes that were there already (foster parenting inserts before them).
                let prev = d.prev_sibling(id).filter(|x| x.0 < id.0);
                let next = d.next_sibling(id).filter(|x| (x.0 as usize) < before);
                Some((id, p, prev, next))
            })
            .collect()
    };
    let n = |id: NodeId| NodeRef { doc: 0, id };
    for (id, p, prev, next) in recs {
        if let Err(e) =
            crate::mutation::tree_record(vm, h, n(p), vec![n(id)], Vec::new(), prev.map(n), next.map(n))
        {
            crate::events::report_exception(vm, e);
        }
    }
}

/// Parse `html` into document 0, running its scripts; then the end of parsing (deferred
/// scripts, DOMContentLoaded). The load event is the embedder's to fire, once subresources
/// are in (`fire_load`).
pub fn load(vm: &mut Vm, h: &Host, html: &str) {
    let mut tb = TreeBuilder::new(html);
    tb.set_scripting(true);
    *h.parser.borrow_mut() = Some(Box::new(tb));
    h.ready_state.set(ReadyState::Loading);
    while let Some(script) = pump(vm, h, None) {
        register_handlers(vm, h);
        // HTML §13.2.6.4.8 a "script" end tag: a microtask checkpoint first (the JavaScript
        // execution context stack is empty here).
        vm.run_jobs();
        prepare_script(vm, h, NodeRef { doc: 0, id: script }, true);
    }
    *h.parser.borrow_mut() = None;
    register_handlers(vm, h);
    the_end(vm, h);
}

/// HTML §13.2.7 "the end", up to DOMContentLoaded.
fn the_end(vm: &mut Vm, h: &Host) {
    h.ready_state.set(ReadyState::Interactive);
    let _ = fire_ready_state_change(vm, h);
    let deferred = std::mem::take(&mut *h.deferred.borrow_mut());
    for s in deferred {
        execute_element(vm, h, s);
    }
    if let Ok(doc) = wrap(vm, h, NodeRef { doc: 0, id: Document::ROOT }) {
        if let Err(e) = crate::events::fire(vm, h, doc, "DOMContentLoaded", true, false) {
            crate::events::report_exception(vm, e);
        }
    }
    vm.run_jobs();
}

/// The rest of "the end": readyState "complete" and the window's load event.
pub fn fire_load(vm: &mut Vm, h: &Host) {
    h.ready_state.set(ReadyState::Complete);
    let _ = fire_ready_state_change(vm, h);
    if let Some(w) = h.window.get() {
        if let Err(e) = crate::events::fire(vm, h, Value::Object(w), "load", false, false) {
            crate::events::report_exception(vm, e);
        }
    }
    vm.run_jobs();
}

fn fire_ready_state_change(vm: &mut Vm, h: &Host) -> JsResult<()> {
    let doc = wrap(vm, h, NodeRef { doc: 0, id: Document::ROOT })?;
    crate::events::fire(vm, h, doc, "readystatechange", false, false)?;
    Ok(())
}

/// Register `on…` content attributes of elements added to document 0 since last time.
pub fn register_handlers(vm: &mut Vm, h: &Host) {
    let (from, to) = (h.scanned.get(), h.docs.borrow().first().map_or(0, |d| d.len() as u32));
    if to <= from {
        return;
    }
    h.scanned.set(to);
    let with_handlers: Vec<NodeId> = {
        let docs = h.docs.borrow();
        let d = &docs[0];
        (from..to)
            .map(NodeId)
            .filter(|&n| d.element(n).is_some_and(|e| e.attrs.iter().any(|a| a.name.starts_with("on"))))
            .collect()
    };
    for n in with_handlers {
        if let Err(e) = crate::events::parsed_attributes(vm, h, NodeRef { doc: 0, id: n }) {
            crate::events::report_exception(vm, e);
        }
    }
}

/// What a script element's type says to do (HTML §4.12.1.1 "prepare the script element"
/// steps 8–12).
#[derive(PartialEq)]
enum ScriptType {
    Classic,
    Module,
    None,
}

fn script_type(d: &Document, id: NodeId) -> ScriptType {
    let ty = d.attr(id, "type");
    let lang = d.attr(id, "language");
    let ty = match (ty, lang) {
        (Some(t), _) if !t.is_empty() => t.trim().to_ascii_lowercase(),
        (None, Some(l)) if !l.is_empty() => format!("text/{}", l.to_ascii_lowercase()),
        _ => return ScriptType::Classic,
    };
    const JS: &[&str] = &[
        "application/ecmascript",
        "application/javascript",
        "application/x-ecmascript",
        "application/x-javascript",
        "text/ecmascript",
        "text/javascript",
        "text/javascript1.0",
        "text/javascript1.1",
        "text/javascript1.2",
        "text/javascript1.3",
        "text/javascript1.4",
        "text/javascript1.5",
        "text/jscript",
        "text/livescript",
        "text/x-ecmascript",
        "text/x-javascript",
    ];
    if JS.contains(&ty.as_str()) {
        ScriptType::Classic
    } else if ty == "module" {
        ScriptType::Module
    } else {
        ScriptType::None
    }
}

/// HTML §4.12.1.1 "prepare the script element" (synchronous fetching).
pub fn prepare_script(vm: &mut Vm, h: &Host, s: NodeRef, parser_inserted: bool) {
    if h.started_scripts.borrow().contains(&s.pack()) {
        return;
    }
    let info = with_doc(vm, h, s.doc, |d| {
        let connected = std::iter::once(s.id).chain(d.ancestors(s.id)).last() == Some(Document::ROOT);
        (
            script_type(d, s.id),
            d.attr(s.id, "src").map(str::to_string),
            d.attr(s.id, "defer").is_some(),
            d.attr(s.id, "async").is_some(),
            d.attr(s.id, "nomodule").is_some(),
            connected,
            d.descendants(s.id)
                .filter_map(|x| d.node(x).as_text().filter(|_| d.parent(x) == Some(s.id)))
                .collect::<String>(),
        )
    });
    let Ok((ty, src, defer, is_async, nomodule, connected, text)) = info else { return };
    if !connected || s.doc != 0 {
        return;
    }
    if src.is_none() && text.is_empty() {
        return;
    }
    if ty == ScriptType::None || (ty == ScriptType::Classic && nomodule) {
        return;
    }
    h.started_scripts.borrow_mut().insert(s.pack());
    if (ty == ScriptType::Module || (defer && src.is_some() && !is_async))
        && parser_inserted
        && h.parser.borrow().is_some()
    {
        h.deferred.borrow_mut().push(s);
        return;
    }
    // A script-inserted external script runs when fetched, as a task.
    if !parser_inserted && src.is_some() {
        h.tasks.borrow_mut().push_back(Task::Script(s));
        return;
    }
    execute_element(vm, h, s);
}

/// Run a queued script element (Task::Script).
pub fn run_script_task(vm: &mut Vm, h: &Host, s: NodeRef) {
    execute_element(vm, h, s);
}

/// Fetch (if external) and run a prepared script element (HTML §4.12.1.2 "execute the
/// script element").
fn execute_element(vm: &mut Vm, h: &Host, s: NodeRef) {
    let info = with_doc(vm, h, s.doc, |d| {
        (
            script_type(d, s.id),
            d.attr(s.id, "src").map(str::to_string),
            d.children(s.id).filter_map(|x| d.node(x).as_text().map(str::to_string)).collect::<String>(),
        )
    });
    let Ok((ty, src, text)) = info else { return };
    let (source, url) = match src {
        Some(src) => {
            let base = base_url(h, s.doc);
            let Ok(u) = bb_url::Url::parse_with_base(src.trim(), Some(&base)) else {
                fire_at(vm, h, s, "error");
                return;
            };
            match h.embedder.fetch_script(&u) {
                Ok(t) => (t, Some(u)),
                Err(e) => {
                    log::debug!("script {}: {e}", u.href());
                    fire_at(vm, h, s, "error");
                    return;
                }
            }
        }
        None => (text, None),
    };
    let el = wrap(vm, h, s).unwrap_or(Value::Null);
    let prev = std::mem::replace(
        &mut *h.current_script.borrow_mut(),
        if ty == ScriptType::Module { Value::Null } else { el },
    );
    let key = url.as_ref().map(|u| u.href()).unwrap_or_else(|| h.url.borrow().href());
    // HTML §4.12.1.2 step 4: an external script can't replace the document by writing.
    let external = url.is_some();
    if external {
        h.frames.borrow_mut().ignore_destructive += 1;
    }
    run_source(vm, &source, &key, ty == ScriptType::Module);
    if external {
        let mut f = h.frames.borrow_mut();
        f.ignore_destructive = f.ignore_destructive.saturating_sub(1);
    }
    *h.current_script.borrow_mut() = prev;
    register_handlers(vm, h);
    if url.is_some() {
        fire_at(vm, h, s, "load");
    }
}

/// Run script text, reporting what it throws; then a microtask checkpoint if no other
/// script is running (HTML §8.1.4.4 "clean up after running script").
pub fn run_source(vm: &mut Vm, source: &str, key: &str, module: bool) {
    vm.terminated = false;
    vm.fuel = Some(crate::SCRIPT_FUEL);
    let r = if module { vm.run_module(key, source) } else { vm.run_script_at(source, Some(key)) };
    if let Err(e) = r {
        if vm.terminated {
            log::warn!("script at {key} ran too long and was stopped");
            vm.terminated = false;
        } else {
            crate::events::report_exception(vm, e);
        }
    }
    vm.run_jobs();
    vm.terminated = false;
}

fn fire_at(vm: &mut Vm, h: &Host, n: NodeRef, ty: &str) {
    if let Ok(t) = wrap(vm, h, n) {
        if let Err(e) = crate::events::fire(vm, h, t, ty, false, false) {
            crate::events::report_exception(vm, e);
        }
    }
}

/// document.write / writeln (HTML §8.4.3).
pub fn document_write(vm: &mut Vm, c: &NativeCall, newline: bool) -> JsResult<Value> {
    let mut text = String::new();
    for a in &c.args {
        text.push_str(&rstr(vm, a)?);
    }
    if newline {
        text.push('\n');
    }
    let (h, n) = this_node(vm, &c.this, C::Document)?;
    if n.doc != 0 {
        // Documents without a browsing context have no parser to write to (not done yet).
        log::debug!("document.write to a document that isn't a window's ignored");
        return Ok(Value::Undefined);
    }
    if h.doc_types.borrow().first().map(String::as_str) != Some("text/html") {
        return crate::exception::throw(vm, "InvalidStateError", "document.write needs an HTML document");
    }
    // Step 5: no insertion point means a new document (unless an external script is running).
    if h.parser.borrow().is_none() {
        if h.frames.borrow().ignore_destructive > 0 {
            return Ok(Value::Undefined);
        }
        document_open(vm, &h, n.doc)?;
        if h.parser.borrow().is_none() {
            return Ok(Value::Undefined);
        }
    }
    // Insert at the insertion point; earlier writes' ends move along.
    let end = {
        let mut p = h.parser.borrow_mut();
        let Some(tb) = p.as_mut() else { return Ok(Value::Undefined) };
        let tok = tb.tokenizer_mut();
        let pos = tok.position();
        let end = tok.insert_input(&text);
        let n = end - pos;
        for e in h.write_ends.borrow_mut().iter_mut() {
            if *e >= pos {
                *e += n;
            }
        }
        end
    };
    h.write_ends.borrow_mut().push(end);
    loop {
        let end = h.write_ends.borrow().last().copied().unwrap_or(end);
        match pump(vm, &h, Some(end)) {
            Some(script) => {
                register_handlers(vm, &h);
                prepare_script(vm, &h, NodeRef { doc: 0, id: script }, true);
            }
            None => break,
        }
    }
    h.write_ends.borrow_mut().pop();
    register_handlers(vm, &h);
    Ok(Value::Undefined)
}

/// A script element inserted by a script (HTML §4.12.1 "node insertion steps"): it runs.
/// The parser's own insertions don't come here: it prepares scripts at their end tags.
pub fn script_inserted(vm: &mut Vm, h: &Host, s: NodeRef) {
    prepare_script(vm, h, s, false);
}

/// HTML §8.4.1 "document open steps" for the window's document: its children go, and a
/// script-created parser takes what document.write inserts, until document.close.
pub fn document_open(vm: &mut Vm, h: &Host, doc: u32) -> JsResult<()> {
    if h.doc_types.borrow().get(doc as usize).map(String::as_str) != Some("text/html") {
        return crate::exception::throw(vm, "InvalidStateError", "document.open needs an HTML document");
    }
    // Only the window's document has a parser here; an active parser keeps the document.
    if doc != 0 || h.parser.borrow().is_some() || h.frames.borrow().ignore_destructive > 0 {
        return Ok(());
    }
    // Step 11: erase the document's and the window's event listeners and handlers.
    let doc_obj = wrap(vm, h, NodeRef { doc: 0, id: Document::ROOT })?;
    let mut targets: Vec<bb_js::vm::heap::ObjRef> = Vec::new();
    if let Value::Object(o) = doc_obj {
        targets.push(o);
    }
    targets.extend(h.window.get());
    for t in &targets {
        h.listeners.borrow_mut().remove(t);
        h.handlers.borrow_mut().retain(|(o, _), _| o != t);
    }
    // Step 12: replace all with null.
    crate::tree::replace_all(vm, h, None, NodeRef { doc: 0, id: Document::ROOT })?;
    // Steps 15–18: a new script-created parser, readiness "loading".
    let mut tb = TreeBuilder::new("");
    tb.set_scripting(true);
    *h.parser.borrow_mut() = Some(Box::new(tb));
    h.frames.borrow_mut().script_parser = true;
    h.ready_state.set(ReadyState::Loading);
    Ok(())
}

/// HTML §8.4.2 document.close: the script-created parser reaches the end of its input, then
/// "the end" (DOMContentLoaded, the load event).
pub fn document_close(vm: &mut Vm, h: &Host, doc: u32) -> JsResult<()> {
    if h.doc_types.borrow().get(doc as usize).map(String::as_str) != Some("text/html") {
        return crate::exception::throw(vm, "InvalidStateError", "document.close needs an HTML document");
    }
    if doc != 0 || !h.frames.borrow().script_parser || h.parser.borrow().is_none() {
        return Ok(());
    }
    h.frames.borrow_mut().script_parser = false;
    while let Some(script) = pump(vm, h, None) {
        register_handlers(vm, h);
        prepare_script(vm, h, NodeRef { doc: 0, id: script }, true);
    }
    *h.parser.borrow_mut() = None;
    register_handlers(vm, h);
    the_end(vm, h);
    fire_load(vm, h);
    Ok(())
}

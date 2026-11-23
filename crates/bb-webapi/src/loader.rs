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
fn pump(h: &Host, end: Option<usize>) -> Option<NodeId> {
    let mut p = h.parser.try_borrow_mut().ok()?;
    let tb = p.as_mut()?;
    let mut docs = h.docs.try_borrow_mut().ok()?;
    std::mem::swap(&mut tb.doc, &mut docs[0]);
    let r = match end {
        Some(e) => tb.pump_until(e),
        None => tb.pump(),
    };
    std::mem::swap(&mut tb.doc, &mut docs[0]);
    h.dirty.set(true);
    r
}

/// Parse `html` into document 0, running its scripts; then the end of parsing (deferred
/// scripts, DOMContentLoaded). The load event is the embedder's to fire, once subresources
/// are in (`fire_load`).
pub fn load(vm: &mut Vm, h: &Host, html: &str) {
    let mut tb = TreeBuilder::new(html);
    tb.set_scripting(true);
    *h.parser.borrow_mut() = Some(Box::new(tb));
    h.ready_state.set(ReadyState::Loading);
    while let Some(script) = pump(h, None) {
        register_handlers(vm, h);
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
    run_source(vm, &source, &key, ty == ScriptType::Module);
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
    if n.doc != 0 || h.parser.borrow().is_none() {
        // No insertion point: document.open() would replace the document; not done here.
        log::debug!("document.write after parsing ignored");
        return Ok(Value::Undefined);
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
        match pump(&h, Some(end)) {
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
pub fn script_inserted(vm: &mut Vm, h: &Host, s: NodeRef) {
    prepare_script(vm, h, s, false);
}

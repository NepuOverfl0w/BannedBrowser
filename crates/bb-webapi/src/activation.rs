//! Activation behavior for clicks scripts dispatch (`element.click()`, `dispatchEvent` of a
//! click MouseEvent): DOM §2.9 "dispatch" steps 5 (the activation target), 11 (legacy-pre-
//! activation behavior), 13 (legacy-canceled-activation behavior) and 14 (activation
//! behavior), with HTML's behaviors for checkboxes and radio buttons (§4.10.5.1.15–16), submit
//! and reset buttons (§4.10.6, §4.10.5.1.19–21), links (§4.6.4 "follow the hyperlink"),
//! labels (§4.10.4) and summaries (§4.11.2).
//!
//! The browser performs the default actions of the user's own (trusted) clicks itself, so
//! these run for untrusted clicks only.

use bb_dom::{InputType, NodeId};
use bb_js::{JsResult, Vm};

use crate::host::*;

/// What legacy-pre-activation changed, to undo if the click is canceled.
pub enum Saved {
    Nothing,
    /// A checkbox's old checkedness.
    Checkbox(bool),
    /// A radio button's old checkedness and the group member that was checked.
    Radio(bool, Option<NodeId>),
}

/// Whether element `n` of document `doc` has activation behavior.
pub fn has_activation(d: &bb_dom::Document, n: NodeId) -> bool {
    if d.is_html(n, "input") {
        return matches!(
            d.input_type(n),
            InputType::Checkbox | InputType::Radio | InputType::Submit | InputType::Image | InputType::Reset
        );
    }
    (d.is_html(n, "a") || d.is_html(n, "area")) && d.attr(n, "href").is_some()
        || d.is_html(n, "button")
        || d.is_html(n, "label")
        || d.is_html(n, "summary") && d.parent(n).is_some_and(|p| d.is_html(p, "details"))
}

/// DOM §2.9 step 5: the target if it has activation behavior, else (for bubbling events) the
/// nearest ancestor that does.
pub fn target(vm: &mut Vm, h: &Host, t: NodeRef, bubbles: bool) -> JsResult<Option<NodeRef>> {
    with_doc(vm, h, t.doc, |d| {
        if has_activation(d, t.id) {
            return Some(t);
        }
        if !bubbles {
            return None;
        }
        d.ancestors(t.id).find(|&a| has_activation(d, a)).map(|a| t.with(a))
    })
}

/// Legacy-pre-activation behavior: a checkbox toggles, a radio button is checked.
pub fn pre(vm: &mut Vm, h: &Host, t: NodeRef) -> JsResult<Saved> {
    let kind = with_doc(vm, h, t.doc, |d| d.is_html(t.id, "input").then(|| d.input_type(t.id)))?;
    Ok(match kind {
        Some(InputType::Checkbox) => {
            let old = with_doc_mut(vm, h, t.doc, |d| {
                let old = d.checkedness(t.id);
                d.set_checkedness(t.id, !old);
                old
            })?;
            Saved::Checkbox(old)
        }
        Some(InputType::Radio) => with_doc_mut(vm, h, t.doc, |d| {
            let old = d.checkedness(t.id);
            let prev = d.radio_group(t.id).into_iter().find(|&r| r != t.id && d.checkedness(r));
            d.set_checkedness(t.id, true);
            Saved::Radio(old, prev)
        })?,
        _ => Saved::Nothing,
    })
}

/// Legacy-canceled-activation behavior: put the checkedness back.
pub fn canceled(vm: &mut Vm, h: &Host, t: NodeRef, saved: &Saved) -> JsResult<()> {
    match *saved {
        Saved::Checkbox(old) => with_doc_mut(vm, h, t.doc, |d| d.set_checkedness(t.id, old)),
        Saved::Radio(old, prev) => with_doc_mut(vm, h, t.doc, |d| {
            if !old {
                d.set_checkedness(t.id, false);
                if let Some(p) = prev {
                    d.set_checkedness(p, true);
                }
            }
        }),
        Saved::Nothing => Ok(()),
    }
}

fn connected(d: &bb_dom::Document, n: NodeId) -> bool {
    d.is_connected(n)
}

fn fire(vm: &mut Vm, h: &Host, t: NodeRef, ty: &str, cancelable: bool) -> JsResult<bool> {
    let e = crate::events::trusted_event(vm, h, crate::class::C::Event, ty, true, cancelable);
    let target = wrap(vm, h, t)?;
    crate::events::dispatch(vm, h, e, target)
}

/// Activation behavior.
pub fn activate(vm: &mut Vm, h: &Host, t: NodeRef, saved: &Saved) -> JsResult<()> {
    enum What {
        Changed,
        Submit(NodeId),
        Reset(NodeId),
        Link(String, String),
        Label,
        Summary(NodeId),
        Nothing,
    }
    let what = with_doc(vm, h, t.doc, |d| {
        let n = t.id;
        let is_input = d.is_html(n, "input");
        let ty = if is_input { Some(d.input_type(n)) } else { None };
        let button_type = d.is_html(n, "button").then(|| {
            d.attr(n, "type").map(|t| t.trim().to_ascii_lowercase()).unwrap_or_else(|| "submit".into())
        });
        match (ty, button_type.as_deref()) {
            (Some(InputType::Checkbox), _) => What::Changed,
            (Some(InputType::Radio), _) => {
                if matches!(saved, Saved::Radio(false, _)) {
                    What::Changed
                } else {
                    What::Nothing
                }
            }
            (Some(InputType::Submit | InputType::Image), _) | (None, Some("submit")) if !d.is_disabled(n) => {
                d.form_owner(n).map_or(What::Nothing, What::Submit)
            }
            (Some(InputType::Reset), _) | (None, Some("reset")) if !d.is_disabled(n) => {
                d.form_owner(n).map_or(What::Nothing, What::Reset)
            }
            _ if d.is_html(n, "a") || d.is_html(n, "area") => What::Link(
                d.attr(n, "href").unwrap_or("").to_string(),
                d.attr(n, "target").unwrap_or("").trim().to_ascii_lowercase(),
            ),
            _ if d.is_html(n, "label") => What::Label,
            _ if d.is_html(n, "summary") => d.parent(n).map_or(What::Nothing, What::Summary),
            _ => What::Nothing,
        }
    })?;
    match what {
        What::Changed => {
            // HTML §4.10.5.1.15: input then change, if the element is still connected.
            if with_doc(vm, h, t.doc, |d| connected(d, t.id))? {
                fire(vm, h, t, "input", false)?;
                fire(vm, h, t, "change", false)?;
            }
        }
        What::Submit(form) => {
            // HTML §4.10.21.3 "submit" from a submitter: the submit event, then the embedder
            // submits (only the window's document submits).
            if t.doc == 0 && crate::events::fire_submit(vm, h, t.with(form), Some(t))? {
                h.pending_submit.set(Some(t.with(form)));
            }
        }
        What::Reset(form) => {
            if fire(vm, h, t.with(form), "reset", true)? {
                with_doc_mut(vm, h, t.doc, |d| d.reset_form(form))?;
            }
        }
        What::Link(href, target) => {
            // §4.6.4 "follow the hyperlink" in this window (other targets open nothing yet).
            if t.doc == 0 && matches!(target.as_str(), "" | "_self") {
                let base = base_url(h, 0);
                if let Ok(u) = bb_url::Url::parse_with_base(href.trim(), Some(&base)) {
                    if u.scheme() != "javascript" {
                        *h.pending_navigation.borrow_mut() = Some(u.href());
                    }
                }
            }
        }
        What::Label => {
            // §4.10.4: the labeled control gets a click, unless the click came from inside it.
            let control = with_doc(vm, h, t.doc, |d| d.labeled_control(t.id))?;
            if let Some(c) = control {
                crate::events::fire_synthetic_click(vm, h, t.with(c))?;
            }
        }
        What::Summary(details) => {
            let open = with_doc(vm, h, t.doc, |d| d.attr(details, "open").is_some())?;
            let el = t.with(details);
            crate::mutation::set_or_remove(vm, h, el, "open", if open { None } else { Some("") })?;
        }
        What::Nothing => {}
    }
    Ok(())
}

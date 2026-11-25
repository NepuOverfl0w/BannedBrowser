//! Form control state (HTML §4.10): values, checkedness, `<select>` options, form owners and
//! `form.elements`, read from and written to bb-dom's form state (shared with the page's
//! own interaction code).

use bb_dom::NodeData;
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::collections::{collection, CollKind};
use crate::host::*;
use crate::idl::{attr, op};

type R = JsResult<Value>;

fn el(vm: &mut Vm, c: &NativeCall, iface: C) -> JsResult<(std::rc::Rc<Host>, NodeRef)> {
    this_node(vm, &c.this, iface)
}

fn form_attr(vm: &mut Vm, c: NativeCall) -> R {
    let (h, n) = el(vm, &c, C::HTMLElement)?;
    let f = with_doc(vm, &h, n.doc, |d| d.form_owner(n.id))?;
    wrap_opt(vm, &h, f.map(|f| n.with(f)))
}

fn value_get(vm: &mut Vm, c: NativeCall) -> R {
    let (h, n) = el(vm, &c, C::HTMLElement)?;
    let v = with_doc(vm, &h, n.doc, |d| d.control_value(n.id))?;
    Ok(js(&v))
}

fn value_set(vm: &mut Vm, c: NativeCall) -> R {
    let v = rstr_null_empty(vm, &c.arg(0))?;
    let (h, n) = el(vm, &c, C::HTMLElement)?;
    with_doc_mut(vm, &h, n.doc, |d| {
        d.set_control_value(n.id, &v);
        // Not a user edit: maxlength/minlength don't apply to it (§4.10.5.3.3).
        d.mark_user_edited(n.id, false);
    })?;
    Ok(Value::Undefined)
}

pub fn setup_input(vm: &mut Vm, proto: ObjRef) {
    attr(vm, proto, "value", value_get, Some(value_set));
    attr(vm, proto, "form", form_attr, None);
    attr(
        vm,
        proto,
        "type",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLInputElement)?;
            let t = with_doc(vm, &h, n.doc, |d| d.input_type(n.id).keyword())?;
            Ok(js(t))
        },
        Some(|vm, c| {
            let v = rstr(vm, &c.arg(0))?;
            let (h, n) = el(vm, &c, C::HTMLInputElement)?;
            crate::mutation::set_or_remove(vm, &h, n, "type", Some(&v))?;
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "checked",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLInputElement)?;
            Ok(Value::Bool(with_doc(vm, &h, n.doc, |d| d.checkedness(n.id))?))
        },
        Some(|vm, c| {
            let v = vm.to_boolean(&c.arg(0));
            let (h, n) = el(vm, &c, C::HTMLInputElement)?;
            with_doc_mut(vm, &h, n.doc, |d| d.set_checkedness(n.id, v))?;
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "indeterminate",
        |vm, c| {
            el(vm, &c, C::HTMLInputElement)?;
            Ok(Value::Bool(false))
        },
        Some(|vm, c| {
            el(vm, &c, C::HTMLInputElement)?;
            Ok(Value::Undefined)
        }),
    );
    // select(): the text selection is the browser's (nothing to select from scripts yet).
    op(vm, proto, "select", 0, |vm, c| {
        el(vm, &c, C::HTMLInputElement)?;
        Ok(Value::Undefined)
    });
    setup_validation(vm, proto);
}

pub fn setup_button(vm: &mut Vm, proto: ObjRef) {
    setup_validation(vm, proto);
    attr(vm, proto, "form", form_attr, None);
    attr(
        vm,
        proto,
        "type",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLButtonElement)?;
            let t = with_doc(vm, &h, n.doc, |d| d.attr(n.id, "type").unwrap_or("").to_ascii_lowercase())?;
            Ok(js(if matches!(t.as_str(), "reset" | "button") { &t } else { "submit" }))
        },
        Some(|vm, c| {
            let v = rstr(vm, &c.arg(0))?;
            let (h, n) = el(vm, &c, C::HTMLButtonElement)?;
            crate::mutation::set_or_remove(vm, &h, n, "type", Some(&v))?;
            Ok(Value::Undefined)
        }),
    );
}

pub fn setup_textarea(vm: &mut Vm, proto: ObjRef) {
    setup_validation(vm, proto);
    attr(vm, proto, "value", value_get, Some(value_set));
    attr(vm, proto, "form", form_attr, None);
    attr(vm, proto, "defaultValue", crate::dom::text_content_get, Some(crate::dom::text_content_set));
    attr(
        vm,
        proto,
        "textLength",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLTextAreaElement)?;
            let v = with_doc(vm, &h, n.doc, |d| d.control_value(n.id))?;
            Ok(Value::Number(utf16_len(&v) as f64))
        },
        None,
    );
    attr(
        vm,
        proto,
        "type",
        |vm, c| {
            el(vm, &c, C::HTMLTextAreaElement)?;
            Ok(js("textarea"))
        },
        None,
    );
}

pub fn setup_select(vm: &mut Vm, proto: ObjRef) {
    setup_validation(vm, proto);
    // §4.10.7 add(element, before): an option or optgroup goes in before `before` (an
    // element, or the index of an option), or at the end.
    op(vm, proto, "add", 1, |vm, c| {
        let (h, n) = el(vm, &c, C::HTMLSelectElement)?;
        let elm = match vm.host_data(&c.arg(0)) {
            Some((k, _)) if k == C::HTMLOptionElement as u32 || k == C::HTMLOptGroupElement as u32 => {
                node_of(vm, &c.arg(0)).ok_or(())
            }
            _ => Err(()),
        };
        let Ok(elm) = elm else { return vm.type_error("add() takes an option or optgroup") };
        let before = match c.arg(1) {
            Value::Undefined | Value::Null => None,
            v if node_of(vm, &v).is_some() => node_of(vm, &v),
            v => {
                let i = vm.to_int32(&v)?;
                let opts = with_doc(vm, &h, n.doc, |d| d.list_of_options(n.id))?;
                usize::try_from(i).ok().and_then(|i| opts.get(i).copied()).map(|o| n.with(o))
            }
        };
        let (cycle, foreign) = with_doc(vm, &h, n.doc, |d| {
            let cycle = elm.doc == n.doc && crate::tree::is_inclusive_ancestor(d, elm.id, n.id);
            let foreign = before.is_some_and(|b| b.doc != n.doc || !d.ancestors(b.id).any(|a| a == n.id));
            (cycle, foreign)
        })?;
        if cycle {
            return crate::exception::throw(vm, "HierarchyRequestError", "the element contains the select");
        }
        if foreign {
            return crate::exception::throw(vm, "NotFoundError", "the reference element isn't in the select");
        }
        if before == Some(elm) {
            return Ok(Value::Undefined);
        }
        let parent = match before {
            Some(b) => with_doc(vm, &h, n.doc, |d| d.parent(b.id))?.map_or(n, |p| n.with(p)),
            None => n,
        };
        crate::tree::pre_insert(vm, &h, elm, parent, before)?;
        Ok(Value::Undefined)
    });
    // remove(index) removes an option; remove() is ChildNode's.
    op(vm, proto, "remove", 0, |vm, c| {
        let (h, n) = el(vm, &c, C::HTMLSelectElement)?;
        if c.args.is_empty() {
            crate::tree::remove(vm, &h, n, false)?;
            return Ok(Value::Undefined);
        }
        let i = vm.to_int32(&c.arg(0))?;
        let opts = with_doc(vm, &h, n.doc, |d| d.list_of_options(n.id))?;
        if let Some(&o) = usize::try_from(i).ok().and_then(|i| opts.get(i)) {
            crate::tree::remove(vm, &h, n.with(o), false)?;
        }
        Ok(Value::Undefined)
    });
    op(vm, proto, "item", 1, |vm, c| {
        let (h, n) = el(vm, &c, C::HTMLSelectElement)?;
        let i = vm.to_uint32(&c.arg(0))? as usize;
        let o = with_doc(vm, &h, n.doc, |d| d.list_of_options(n.id).get(i).copied())?;
        wrap_opt(vm, &h, o.map(|o| n.with(o)))
    });
    op(vm, proto, "namedItem", 1, |vm, c| {
        let name = rstr(vm, &c.arg(0))?;
        let (h, n) = el(vm, &c, C::HTMLSelectElement)?;
        let o = with_doc(vm, &h, n.doc, |d| {
            d.list_of_options(n.id)
                .into_iter()
                .find(|&o| d.attr(o, "id") == Some(name.as_str()) || d.attr(o, "name") == Some(name.as_str()))
        })?;
        wrap_opt(vm, &h, o.map(|o| n.with(o)))
    });
    attr(vm, proto, "form", form_attr, None);
    attr(
        vm,
        proto,
        "options",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLSelectElement)?;
            collection(vm, &h, CollKind::Options(n), Some((5, n.pack())))
        },
        None,
    );
    attr(
        vm,
        proto,
        "length",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLSelectElement)?;
            Ok(Value::Number(with_doc(vm, &h, n.doc, |d| d.list_of_options(n.id).len())? as f64))
        },
        None,
    );
    attr(
        vm,
        proto,
        "type",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLSelectElement)?;
            let m = with_doc(vm, &h, n.doc, |d| d.attr(n.id, "multiple").is_some())?;
            Ok(js(if m { "select-multiple" } else { "select-one" }))
        },
        None,
    );
    attr(
        vm,
        proto,
        "selectedIndex",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLSelectElement)?;
            let i = with_doc(vm, &h, n.doc, |d| {
                d.list_of_options(n.id).iter().position(|&o| d.option_selectedness(o))
            })?;
            Ok(Value::Number(i.map_or(-1.0, |i| i as f64)))
        },
        Some(|vm, c| {
            let i = vm.to_int32(&c.arg(0))?;
            let (h, n) = el(vm, &c, C::HTMLSelectElement)?;
            with_doc_mut(vm, &h, n.doc, |d| {
                let opts = d.list_of_options(n.id);
                if let Some(&o) = usize::try_from(i).ok().and_then(|i| opts.get(i)) {
                    d.user_select_option(n.id, o);
                }
            })?;
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "value",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLSelectElement)?;
            let v = with_doc(vm, &h, n.doc, |d| {
                d.list_of_options(n.id)
                    .into_iter()
                    .find(|&o| d.option_selectedness(o))
                    .map(|o| option_value(d, o))
            })?;
            Ok(js(&v.unwrap_or_default()))
        },
        Some(|vm, c| {
            let v = rstr(vm, &c.arg(0))?;
            let (h, n) = el(vm, &c, C::HTMLSelectElement)?;
            with_doc_mut(vm, &h, n.doc, |d| {
                let opts = d.list_of_options(n.id);
                if let Some(&o) = opts.iter().find(|&&o| option_value(d, o) == v) {
                    d.user_select_option(n.id, o);
                }
            })?;
            Ok(Value::Undefined)
        }),
    );
}

/// HTML §4.10.10 option value: its value attribute, else its text.
fn option_value(d: &bb_dom::Document, o: bb_dom::NodeId) -> String {
    match d.attr(o, "value") {
        Some(v) => v.to_string(),
        None => option_text(d, o),
    }
}

/// HTML §4.10.10 option text: descendant text (not in scripts), whitespace collapsed.
fn option_text(d: &bb_dom::Document, o: bb_dom::NodeId) -> String {
    let mut s = String::new();
    for x in d.descendants(o) {
        if let NodeData::Text(t) = &d.node(x).data {
            if !d.ancestors(x).take_while(|&a| a != o).any(|a| d.is_html(a, "script")) {
                s.push_str(t);
            }
        }
    }
    s.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn setup_option(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "value",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLOptionElement)?;
            Ok(js(&with_doc(vm, &h, n.doc, |d| option_value(d, n.id))?))
        },
        Some(|vm, c| {
            let v = rstr(vm, &c.arg(0))?;
            let (h, n) = el(vm, &c, C::HTMLOptionElement)?;
            crate::mutation::set_or_remove(vm, &h, n, "value", Some(&v))?;
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "text",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLOptionElement)?;
            Ok(js(&with_doc(vm, &h, n.doc, |d| option_text(d, n.id))?))
        },
        Some(crate::dom::text_content_set),
    );
    attr(
        vm,
        proto,
        "selected",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLOptionElement)?;
            Ok(Value::Bool(with_doc(vm, &h, n.doc, |d| d.option_selectedness(n.id))?))
        },
        Some(|vm, c| {
            let v = vm.to_boolean(&c.arg(0));
            let (h, n) = el(vm, &c, C::HTMLOptionElement)?;
            with_doc_mut(vm, &h, n.doc, |d| {
                if v {
                    if let Some(s) = d.nearest_ancestor_select(n.id) {
                        d.user_select_option(s, n.id);
                    }
                }
            })?;
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "index",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLOptionElement)?;
            let i = with_doc(vm, &h, n.doc, |d| {
                d.nearest_ancestor_select(n.id)
                    .and_then(|s| d.list_of_options(s).iter().position(|&o| o == n.id))
            })?;
            Ok(Value::Number(i.unwrap_or(0) as f64))
        },
        None,
    );
    attr(vm, proto, "form", form_attr, None);
}

/// HTMLFieldSetElement (§4.10.15): `elements`, `form`, `type`.
pub fn setup_fieldset(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "elements",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLFieldSetElement)?;
            collection(vm, &h, CollKind::Filtered(n, DescFilter::FieldsetElements), Some((30, n.pack())))
        },
        None,
    );
    attr(vm, proto, "form", form_attr, None);
    setup_validation(vm, proto);
    attr(
        vm,
        proto,
        "type",
        |vm, c| {
            el(vm, &c, C::HTMLFieldSetElement)?;
            Ok(js("fieldset"))
        },
        None,
    );
}

/// HTMLLabelElement (§4.10.4): `control` and `form`.
pub fn setup_label(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "control",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLLabelElement)?;
            let t = with_doc(vm, &h, n.doc, |d| d.labeled_control(n.id))?;
            wrap_opt(vm, &h, t.map(|t| n.with(t)))
        },
        None,
    );
    attr(
        vm,
        proto,
        "form",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLLabelElement)?;
            let f = with_doc(vm, &h, n.doc, |d| d.labeled_control(n.id).and_then(|t| d.form_owner(t)))?;
            wrap_opt(vm, &h, f.map(|f| n.with(f)))
        },
        None,
    );
}

pub fn setup_form(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "elements",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLFormElement)?;
            collection(vm, &h, CollKind::FormElements(n), Some((6, n.pack())))
        },
        None,
    );
    attr(
        vm,
        proto,
        "length",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLFormElement)?;
            Ok(Value::Number(with_doc(vm, &h, n.doc, |d| d.form_controls(n.id).len())? as f64))
        },
        None,
    );
    attr(
        vm,
        proto,
        "method",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLFormElement)?;
            let m = with_doc(vm, &h, n.doc, |d| d.attr(n.id, "method").unwrap_or("").to_ascii_lowercase())?;
            Ok(js(if matches!(m.as_str(), "post" | "dialog") { &m } else { "get" }))
        },
        Some(|vm, c| {
            let v = rstr(vm, &c.arg(0))?;
            let (h, n) = el(vm, &c, C::HTMLFormElement)?;
            crate::mutation::set_or_remove(vm, &h, n, "method", Some(&v))?;
            Ok(Value::Undefined)
        }),
    );
    // enctype and its alias encoding reflect the enctype attribute, limited to known values
    // (HTML §4.10.18.6).
    for name in ["enctype", "encoding"] {
        attr(
            vm,
            proto,
            name,
            |vm, c| {
                let (h, n) = el(vm, &c, C::HTMLFormElement)?;
                let m =
                    with_doc(vm, &h, n.doc, |d| d.attr(n.id, "enctype").unwrap_or("").to_ascii_lowercase())?;
                Ok(js(if matches!(m.as_str(), "multipart/form-data" | "text/plain") {
                    &m
                } else {
                    "application/x-www-form-urlencoded"
                }))
            },
            Some(|vm, c| {
                let v = rstr(vm, &c.arg(0))?;
                let (h, n) = el(vm, &c, C::HTMLFormElement)?;
                crate::mutation::set_or_remove(vm, &h, n, "enctype", Some(&v))?;
                Ok(Value::Undefined)
            }),
        );
    }
    op(vm, proto, "reset", 0, |vm, c| {
        let (h, n) = el(vm, &c, C::HTMLFormElement)?;
        // [CEReactions]: formResetCallback runs before reset() returns.
        crate::ce::enter(&h);
        let r = reset(vm, &h, n);
        crate::ce::leave(vm, &h);
        r?;
        Ok(Value::Undefined)
    });
    // submit() submits without a submit event; requestSubmit(submitter) fires one first and
    // submits unless it is canceled (HTML §4.10.18.4). The embedder does the submission.
    op(vm, proto, "submit", 0, |vm, c| {
        let (h, n) = el(vm, &c, C::HTMLFormElement)?;
        h.pending_submit.set(Some(n));
        Ok(Value::Undefined)
    });
    op(vm, proto, "requestSubmit", 0, |vm, c| {
        let (h, n) = el(vm, &c, C::HTMLFormElement)?;
        let submitter = match c.arg(0) {
            Value::Undefined | Value::Null => None,
            v => {
                let Some(s) = node_of(vm, &v) else {
                    return vm.type_error("the submitter must be an element");
                };
                let ok = with_doc(vm, &h, s.doc, |d| {
                    d.form_owner(s.id) == Some(n.id) && crate::forms::submit_button(d, s.id)
                })?;
                if !ok || s.doc != n.doc {
                    return crate::exception::throw(
                        vm,
                        "NotFoundError",
                        "the submitter isn't a submit button of this form",
                    );
                }
                Some(s)
            }
        };
        if crate::events::fire_submit(vm, &h, n, submitter)? {
            h.pending_submit.set(Some(n));
        }
        Ok(Value::Undefined)
    });
    for name in ["checkValidity", "reportValidity"] {
        op(vm, proto, name, 0, |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLFormElement)?;
            Ok(Value::Bool(check_form(vm, &h, n)?))
        });
    }
}

/// HTML §4.10.21.5 "reset" form `form` (after its reset event): the controls' reset
/// algorithms, then its form-associated custom elements' formResetCallback.
/// (Called from `form.reset()` its callbacks run at the end of that [CEReactions] scope;
/// from a reset button, in the backup element queue.)
pub fn reset(vm: &mut Vm, h: &Host, form: NodeRef) -> JsResult<()> {
    let r = with_doc_mut(vm, h, form.doc, |d| d.reset_form(form.id));
    crate::internals::form_reset(vm, h, form);
    h.mark_dirty();
    r
}

// ---- constraint validation (HTML §4.10.21) ----

/// HTML §4.10.5.3.6: whether `value` matches `pattern` compiled as `^(?:pattern)$` with the
/// `v` flag (None if the pattern isn't a valid regular expression with that flag).
fn pattern_matches(pattern: &str, value: &str) -> Option<bool> {
    bb_js::regexp::validate(pattern, true, true).ok()?;
    let body = format!("^(?:{pattern})$");
    bb_js::regexp::validate(&body, true, true).ok()?;
    let flags = bb_js::vm::regex::Flags { unicode: true, sets: true, ..Default::default() };
    let re = bb_js::vm::regex::Regex::new(&body.encode_utf16().collect::<Vec<u16>>(), flags);
    let text: Vec<u16> = value.encode_utf16().collect();
    let mut budget = 10_000_000u64;
    match re.match_at(&text, 0, &mut budget) {
        Ok(m) => Some(m.is_some()),
        // A pattern that backtracks past the budget doesn't constrain.
        Err(_) => None,
    }
}

/// Give bb-dom the checks it can't do itself (patterns, absolute URLs).
pub fn install_validity_hooks() {
    bb_dom::validity::set_hooks(bb_dom::validity::Hooks {
        pattern_matches,
        valid_absolute_url: |s| bb_url::Url::parse(s).is_ok(),
    });
}

/// §4.10.21.2 "check validity" of a built-in control or form-associated custom element: an
/// invalid candidate fires a cancelable `invalid`; returns (valid, the event wasn't
/// canceled).
pub fn check_element(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<(bool, bool)> {
    let (candidate, face) =
        with_doc(vm, h, n.doc, |d| (d.is_validation_candidate(n.id), d.is_form_associated_custom(n.id)))?;
    if face {
        return Ok((crate::internals::check_face(vm, h, n)?, true));
    }
    if !candidate || with_doc(vm, h, n.doc, |d| d.is_valid_control(n.id))? {
        return Ok((true, true));
    }
    let t = wrap(vm, h, n)?;
    let not_canceled = crate::events::fire(vm, h, t, "invalid", false, true)?;
    Ok((false, not_canceled))
}

/// §4.10.21.2 "statically validate the constraints" of a form: `invalid` at each invalid
/// candidate; returns whether all were valid.
pub fn check_form(vm: &mut Vm, h: &Host, form: NodeRef) -> JsResult<bool> {
    let controls = with_doc(vm, h, form.doc, |d| d.form_controls(form.id))?;
    let mut ok = true;
    for c in controls {
        if !check_element(vm, h, form.with(c))?.0 {
            ok = false;
        }
    }
    Ok(ok)
}

/// The constraint validation API on a control interface (§4.10.21.3): willValidate,
/// validity, validationMessage, checkValidity, reportValidity, setCustomValidity.
pub fn setup_validation(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "willValidate",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLElement)?;
            Ok(Value::Bool(with_doc(vm, &h, n.doc, |d| d.is_validation_candidate(n.id))?))
        },
        None,
    );
    attr(
        vm,
        proto,
        "validity",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLElement)?;
            crate::collections::cached_object(vm, &h, (40, n.pack()), |vm, h| {
                vm.new_host_object(Some(h.proto(C::ValidityState)), C::ValidityState as u32, n.pack(), false)
            })
        },
        None,
    );
    attr(
        vm,
        proto,
        "validationMessage",
        |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLElement)?;
            Ok(js(&with_doc(vm, &h, n.doc, |d| d.validation_message(n.id))?))
        },
        None,
    );
    for name in ["checkValidity", "reportValidity"] {
        // reportValidity would also show the problem; the browser does that only when a
        // submission is blocked (there is no reporting from scripts yet).
        op(vm, proto, name, 0, |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLElement)?;
            Ok(Value::Bool(check_element(vm, &h, n)?.0))
        });
    }
    op(vm, proto, "setCustomValidity", 1, |vm, c| {
        let m = rstr(vm, &c.arg(0))?;
        let (h, n) = el(vm, &c, C::HTMLElement)?;
        with_doc_mut(vm, &h, n.doc, |d| d.set_custom_validity(n.id, &bb_dom::normalize_newlines(&m)))?;
        h.mark_dirty();
        Ok(Value::Undefined)
    });
}

/// Whether `el` is a submit button (HTML §4.10.2): a `<button>` in the Submit Button state
/// (the default), `<input type=submit>` or `<input type=image>`.
pub fn submit_button(d: &bb_dom::Document, el: bb_dom::NodeId) -> bool {
    if d.is_html(el, "button") {
        let ty = d.attr(el, "type").map(|t| t.trim().to_ascii_lowercase());
        return !matches!(ty.as_deref(), Some("button") | Some("reset"));
    }
    d.is_html(el, "input") && matches!(d.input_type(el), bb_dom::InputType::Submit | bb_dom::InputType::Image)
}

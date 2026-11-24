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
    with_doc_mut(vm, &h, n.doc, |d| d.set_control_value(n.id, &v))?;
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
    for name in ["select", "checkValidity", "reportValidity"] {
        op(vm, proto, name, 0, |vm, c| {
            el(vm, &c, C::HTMLInputElement)?;
            Ok(Value::Bool(true))
        });
    }
}

pub fn setup_button(vm: &mut Vm, proto: ObjRef) {
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
        with_doc_mut(vm, &h, n.doc, |d| d.reset_form(n.id))?;
        Ok(Value::Undefined)
    });
    for name in ["submit", "requestSubmit"] {
        op(vm, proto, name, 0, |vm, c| {
            let (h, n) = el(vm, &c, C::HTMLFormElement)?;
            h.pending_submit.set(Some(n));
            Ok(Value::Undefined)
        });
    }
    for name in ["checkValidity", "reportValidity"] {
        op(vm, proto, name, 0, |vm, c| {
            el(vm, &c, C::HTMLFormElement)?;
            Ok(Value::Bool(true))
        });
    }
}

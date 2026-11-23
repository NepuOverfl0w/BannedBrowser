//! CSSOM (CSSOM §6.6 CSSStyleDeclaration, §9 getComputedStyle) and CSSOM View (§6
//! element geometry, DOMRect from Geometry Interfaces §2).
//!
//! `element.style` reads and writes the element's `style` attribute; computed values,
//! boxes and hit tests come from the embedder (which owns style and layout).

use std::rc::Rc;

use bb_dom::Document;
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::collections::{collection, CollKind};
use crate::exception::throw;
use crate::host::*;
use crate::idl::{attr, attr_with, callee_slots, op};

type R = JsResult<Value>;

/// The CSS properties CSSStyleDeclaration exposes as attributes (CSSOM §6.7: camel-cased,
/// dashed and, for `-webkit-` ones, webkit-cased).
const PROPERTIES: &[&str] = &[
    "align-content",
    "align-items",
    "align-self",
    "all",
    "animation",
    "animation-delay",
    "animation-direction",
    "animation-duration",
    "animation-fill-mode",
    "animation-iteration-count",
    "animation-name",
    "animation-play-state",
    "animation-timing-function",
    "appearance",
    "aspect-ratio",
    "backface-visibility",
    "background",
    "background-attachment",
    "background-blend-mode",
    "background-clip",
    "background-color",
    "background-image",
    "background-origin",
    "background-position",
    "background-position-x",
    "background-position-y",
    "background-repeat",
    "background-size",
    "block-size",
    "border",
    "border-block",
    "border-bottom",
    "border-bottom-color",
    "border-bottom-left-radius",
    "border-bottom-right-radius",
    "border-bottom-style",
    "border-bottom-width",
    "border-collapse",
    "border-color",
    "border-image",
    "border-inline",
    "border-left",
    "border-left-color",
    "border-left-style",
    "border-left-width",
    "border-radius",
    "border-right",
    "border-right-color",
    "border-right-style",
    "border-right-width",
    "border-spacing",
    "border-style",
    "border-top",
    "border-top-color",
    "border-top-left-radius",
    "border-top-right-radius",
    "border-top-style",
    "border-top-width",
    "border-width",
    "bottom",
    "box-shadow",
    "box-sizing",
    "break-after",
    "break-before",
    "break-inside",
    "caption-side",
    "caret-color",
    "clear",
    "clip",
    "clip-path",
    "color",
    "color-scheme",
    "column-count",
    "column-gap",
    "column-width",
    "columns",
    "contain",
    "content",
    "counter-increment",
    "counter-reset",
    "cursor",
    "direction",
    "display",
    "empty-cells",
    "fill",
    "filter",
    "flex",
    "flex-basis",
    "flex-direction",
    "flex-flow",
    "flex-grow",
    "flex-shrink",
    "flex-wrap",
    "float",
    "font",
    "font-family",
    "font-feature-settings",
    "font-kerning",
    "font-size",
    "font-stretch",
    "font-style",
    "font-variant",
    "font-weight",
    "gap",
    "grid",
    "grid-area",
    "grid-auto-columns",
    "grid-auto-flow",
    "grid-auto-rows",
    "grid-column",
    "grid-column-end",
    "grid-column-start",
    "grid-row",
    "grid-row-end",
    "grid-row-start",
    "grid-template",
    "grid-template-areas",
    "grid-template-columns",
    "grid-template-rows",
    "height",
    "hyphens",
    "image-rendering",
    "inline-size",
    "inset",
    "isolation",
    "justify-content",
    "justify-items",
    "justify-self",
    "left",
    "letter-spacing",
    "line-break",
    "line-height",
    "list-style",
    "list-style-image",
    "list-style-position",
    "list-style-type",
    "margin",
    "margin-block",
    "margin-bottom",
    "margin-inline",
    "margin-left",
    "margin-right",
    "margin-top",
    "mask",
    "max-block-size",
    "max-height",
    "max-inline-size",
    "max-width",
    "min-block-size",
    "min-height",
    "min-inline-size",
    "min-width",
    "mix-blend-mode",
    "object-fit",
    "object-position",
    "opacity",
    "order",
    "outline",
    "outline-color",
    "outline-offset",
    "outline-style",
    "outline-width",
    "overflow",
    "overflow-wrap",
    "overflow-x",
    "overflow-y",
    "overscroll-behavior",
    "padding",
    "padding-block",
    "padding-bottom",
    "padding-inline",
    "padding-left",
    "padding-right",
    "padding-top",
    "perspective",
    "place-content",
    "place-items",
    "place-self",
    "pointer-events",
    "position",
    "quotes",
    "resize",
    "right",
    "rotate",
    "row-gap",
    "scale",
    "scroll-behavior",
    "stroke",
    "stroke-width",
    "tab-size",
    "table-layout",
    "text-align",
    "text-decoration",
    "text-decoration-color",
    "text-decoration-line",
    "text-decoration-style",
    "text-indent",
    "text-overflow",
    "text-shadow",
    "text-transform",
    "text-wrap",
    "top",
    "touch-action",
    "transform",
    "transform-origin",
    "transition",
    "transition-delay",
    "transition-duration",
    "transition-property",
    "transition-timing-function",
    "translate",
    "unicode-bidi",
    "user-select",
    "vertical-align",
    "visibility",
    "white-space",
    "width",
    "will-change",
    "word-break",
    "word-spacing",
    "word-wrap",
    "writing-mode",
    "z-index",
    "-webkit-appearance",
    "-webkit-transform",
    "-webkit-transition",
    "-webkit-user-select",
    "-webkit-box-shadow",
    "-webkit-text-size-adjust",
];

/// CSSOM §6.7 "IDL attribute to CSS property": `backgroundColor` for `background-color`.
fn camel(p: &str) -> String {
    let p = p.strip_prefix('-').unwrap_or(p);
    let mut out = String::new();
    let mut up = false;
    for c in p.chars() {
        if c == '-' {
            up = true;
        } else if up {
            out.push(c.to_ascii_uppercase());
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// The declarations an inline or computed style object has: (name, value, important).
fn decls(h: &Host, kind: &CollKind) -> Vec<(String, String, bool)> {
    match kind {
        CollKind::InlineStyle(n) => {
            let text = {
                let docs = h.docs.borrow();
                docs.get(n.doc as usize).and_then(|d| d.attr(n.id, "style")).unwrap_or("").to_string()
            };
            let mut out: Vec<(String, String, bool)> = Vec::new();
            for d in bb_css::parser::parse_declaration_list(&text) {
                let v = bb_css::parser::serialize_cvs(&d.value);
                out.retain(|x| x.0 != d.name);
                out.push((d.name, v, d.important));
            }
            out
        }
        CollKind::Computed(_, v) => v.iter().map(|(k, v)| (k.clone(), v.clone(), false)).collect(),
        _ => Vec::new(),
    }
}

/// Property names, for length / item / the indexed properties.
pub fn decl_names(h: &Host, kind: &CollKind) -> Vec<String> {
    decls(h, kind).into_iter().map(|d| d.0).collect()
}

fn serialize(ds: &[(String, String, bool)]) -> String {
    ds.iter()
        .map(|(n, v, imp)| if *imp { format!("{n}: {v} !important;") } else { format!("{n}: {v};") })
        .collect::<Vec<_>>()
        .join(" ")
}

fn this_decl(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, CollKind)> {
    let (_, id) = this_of(vm, &c.this, C::CSSStyleDeclaration)?;
    let h = host(vm)?;
    let k = h.collections.borrow().get(id as usize).cloned();
    match k {
        Some(k) => Ok((h, k)),
        None => vm.type_error("Illegal invocation"),
    }
}

/// Replace an inline style's declarations (the readonly computed ones refuse).
fn write(vm: &mut Vm, h: &Host, kind: &CollKind, ds: &[(String, String, bool)]) -> JsResult<()> {
    let CollKind::InlineStyle(n) = kind else {
        return throw(vm, "NoModificationAllowedError", "computed styles are read-only");
    };
    let text = serialize(ds);
    with_doc_mut(vm, h, n.doc, |d| d.set_attr(n.id, "style", &text))
}

fn property_name(p: &str) -> String {
    if p.starts_with("--") {
        p.to_string()
    } else {
        p.to_ascii_lowercase()
    }
}

/// CSSOM §6.6 setProperty.
fn set_property(vm: &mut Vm, h: &Host, kind: &CollKind, p: &str, v: &str, priority: &str) -> JsResult<()> {
    if matches!(kind, CollKind::Computed(..)) {
        return throw(vm, "NoModificationAllowedError", "computed styles are read-only");
    }
    let p = property_name(p);
    let mut ds = decls(h, kind);
    if v.is_empty() {
        ds.retain(|d| d.0 != p);
        return write(vm, h, kind, &ds);
    }
    let important = match priority.to_ascii_lowercase().as_str() {
        "" => false,
        "important" => true,
        _ => return Ok(()),
    };
    // The value must parse as a single declaration of this property (otherwise ignored).
    let parsed = bb_css::parser::parse_declaration_list(&format!("{p}: {v}"));
    let [d] = parsed.as_slice() else { return Ok(()) };
    if d.name != p || d.important {
        return Ok(());
    }
    let value = bb_css::parser::serialize_cvs(&d.value);
    match ds.iter_mut().find(|x| x.0 == p) {
        Some(x) => {
            x.1 = value;
            x.2 = important;
        }
        None => ds.push((p, value, important)),
    }
    write(vm, h, kind, &ds)
}

fn prop_get(vm: &mut Vm, c: NativeCall) -> R {
    let p = match callee_slots(vm, c.callee).first() {
        Some(v) => rstr(vm, v)?,
        None => return Ok(js("")),
    };
    let (h, k) = this_decl(vm, &c)?;
    let v = decls(&h, &k).into_iter().find(|d| d.0 == p).map(|d| d.1).unwrap_or_default();
    Ok(js(&v))
}

fn prop_set(vm: &mut Vm, c: NativeCall) -> R {
    let p = match callee_slots(vm, c.callee).first() {
        Some(v) => rstr(vm, v)?,
        None => return Ok(Value::Undefined),
    };
    let v = rstr_null_empty(vm, &c.arg(0))?;
    let (h, k) = this_decl(vm, &c)?;
    set_property(vm, &h, &k, &p, &v, "")?;
    Ok(Value::Undefined)
}

pub fn setup_style_declaration(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "cssText",
        |vm, c| {
            let (h, k) = this_decl(vm, &c)?;
            if matches!(k, CollKind::Computed(..)) {
                return Ok(js(""));
            }
            Ok(js(&serialize(&decls(&h, &k))))
        },
        Some(|vm, c| {
            let v = rstr_null_empty(vm, &c.arg(0))?;
            let (h, k) = this_decl(vm, &c)?;
            let mut ds: Vec<(String, String, bool)> = Vec::new();
            for d in bb_css::parser::parse_declaration_list(&v) {
                let val = bb_css::parser::serialize_cvs(&d.value);
                ds.retain(|x| x.0 != d.name);
                ds.push((d.name, val, d.important));
            }
            write(vm, &h, &k, &ds)?;
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "length",
        |vm, c| {
            let (h, k) = this_decl(vm, &c)?;
            Ok(Value::Number(decls(&h, &k).len() as f64))
        },
        None,
    );
    attr(vm, proto, "parentRule", |_, _| Ok(Value::Null), None);
    op(vm, proto, "item", 1, |vm, c| {
        let i = to_ulong(vm, &c.arg(0))?;
        let (h, k) = this_decl(vm, &c)?;
        Ok(js(&decl_names(&h, &k).get(i as usize).cloned().unwrap_or_default()))
    });
    op(vm, proto, "getPropertyValue", 1, |vm, c| {
        let p = property_name(&rstr(vm, &c.arg(0))?);
        let (h, k) = this_decl(vm, &c)?;
        Ok(js(&decls(&h, &k).into_iter().find(|d| d.0 == p).map(|d| d.1).unwrap_or_default()))
    });
    op(vm, proto, "getPropertyPriority", 1, |vm, c| {
        let p = property_name(&rstr(vm, &c.arg(0))?);
        let (h, k) = this_decl(vm, &c)?;
        let imp = decls(&h, &k).into_iter().any(|d| d.0 == p && d.2);
        Ok(js(if imp { "important" } else { "" }))
    });
    op(vm, proto, "setProperty", 2, |vm, c| {
        let p = rstr(vm, &c.arg(0))?;
        let v = rstr_null_empty(vm, &c.arg(1))?;
        let prio = match c.arg(2) {
            Value::Undefined => String::new(),
            x => rstr_null_empty(vm, &x)?,
        };
        let (h, k) = this_decl(vm, &c)?;
        set_property(vm, &h, &k, &p, &v, &prio)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "removeProperty", 1, |vm, c| {
        let p = property_name(&rstr(vm, &c.arg(0))?);
        let (h, k) = this_decl(vm, &c)?;
        let mut ds = decls(&h, &k);
        let old = ds.iter().find(|d| d.0 == p).map(|d| d.1.clone()).unwrap_or_default();
        if matches!(k, CollKind::Computed(..)) {
            return throw(vm, "NoModificationAllowedError", "computed styles are read-only");
        }
        if ds.iter().any(|d| d.0 == p) {
            ds.retain(|d| d.0 != p);
            write(vm, &h, &k, &ds)?;
        }
        Ok(js(&old))
    });
    for &p in PROPERTIES {
        let data = vec![js(p)];
        attr_with(vm, proto, &camel(p), prop_get, Some(prop_set), data.clone());
        attr_with(vm, proto, p, prop_get, Some(prop_set), data.clone());
        if let Some(rest) = p.strip_prefix("-webkit-") {
            // `-webkit-transform` is also `webkitTransform` (CSSOM §6.7 "webkit-cased attribute").
            let c = camel(rest);
            let mut cs = c.chars();
            let cap: String = cs.next().map(|f| f.to_ascii_uppercase()).into_iter().chain(cs).collect();
            attr_with(vm, proto, &format!("webkit{cap}"), prop_get, Some(prop_set), data);
        }
    }
    attr_with(vm, proto, "cssFloat", prop_get, Some(prop_set), vec![js("float")]);
}

/// ElementCSSInlineStyle (CSSOM §6.7.1) on HTML, SVG and MathML elements.
pub fn setup_inline_style(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "style",
        |vm, c| {
            let (h, n) = this_node(vm, &c.this, C::Element)?;
            collection(vm, &h, CollKind::InlineStyle(n), Some((7, n.pack())))
        },
        Some(|vm, c| {
            // [PutForwards=cssText]
            let v = rstr_null_empty(vm, &c.arg(0))?;
            let (h, n) = this_node(vm, &c.this, C::Element)?;
            with_doc_mut(vm, &h, n.doc, |d| d.set_attr(n.id, "style", &v))?;
            Ok(Value::Undefined)
        }),
    );
}

/// window.getComputedStyle (CSSOM §9).
pub fn get_computed_style(vm: &mut Vm, c: NativeCall) -> R {
    let n = match node_of(vm, &c.arg(0)) {
        Some(n) => n,
        None => return vm.type_error("getComputedStyle: parameter 1 is not an Element"),
    };
    let h = host(vm)?;
    let is_el = with_doc(vm, &h, n.doc, |d| d.node(n.id).is_element())?;
    if !is_el {
        return vm.type_error("getComputedStyle: parameter 1 is not an Element");
    }
    let values = if n.doc == 0 {
        let docs = h.docs.borrow();
        h.embedder.computed_style(&docs[0], n.id)
    } else {
        Vec::new()
    };
    collection(vm, &h, CollKind::Computed(n, values), None)
}

// ---- geometry ----

fn new_rect(vm: &mut Vm, h: &Host, r: [f64; 4]) -> Value {
    let o = vm.new_host_object(Some(h.proto(C::DOMRect)), C::DOMRect as u32, 0, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = r.iter().map(|&x| Value::Number(x)).collect();
    }
    Value::Object(o)
}

pub fn rect_ctor(vm: &mut Vm, c: NativeCall) -> R {
    if c.new_target.is_none() {
        return vm.type_error("DOMRect requires 'new'");
    }
    let mut r = [0.0; 4];
    for (i, v) in r.iter_mut().enumerate() {
        let a = c.arg(i);
        *v = if a.is_undefined() { 0.0 } else { vm.to_number(&a)? };
    }
    let h = host(vm)?;
    let class = if c.callee == h.ctor(C::DOMRectReadOnly) { C::DOMRectReadOnly } else { C::DOMRect };
    let o = vm.new_host_object(Some(h.proto(class)), class as u32, 0, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = r.iter().map(|&x| Value::Number(x)).collect();
    }
    Ok(Value::Object(o))
}

fn rect_of(vm: &mut Vm, c: &NativeCall) -> JsResult<(ObjRef, [f64; 4])> {
    this_of(vm, &c.this, C::DOMRectReadOnly)?;
    let Value::Object(o) = c.this else { return vm.type_error("Illegal invocation") };
    let mut r = [0.0; 4];
    if let Some(ho) = vm.host_obj(o) {
        for (i, v) in r.iter_mut().enumerate() {
            if let Some(Value::Number(n)) = ho.slots.get(i) {
                *v = *n;
            }
        }
    }
    Ok((o, r))
}

fn rect_field(vm: &mut Vm, c: NativeCall) -> R {
    let (_, r) = rect_of(vm, &c)?;
    let which = match callee_slots(vm, c.callee).first() {
        Some(Value::Number(n)) => *n as usize,
        _ => 0,
    };
    let [x, y, w, h] = r;
    Ok(Value::Number(match which {
        0 => x,
        1 => y,
        2 => w,
        3 => h,
        4 => y.min(y + h),
        5 => x.max(x + w),
        6 => y.max(y + h),
        _ => x.min(x + w),
    }))
}

pub fn setup_rect_readonly(vm: &mut Vm, proto: ObjRef) {
    for (i, n) in ["x", "y", "width", "height", "top", "right", "bottom", "left"].iter().enumerate() {
        attr_with(vm, proto, n, rect_field, None, vec![Value::Number(i as f64)]);
    }
    op(vm, proto, "toJSON", 0, |vm, c| {
        let (_, r) = rect_of(vm, &c)?;
        let o = vm.new_object();
        let [x, y, w, h] = r;
        for (k, v) in [
            ("x", x),
            ("y", y),
            ("width", w),
            ("height", h),
            ("top", y),
            ("right", x + w),
            ("bottom", y + h),
            ("left", x),
        ] {
            bb_js::vm::realm::value(vm, o, k, Value::Number(v));
        }
        Ok(Value::Object(o))
    });
}

pub fn setup_rect(vm: &mut Vm, proto: ObjRef) {
    for (i, n) in ["x", "y", "width", "height"].iter().enumerate() {
        attr_with(
            vm,
            proto,
            n,
            rect_field,
            Some(|vm, c| {
                let v = vm.to_number(&c.arg(0))?;
                let (o, _) = rect_of(vm, &c)?;
                let which = match callee_slots(vm, c.callee).first() {
                    Some(Value::Number(n)) => *n as usize,
                    _ => 0,
                };
                if let Some(ho) = vm.host_obj_mut(o) {
                    if let Some(s) = ho.slots.get_mut(which) {
                        *s = Value::Number(v);
                    }
                }
                Ok(Value::Undefined)
            }),
            vec![Value::Number(i as f64)],
        );
    }
}

pub fn setup_rect_list(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "item", 1, |vm, c| {
        let i = to_ulong(vm, &c.arg(0))?;
        let Value::Object(o) = c.this else { return Ok(Value::Null) };
        let v = vm.get(o, &bb_js::vm::PropKey::Index(i))?;
        Ok(if v.is_undefined() { Value::Null } else { v })
    });
}

/// The box facts for an element of the window's document.
fn box_info(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<Option<BoxInfo>> {
    if n.doc != 0 {
        return Ok(None);
    }
    let docs = match h.docs.try_borrow() {
        Ok(d) => d,
        Err(_) => return throw(vm, "InvalidStateError", "the document is busy"),
    };
    Ok(h.embedder.box_info(&docs[0], n.id))
}

fn el(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, NodeRef)> {
    this_node(vm, &c.this, C::Element)
}

/// Element geometry (CSSOM View §6.1) and HTMLElement's offset* (§7.1).
pub fn setup_element_geometry(vm: &mut Vm, el_proto: ObjRef, html_proto: ObjRef) {
    op(vm, el_proto, "getBoundingClientRect", 0, |vm, c| {
        let (h, n) = el(vm, &c)?;
        let b = box_info(vm, &h, n)?.map_or([0.0; 4], |b| b.border);
        Ok(new_rect(vm, &h, b))
    });
    op(vm, el_proto, "getClientRects", 0, |vm, c| {
        let (h, n) = el(vm, &c)?;
        let rects = match box_info(vm, &h, n)? {
            Some(b) => vec![new_rect(vm, &h, b.border)],
            None => Vec::new(),
        };
        let a = vm.new_array(rects);
        vm.heap.obj_mut(a).proto = Some(h.proto(C::DOMRectList));
        Ok(Value::Object(a))
    });
    for (name, i) in [("clientLeft", 0usize), ("clientTop", 1), ("clientWidth", 2), ("clientHeight", 3)] {
        attr_with(
            vm,
            el_proto,
            name,
            |vm, c| {
                let i = match callee_slots(vm, c.callee).first() {
                    Some(Value::Number(n)) => *n as usize,
                    _ => 0,
                };
                let (h, n) = el(vm, &c)?;
                let v = box_info(vm, &h, n)?.map_or(0.0, |b| b.client[i]);
                Ok(Value::Number(v.round()))
            },
            None,
            vec![Value::Number(i as f64)],
        );
    }
    for (name, i) in [("scrollWidth", 0usize), ("scrollHeight", 1)] {
        attr_with(
            vm,
            el_proto,
            name,
            |vm, c| {
                let i = match callee_slots(vm, c.callee).first() {
                    Some(Value::Number(n)) => *n as usize,
                    _ => 0,
                };
                let (h, n) = el(vm, &c)?;
                let v = box_info(vm, &h, n)?.map_or(0.0, |b| if i == 0 { b.scroll.0 } else { b.scroll.1 });
                Ok(Value::Number(v.round()))
            },
            None,
            vec![Value::Number(i as f64)],
        );
    }
    for name in ["scrollTop", "scrollLeft"] {
        attr(vm, el_proto, name, |_, _| Ok(Value::Number(0.0)), Some(|_, _| Ok(Value::Undefined)));
    }
    for name in ["scrollIntoView", "scroll", "scrollTo", "scrollBy", "focus", "blur"] {
        op(vm, el_proto, name, 0, |vm, c| {
            el(vm, &c)?;
            Ok(Value::Undefined)
        });
    }
    // offsetParent is approximated as the body (no positioned-ancestor search).
    attr(
        vm,
        html_proto,
        "offsetParent",
        |vm, c| {
            let (h, n) = el(vm, &c)?;
            let (body, is_root) = with_doc(vm, &h, n.doc, |d| {
                (d.body(), Some(n.id) == d.body() || Some(n.id) == d.document_element())
            })?;
            if is_root || box_info(vm, &h, n)?.is_none() {
                return Ok(Value::Null);
            }
            wrap_opt(vm, &h, body.map(|b| n.with(b)))
        },
        None,
    );
    for (name, i) in [("offsetLeft", 0usize), ("offsetTop", 1), ("offsetWidth", 2), ("offsetHeight", 3)] {
        attr_with(
            vm,
            html_proto,
            name,
            |vm, c| {
                let i = match callee_slots(vm, c.callee).first() {
                    Some(Value::Number(n)) => *n as usize,
                    _ => 0,
                };
                let (h, n) = el(vm, &c)?;
                let v = box_info(vm, &h, n)?.map_or(0.0, |b| b.border[i]);
                Ok(Value::Number(v.round()))
            },
            None,
            vec![Value::Number(i as f64)],
        );
    }
}

/// document.elementFromPoint / elementsFromPoint (CSSOM View §5).
pub fn setup_document_geometry(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "elementFromPoint", 2, |vm, c| {
        let x = vm.to_number(&c.arg(0))?;
        let y = vm.to_number(&c.arg(1))?;
        let (h, n) = this_node(vm, &c.this, C::Document)?;
        if n.doc != 0 {
            return Ok(Value::Null);
        }
        let hit = {
            let docs = h.docs.borrow();
            h.embedder.element_at(&docs[0], x, y)
        };
        wrap_opt(vm, &h, hit.map(|e| n.with(e)))
    });
    op(vm, proto, "elementsFromPoint", 2, |vm, c| {
        let x = vm.to_number(&c.arg(0))?;
        let y = vm.to_number(&c.arg(1))?;
        let (h, n) = this_node(vm, &c.this, C::Document)?;
        let chain: Vec<bb_dom::NodeId> = if n.doc == 0 {
            let docs = h.docs.borrow();
            let d: &Document = &docs[0];
            match h.embedder.element_at(d, x, y) {
                Some(e) => {
                    std::iter::once(e).chain(d.ancestors(e)).filter(|&a| d.node(a).is_element()).collect()
                }
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };
        let mut out = Vec::new();
        for e in chain {
            out.push(wrap(vm, &h, n.with(e))?);
        }
        Ok(Value::Object(vm.new_array(out)))
    });
}

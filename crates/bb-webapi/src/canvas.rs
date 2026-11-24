//! The canvas element's 2D context (HTML §4.12.5): HTMLCanvasElement's getContext, toDataURL
//! and toBlob; CanvasRenderingContext2D over bb-paint's `Context2D`; CanvasGradient,
//! CanvasPattern, ImageData, TextMetrics and Path2D.
//!
//! A canvas's context lives in `Host::canvases`, by the element (packed node). Gradients,
//! patterns and Path2D objects keep their Rust data in host tables by the objects' payloads.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::typed::TaKind;
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};
use bb_paint::canvas::Color;
use bb_paint::canvas2d::{
    Bitmap, CanvasPath, Composite, Context2D, Gradient, GradientKind, LineCap, LineJoin, Matrix, PathBuilder,
    Pattern, Style,
};

use crate::class::C;
use crate::exception::throw;
use crate::host::{dict_get, host, js, rstr, this_node, this_of, with_doc, Host, NodeRef};
use crate::idl::{attr, op};

type R = JsResult<Value>;

/// The canvases' contexts and the 2D API objects' data.
#[derive(Default)]
pub struct CanvasState {
    pub contexts: HashMap<u64, Context2D>,
    /// The context object of each canvas ([SameObject] from getContext).
    pub objects: HashMap<u64, ObjRef>,
    pub gradients: Vec<Gradient>,
    pub patterns: Vec<Pattern>,
    pub paths: Vec<CanvasPath>,
    /// ImageBitmaps' pixels (None once closed).
    pub bitmaps: Vec<Option<Bitmap>>,
    /// Bumped by each drawing operation (the embedder repaints).
    pub generation: u64,
}

impl CanvasState {
    pub fn trace(&self, out: &mut Vec<ObjRef>) {
        out.extend(self.objects.values());
    }
}

// Context slots: canvas element, fill style object, stroke style object.
const X_CANVAS: usize = 0;
const X_FILL: usize = 1;
const X_STROKE: usize = 2;

fn slot(vm: &Vm, o: ObjRef, i: usize) -> Value {
    vm.host_obj(o).and_then(|h| h.slots.get(i).cloned()).unwrap_or(Value::Undefined)
}

fn set_slot(vm: &mut Vm, o: ObjRef, i: usize, v: Value) {
    if let Some(h) = vm.host_obj_mut(o) {
        if h.slots.len() <= i {
            h.slots.resize(i + 1, Value::Null);
        }
        h.slots[i] = v;
    }
}

/// The canvas's size from its width and height attributes (HTML §4.12.5: 300 × 150 by
/// default; non-negative integers).
fn canvas_size(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<(u32, u32)> {
    with_doc(vm, h, n.doc, |d| {
        let get = |name: &str, def: u32| {
            // HTML §2.3.4.2 rules for parsing non-negative integers.
            d.attr(n.id, name)
                .and_then(crate::html::parse_int)
                .filter(|v| *v >= 0)
                .map_or(def, |v| v.min(1 << 15) as u32)
        };
        (get("width", 300), get("height", 150))
    })
}

/// A canvas's width or height attribute changed: a new bitmap and a reset context.
pub fn size_changed(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<()> {
    let has = h.canvas.borrow().contexts.contains_key(&n.pack());
    if !has {
        return Ok(());
    }
    let (w, ht) = canvas_size(vm, h, n)?;
    let mut st = h.canvas.borrow_mut();
    if let Some(c) = st.contexts.get_mut(&n.pack()) {
        c.reset(w, ht);
    }
    st.generation += 1;
    drop(st);
    if let Some(o) = h.canvas.borrow().objects.get(&n.pack()).copied() {
        set_slot(vm, o, X_FILL, Value::Null);
        set_slot(vm, o, X_STROKE, Value::Null);
    }
    h.mark_dirty();
    Ok(())
}

/// A canvas's bitmap (a copy), for the embedder to paint: None without a 2D context.
pub fn bitmap_of(h: &Host, n: NodeRef) -> Option<Bitmap> {
    h.canvas.borrow().contexts.get(&n.pack()).map(|c| c.bitmap.clone())
}

pub fn setup_canvas_element(vm: &mut Vm, proto: ObjRef) {
    // HTML §4.12.5 getContext(contextId, options).
    op(vm, proto, "getContext", 1, |vm, c| {
        let id = rstr(vm, &c.arg(0))?;
        let (h, n) = this_node(vm, &c.this, C::HTMLCanvasElement)?;
        if id != "2d" {
            return Ok(Value::Null);
        }
        let key = n.pack();
        if let Some(o) = h.canvas.borrow().objects.get(&key).copied() {
            return Ok(Value::Object(o));
        }
        let alpha = match dict_get(vm, &c.arg(1), "alpha")? {
            Value::Undefined => true,
            v => vm.to_boolean(&v),
        };
        let (w, ht) = canvas_size(vm, &h, n)?;
        let mut ctx = Context2D::new(w, ht);
        ctx.opaque = !alpha;
        ctx.reset(w, ht);
        let o = vm.new_host_object(
            Some(h.proto(C::CanvasRenderingContext2D)),
            C::CanvasRenderingContext2D as u32,
            key,
            false,
        );
        set_slot(vm, o, X_CANVAS, c.this.clone());
        set_slot(vm, o, X_FILL, Value::Null);
        set_slot(vm, o, X_STROKE, Value::Null);
        let mut st = h.canvas.borrow_mut();
        st.contexts.insert(key, ctx);
        st.objects.insert(key, o);
        Ok(Value::Object(o))
    });
    // HTML §4.12.5 toDataURL(type, quality): PNG (other types fall back to it).
    op(vm, proto, "toDataURL", 0, |vm, c| {
        let (h, n) = this_node(vm, &c.this, C::HTMLCanvasElement)?;
        let png = png_of(vm, &h, n)?;
        match png {
            None => Ok(js("data:,")),
            Some(b) => Ok(js(&format!("data:image/png;base64,{}", crate::window::base64_encode(&b)))),
        }
    });
    // HTML §4.12.5 toBlob(callback, type, quality): the callback gets a PNG Blob in a task.
    op(vm, proto, "toBlob", 1, |vm, c| {
        let cb = c.arg(0);
        if !vm.is_callable(&cb) {
            return vm.type_error("toBlob needs a callback");
        }
        let (h, n) = this_node(vm, &c.this, C::HTMLCanvasElement)?;
        let blob = match png_of(vm, &h, n)? {
            Some(b) => Value::Object(crate::blob::new_blob(vm, &h, b, "image/png", None, None)),
            None => Value::Null,
        };
        h.tasks.borrow_mut().push_back(crate::host::Task::Call(cb, Value::Undefined, vec![blob]));
        Ok(Value::Undefined)
    });
}

/// The canvas's bitmap as PNG (None for a zero-sized canvas).
fn png_of(vm: &mut Vm, h: &Host, n: NodeRef) -> JsResult<Option<Vec<u8>>> {
    let (w, ht) = canvas_size(vm, h, n)?;
    if w == 0 || ht == 0 {
        return Ok(None);
    }
    let bmp = bitmap_of(h, n).unwrap_or_else(|| Bitmap::new(w, ht));
    Ok(Some(bmp.encode_png()))
}

// ---- the context ----

fn ctx_key(vm: &mut Vm, c: &NativeCall) -> JsResult<(Rc<Host>, u64, ObjRef)> {
    let (_, id) = this_of(vm, &c.this, C::CanvasRenderingContext2D)?;
    let Value::Object(o) = c.this else { return vm.type_error("Illegal invocation") };
    Ok((host(vm)?, id, o))
}

/// Run `f` on the context of `this`; drawing bumps the generation.
fn with_ctx<T>(vm: &mut Vm, c: &NativeCall, draw: bool, f: impl FnOnce(&mut Context2D) -> T) -> JsResult<T> {
    let (h, key, _) = ctx_key(vm, c)?;
    let mut st = h.canvas.borrow_mut();
    if draw {
        st.generation += 1;
        h.dirty.set(true);
    }
    match st.contexts.get_mut(&key) {
        Some(ctx) => Ok(f(ctx)),
        None => vm.type_error("the context's canvas is gone"),
    }
}

/// `unrestricted double` arguments as f32: None if any is not finite (the many methods
/// that do nothing then).
fn finite_args(vm: &mut Vm, c: &NativeCall, n: usize) -> JsResult<Option<Vec<f32>>> {
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        v.push(vm.to_number(&c.arg(i))?);
    }
    if v.iter().all(|x| x.is_finite()) {
        Ok(Some(v.into_iter().map(|x| x as f32).collect()))
    } else {
        Ok(None)
    }
}

/// `double` arguments (restricted: TypeError when not finite).
fn doubles(vm: &mut Vm, c: &NativeCall, n: usize) -> JsResult<Vec<f32>> {
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let x = vm.to_number(&c.arg(i))?;
        if !x.is_finite() {
            return vm.type_error("the argument is not a finite number");
        }
        v.push(x as f32);
    }
    Ok(v)
}

/// CSS color text for a canvas color (HTML "serialization of a color" for canvas).
fn serialize_color(c: Color) -> String {
    if c.a == 255 {
        format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
    } else {
        let a = c.a as f64 / 255.0;
        // The shortest decimal that round-trips the 8-bit alpha.
        let mut s = String::new();
        for digits in 1..=6 {
            s = format!("{:.*}", digits, a);
            if ((s.parse::<f64>().unwrap_or(0.0) * 255.0).round() as u8) == c.a {
                break;
            }
        }
        let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
        let s = if s.is_empty() { "0".to_string() } else { s };
        format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, s)
    }
}

/// Parse a CSS color for the canvas (currentColor is black: the canvas's own color isn't
/// computed here).
pub fn parse_color(s: &str) -> Option<Color> {
    let cvs = bb_css::parser::parse_component_values(s);
    let v = bb_css::values::parse_color(&cvs)?;
    let c = v.resolve(bb_css::values::Rgba { r: 0, g: 0, b: 0, a: 255 }, false);
    Some(Color::rgba(c.r, c.g, c.b, c.a))
}

fn style_get(vm: &mut Vm, c: &NativeCall, stroke: bool) -> R {
    let (_, _, o) = ctx_key(vm, c)?;
    let obj = slot(vm, o, if stroke { X_STROKE } else { X_FILL });
    if matches!(obj, Value::Object(_)) {
        return Ok(obj);
    }
    let s = with_ctx(vm, c, false, |ctx| match if stroke { &ctx.state.stroke } else { &ctx.state.fill } {
        Style::Color(col) => serialize_color(*col),
        _ => String::new(),
    })?;
    Ok(js(&s))
}

fn style_set(vm: &mut Vm, c: &NativeCall, stroke: bool) -> R {
    let v = c.arg(0);
    let (h, _, o) = ctx_key(vm, c)?;
    let i = if stroke { X_STROKE } else { X_FILL };
    if let Some((cls, id)) = vm.host_data(&v) {
        if cls == C::CanvasGradient as u32 {
            let g = h.canvas.borrow().gradients.get(id as usize).cloned();
            if let Some(g) = g {
                with_ctx(vm, c, false, |ctx| {
                    let s = Style::Gradient(Arc::new(g));
                    if stroke {
                        ctx.state.stroke = s;
                    } else {
                        ctx.state.fill = s;
                    }
                })?;
                set_slot(vm, o, i, v);
            }
            return Ok(Value::Undefined);
        }
        if cls == C::CanvasPattern as u32 {
            let p = h.canvas.borrow().patterns.get(id as usize).cloned();
            if let Some(p) = p {
                with_ctx(vm, c, false, |ctx| {
                    let s = Style::Pattern(Arc::new(p));
                    if stroke {
                        ctx.state.stroke = s;
                    } else {
                        ctx.state.fill = s;
                    }
                })?;
                set_slot(vm, o, i, v);
            }
            return Ok(Value::Undefined);
        }
    }
    let s = rstr(vm, &v)?;
    let col = if s.trim().eq_ignore_ascii_case("currentcolor") {
        // HTML: currentColor is the canvas element's computed color (when it is set).
        let canvas = crate::host::node_of(vm, &slot(vm, o, X_CANVAS));
        Some(canvas.and_then(|n| computed_color(vm, &h, n)).unwrap_or(Color::rgba(0, 0, 0, 255)))
    } else {
        parse_color(&s)
    };
    if let Some(col) = col {
        with_ctx(vm, c, false, |ctx| {
            if stroke {
                ctx.state.stroke = Style::Color(col);
            } else {
                ctx.state.fill = Style::Color(col);
            }
        })?;
        set_slot(vm, o, i, Value::Null);
    }
    Ok(Value::Undefined)
}

/// Element `n`'s computed `color`, from the embedder's style.
fn computed_color(vm: &mut Vm, h: &Host, n: NodeRef) -> Option<Color> {
    let values = {
        let docs = h.docs.try_borrow().ok()?;
        let d = docs.get(n.doc as usize)?;
        h.embedder.computed_style(d, n.id)
    };
    let _ = vm;
    let v = values.into_iter().find(|(k, _)| k == "color")?.1;
    parse_color(&v)
}

/// A path argument: a Path2D's path, or None for the current default path.
fn path_arg(vm: &mut Vm, h: &Host, v: &Value) -> Option<CanvasPath> {
    let (cls, id) = vm.host_data(v)?;
    if cls != C::Path2D as u32 {
        return None;
    }
    h.canvas.borrow().paths.get(id as usize).cloned()
}

/// A CanvasFillRule argument ("nonzero" or "evenodd"; others are a TypeError).
fn fill_rule(vm: &mut Vm, v: &Value) -> JsResult<bool> {
    match v {
        Value::Undefined => Ok(false),
        v => match rstr(vm, v)?.as_str() {
            "nonzero" => Ok(false),
            "evenodd" => Ok(true),
            other => vm.type_error(&format!("'{other}' is not a CanvasFillRule")),
        },
    }
}

/// The (path, rule) of fill/clip/isPointInPath-style overloads: ([path,] [rule]).
fn path_and_rule(
    vm: &mut Vm,
    h: &Host,
    c: &NativeCall,
    first: usize,
) -> JsResult<(Option<CanvasPath>, bool)> {
    match path_arg(vm, h, &c.arg(first)) {
        Some(p) => Ok((Some(p), fill_rule(vm, &c.arg(first + 1))?)),
        None => Ok((None, fill_rule(vm, &c.arg(first))?)),
    }
}

/// An image source's pixels (HTML CanvasImageSource): a canvas, or a loaded image.
fn image_source(vm: &mut Vm, h: &Host, v: &Value) -> JsResult<Option<Bitmap>> {
    let Some(n) = crate::host::node_of(vm, v) else {
        if let Some((cls, id)) = vm.host_data(v) {
            if cls == C::ImageBitmap as u32 {
                return match bitmap_pixels(h, id) {
                    Some(b) => Ok(Some(b)),
                    None => throw(vm, "InvalidStateError", "the ImageBitmap is closed"),
                };
            }
            if cls == C::ImageData as u32 {
                return vm.type_error("ImageData is not a CanvasImageSource");
            }
        }
        return vm.type_error("the image is not an image, canvas or video element");
    };
    let (is_canvas, is_img, src) = with_doc(vm, h, n.doc, |d| {
        (d.is_html(n.id, "canvas"), d.is_html(n.id, "img"), d.attr(n.id, "src").map(str::to_string))
    })?;
    if is_canvas {
        let (w, ht) = canvas_size(vm, h, n)?;
        if w == 0 || ht == 0 {
            return throw(vm, "InvalidStateError", "the canvas has no pixels");
        }
        return Ok(Some(bitmap_of(h, n).unwrap_or_else(|| Bitmap::new(w, ht))));
    }
    if is_img {
        let base = crate::host::base_url(h, n.doc);
        let Some(url) = src.and_then(|s| bb_url::Url::parse_with_base(&s, Some(&base)).ok()) else {
            return Ok(None);
        };
        return Ok(h.embedder.image_bitmap(&url).map(|(w, ht, data)| Bitmap { w, h: ht, data }));
    }
    vm.type_error("the image is not an image or canvas element")
}

pub fn setup_context(vm: &mut Vm, proto: ObjRef) {
    attr(
        vm,
        proto,
        "canvas",
        |vm, c| {
            let (_, _, o) = ctx_key(vm, &c)?;
            Ok(slot(vm, o, X_CANVAS))
        },
        None,
    );
    op(vm, proto, "getContextAttributes", 0, |vm, c| {
        let opaque = with_ctx(vm, &c, false, |ctx| ctx.opaque)?;
        let o = vm.new_object();
        vm.create_data_property(o, PropKey::lit("alpha"), Value::Bool(!opaque))?;
        vm.create_data_property(o, PropKey::lit("colorSpace"), js("srgb"))?;
        vm.create_data_property(o, PropKey::lit("desynchronized"), Value::Bool(false))?;
        vm.create_data_property(o, PropKey::lit("willReadFrequently"), Value::Bool(false))?;
        Ok(Value::Object(o))
    });
    op(vm, proto, "isContextLost", 0, |vm, c| {
        ctx_key(vm, &c)?;
        Ok(Value::Bool(false))
    });
    // State (HTML §4.12.5.1.2).
    op(vm, proto, "save", 0, |vm, c| with_ctx(vm, &c, false, |x| x.save()).map(|_| Value::Undefined));
    op(vm, proto, "restore", 0, |vm, c| {
        let (_, _, o) = ctx_key(vm, &c)?;
        // Style objects are part of the state; restoring a color drops the object.
        with_ctx(vm, &c, false, |x| x.restore())?;
        let (fill_obj, stroke_obj) = with_ctx(vm, &c, false, |x| {
            (!matches!(x.state.fill, Style::Color(_)), !matches!(x.state.stroke, Style::Color(_)))
        })?;
        if !fill_obj {
            set_slot(vm, o, X_FILL, Value::Null);
        }
        if !stroke_obj {
            set_slot(vm, o, X_STROKE, Value::Null);
        }
        Ok(Value::Undefined)
    });
    op(vm, proto, "reset", 0, |vm, c| {
        let (_, _, o) = ctx_key(vm, &c)?;
        with_ctx(vm, &c, true, |x| {
            let (w, h) = (x.bitmap.w, x.bitmap.h);
            x.reset(w, h);
        })?;
        set_slot(vm, o, X_FILL, Value::Null);
        set_slot(vm, o, X_STROKE, Value::Null);
        Ok(Value::Undefined)
    });
    // Transformations (HTML §4.12.5.1.8).
    op(vm, proto, "scale", 2, |vm, c| {
        let Some(a) = finite_args(vm, &c, 2)? else { return Ok(Value::Undefined) };
        with_ctx(vm, &c, false, |x| {
            x.state.transform = x.state.transform.mul(&Matrix::new(a[0], 0.0, 0.0, a[1], 0.0, 0.0))
        })?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "rotate", 1, |vm, c| {
        let Some(a) = finite_args(vm, &c, 1)? else { return Ok(Value::Undefined) };
        let (s, co) = a[0].sin_cos();
        with_ctx(vm, &c, false, |x| {
            x.state.transform = x.state.transform.mul(&Matrix::new(co, s, -s, co, 0.0, 0.0))
        })?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "translate", 2, |vm, c| {
        let Some(a) = finite_args(vm, &c, 2)? else { return Ok(Value::Undefined) };
        with_ctx(vm, &c, false, |x| {
            x.state.transform = x.state.transform.mul(&Matrix::new(1.0, 0.0, 0.0, 1.0, a[0], a[1]))
        })?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "transform", 6, |vm, c| {
        let Some(a) = finite_args(vm, &c, 6)? else { return Ok(Value::Undefined) };
        with_ctx(vm, &c, false, |x| {
            x.state.transform = x.state.transform.mul(&Matrix::new(a[0], a[1], a[2], a[3], a[4], a[5]))
        })?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "setTransform", 0, |vm, c| {
        let m = if c.args.len() >= 6 {
            let Some(a) = finite_args(vm, &c, 6)? else { return Ok(Value::Undefined) };
            Matrix::new(a[0], a[1], a[2], a[3], a[4], a[5])
        } else {
            matrix_init(vm, &c.arg(0))?
        };
        with_ctx(vm, &c, false, |x| x.state.transform = m)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "resetTransform", 0, |vm, c| {
        with_ctx(vm, &c, false, |x| x.state.transform = Matrix::IDENTITY).map(|_| Value::Undefined)
    });
    op(vm, proto, "getTransform", 0, |vm, c| {
        let m = with_ctx(vm, &c, false, |x| x.state.transform)?;
        let o = vm.new_object();
        for (k, v) in [("a", m.a), ("b", m.b), ("c", m.c), ("d", m.d), ("e", m.e), ("f", m.f)] {
            vm.create_data_property(o, PropKey::named(k), Value::Number(v as f64))?;
        }
        Ok(Value::Object(o))
    });
    // Compositing and image smoothing.
    num_attr(
        vm,
        proto,
        "globalAlpha",
        |x| x.state.alpha,
        |x, v| {
            if (0.0..=1.0).contains(&v) {
                x.state.alpha = v;
            }
        },
    );
    attr(
        vm,
        proto,
        "globalCompositeOperation",
        |vm, c| {
            let s = with_ctx(vm, &c, false, |x| x.state.composite.name())?;
            Ok(js(s))
        },
        Some(|vm, c| {
            let s = rstr(vm, &c.arg(0))?;
            if let Some(op) = Composite::parse(&s) {
                with_ctx(vm, &c, false, |x| x.state.composite = op)?;
            }
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "imageSmoothingEnabled",
        |vm, c| Ok(Value::Bool(with_ctx(vm, &c, false, |x| x.state.smoothing)?)),
        Some(|vm, c| {
            let b = vm.to_boolean(&c.arg(0));
            with_ctx(vm, &c, false, |x| x.state.smoothing = b)?;
            Ok(Value::Undefined)
        }),
    );
    // Fill and stroke styles (HTML §4.12.5.1.10).
    attr(vm, proto, "fillStyle", |vm, c| style_get(vm, &c, false), Some(|vm, c| style_set(vm, &c, false)));
    attr(vm, proto, "strokeStyle", |vm, c| style_get(vm, &c, true), Some(|vm, c| style_set(vm, &c, true)));
    op(vm, proto, "createLinearGradient", 4, |vm, c| {
        let a = doubles(vm, &c, 4)?;
        new_gradient(vm, GradientKind::Linear { x0: a[0], y0: a[1], x1: a[2], y1: a[3] })
    });
    op(vm, proto, "createRadialGradient", 6, |vm, c| {
        let a = doubles(vm, &c, 6)?;
        if a[2] < 0.0 || a[5] < 0.0 {
            return throw(vm, "IndexSizeError", "a radius is negative");
        }
        new_gradient(vm, GradientKind::Radial { x0: a[0], y0: a[1], r0: a[2], x1: a[3], y1: a[4], r1: a[5] })
    });
    op(vm, proto, "createConicGradient", 3, |vm, c| {
        let a = doubles(vm, &c, 3)?;
        new_gradient(vm, GradientKind::Conic { angle: a[0], x: a[1], y: a[2] })
    });
    op(vm, proto, "createPattern", 2, |vm, c| {
        let (h, _, _) = ctx_key(vm, &c)?;
        let img = image_source(vm, &h, &c.arg(0))?;
        let rep = match c.arg(1) {
            Value::Null => String::new(),
            v => rstr(vm, &v)?,
        };
        let (rx, ry) = match rep.as_str() {
            "" | "repeat" => (true, true),
            "repeat-x" => (true, false),
            "repeat-y" => (false, true),
            "no-repeat" => (false, false),
            _ => return throw(vm, "SyntaxError", &format!("'{rep}' is not a repetition")),
        };
        let Some(img) = img else { return Ok(Value::Null) };
        let id = {
            let mut st = h.canvas.borrow_mut();
            st.patterns.push(Pattern {
                image: Arc::new(img),
                repeat_x: rx,
                repeat_y: ry,
                transform: Matrix::IDENTITY,
            });
            st.patterns.len() as u64 - 1
        };
        let o = vm.new_host_object(Some(h.proto(C::CanvasPattern)), C::CanvasPattern as u32, id, false);
        Ok(Value::Object(o))
    });
    // Shadows (HTML §4.12.5.1.17).
    num_attr(vm, proto, "shadowOffsetX", |x| x.state.shadow_x, |x, v| x.state.shadow_x = v);
    num_attr(vm, proto, "shadowOffsetY", |x| x.state.shadow_y, |x, v| x.state.shadow_y = v);
    num_attr(
        vm,
        proto,
        "shadowBlur",
        |x| x.state.shadow_blur,
        |x, v| {
            if v >= 0.0 {
                x.state.shadow_blur = v;
            }
        },
    );
    attr(
        vm,
        proto,
        "shadowColor",
        |vm, c| {
            let col = with_ctx(vm, &c, false, |x| x.state.shadow_color)?;
            Ok(js(&serialize_color(col)))
        },
        Some(|vm, c| {
            let s = rstr(vm, &c.arg(0))?;
            if let Some(col) = parse_color(&s) {
                with_ctx(vm, &c, false, |x| x.state.shadow_color = col)?;
            }
            Ok(Value::Undefined)
        }),
    );
    // Line styles (HTML §4.12.5.1.4).
    num_attr(
        vm,
        proto,
        "lineWidth",
        |x| x.state.line_width,
        |x, v| {
            if v > 0.0 {
                x.state.line_width = v;
            }
        },
    );
    num_attr(
        vm,
        proto,
        "miterLimit",
        |x| x.state.miter_limit,
        |x, v| {
            if v > 0.0 {
                x.state.miter_limit = v;
            }
        },
    );
    num_attr(vm, proto, "lineDashOffset", |x| x.state.dash_offset, |x, v| x.state.dash_offset = v);
    attr(
        vm,
        proto,
        "lineCap",
        |vm, c| {
            let s = with_ctx(vm, &c, false, |x| match x.state.cap {
                LineCap::Butt => "butt",
                LineCap::Round => "round",
                LineCap::Square => "square",
            })?;
            Ok(js(s))
        },
        Some(|vm, c| {
            let s = rstr(vm, &c.arg(0))?;
            let cap = match s.as_str() {
                "butt" => LineCap::Butt,
                "round" => LineCap::Round,
                "square" => LineCap::Square,
                _ => return Ok(Value::Undefined),
            };
            with_ctx(vm, &c, false, |x| x.state.cap = cap)?;
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        proto,
        "lineJoin",
        |vm, c| {
            let s = with_ctx(vm, &c, false, |x| match x.state.join {
                LineJoin::Round => "round",
                LineJoin::Bevel => "bevel",
                LineJoin::Miter => "miter",
            })?;
            Ok(js(s))
        },
        Some(|vm, c| {
            let s = rstr(vm, &c.arg(0))?;
            let j = match s.as_str() {
                "round" => LineJoin::Round,
                "bevel" => LineJoin::Bevel,
                "miter" => LineJoin::Miter,
                _ => return Ok(Value::Undefined),
            };
            with_ctx(vm, &c, false, |x| x.state.join = j)?;
            Ok(Value::Undefined)
        }),
    );
    op(vm, proto, "setLineDash", 1, |vm, c| {
        let arr = c.arg(0);
        let Value::Object(o) = &arr else { return vm.type_error("setLineDash needs a sequence") };
        let key = PropKey::Sym(vm.realm.sym_iterator.clone());
        let f = vm.get(*o, &key)?;
        let items = crate::url::iter_values(vm, &arr, &f)?;
        let mut v = Vec::with_capacity(items.len());
        for i in items {
            let x = vm.to_number(&i)?;
            if !x.is_finite() || x < 0.0 {
                return Ok(Value::Undefined);
            }
            v.push(x as f32);
        }
        if v.len() % 2 == 1 {
            let copy = v.clone();
            v.extend(copy);
        }
        with_ctx(vm, &c, false, |x| x.state.dash = v)?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "getLineDash", 0, |vm, c| {
        let d = with_ctx(vm, &c, false, |x| x.state.dash.clone())?;
        let vals = d.into_iter().map(|x| Value::Number(x as f64)).collect();
        Ok(Value::Object(vm.new_array(vals)))
    });
    // Text styles (HTML §4.12.5.1.5).
    str_attr(
        vm,
        proto,
        "font",
        |x| serialize_font(&x.state.font),
        |x, v| {
            if bb_paint::canvas2d::parse_font(v).is_some() {
                x.state.font = v.to_string();
            }
        },
    );
    str_attr(
        vm,
        proto,
        "textAlign",
        |x| x.state.text_align.clone(),
        |x, v| {
            if matches!(v, "start" | "end" | "left" | "right" | "center") {
                x.state.text_align = v.to_string();
            }
        },
    );
    str_attr(
        vm,
        proto,
        "textBaseline",
        |x| x.state.text_baseline.clone(),
        |x, v| {
            if matches!(v, "top" | "hanging" | "middle" | "alphabetic" | "ideographic" | "bottom") {
                x.state.text_baseline = v.to_string();
            }
        },
    );
    str_attr(
        vm,
        proto,
        "direction",
        |x| x.state.direction.clone(),
        |x, v| {
            if matches!(v, "ltr" | "rtl" | "inherit") {
                x.state.direction = v.to_string();
            }
        },
    );
    str_attr(
        vm,
        proto,
        "letterSpacing",
        |x| format!("{}px", x.state.letter_spacing),
        |x, v| {
            if let Some(n) = v.strip_suffix("px").and_then(|n| n.trim().parse::<f32>().ok()) {
                x.state.letter_spacing = n;
            }
        },
    );
    // Rectangles (HTML §4.12.5.1.14).
    for (name, kind) in [("fillRect", 0.0), ("strokeRect", 1.0), ("clearRect", 2.0)] {
        let f = bb_js::vm::realm::native_with(vm, name, 4, rect_op, false, vec![Value::Number(kind)]);
        vm.heap.obj_mut(proto).props.insert(PropKey::named(name), method_prop(f));
    }
    // Paths (HTML §4.12.5.1.6 CanvasPath, and the current default path).
    setup_path_methods(vm, proto, PathOwner::Context);
    op(vm, proto, "beginPath", 0, |vm, c| {
        with_ctx(vm, &c, false, |x| x.path = CanvasPath::new()).map(|_| Value::Undefined)
    });
    op(vm, proto, "fill", 0, |vm, c| {
        let (h, _, _) = ctx_key(vm, &c)?;
        let (p, rule) = path_and_rule(vm, &h, &c, 0)?;
        with_ctx(vm, &c, true, |x| x.fill(p.as_ref(), rule))?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "stroke", 0, |vm, c| {
        let (h, _, _) = ctx_key(vm, &c)?;
        let p = path_arg(vm, &h, &c.arg(0));
        with_ctx(vm, &c, true, |x| x.stroke(p.as_ref()))?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "clip", 0, |vm, c| {
        let (h, _, _) = ctx_key(vm, &c)?;
        let (p, rule) = path_and_rule(vm, &h, &c, 0)?;
        with_ctx(vm, &c, false, |x| x.clip(p.as_ref(), rule))?;
        Ok(Value::Undefined)
    });
    op(vm, proto, "isPointInPath", 2, |vm, c| {
        let (h, _, _) = ctx_key(vm, &c)?;
        let p = path_arg(vm, &h, &c.arg(0));
        let first = if p.is_some() { 1 } else { 0 };
        let x = vm.to_number(&c.arg(first))? as f32;
        let y = vm.to_number(&c.arg(first + 1))? as f32;
        let rule = fill_rule(vm, &c.arg(first + 2))?;
        let r = with_ctx(vm, &c, false, |cx| cx.point_in_path(p.as_ref(), x, y, rule))?;
        Ok(Value::Bool(r))
    });
    op(vm, proto, "isPointInStroke", 2, |vm, c| {
        let (h, _, _) = ctx_key(vm, &c)?;
        let p = path_arg(vm, &h, &c.arg(0));
        let first = if p.is_some() { 1 } else { 0 };
        let x = vm.to_number(&c.arg(first))? as f32;
        let y = vm.to_number(&c.arg(first + 1))? as f32;
        let r = with_ctx(vm, &c, false, |cx| cx.point_in_stroke(p.as_ref(), x, y))?;
        Ok(Value::Bool(r))
    });
    // Text (HTML §4.12.5.1.7).
    for (name, stroke) in [("fillText", false), ("strokeText", true)] {
        let f: bb_js::vm::heap::NativeFn =
            if stroke { |vm, c| text_op(vm, &c, true) } else { |vm, c| text_op(vm, &c, false) };
        op(vm, proto, name, 3, f);
    }
    op(vm, proto, "measureText", 1, |vm, c| {
        let s = rstr(vm, &c.arg(0))?;
        let m = with_ctx(vm, &c, false, |x| x.measure_text(&s))?;
        let h = host(vm)?;
        let o = vm.new_host_object(Some(h.proto(C::TextMetrics)), C::TextMetrics as u32, 0, false);
        let vals = [
            m.width,
            m.actual_left,
            m.actual_right,
            m.font_ascent,
            m.font_descent,
            m.actual_ascent,
            m.actual_descent,
            m.em_ascent,
            m.em_descent,
            m.hanging,
            m.alphabetic,
            m.ideographic,
        ];
        if let Some(ho) = vm.host_obj_mut(o) {
            ho.slots = vals.iter().map(|&v| Value::Number(v as f64)).collect();
        }
        Ok(Value::Object(o))
    });
    // Images (HTML §4.12.5.1.15).
    op(vm, proto, "drawImage", 3, |vm, c| {
        let (h, _, _) = ctx_key(vm, &c)?;
        let n = c.args.len();
        if !(n == 3 || n == 5 || n == 9) {
            return vm.type_error("drawImage takes 3, 5 or 9 arguments");
        }
        let mut v = Vec::with_capacity(n - 1);
        for i in 1..n {
            v.push(vm.to_number(&c.arg(i))? as f32);
        }
        let Some(img) = image_source(vm, &h, &c.arg(0))? else { return Ok(Value::Undefined) };
        let (iw, ih) = (img.w as f32, img.h as f32);
        let (sx, sy, sw, sh, dx, dy, dw, dh) = match n {
            3 => (0.0, 0.0, iw, ih, v[0], v[1], iw, ih),
            5 => (0.0, 0.0, iw, ih, v[0], v[1], v[2], v[3]),
            _ => (v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]),
        };
        with_ctx(vm, &c, true, |x| x.draw_image(&img, sx, sy, sw, sh, dx, dy, dw, dh))?;
        Ok(Value::Undefined)
    });
    // Pixel manipulation (HTML §4.12.5.1.16).
    op(vm, proto, "createImageData", 1, |vm, c| {
        let (w, ht) = if vm.host_data(&c.arg(0)).is_some_and(|(k, _)| k == C::ImageData as u32) {
            let Value::Object(o) = c.arg(0) else { return Ok(Value::Undefined) };
            (num_slot(vm, o, 1) as u32, num_slot(vm, o, 2) as u32)
        } else {
            let w = long(vm, &c.arg(0))?;
            let ht = long(vm, &c.arg(1))?;
            if w == 0 || ht == 0 {
                return throw(vm, "IndexSizeError", "the size is zero");
            }
            (w.unsigned_abs() as u32, ht.unsigned_abs() as u32)
        };
        new_image_data(vm, vec![0; (w as usize) * (ht as usize) * 4], w, ht)
    });
    op(vm, proto, "getImageData", 4, |vm, c| {
        let mut a = [0i64; 4];
        for (i, slot) in a.iter_mut().enumerate() {
            *slot = long(vm, &c.arg(i))?;
        }
        if a[2] == 0 || a[3] == 0 {
            return throw(vm, "IndexSizeError", "the size is zero");
        }
        let (mut sx, mut sy, mut sw, mut sh) = (a[0], a[1], a[2], a[3]);
        if sw < 0 {
            sx += sw;
            sw = -sw;
        }
        if sh < 0 {
            sy += sh;
            sh = -sh;
        }
        if sw.saturating_mul(sh) > (1 << 28) {
            return throw(vm, "IndexSizeError", "the rectangle is too large");
        }
        let data = with_ctx(vm, &c, false, |x| x.get_image_data(sx as i32, sy as i32, sw as u32, sh as u32))?;
        new_image_data(vm, data, sw as u32, sh as u32)
    });
    op(vm, proto, "putImageData", 3, |vm, c| {
        let img = c.arg(0);
        if !vm.host_data(&img).is_some_and(|(k, _)| k == C::ImageData as u32) {
            return vm.type_error("putImageData needs an ImageData");
        }
        let Value::Object(io) = img else { return Ok(Value::Undefined) };
        let dx = long(vm, &c.arg(1))? as i32;
        let dy = long(vm, &c.arg(2))? as i32;
        let (w, ht) = (num_slot(vm, io, 1) as u32, num_slot(vm, io, 2) as u32);
        let dirty = if c.args.len() >= 7 {
            let mut d = [0i32; 4];
            for (i, slot) in d.iter_mut().enumerate() {
                *slot = long(vm, &c.arg(3 + i))? as i32;
            }
            (d[0], d[1], d[2], d[3])
        } else {
            (0, 0, w as i32, ht as i32)
        };
        let bytes = vm.buffer_source_bytes(&slot(vm, io, 0)).unwrap_or_default();
        with_ctx(vm, &c, true, |x| x.put_image_data(&bytes, w, ht, dx, dy, dirty))?;
        Ok(Value::Undefined)
    });
}

fn method_prop(f: ObjRef) -> bb_js::vm::heap::Prop {
    bb_js::vm::heap::Prop {
        slot: bb_js::vm::heap::Slot::Data(Value::Object(f)),
        writable: true,
        enumerable: true,
        configurable: true,
    }
}

fn num_slot(vm: &Vm, o: ObjRef, i: usize) -> f64 {
    match slot(vm, o, i) {
        Value::Number(n) => n,
        _ => 0.0,
    }
}

/// WebIDL `long` conversion.
fn long(vm: &mut Vm, v: &Value) -> JsResult<i64> {
    Ok(vm.to_int32(v)? as i64)
}

/// DOMMatrix2DInit (a, b, c, d, e, f with m11 … m42 aliases), for setTransform.
fn matrix_init(vm: &mut Vm, v: &Value) -> JsResult<Matrix> {
    if matches!(v, Value::Undefined | Value::Null) {
        return Ok(Matrix::IDENTITY);
    }
    let mut out = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    for (i, (short, long_name)) in
        [("a", "m11"), ("b", "m12"), ("c", "m21"), ("d", "m22"), ("e", "m41"), ("f", "m42")]
            .iter()
            .enumerate()
    {
        let s = dict_get(vm, v, short)?;
        let l = dict_get(vm, v, long_name)?;
        let x = if !s.is_undefined() {
            vm.to_number(&s)?
        } else if !l.is_undefined() {
            vm.to_number(&l)?
        } else {
            out[i] as f64
        };
        if !x.is_finite() {
            return vm.type_error("the matrix has a non-finite component");
        }
        out[i] = x as f32;
    }
    Ok(Matrix::new(out[0], out[1], out[2], out[3], out[4], out[5]))
}

/// A numeric attribute of the context (unrestricted double: non-finite values are ignored).
fn num_attr(
    vm: &mut Vm,
    proto: ObjRef,
    name: &str,
    get: fn(&Context2D) -> f32,
    set: fn(&mut Context2D, f32),
) {
    // Natives can't capture: the getter and setter find theirs in a table, by index.
    NUM_ATTRS.with(|t| t.borrow_mut().push((name.to_string(), get, set)));
    let idx = NUM_ATTRS.with(|t| t.borrow().len() - 1);
    crate::idl::attr_with(
        vm,
        proto,
        name,
        |vm, c| {
            let i = attr_index(vm, &c);
            let Some(get) = NUM_ATTRS.with(|t| t.borrow().get(i).map(|x| x.1)) else {
                return Ok(Value::Undefined);
            };
            let v = with_ctx(vm, &c, false, |x| get(x))?;
            Ok(Value::Number(v as f64))
        },
        Some(|vm, c| {
            let v = vm.to_number(&c.arg(0))?;
            if !v.is_finite() {
                return Ok(Value::Undefined);
            }
            let i = attr_index(vm, &c);
            let Some(set) = NUM_ATTRS.with(|t| t.borrow().get(i).map(|x| x.2)) else {
                return Ok(Value::Undefined);
            };
            with_ctx(vm, &c, false, |x| set(x, v as f32))?;
            Ok(Value::Undefined)
        }),
        vec![Value::Number(idx as f64)],
    );
}

type NumAttr = (String, fn(&Context2D) -> f32, fn(&mut Context2D, f32));
type StrAttr = (String, fn(&Context2D) -> String, fn(&mut Context2D, &str));

thread_local! {
    static NUM_ATTRS: std::cell::RefCell<Vec<NumAttr>> = const { std::cell::RefCell::new(Vec::new()) };
    static STR_ATTRS: std::cell::RefCell<Vec<StrAttr>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn attr_index(vm: &Vm, c: &NativeCall) -> usize {
    match crate::idl::callee_slots(vm, c.callee).first() {
        Some(Value::Number(n)) => *n as usize,
        _ => usize::MAX,
    }
}

/// A string attribute of the context (DOMString; the setter ignores unknown values).
fn str_attr(
    vm: &mut Vm,
    proto: ObjRef,
    name: &str,
    get: fn(&Context2D) -> String,
    set: fn(&mut Context2D, &str),
) {
    STR_ATTRS.with(|t| t.borrow_mut().push((name.to_string(), get, set)));
    let idx = STR_ATTRS.with(|t| t.borrow().len() - 1);
    crate::idl::attr_with(
        vm,
        proto,
        name,
        |vm, c| {
            let i = attr_index(vm, &c);
            let Some(get) = STR_ATTRS.with(|t| t.borrow().get(i).map(|x| x.1)) else {
                return Ok(Value::Undefined);
            };
            let v = with_ctx(vm, &c, false, |x| get(x))?;
            Ok(js(&v))
        },
        Some(|vm, c| {
            let v = rstr(vm, &c.arg(0))?;
            let i = attr_index(vm, &c);
            let Some(set) = STR_ATTRS.with(|t| t.borrow().get(i).map(|x| x.2)) else {
                return Ok(Value::Undefined);
            };
            with_ctx(vm, &c, false, |x| set(x, &v))?;
            Ok(Value::Undefined)
        }),
        vec![Value::Number(idx as f64)],
    );
}

/// The canvas font serialized as CSS (HTML: the font attribute's getter).
fn serialize_font(s: &str) -> String {
    let Some(f) = bb_paint::canvas2d::parse_font(s) else { return s.to_string() };
    let mut out = String::new();
    if f.italic {
        out.push_str("italic ");
    }
    match f.weight {
        400 => {}
        700 => out.push_str("bold "),
        w => out.push_str(&format!("{w} ")),
    }
    let size = if f.size.fract() == 0.0 { format!("{}", f.size as i64) } else { format!("{}", f.size) };
    out.push_str(&format!("{size}px "));
    let fams: Vec<String> =
        f.families.iter().map(|x| if x.contains(' ') { format!("\"{x}\"") } else { x.clone() }).collect();
    out.push_str(&fams.join(", "));
    out
}

fn rect_op(vm: &mut Vm, c: NativeCall) -> R {
    let kind = match crate::idl::callee_slots(vm, c.callee).first() {
        Some(Value::Number(n)) => *n as u8,
        _ => 0,
    };
    let Some(a) = finite_args(vm, &c, 4)? else {
        ctx_key(vm, &c)?;
        return Ok(Value::Undefined);
    };
    with_ctx(vm, &c, true, |x| match kind {
        0 => x.fill_rect(a[0], a[1], a[2], a[3]),
        1 => x.stroke_rect(a[0], a[1], a[2], a[3]),
        _ => x.clear_rect(a[0], a[1], a[2], a[3]),
    })?;
    Ok(Value::Undefined)
}

fn text_op(vm: &mut Vm, c: &NativeCall, stroke: bool) -> R {
    let s = rstr(vm, &c.arg(0))?;
    let x = vm.to_number(&c.arg(1))? as f32;
    let y = vm.to_number(&c.arg(2))? as f32;
    let max = match c.arg(3) {
        Value::Undefined => None,
        v => Some(vm.to_number(&v)? as f32),
    };
    with_ctx(vm, c, true, |cx| cx.text(&s, x, y, max, stroke))?;
    Ok(Value::Undefined)
}

fn new_gradient(vm: &mut Vm, kind: GradientKind) -> R {
    let h = host(vm)?;
    let id = {
        let mut st = h.canvas.borrow_mut();
        st.gradients.push(Gradient { kind, stops: Vec::new() });
        st.gradients.len() as u64 - 1
    };
    Ok(Value::Object(vm.new_host_object(
        Some(h.proto(C::CanvasGradient)),
        C::CanvasGradient as u32,
        id,
        false,
    )))
}

pub fn setup_gradient(vm: &mut Vm, proto: ObjRef) {
    // HTML addColorStop(offset, color).
    op(vm, proto, "addColorStop", 2, |vm, c| {
        let (_, id) = this_of(vm, &c.this, C::CanvasGradient)?;
        let off = vm.to_number(&c.arg(0))?;
        if !off.is_finite() {
            return vm.type_error("the offset is not finite");
        }
        let s = rstr(vm, &c.arg(1))?;
        if !(0.0..=1.0).contains(&off) {
            return throw(vm, "IndexSizeError", "the offset must be in 0–1");
        }
        let Some(col) = parse_color(&s) else {
            return throw(vm, "SyntaxError", &format!("'{s}' is not a color"));
        };
        let h = host(vm)?;
        if let Some(g) = h.canvas.borrow_mut().gradients.get_mut(id as usize) {
            g.add_stop(off as f32, col);
        }
        Ok(Value::Undefined)
    });
}

pub fn setup_pattern(vm: &mut Vm, proto: ObjRef) {
    op(vm, proto, "setTransform", 0, |vm, c| {
        let (_, id) = this_of(vm, &c.this, C::CanvasPattern)?;
        let m = matrix_init(vm, &c.arg(0))?;
        let h = host(vm)?;
        if let Some(p) = h.canvas.borrow_mut().patterns.get_mut(id as usize) {
            p.transform = m;
        }
        Ok(Value::Undefined)
    });
}

pub fn setup_text_metrics(vm: &mut Vm, proto: ObjRef) {
    let names = [
        "width",
        "actualBoundingBoxLeft",
        "actualBoundingBoxRight",
        "fontBoundingBoxAscent",
        "fontBoundingBoxDescent",
        "actualBoundingBoxAscent",
        "actualBoundingBoxDescent",
        "emHeightAscent",
        "emHeightDescent",
        "hangingBaseline",
        "alphabeticBaseline",
        "ideographicBaseline",
    ];
    for (i, name) in names.iter().enumerate() {
        crate::idl::attr_with(
            vm,
            proto,
            name,
            |vm, c| {
                this_of(vm, &c.this, C::TextMetrics)?;
                let i = attr_index(vm, &c);
                let Value::Object(o) = c.this else { return Ok(Value::Undefined) };
                Ok(slot(vm, o, i))
            },
            None,
            vec![Value::Number(i as f64)],
        );
    }
}

// ---- ImageData (HTML §4.12.5.1.16) ----

fn new_image_data(vm: &mut Vm, data: Vec<u8>, w: u32, h: u32) -> R {
    let host = host(vm)?;
    let buf = vm.new_array_buffer(data);
    let ctor = vm.realm.ta_ctors[TaKind::Uint8Clamped.index()];
    let arr = vm.construct(ctor, vec![Value::Object(buf)], None)?;
    let o = vm.new_host_object(Some(host.proto(C::ImageData)), C::ImageData as u32, 0, false);
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = vec![arr, Value::Number(w as f64), Value::Number(h as f64)];
    }
    Ok(Value::Object(o))
}

pub fn image_data_ctor(vm: &mut Vm, c: NativeCall) -> R {
    if c.new_target.is_none() {
        return vm.type_error("ImageData requires 'new'");
    }
    if c.args.len() < 2 {
        return vm.type_error("ImageData needs at least 2 arguments");
    }
    // new ImageData(data, sw [, sh]) or new ImageData(sw, sh).
    let first = c.arg(0);
    let is_array =
        matches!(&first, Value::Object(o) if vm.ta_view(*o).is_some_and(|v| v.0 == TaKind::Uint8Clamped));
    if is_array {
        let Value::Object(arr) = first else { return Ok(Value::Undefined) };
        let len = vm.ta_view(arr).map_or(0, |v| v.3) as u64;
        let sw = vm.to_uint32(&c.arg(1))? as u64;
        if len == 0 || len % 4 != 0 {
            return throw(vm, "InvalidStateError", "the data length is not a non-zero multiple of 4");
        }
        if sw == 0 || (len / 4) % sw != 0 {
            return throw(vm, "IndexSizeError", "the data length is not a multiple of the width");
        }
        let sh = len / 4 / sw;
        if !c.arg(2).is_undefined() && vm.to_uint32(&c.arg(2))? as u64 != sh {
            return throw(vm, "IndexSizeError", "the height doesn't match the data");
        }
        let h = host(vm)?;
        let o = vm.new_host_object(Some(h.proto(C::ImageData)), C::ImageData as u32, 0, false);
        if let Some(ho) = vm.host_obj_mut(o) {
            ho.slots = vec![Value::Object(arr), Value::Number(sw as f64), Value::Number(sh as f64)];
        }
        return Ok(Value::Object(o));
    }
    let sw = vm.to_uint32(&c.arg(0))?;
    let sh = vm.to_uint32(&c.arg(1))?;
    if sw == 0 || sh == 0 {
        return throw(vm, "IndexSizeError", "the size is zero");
    }
    if (sw as u64) * (sh as u64) > (1 << 28) {
        return throw(vm, "IndexSizeError", "the image data is too large");
    }
    new_image_data(vm, vec![0; sw as usize * sh as usize * 4], sw, sh)
}

pub fn setup_image_data(vm: &mut Vm, proto: ObjRef) {
    for (name, i) in [("data", 0usize), ("width", 1), ("height", 2)] {
        crate::idl::attr_with(
            vm,
            proto,
            name,
            |vm, c| {
                this_of(vm, &c.this, C::ImageData)?;
                let i = attr_index(vm, &c);
                let Value::Object(o) = c.this else { return Ok(Value::Undefined) };
                Ok(slot(vm, o, i))
            },
            None,
            vec![Value::Number(i as f64)],
        );
    }
    attr(
        vm,
        proto,
        "colorSpace",
        |vm, c| {
            this_of(vm, &c.this, C::ImageData)?;
            Ok(js("srgb"))
        },
        None,
    );
}

// ---- Path2D (HTML §4.12.5.1.6) ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathOwner {
    Context,
    Path2D,
}

pub fn path2d_ctor(vm: &mut Vm, c: NativeCall) -> R {
    let Some(nt) = c.new_target else { return vm.type_error("Path2D requires 'new'") };
    let h = host(vm)?;
    let path = match c.arg(0) {
        Value::Undefined => CanvasPath::new(),
        v => match path_arg(vm, &h, &v) {
            Some(p) => p,
            None => {
                let d = rstr(vm, &v)?;
                bb_paint::canvas2d::parse_svg_path(&d)
            }
        },
    };
    let id = {
        let mut st = h.canvas.borrow_mut();
        st.paths.push(path);
        st.paths.len() as u64 - 1
    };
    let proto = match vm.get(nt, &PropKey::lit("prototype"))? {
        Value::Object(p) => p,
        _ => h.proto(C::Path2D),
    };
    Ok(Value::Object(vm.new_host_object(Some(proto), C::Path2D as u32, id, false)))
}

pub fn setup_path2d(vm: &mut Vm, proto: ObjRef) {
    setup_path_methods(vm, proto, PathOwner::Path2D);
    op(vm, proto, "addPath", 1, |vm, c| {
        let (_, id) = this_of(vm, &c.this, C::Path2D)?;
        let h = host(vm)?;
        let Some(other) = path_arg(vm, &h, &c.arg(0)) else { return vm.type_error("addPath needs a Path2D") };
        let m = matrix_init(vm, &c.arg(1))?;
        if let Some(p) = h.canvas.borrow_mut().paths.get_mut(id as usize) {
            p.add(&other.transformed(&m));
        }
        Ok(Value::Undefined)
    });
}

/// Run `f` with the path builder of `this`: the context's default path (with its
/// transform) or a Path2D's (identity).
fn with_builder(vm: &mut Vm, c: &NativeCall, f: impl FnOnce(&mut PathBuilder)) -> JsResult<()> {
    if let Some((cls, id)) = vm.host_data(&c.this) {
        if cls == C::Path2D as u32 {
            let h = host(vm)?;
            let mut st = h.canvas.borrow_mut();
            if let Some(p) = st.paths.get_mut(id as usize) {
                f(&mut PathBuilder { path: p, m: Matrix::IDENTITY });
            }
            return Ok(());
        }
    }
    with_ctx(vm, c, false, |x| f(&mut x.builder()))
}

fn path_this_check(vm: &mut Vm, c: &NativeCall, owner: PathOwner) -> JsResult<()> {
    match owner {
        PathOwner::Context => this_of(vm, &c.this, C::CanvasRenderingContext2D).map(|_| ()),
        PathOwner::Path2D => this_of(vm, &c.this, C::Path2D).map(|_| ()),
    }
}

fn setup_path_methods(vm: &mut Vm, proto: ObjRef, owner: PathOwner) {
    let tag = Value::Number(if owner == PathOwner::Context { 0.0 } else { 1.0 });
    let define = |vm: &mut Vm, name: &str, len: u32, f: bb_js::vm::heap::NativeFn| {
        let m = bb_js::vm::realm::native_with(vm, name, len, f, false, vec![tag.clone()]);
        vm.heap.obj_mut(proto).props.insert(PropKey::named(name), method_prop(m));
    };
    fn owner_of(vm: &Vm, c: &NativeCall) -> PathOwner {
        match crate::idl::callee_slots(vm, c.callee).first() {
            Some(Value::Number(n)) if *n == 1.0 => PathOwner::Path2D,
            _ => PathOwner::Context,
        }
    }
    define(vm, "closePath", 0, |vm, c| {
        path_this_check(vm, &c, owner_of(vm, &c))?;
        with_builder(vm, &c, |b| b.close())?;
        Ok(Value::Undefined)
    });
    define(vm, "moveTo", 2, |vm, c| {
        path_this_check(vm, &c, owner_of(vm, &c))?;
        let Some(a) = finite_args(vm, &c, 2)? else { return Ok(Value::Undefined) };
        with_builder(vm, &c, |b| b.move_to(a[0], a[1]))?;
        Ok(Value::Undefined)
    });
    define(vm, "lineTo", 2, |vm, c| {
        path_this_check(vm, &c, owner_of(vm, &c))?;
        let Some(a) = finite_args(vm, &c, 2)? else { return Ok(Value::Undefined) };
        with_builder(vm, &c, |b| b.line_to(a[0], a[1]))?;
        Ok(Value::Undefined)
    });
    define(vm, "quadraticCurveTo", 4, |vm, c| {
        path_this_check(vm, &c, owner_of(vm, &c))?;
        let Some(a) = finite_args(vm, &c, 4)? else { return Ok(Value::Undefined) };
        with_builder(vm, &c, |b| b.quad_to(a[0], a[1], a[2], a[3]))?;
        Ok(Value::Undefined)
    });
    define(vm, "bezierCurveTo", 6, |vm, c| {
        path_this_check(vm, &c, owner_of(vm, &c))?;
        let Some(a) = finite_args(vm, &c, 6)? else { return Ok(Value::Undefined) };
        with_builder(vm, &c, |b| b.cubic_to(a[0], a[1], a[2], a[3], a[4], a[5]))?;
        Ok(Value::Undefined)
    });
    define(vm, "arcTo", 5, |vm, c| {
        path_this_check(vm, &c, owner_of(vm, &c))?;
        let Some(a) = finite_args(vm, &c, 5)? else { return Ok(Value::Undefined) };
        if a[4] < 0.0 {
            return throw(vm, "IndexSizeError", "the radius is negative");
        }
        with_builder(vm, &c, |b| b.arc_to(a[0], a[1], a[2], a[3], a[4]))?;
        Ok(Value::Undefined)
    });
    define(vm, "rect", 4, |vm, c| {
        path_this_check(vm, &c, owner_of(vm, &c))?;
        let Some(a) = finite_args(vm, &c, 4)? else { return Ok(Value::Undefined) };
        with_builder(vm, &c, |b| b.rect(a[0], a[1], a[2], a[3]))?;
        Ok(Value::Undefined)
    });
    define(vm, "roundRect", 4, |vm, c| {
        path_this_check(vm, &c, owner_of(vm, &c))?;
        let Some(a) = finite_args(vm, &c, 4)? else { return Ok(Value::Undefined) };
        let radii = round_rect_radii(vm, &c.arg(4))?;
        let Some(radii) = radii else { return Ok(Value::Undefined) };
        with_builder(vm, &c, |b| b.round_rect(a[0], a[1], a[2], a[3], radii))?;
        Ok(Value::Undefined)
    });
    define(vm, "arc", 5, |vm, c| {
        path_this_check(vm, &c, owner_of(vm, &c))?;
        let Some(a) = finite_args(vm, &c, 5)? else { return Ok(Value::Undefined) };
        if a[2] < 0.0 {
            return throw(vm, "IndexSizeError", "the radius is negative");
        }
        let ccw = vm.to_boolean(&c.arg(5));
        with_builder(vm, &c, |b| b.ellipse(a[0], a[1], a[2], a[2], 0.0, a[3], a[4], ccw))?;
        Ok(Value::Undefined)
    });
    define(vm, "ellipse", 7, |vm, c| {
        path_this_check(vm, &c, owner_of(vm, &c))?;
        let Some(a) = finite_args(vm, &c, 7)? else { return Ok(Value::Undefined) };
        if a[2] < 0.0 || a[3] < 0.0 {
            return throw(vm, "IndexSizeError", "a radius is negative");
        }
        let ccw = vm.to_boolean(&c.arg(7));
        with_builder(vm, &c, |b| b.ellipse(a[0], a[1], a[2], a[3], a[4], a[5], a[6], ccw))?;
        Ok(Value::Undefined)
    });
}

/// roundRect's radii argument (HTML roundRect steps 1–8): 1 to 4 numbers or DOMPointInits.
/// None when a value isn't finite (the call does nothing).
fn round_rect_radii(vm: &mut Vm, v: &Value) -> JsResult<Option<[(f32, f32); 4]>> {
    let items: Vec<Value> = match v {
        Value::Undefined => vec![Value::Number(0.0)],
        Value::Object(o)
            if !vm.is_callable(v)
                && vm.get(*o, &PropKey::Sym(vm.realm.sym_iterator.clone()))?.is_nullish() =>
        {
            vec![v.clone()]
        }
        Value::Object(o) => {
            let key = PropKey::Sym(vm.realm.sym_iterator.clone());
            let f = vm.get(*o, &key)?;
            crate::url::iter_values(vm, v, &f)?
        }
        v => vec![v.clone()],
    };
    if items.is_empty() || items.len() > 4 {
        return vm.range_error("roundRect takes 1 to 4 radii");
    }
    let mut rs = Vec::with_capacity(items.len());
    for it in items {
        let (x, y) = match &it {
            Value::Object(_) => {
                let x = dict_get(vm, &it, "x")?;
                let y = dict_get(vm, &it, "y")?;
                let x = if x.is_undefined() { 0.0 } else { vm.to_number(&x)? };
                let y = if y.is_undefined() { 0.0 } else { vm.to_number(&y)? };
                (x, y)
            }
            _ => {
                let n = vm.to_number(&it)?;
                (n, n)
            }
        };
        if !x.is_finite() || !y.is_finite() {
            return Ok(None);
        }
        if x < 0.0 || y < 0.0 {
            return vm.range_error("a radius is negative");
        }
        rs.push((x as f32, y as f32));
    }
    Ok(Some(match rs.len() {
        1 => [rs[0]; 4],
        2 => [rs[0], rs[1], rs[0], rs[1]],
        3 => [rs[0], rs[1], rs[2], rs[1]],
        _ => [rs[0], rs[1], rs[2], rs[3]],
    }))
}

// ---- ImageBitmap (HTML §8.10) ----

fn bitmap_object(vm: &mut Vm, h: &Host, b: Bitmap) -> Value {
    let id = {
        let mut st = h.canvas.borrow_mut();
        st.bitmaps.push(Some(b));
        st.bitmaps.len() as u64 - 1
    };
    Value::Object(vm.new_host_object(Some(h.proto(C::ImageBitmap)), C::ImageBitmap as u32, id, false))
}

/// An ImageBitmap's pixels (None once closed).
fn bitmap_pixels(h: &Host, id: u64) -> Option<Bitmap> {
    h.canvas.borrow().bitmaps.get(id as usize).and_then(|b| b.clone())
}

pub fn setup_image_bitmap(vm: &mut Vm, proto: ObjRef) {
    for (name, w) in [("width", true), ("height", false)] {
        let f: bb_js::vm::heap::NativeFn = if w {
            |vm, c| {
                let (_, id) = this_of(vm, &c.this, C::ImageBitmap)?;
                let h = host(vm)?;
                Ok(Value::Number(bitmap_pixels(&h, id).map_or(0.0, |b| b.w as f64)))
            }
        } else {
            |vm, c| {
                let (_, id) = this_of(vm, &c.this, C::ImageBitmap)?;
                let h = host(vm)?;
                Ok(Value::Number(bitmap_pixels(&h, id).map_or(0.0, |b| b.h as f64)))
            }
        };
        attr(vm, proto, name, f, None);
    }
    op(vm, proto, "close", 0, |vm, c| {
        let (_, id) = this_of(vm, &c.this, C::ImageBitmap)?;
        let h = host(vm)?;
        if let Some(b) = h.canvas.borrow_mut().bitmaps.get_mut(id as usize) {
            *b = None;
        }
        Ok(Value::Undefined)
    });
}

/// The pixels of an ImageBitmapSource (HTML §8.10 createImageBitmap step 3 by type).
fn bitmap_source(vm: &mut Vm, h: &Host, v: &Value) -> JsResult<Result<Bitmap, &'static str>> {
    if let Some((cls, id)) = vm.host_data(v) {
        if cls == C::ImageBitmap as u32 {
            return Ok(bitmap_pixels(h, id).ok_or("the ImageBitmap is closed"));
        }
        if cls == C::ImageData as u32 {
            let &Value::Object(o) = v else { return Ok(Err("not an ImageData")) };
            let (w, ht) = (num_slot(vm, o, 1) as u32, num_slot(vm, o, 2) as u32);
            let mut data = vm.buffer_source_bytes(&slot(vm, o, 0)).unwrap_or_default();
            bb_image::premultiply(&mut data);
            return Ok(Ok(Bitmap { w, h: ht, data }));
        }
        if let Some((bytes, _, _)) = crate::blob::blob_data(vm, v) {
            return Ok(match bb_image::decode(&bytes) {
                Ok(img) => Ok(Bitmap { w: img.width, h: img.height, data: img.pixels().to_vec() }),
                Err(_) => Err("the blob isn't a decodable image"),
            });
        }
    }
    match image_source(vm, h, v)? {
        Some(b) => Ok(Ok(b)),
        None => Ok(Err("the image isn't available")),
    }
}

/// createImageBitmap(image [, sx, sy, sw, sh] [, options]) (HTML §8.10).
pub fn create_image_bitmap(vm: &mut Vm, c: NativeCall) -> R {
    let p = vm.new_promise(None);
    let (resolve, reject) = vm.create_resolving_functions(p);
    let r = (|| -> JsResult<Value> {
        let h = host(vm)?;
        let src = bitmap_source(vm, &h, &c.arg(0))?;
        let crop = if c.args.len() >= 5 {
            let mut a = [0i64; 4];
            for (i, s) in a.iter_mut().enumerate() {
                *s = long(vm, &c.arg(1 + i))?;
            }
            if a[2] == 0 || a[3] == 0 {
                return vm.range_error("the crop rectangle is empty");
            }
            Some(a)
        } else {
            None
        };
        let opts = if crop.is_some() { c.arg(5) } else { c.arg(1) };
        let rw = dict_get(vm, &opts, "resizeWidth")?;
        let rh = dict_get(vm, &opts, "resizeHeight")?;
        let rw = if rw.is_undefined() { None } else { Some(vm.to_uint32(&rw)?) };
        let rh = if rh.is_undefined() { None } else { Some(vm.to_uint32(&rh)?) };
        if rw == Some(0) || rh == Some(0) {
            return throw(vm, "InvalidStateError", "the resize size is zero");
        }
        let orient = dict_get(vm, &opts, "imageOrientation")?;
        let flip = !orient.is_undefined() && rstr(vm, &orient)? == "flipY";
        let src = match src {
            Ok(b) => b,
            Err(why) => return throw(vm, "InvalidStateError", why),
        };
        // Crop (areas outside the source are transparent), then resize and flip.
        let (sx, sy, sw, sh) = match crop {
            Some([x, y, w, ht]) => {
                let (x, w) = if w < 0 { (x + w, -w) } else { (x, w) };
                let (y, ht) = if ht < 0 { (y + ht, -ht) } else { (y, ht) };
                (x, y, w as u32, ht as u32)
            }
            None => (0, 0, src.w, src.h),
        };
        if sw == 0 || sh == 0 || (sw as u64) * (sh as u64) > (1 << 28) {
            return throw(vm, "InvalidStateError", "the image has no pixels");
        }
        let (ow, oh) = match (rw, rh) {
            (Some(w), Some(ht)) => (w, ht),
            (Some(w), None) => (w, ((sh as u64 * w as u64) / sw as u64).max(1) as u32),
            (None, Some(ht)) => (((sw as u64 * ht as u64) / sh as u64).max(1) as u32, ht),
            (None, None) => (sw, sh),
        };
        if (ow as u64) * (oh as u64) > (1 << 28) {
            return throw(vm, "InvalidStateError", "the image is too large");
        }
        let mut ctx = Context2D::new(ow, oh);
        ctx.state.smoothing = true;
        if flip {
            ctx.state.transform = Matrix::new(1.0, 0.0, 0.0, -1.0, 0.0, oh as f32);
        }
        ctx.state.composite = Composite::Copy;
        ctx.draw_image(&src, sx as f32, sy as f32, sw as f32, sh as f32, 0.0, 0.0, ow as f32, oh as f32);
        Ok(bitmap_object(vm, &h, ctx.bitmap))
    })();
    match r {
        Ok(v) => vm.call(&resolve, Value::Undefined, vec![v])?,
        Err(e) => vm.call(&reject, Value::Undefined, vec![e])?,
    };
    Ok(Value::Object(p))
}

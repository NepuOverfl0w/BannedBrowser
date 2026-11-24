//! The Window global (HTML §7.2) and WindowOrWorkerGlobalScope (HTML §8.1.7): timers,
//! microtasks, animation frames, console, location, navigator, screen, performance,
//! base64. Window is a [Global] interface, so its members live on the global object itself
//! (WebIDL §3.7.6).

use bb_dom::NodeId;
use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::{realm, PropKey};
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::exception::throw;
use crate::host::*;
use crate::idl::{attr, constant, op};

type R = JsResult<Value>;

fn window_value(vm: &mut Vm) -> R {
    let h = host(vm)?;
    Ok(h.window.get().map_or(Value::Undefined, Value::Object))
}

pub fn setup_window(vm: &mut Vm, _proto: ObjRef) {
    let g = vm.realm.global;
    for name in ["window", "self", "frames", "top", "parent"] {
        attr(vm, g, name, |vm, _| window_value(vm), None);
    }
    attr(vm, g, "opener", |_, _| Ok(Value::Null), Some(|_, _| Ok(Value::Undefined)));
    attr(vm, g, "frameElement", |_, _| Ok(Value::Null), None);
    attr(vm, g, "closed", |_, _| Ok(Value::Bool(false)), None);
    attr(vm, g, "length", |_, _| Ok(Value::Number(0.0)), None);
    attr(
        vm,
        g,
        "name",
        |vm, _| {
            let h = host(vm)?;
            let n = h.misc_str("window.name");
            Ok(js(&n))
        },
        Some(|vm, c| {
            let v = rstr(vm, &c.arg(0))?;
            let h = host(vm)?;
            h.set_misc_str("window.name", &v);
            Ok(Value::Undefined)
        }),
    );
    attr(
        vm,
        g,
        "document",
        |vm, _| {
            let h = host(vm)?;
            wrap(vm, &h, NodeRef { doc: 0, id: bb_dom::Document::ROOT })
        },
        None,
    );
    attr(vm, g, "event", |_, _| Ok(Value::Undefined), Some(|_, _| Ok(Value::Undefined)));
    for (name, f) in [
        ("innerWidth", (|vm, _| viewport(vm, 0)) as bb_js::vm::heap::NativeFn),
        ("innerHeight", |vm, _| viewport(vm, 1)),
        ("outerWidth", |vm, _| viewport(vm, 0)),
        ("outerHeight", |vm, _| viewport(vm, 1)),
    ] {
        attr(vm, g, name, f, None);
    }
    for name in
        ["scrollX", "scrollY", "pageXOffset", "pageYOffset", "screenX", "screenY", "screenLeft", "screenTop"]
    {
        attr(vm, g, name, |_, _| Ok(Value::Number(0.0)), None);
    }
    attr(vm, g, "devicePixelRatio", |_, _| Ok(Value::Number(1.0)), None);
    op(vm, g, "postMessage", 1, crate::worker::window_post_message);
    op(vm, g, "requestAnimationFrame", 1, |vm, c| {
        let f = c.arg(0);
        if !vm.is_callable(&f) {
            return vm.type_error("requestAnimationFrame needs a function");
        }
        let h = host(vm)?;
        let id = h.next_timer.get();
        h.next_timer.set(id + 1);
        h.frame_callbacks.borrow_mut().push((id, f));
        Ok(Value::Number(id as f64))
    });
    op(vm, g, "cancelAnimationFrame", 1, |vm, c| {
        let id = vm.to_uint32(&c.arg(0))?;
        let h = host(vm)?;
        h.frame_callbacks.borrow_mut().retain(|x| x.0 != id);
        Ok(Value::Undefined)
    });
    op(vm, g, "alert", 0, |vm, c| {
        let m = if c.args.is_empty() { String::new() } else { rstr(vm, &c.arg(0))? };
        let h = host(vm)?;
        h.console.borrow_mut().push(format!("[alert] {m}"));
        Ok(Value::Undefined)
    });
    op(vm, g, "confirm", 0, |_, _| Ok(Value::Bool(false)));
    op(vm, g, "prompt", 0, |_, _| Ok(Value::Null));
    op(vm, g, "print", 0, |_, _| Ok(Value::Undefined));
    for name in [
        "focus", "blur", "stop", "close", "scroll", "scrollTo", "scrollBy", "moveTo", "moveBy", "resizeTo",
        "resizeBy",
    ] {
        op(vm, g, name, 0, |_, _| Ok(Value::Undefined));
    }
    install_factories(vm, g);
    crate::storage::install(vm, g);
    crate::events::setup_window_handlers(vm, g);
    op(vm, g, "getComputedStyle", 1, crate::css::get_computed_style);
    install_location(vm, g);
    install_navigator(vm, g);
    install_screen(vm, g);
    setup_global_scope(vm, g);
}

/// The WindowOrWorkerGlobalScope members (HTML §8.1.7): timers, microtasks, base64,
/// fetch, crypto, structuredClone, console and performance, on global `g`.
pub fn setup_global_scope(vm: &mut Vm, g: ObjRef) {
    attr(
        vm,
        g,
        "isSecureContext",
        |vm, _| {
            let h = host(vm)?;
            let s = h.url.borrow().scheme().to_string();
            Ok(Value::Bool(matches!(s.as_str(), "https" | "file" | "about")))
        },
        None,
    );
    attr(
        vm,
        g,
        "origin",
        |vm, _| {
            let h = host(vm)?;
            let o = origin_of(&h.url.borrow());
            Ok(js(&o))
        },
        None,
    );
    op(vm, g, "setTimeout", 1, |vm, c| set_timer(vm, &c, false));
    op(vm, g, "setInterval", 1, |vm, c| set_timer(vm, &c, true));
    for name in ["clearTimeout", "clearInterval"] {
        op(vm, g, name, 0, |vm, c| {
            let id = vm.to_int32(&c.arg(0))?;
            let h = host(vm)?;
            h.timers.borrow_mut().retain(|t| t.id as i32 != id);
            Ok(Value::Undefined)
        });
    }
    op(vm, g, "queueMicrotask", 1, |vm, c| {
        let f = c.arg(0);
        if !vm.is_callable(&f) {
            return vm.type_error("queueMicrotask needs a function");
        }
        vm.enqueue_microtask(f);
        Ok(Value::Undefined)
    });
    op(vm, g, "btoa", 1, |vm, c| {
        let s = vm.to_string(&c.arg(0))?;
        if s.iter().any(|&u| u > 0xFF) {
            return throw(vm, "InvalidCharacterError", "btoa: a character is outside Latin-1");
        }
        let bytes: Vec<u8> = s.iter().map(|&u| u as u8).collect();
        Ok(js(&base64_encode(&bytes)))
    });
    op(vm, g, "atob", 1, |vm, c| {
        let s = rstr(vm, &c.arg(0))?;
        match base64_decode(&s) {
            Some(b) => Ok(Value::String(b.into_iter().map(u16::from).collect::<Vec<u16>>().into())),
            None => throw(vm, "InvalidCharacterError", "atob: not valid base64"),
        }
    });
    op(vm, g, "fetch", 1, crate::net::fetch);
    op(vm, g, "structuredClone", 1, crate::worker::structured_clone);
    install_console(vm, g);
    install_performance(vm, g);
    if let Ok(h) = host(vm) {
        crate::blob::install_crypto(vm, &h);
    }
}

fn install_performance(vm: &mut Vm, g: ObjRef) {
    let perf = vm.new_object();
    op(vm, perf, "now", 0, |vm, _| {
        let h = host(vm)?;
        Ok(Value::Number(h.embedder.now_ms()))
    });
    let origin = host(vm).map(|h| h.embedder.time_origin_ms()).unwrap_or(0.0);
    constant(vm, perf, "timeOrigin", Value::Number(origin));
    for name in ["mark", "measure", "clearMarks", "clearMeasures"] {
        op(vm, perf, name, 0, |_, _| Ok(Value::Undefined));
    }
    for name in ["getEntries", "getEntriesByName", "getEntriesByType"] {
        op(vm, perf, name, 0, |vm, _| Ok(Value::Object(vm.new_array(Vec::new()))));
    }
    bb_js::vm::realm::to_string_tag(vm, perf, "Performance");
    bb_js::vm::realm::value(vm, g, "performance", Value::Object(perf));
}

/// A new HTML element of the window's document.
fn create_html(vm: &mut Vm, name: &str) -> JsResult<(std::rc::Rc<Host>, NodeRef)> {
    let h = host(vm)?;
    let id = with_doc_mut(vm, &h, 0, |d| d.create_html_element(name))?;
    Ok((h, NodeRef { doc: 0, id }))
}

/// The legacy factory functions Image (HTML §4.8.3), Option (§4.10.10) and Audio
/// (§4.8.11.1): constructors whose prototype is the element interface's.
fn install_factories(vm: &mut Vm, g: ObjRef) {
    let Ok(h) = host(vm) else { return };
    for (name, len, f, iface) in [
        (
            "Image",
            0,
            (|vm, c| {
                if c.new_target.is_none() {
                    return vm.type_error("Image requires 'new'");
                }
                let (w, ht) = (c.arg(0), c.arg(1));
                let w = if w.is_undefined() { None } else { Some(vm.to_uint32(&w)?) };
                let ht = if ht.is_undefined() { None } else { Some(vm.to_uint32(&ht)?) };
                let (h, n) = create_html(vm, "img")?;
                with_doc_mut(vm, &h, 0, |d| {
                    if let Some(w) = w {
                        d.set_attr(n.id, "width", &w.to_string());
                    }
                    if let Some(ht) = ht {
                        d.set_attr(n.id, "height", &ht.to_string());
                    }
                })?;
                wrap(vm, &h, n)
            }) as bb_js::vm::heap::NativeFn,
            C::HTMLImageElement,
        ),
        (
            "Option",
            0,
            |vm, c| {
                if c.new_target.is_none() {
                    return vm.type_error("Option requires 'new'");
                }
                let text = if c.arg(0).is_undefined() { String::new() } else { rstr(vm, &c.arg(0))? };
                let value = if c.arg(1).is_undefined() { None } else { Some(rstr(vm, &c.arg(1))?) };
                let default_selected = vm.to_boolean(&c.arg(2));
                let selected = vm.to_boolean(&c.arg(3));
                let (h, n) = create_html(vm, "option")?;
                with_doc_mut(vm, &h, 0, |d| {
                    if !text.is_empty() {
                        let t = d.create_text(&text);
                        d.append_child(n.id, t);
                    }
                    if let Some(v) = &value {
                        d.set_attr(n.id, "value", v);
                    }
                    if default_selected {
                        d.set_attr(n.id, "selected", "");
                    }
                })?;
                let o = wrap(vm, &h, n)?;
                if selected {
                    if let Value::Object(oo) = o {
                        vm.set(oo, PropKey::lit("selected"), Value::Bool(true), Value::Object(oo))?;
                    }
                }
                Ok(o)
            },
            C::HTMLOptionElement,
        ),
        (
            "Audio",
            0,
            |vm, c| {
                if c.new_target.is_none() {
                    return vm.type_error("Audio requires 'new'");
                }
                let src = if c.arg(0).is_undefined() { None } else { Some(rstr(vm, &c.arg(0))?) };
                let (h, n) = create_html(vm, "audio")?;
                with_doc_mut(vm, &h, 0, |d| {
                    d.set_attr(n.id, "preload", "auto");
                    if let Some(s) = &src {
                        d.set_attr(n.id, "src", s);
                    }
                })?;
                wrap(vm, &h, n)
            },
            C::HTMLAudioElement,
        ),
    ] {
        let ctor = bb_js::vm::realm::native_with(vm, name, len, f, true, Vec::new());
        let proto = h.proto(iface);
        vm.heap.obj_mut(ctor).props.insert(
            PropKey::lit("prototype"),
            bb_js::vm::heap::Prop::data(Value::Object(proto), false, false, false),
        );
        bb_js::vm::realm::value(vm, g, name, Value::Object(ctor));
    }
}

fn viewport(vm: &mut Vm, i: usize) -> R {
    let h = host(vm)?;
    let (w, ht) = h.embedder.viewport();
    Ok(Value::Number(if i == 0 { w } else { ht }))
}

/// The serialized origin of a URL (HTML §7.1.1).
pub fn origin_of(u: &bb_url::Url) -> String {
    u.origin().serialize()
}

/// setTimeout / setInterval (HTML §8.6 "timer initialization steps").
fn set_timer(vm: &mut Vm, c: &NativeCall, repeat: bool) -> R {
    let handler = c.arg(0);
    let handler = if vm.is_callable(&handler) { handler } else { js(&rstr(vm, &handler)?) };
    let timeout = vm.to_int32(&c.arg(1))?.max(0) as f64;
    let args = c.args.iter().skip(2).cloned().collect();
    let h = host(vm)?;
    let id = h.next_timer.get();
    h.next_timer.set(id + 1);
    let seq = h.timer_seq.get();
    h.timer_seq.set(seq + 1);
    let due = h.embedder.now_ms() + timeout;
    h.timers.borrow_mut().push(Timer {
        id,
        due,
        interval: repeat.then_some(timeout),
        callback: handler,
        args,
        seq,
    });
    Ok(Value::Number(id as f64))
}

// ---- console ----

fn console_line(vm: &mut Vm, c: &NativeCall, level: &str) -> R {
    let mut parts = Vec::with_capacity(c.args.len());
    for a in &c.args {
        parts.push(match a {
            Value::String(_) => rstr(vm, a)?,
            _ => vm.describe(a),
        });
    }
    let line = parts.join(" ");
    let h = host(vm)?;
    log::debug!("console.{level}: {line}");
    h.console.borrow_mut().push(if level == "log" { line } else { format!("[{level}] {line}") });
    Ok(Value::Undefined)
}

fn install_console(vm: &mut Vm, g: ObjRef) {
    let con = vm.new_object();
    for (name, f) in [
        ("log", (|vm, c| console_line(vm, &c, "log")) as bb_js::vm::heap::NativeFn),
        ("info", |vm, c| console_line(vm, &c, "info")),
        ("warn", |vm, c| console_line(vm, &c, "warn")),
        ("error", |vm, c| console_line(vm, &c, "error")),
        ("debug", |vm, c| console_line(vm, &c, "debug")),
        ("trace", |vm, c| console_line(vm, &c, "trace")),
        ("dir", |vm, c| console_line(vm, &c, "dir")),
        ("dirxml", |vm, c| console_line(vm, &c, "dir")),
        ("table", |vm, c| console_line(vm, &c, "table")),
        ("group", |vm, c| console_line(vm, &c, "group")),
        ("groupCollapsed", |vm, c| console_line(vm, &c, "group")),
        ("groupEnd", |_, _| Ok(Value::Undefined)),
        ("time", |_, _| Ok(Value::Undefined)),
        ("timeEnd", |_, _| Ok(Value::Undefined)),
        ("timeLog", |_, _| Ok(Value::Undefined)),
        ("count", |_, _| Ok(Value::Undefined)),
        ("countReset", |_, _| Ok(Value::Undefined)),
        ("clear", |_, _| Ok(Value::Undefined)),
        ("assert", |vm, c| {
            if vm.to_boolean(&c.arg(0)) {
                return Ok(Value::Undefined);
            }
            let rest = NativeCall {
                callee: c.callee,
                this: c.this.clone(),
                args: c.args.iter().skip(1).cloned().collect(),
                new_target: None,
            };
            console_line(vm, &rest, "assert")
        }),
    ] {
        bb_js::vm::realm::method(vm, con, name, 0, f);
    }
    bb_js::vm::realm::value(vm, g, "console", Value::Object(con));
}

// ---- location ----

fn url_part(vm: &mut Vm, part: &str) -> R {
    let h = host(vm)?;
    let u = h.url.borrow().clone();
    let s = match part {
        "href" => u.href(),
        "protocol" => u.protocol(),
        "host" => u.host_with_port(),
        "hostname" => u.hostname(),
        "port" => u.port().map(|p| p.to_string()).unwrap_or_default(),
        "pathname" => u.pathname(),
        "search" => u.search(),
        "hash" => u.hash(),
        "origin" => u.origin().serialize(),
        _ => String::new(),
    };
    Ok(js(&s))
}

/// Ask the embedder to navigate (HTML §7.4.2 "navigate"), resolving `to` first.
fn navigate(vm: &mut Vm, to: &str) -> R {
    let h = host(vm)?;
    let base = base_url(&h, 0);
    match bb_url::Url::parse_with_base(to, Some(&base)) {
        Ok(u) => {
            *h.pending_navigation.borrow_mut() = Some(u.href());
            Ok(Value::Undefined)
        }
        Err(_) => throw(vm, "SyntaxError", &format!("'{to}' is not a valid URL")),
    }
}

fn install_location(vm: &mut Vm, g: ObjRef) {
    let loc = vm.new_object();
    for part in ["protocol", "host", "hostname", "port", "pathname", "search", "hash", "origin"] {
        let f: bb_js::vm::heap::NativeFn = match part {
            "protocol" => |vm, _| url_part(vm, "protocol"),
            "host" => |vm, _| url_part(vm, "host"),
            "hostname" => |vm, _| url_part(vm, "hostname"),
            "port" => |vm, _| url_part(vm, "port"),
            "pathname" => |vm, _| url_part(vm, "pathname"),
            "search" => |vm, _| url_part(vm, "search"),
            "hash" => |vm, _| url_part(vm, "hash"),
            _ => |vm, _| url_part(vm, "origin"),
        };
        attr(vm, loc, part, f, None);
    }
    attr(
        vm,
        loc,
        "href",
        |vm, _| url_part(vm, "href"),
        Some(|vm, c| {
            let to = rstr(vm, &c.arg(0))?;
            navigate(vm, &to)
        }),
    );
    for name in ["assign", "replace"] {
        op(vm, loc, name, 1, |vm, c| {
            let to = rstr(vm, &c.arg(0))?;
            navigate(vm, &to)
        });
    }
    op(vm, loc, "reload", 0, |vm, _| {
        let h = host(vm)?;
        let u = h.url.borrow().href();
        *h.pending_navigation.borrow_mut() = Some(u);
        Ok(Value::Undefined)
    });
    op(vm, loc, "toString", 0, |vm, _| url_part(vm, "href"));
    attr(
        vm,
        g,
        "location",
        |vm, _| {
            let h = host(vm)?;
            let loc = h.misc.borrow().get("location").copied();
            Ok(loc.map_or(Value::Undefined, Value::Object))
        },
        Some(|vm, c| {
            let to = rstr(vm, &c.arg(0))?;
            navigate(vm, &to)
        }),
    );
    if let Ok(h) = host(vm) {
        h.misc.borrow_mut().insert("location", loc);
    }
    // document.location is the same object.
    let dp = host(vm).ok().map(|h| h.proto(crate::class::C::Document));
    if let Some(dp) = dp {
        attr(
            vm,
            dp,
            "location",
            |vm, c| {
                let h = host(vm)?;
                // Only a document with a browsing context has a location (HTML §7.10.1).
                if node_of(vm, &c.this).is_none_or(|n| n.doc != 0) {
                    return Ok(Value::Null);
                }
                let loc = h.misc.borrow().get("location").copied();
                Ok(loc.map_or(Value::Null, Value::Object))
            },
            Some(|vm, c| {
                let to = rstr(vm, &c.arg(0))?;
                navigate(vm, &to)
            }),
        );
    }
}

// ---- navigator, screen, performance ----

fn install_navigator(vm: &mut Vm, g: ObjRef) {
    let nav = vm.new_object();
    let ua = host(vm).map(|h| h.embedder.user_agent()).unwrap_or_default();
    let app_version = ua.strip_prefix("Mozilla/").unwrap_or(&ua).to_string();
    for (k, v) in [
        ("userAgent", ua.as_str()),
        ("appVersion", app_version.as_str()),
        ("appName", "Netscape"),
        ("appCodeName", "Mozilla"),
        ("product", "Gecko"),
        ("productSub", "20030107"),
        ("vendor", ""),
        ("vendorSub", ""),
        ("language", "en-US"),
        ("platform", platform()),
        ("oscpu", platform()),
        ("doNotTrack", "unspecified"),
    ] {
        constant(vm, nav, k, js(v));
    }
    let langs = vec![js("en-US"), js("en")];
    let langs = vm.new_array(langs);
    constant(vm, nav, "languages", Value::Object(langs));
    constant(vm, nav, "onLine", Value::Bool(true));
    constant(vm, nav, "cookieEnabled", Value::Bool(true));
    constant(vm, nav, "webdriver", Value::Bool(false));
    constant(vm, nav, "pdfViewerEnabled", Value::Bool(false));
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    constant(vm, nav, "hardwareConcurrency", Value::Number(cores as f64));
    constant(vm, nav, "maxTouchPoints", Value::Number(0.0));
    op(vm, nav, "javaEnabled", 0, |_, _| Ok(Value::Bool(false)));
    bb_js::vm::realm::to_string_tag(vm, nav, "Navigator");
    constant(vm, g, "navigator", Value::Object(nav));
    if let Some(p) = vm.heap.obj_mut(g).props.get_mut(&PropKey::lit("navigator")) {
        p.configurable = true;
    }
}

/// navigator.platform (HTML §8.10.1.1: the values browsers report).
pub fn platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "Win32"
    } else if cfg!(target_os = "macos") {
        "MacIntel"
    } else {
        "Linux x86_64"
    }
}

fn install_screen(vm: &mut Vm, g: ObjRef) {
    let scr = vm.new_object();
    let (w, h) = host(vm).map(|h| h.embedder.viewport()).unwrap_or((1024.0, 768.0));
    for (k, v) in [
        ("width", w),
        ("height", h),
        ("availWidth", w),
        ("availHeight", h),
        ("colorDepth", 24.0),
        ("pixelDepth", 24.0),
        ("availLeft", 0.0),
        ("availTop", 0.0),
    ] {
        constant(vm, scr, k, Value::Number(v));
    }
    bb_js::vm::realm::to_string_tag(vm, scr, "Screen");
    bb_js::vm::realm::value(vm, g, "screen", Value::Object(scr));
}

// ---- base64 (Infra §4.5 forgiving-base64) ----

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for ch in b.chunks(3) {
        let n =
            (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= ch.len() {
                s.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// Infra §4.5 "forgiving-base64 decode".
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut d: Vec<u8> = s.bytes().filter(|c| !matches!(c, b'\t' | b'\n' | b'\x0C' | b'\r' | b' ')).collect();
    if d.len() % 4 == 0 {
        if d.ends_with(b"==") {
            d.truncate(d.len() - 2);
        } else if d.ends_with(b"=") {
            d.truncate(d.len() - 1);
        }
    }
    if d.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(d.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in d {
        let v = B64.iter().position(|&x| x == c)? as u32;
        buf = buf << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ---- named access on the Window object (HTML §7.2.2.3) ----

/// The class of the WindowProperties object (WebIDL "named properties object"), which is
/// no interface of its own.
pub const WINDOW_PROPERTIES: u32 = 0xffff_0001;

/// Whether element `x` of `d` is a named object for `name` (HTML §7.2.2.3, without
/// navigables: embed, form, img and object by name; any HTML element by id).
fn is_named(d: &bb_dom::Document, x: NodeId, name: &str) -> bool {
    let Some(e) = d.element(x) else { return false };
    if e.ns != bb_dom::Namespace::Html {
        return false;
    }
    if matches!(e.name.as_str(), "embed" | "form" | "img" | "object") && e.attr("name") == Some(name) {
        return true;
    }
    e.attr("id") == Some(name)
}

/// The named objects for `name`, in tree order.
pub fn named_objects(h: &Host, name: &str) -> Vec<NodeId> {
    if name.is_empty() {
        return Vec::new();
    }
    let Ok(docs) = h.docs.try_borrow() else { return Vec::new() };
    let d = &docs[0];
    d.descendants(bb_dom::Document::ROOT).filter(|&x| is_named(d, x, name)).collect()
}

/// The Window's supported property names, in tree order without duplicates.
fn window_names(h: &Host) -> Vec<String> {
    let Ok(docs) = h.docs.try_borrow() else { return Vec::new() };
    let d = &docs[0];
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for x in d.descendants(bb_dom::Document::ROOT) {
        let Some(e) = d.element(x) else { continue };
        if e.ns != bb_dom::Namespace::Html {
            continue;
        }
        let mut push = |s: &str| {
            if !s.is_empty() && seen.insert(s.to_string()) {
                out.push(s.to_string());
            }
        };
        if matches!(e.name.as_str(), "embed" | "form" | "img" | "object") {
            if let Some(n) = e.attr("name") {
                push(n);
            }
        }
        if let Some(i) = e.attr("id") {
            push(i);
        }
    }
    out
}

/// The named property's value: the element, or an HTMLCollection when there are several.
fn named_value(vm: &mut Vm, h: &Host, name: &str) -> JsResult<Option<Value>> {
    let nodes = named_objects(h, name);
    match nodes.len() {
        0 => Ok(None),
        1 => wrap(vm, h, NodeRef { doc: 0, id: nodes[0] }).map(Some),
        _ => crate::collections::collection(vm, h, CollKind::WindowNamed(name.to_string()), None).map(Some),
    }
}

/// WebIDL named property visibility for the Window `w` (the named properties object's global).
fn visible(vm: &mut Vm, w: ObjRef, k: &PropKey) -> JsResult<bool> {
    if vm.heap.obj(w).props.get(k).is_some() {
        return Ok(false);
    }
    let mut p = vm.heap.obj(w).proto;
    while let Some(o) = p {
        let named = matches!(vm.host_data(&Value::Object(o)), Some((c, _)) if c == WINDOW_PROPERTIES);
        if !named && vm.heap.obj(o).props.get(k).is_some() {
            return Ok(false);
        }
        p = vm.heap.obj(o).proto;
    }
    Ok(true)
}

/// The named properties object's [[GetOwnProperty]] (WebIDL §3.7.4.1).
pub fn properties_get_own(vm: &mut Vm, k: &PropKey) -> JsResult<Option<bb_js::vm::object::PropDesc>> {
    let PropKey::Str(_) = k else { return Ok(None) };
    let h = host(vm)?;
    let Some(w) = h.window.get() else { return Ok(None) };
    let name = k.display();
    if !visible(vm, w, k)? {
        return Ok(None);
    }
    Ok(named_value(vm, &h, &name)?.map(|v| bb_js::vm::object::PropDesc::data(v, true, false, true)))
}

pub fn properties_keys(vm: &mut Vm) -> JsResult<Vec<PropKey>> {
    let h = host(vm)?;
    Ok(window_names(&h).iter().map(|n| PropKey::named(n)).collect())
}

/// Put the WindowProperties object between Window.prototype and EventTarget.prototype.
pub fn install_window_properties(vm: &mut Vm, h: &Host) {
    let wp = h.proto(C::Window);
    let et = h.proto(C::EventTarget);
    let o = vm.new_host_object(Some(et), WINDOW_PROPERTIES, 0, true);
    realm::to_string_tag(vm, o, "WindowProperties");
    vm.heap.obj_mut(wp).proto = Some(o);
    h.misc.borrow_mut().insert("WindowProperties", o);
}

//! String (§22.1). Methods taking regular expressions use their string forms until the
//! regexp engine lands.

use super::super::heap::*;
use super::super::realm::{self, method, method_sym};
use super::super::value::{is_js_space, js_str, JsString, PropKey, Value};
use super::super::{JsResult, Vm};
use super::arg_string;

/// thisStringValue / RequireObjectCoercible + ToString for String.prototype methods.
fn this_str(vm: &mut Vm, c: &NativeCall) -> JsResult<JsString> {
    if c.this.is_nullish() {
        return vm.type_error("String.prototype method called on null or undefined");
    }
    vm.to_string(&c.this)
}

fn this_string_value(vm: &mut Vm, v: &Value) -> JsResult<JsString> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Object(o) => match &vm.heap.obj(*o).kind {
            Kind::String(s) => Ok(s.clone()),
            _ => vm.type_error("not a String"),
        },
        _ => vm.type_error("not a String"),
    }
}

fn find(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return (from <= hay.len()).then_some(from);
    }
    if needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn rfind(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    let start = from.min(hay.len() - needle.len());
    (0..=start).rev().find(|&i| &hay[i..i + needle.len()] == needle)
}

fn clamp_pos(vm: &mut Vm, v: &Value, len: usize, default: f64) -> JsResult<usize> {
    let n = if v.is_undefined() { default } else { vm.to_integer_or_infinity(v)? };
    Ok(n.clamp(0.0, len as f64) as usize)
}

/// Case mapping over well-formed runs (lone surrogates pass through).
fn map_case(s: &[u16], upper: bool) -> JsString {
    let mut out: Vec<u16> = Vec::with_capacity(s.len());
    let mut run: Vec<u16> = Vec::new();
    let flush = |run: &mut Vec<u16>, out: &mut Vec<u16>| {
        if run.is_empty() {
            return;
        }
        let st = String::from_utf16_lossy(run);
        let m = if upper { st.to_uppercase() } else { st.to_lowercase() };
        out.extend(m.encode_utf16());
        run.clear();
    };
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if (0xD800..0xDC00).contains(&c) && s.get(i + 1).is_some_and(|d| (0xDC00..0xE000).contains(d)) {
            run.push(c);
            run.push(s[i + 1]);
            i += 2;
            continue;
        }
        if (0xD800..0xE000).contains(&c) {
            flush(&mut run, &mut out);
            out.push(c);
        } else {
            run.push(c);
        }
        i += 1;
    }
    flush(&mut run, &mut out);
    out.into()
}

fn trim(s: &[u16], start: bool, end: bool) -> JsString {
    let mut a = 0;
    let mut b = s.len();
    if start {
        while a < b && is_js_space(s[a]) {
            a += 1;
        }
    }
    if end {
        while b > a && is_js_space(s[b - 1]) {
            b -= 1;
        }
    }
    s[a..b].into()
}

/// GetSubstitution (§22.1.3.19.1) for string patterns (no captures).
fn substitution(matched: &[u16], s: &[u16], pos: usize, repl: &[u16]) -> Vec<u16> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < repl.len() {
        let c = repl[i];
        if c == b'$' as u16 && i + 1 < repl.len() {
            let n = repl[i + 1];
            match n {
                0x24 => {
                    out.push(b'$' as u16);
                    i += 2;
                    continue;
                }
                0x26 => {
                    out.extend_from_slice(matched);
                    i += 2;
                    continue;
                }
                0x60 => {
                    out.extend_from_slice(&s[..pos]);
                    i += 2;
                    continue;
                }
                0x27 => {
                    let tail = (pos + matched.len()).min(s.len());
                    out.extend_from_slice(&s[tail..]);
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn is_regexp(vm: &mut Vm, v: &Value) -> JsResult<bool> {
    super::regexp::is_regexp(vm, v)
}

fn replace(vm: &mut Vm, c: NativeCall, all: bool) -> JsResult<Value> {
    if c.this.is_nullish() {
        return vm.type_error("String.prototype.replace called on null or undefined");
    }
    let pat = c.arg(0);
    let repl = c.arg(1);
    if !pat.is_nullish() {
        if all && is_regexp(vm, &pat)? {
            let fk = PropKey::lit("flags");
            let flags = vm.get_value_key(&pat, &fk)?;
            if flags.is_nullish() {
                return vm.type_error("flags is null or undefined");
            }
            let f = vm.to_string(&flags)?;
            if !f.contains(&(b'g' as u16)) {
                return vm.type_error("replaceAll must be called with a global RegExp");
            }
        }
        let rk = PropKey::Sym(vm.realm.sym_replace.clone());
        if let Some(r) = vm.get_method(&pat, &rk)? {
            return vm.call(&r, pat, vec![c.this.clone(), repl]);
        }
    }
    let s = vm.to_string(&c.this)?;
    let search = vm.to_string(&pat)?;
    let functional = vm.is_callable(&repl);
    let repl_str = if functional { js_str("") } else { vm.to_string(&repl)? };
    let mut positions = Vec::new();
    let adv = search.len().max(1);
    let mut p = find(&s, &search, 0);
    while let Some(x) = p {
        positions.push(x);
        if !all {
            break;
        }
        p = find(&s, &search, x + adv);
    }
    let mut out: Vec<u16> = Vec::new();
    let mut end = 0;
    for pos in positions {
        out.extend_from_slice(&s[end..pos]);
        let r = if functional {
            let v = vm.call(
                &repl,
                Value::Undefined,
                vec![Value::String(search.clone()), Value::Number(pos as f64), Value::String(s.clone())],
            )?;
            vm.to_string(&v)?.to_vec()
        } else {
            substitution(&search, &s, pos, &repl_str)
        };
        out.extend(r);
        end = pos + search.len();
    }
    out.extend_from_slice(&s[end.min(s.len())..]);
    Ok(Value::String(out.into()))
}

pub(super) fn install(vm: &mut Vm) {
    let sp = vm.realm.string_proto;
    let ctor = realm::constructor(
        vm,
        "String",
        1,
        |vm, c| {
            let s = if c.args.is_empty() {
                js_str("")
            } else {
                match (&c.args[0], c.new_target) {
                    (Value::Symbol(sym), None) => {
                        return Ok(Value::String(super::symbol_descriptive_string(sym)))
                    }
                    (v, _) => vm.to_string(v)?,
                }
            };
            let Some(nt) = c.new_target else { return Ok(Value::String(s)) };
            let proto = vm.prototype_from_ctor(nt, |r| r.string_proto)?;
            let o = vm.heap.alloc(Object::new(Some(proto), Kind::String(s.clone())));
            vm.heap.obj_mut(o).props.insert(
                PropKey::lit("length"),
                Prop::data(Value::Number(s.len() as f64), false, false, false),
            );
            Ok(Value::Object(o))
        },
        sp,
    );
    method(vm, ctor, "fromCharCode", 1, |vm, c| {
        let mut out = Vec::with_capacity(c.args.len());
        for a in &c.args {
            out.push(vm.to_uint16(a)?);
        }
        Ok(Value::String(out.into()))
    });
    method(vm, ctor, "fromCodePoint", 1, |vm, c| {
        let mut out: Vec<u16> = Vec::new();
        for a in &c.args {
            let n = vm.to_number(a)?;
            if n.fract() != 0.0 || !(0.0..=1114111.0).contains(&n) || n.is_nan() {
                return vm.range_error("Invalid code point");
            }
            let cp = n as u32;
            if cp < 0x10000 {
                out.push(cp as u16);
            } else {
                let v = cp - 0x10000;
                out.push(0xD800 + (v >> 10) as u16);
                out.push(0xDC00 + (v & 0x3FF) as u16);
            }
        }
        Ok(Value::String(out.into()))
    });
    method(vm, ctor, "raw", 1, |vm, c| {
        let cooked = vm.to_object(&c.arg(0))?;
        let raw = vm.get(cooked, &PropKey::lit("raw"))?;
        let raw = vm.to_object(&raw)?;
        let len = vm.length_of_array_like(raw)?;
        let mut out: Vec<u16> = Vec::new();
        let mut i = 0.0;
        while i < len {
            let seg = vm.get(raw, &PropKey::from_f64(i))?;
            out.extend_from_slice(&vm.to_string(&seg)?);
            if i + 1.0 < len {
                if let Some(sub) = c.args.get(i as usize + 1) {
                    out.extend_from_slice(&vm.to_string(sub)?);
                }
            }
            i += 1.0;
        }
        Ok(Value::String(out.into()))
    });

    method(vm, sp, "toString", 0, |vm, c| Ok(Value::String(this_string_value(vm, &c.this)?)));
    method(vm, sp, "valueOf", 0, |vm, c| Ok(Value::String(this_string_value(vm, &c.this)?)));
    method(vm, sp, "charAt", 1, |vm, c| {
        let s = this_str(vm, &c)?;
        let p = vm.to_integer_or_infinity(&c.arg(0))?;
        Ok(Value::String(if p < 0.0 || p >= s.len() as f64 {
            js_str("")
        } else {
            vec![s[p as usize]].into()
        }))
    });
    method(vm, sp, "charCodeAt", 1, |vm, c| {
        let s = this_str(vm, &c)?;
        let p = vm.to_integer_or_infinity(&c.arg(0))?;
        Ok(Value::Number(if p < 0.0 || p >= s.len() as f64 { f64::NAN } else { s[p as usize] as f64 }))
    });
    method(vm, sp, "codePointAt", 1, |vm, c| {
        let s = this_str(vm, &c)?;
        let p = vm.to_integer_or_infinity(&c.arg(0))?;
        if p < 0.0 || p >= s.len() as f64 {
            return Ok(Value::Undefined);
        }
        let i = p as usize;
        let a = s[i];
        if (0xD800..0xDC00).contains(&a) {
            if let Some(&b) = s.get(i + 1) {
                if (0xDC00..0xE000).contains(&b) {
                    return Ok(Value::Number(
                        (0x10000 + ((a as u32 - 0xD800) << 10) + (b as u32 - 0xDC00)) as f64,
                    ));
                }
            }
        }
        Ok(Value::Number(a as f64))
    });
    method(vm, sp, "at", 1, |vm, c| {
        let s = this_str(vm, &c)?;
        let r = vm.to_integer_or_infinity(&c.arg(0))?;
        let k = if r >= 0.0 { r } else { s.len() as f64 + r };
        Ok(if k < 0.0 || k >= s.len() as f64 {
            Value::Undefined
        } else {
            Value::String(vec![s[k as usize]].into())
        })
    });
    method(vm, sp, "indexOf", 1, |vm, c| {
        let s = this_str(vm, &c)?;
        let search = arg_string(vm, &c, 0)?;
        let pos = clamp_pos(vm, &c.arg(1), s.len(), 0.0)?;
        Ok(Value::Number(find(&s, &search, pos).map_or(-1.0, |p| p as f64)))
    });
    method(vm, sp, "lastIndexOf", 1, |vm, c| {
        let s = this_str(vm, &c)?;
        let search = arg_string(vm, &c, 0)?;
        let n = vm.to_number(&c.arg(1))?;
        let pos = if n.is_nan() { f64::INFINITY } else { n.trunc() };
        let start = pos.clamp(0.0, s.len() as f64) as usize;
        Ok(Value::Number(rfind(&s, &search, start).map_or(-1.0, |p| p as f64)))
    });
    method(vm, sp, "includes", 1, |vm, c| {
        let s = this_str(vm, &c)?;
        if is_regexp(vm, &c.arg(0))? {
            return vm
                .type_error("First argument to String.prototype.includes must not be a regular expression");
        }
        let search = arg_string(vm, &c, 0)?;
        let pos = clamp_pos(vm, &c.arg(1), s.len(), 0.0)?;
        Ok(Value::Bool(find(&s, &search, pos).is_some()))
    });
    method(vm, sp, "startsWith", 1, |vm, c| {
        let s = this_str(vm, &c)?;
        if is_regexp(vm, &c.arg(0))? {
            return vm.type_error(
                "First argument to String.prototype.startsWith must not be a regular expression",
            );
        }
        let search = arg_string(vm, &c, 0)?;
        let pos = clamp_pos(vm, &c.arg(1), s.len(), 0.0)?;
        Ok(Value::Bool(s.len() >= pos + search.len() && s[pos..pos + search.len()] == search[..]))
    });
    method(vm, sp, "endsWith", 1, |vm, c| {
        let s = this_str(vm, &c)?;
        if is_regexp(vm, &c.arg(0))? {
            return vm
                .type_error("First argument to String.prototype.endsWith must not be a regular expression");
        }
        let search = arg_string(vm, &c, 0)?;
        let end = clamp_pos(vm, &c.arg(1), s.len(), s.len() as f64)?;
        Ok(Value::Bool(end >= search.len() && s[end - search.len()..end] == search[..]))
    });
    method(vm, sp, "slice", 2, |vm, c| {
        let s = this_str(vm, &c)?;
        let len = s.len() as f64;
        let rel = |vm: &mut Vm, v: &Value, d: f64| -> JsResult<f64> {
            if v.is_undefined() {
                return Ok(d);
            }
            let r = vm.to_integer_or_infinity(v)?;
            Ok(if r < 0.0 { (len + r).max(0.0) } else { r.min(len) })
        };
        let from = rel(vm, &c.arg(0), 0.0)?;
        let to = rel(vm, &c.arg(1), len)?;
        Ok(Value::String(if from >= to { js_str("") } else { s[from as usize..to as usize].into() }))
    });
    method(vm, sp, "substring", 2, |vm, c| {
        let s = this_str(vm, &c)?;
        let a = clamp_pos(vm, &c.arg(0), s.len(), 0.0)?;
        let b = clamp_pos(vm, &c.arg(1), s.len(), s.len() as f64)?;
        let (x, y) = (a.min(b), a.max(b));
        Ok(Value::String(s[x..y].into()))
    });
    method(vm, sp, "substr", 2, |vm, c| {
        let s = this_str(vm, &c)?;
        let len = s.len() as f64;
        let mut start = vm.to_integer_or_infinity(&c.arg(0))?;
        if start == f64::NEG_INFINITY {
            start = 0.0;
        } else if start < 0.0 {
            start = (len + start).max(0.0);
        } else {
            start = start.min(len);
        }
        let l = if c.arg(1).is_undefined() { len } else { vm.to_integer_or_infinity(&c.arg(1))? };
        let end = (start + l.clamp(0.0, len)).min(len);
        Ok(Value::String(if start >= end { js_str("") } else { s[start as usize..end as usize].into() }))
    });
    method(vm, sp, "toUpperCase", 0, |vm, c| Ok(Value::String(map_case(&this_str(vm, &c)?, true))));
    method(vm, sp, "toLowerCase", 0, |vm, c| Ok(Value::String(map_case(&this_str(vm, &c)?, false))));
    method(vm, sp, "toLocaleUpperCase", 0, |vm, c| Ok(Value::String(map_case(&this_str(vm, &c)?, true))));
    method(vm, sp, "toLocaleLowerCase", 0, |vm, c| Ok(Value::String(map_case(&this_str(vm, &c)?, false))));
    method(vm, sp, "trim", 0, |vm, c| Ok(Value::String(trim(&this_str(vm, &c)?, true, true))));
    let ts = method(vm, sp, "trimStart", 0, |vm, c| Ok(Value::String(trim(&this_str(vm, &c)?, true, false))));
    let te = method(vm, sp, "trimEnd", 0, |vm, c| Ok(Value::String(trim(&this_str(vm, &c)?, false, true))));
    // Annex B.2.2.15–16: the same function objects.
    realm::value(vm, sp, "trimLeft", Value::Object(ts));
    realm::value(vm, sp, "trimRight", Value::Object(te));
    for (name, at_start) in [("padStart", true), ("padEnd", false)] {
        let f: NativeFn = if at_start { |vm, c| pad(vm, c, true) } else { |vm, c| pad(vm, c, false) };
        let _ = at_start;
        method(vm, sp, name, 1, f);
    }
    method(vm, sp, "repeat", 1, |vm, c| {
        let s = this_str(vm, &c)?;
        let n = vm.to_integer_or_infinity(&c.arg(0))?;
        if !(0.0..f64::INFINITY).contains(&n) {
            return vm.range_error("Invalid count value");
        }
        if s.is_empty() || n == 0.0 {
            return Ok(Value::str(""));
        }
        if s.len() as f64 * n > super::super::value::MAX_STRING_LEN as f64 {
            return vm.range_error("Invalid string length");
        }
        Ok(Value::String(s.repeat(n as usize).into()))
    });
    method(vm, sp, "concat", 1, |vm, c| {
        let mut out = this_str(vm, &c)?.to_vec();
        for a in &c.args {
            out.extend_from_slice(&vm.to_string(a)?);
        }
        Ok(Value::String(out.into()))
    });
    method(vm, sp, "split", 2, |vm, c| {
        if c.this.is_nullish() {
            return vm.type_error("String.prototype.split called on null or undefined");
        }
        let sep = c.arg(0);
        if !sep.is_nullish() {
            let sk = PropKey::Sym(vm.realm.sym_split.clone());
            if let Some(f) = vm.get_method(&sep, &sk)? {
                return vm.call(&f, sep, vec![c.this.clone(), c.arg(1)]);
            }
        }
        let s = vm.to_string(&c.this)?;
        let lim = if c.arg(1).is_undefined() { u32::MAX } else { vm.to_uint32(&c.arg(1))? };
        let r = vm.to_string(&sep)?;
        if lim == 0 {
            return Ok(Value::Object(vm.new_array(Vec::new())));
        }
        if sep.is_undefined() {
            return Ok(Value::Object(vm.new_array(vec![Value::String(s)])));
        }
        let mut parts: Vec<Value> = Vec::new();
        if r.is_empty() {
            for &u in s.iter().take(lim as usize) {
                parts.push(Value::String(vec![u].into()));
            }
            return Ok(Value::Object(vm.new_array(parts)));
        }
        if s.is_empty() {
            return Ok(Value::Object(vm.new_array(vec![Value::String(s)])));
        }
        let mut p = 0;
        while let Some(q) = find(&s, &r, p) {
            parts.push(Value::String(s[p..q].into()));
            if parts.len() as u32 >= lim {
                return Ok(Value::Object(vm.new_array(parts)));
            }
            p = q + r.len();
        }
        parts.push(Value::String(s[p..].into()));
        Ok(Value::Object(vm.new_array(parts)))
    });
    method(vm, sp, "replace", 2, |vm, c| replace(vm, c, false));
    method(vm, sp, "replaceAll", 2, |vm, c| replace(vm, c, true));
    method(vm, sp, "localeCompare", 1, |vm, c| {
        let s = this_str(vm, &c)?;
        let t = arg_string(vm, &c, 0)?;
        Ok(Value::Number(match s[..].cmp(&t[..]) {
            std::cmp::Ordering::Less => -1.0,
            std::cmp::Ordering::Equal => 0.0,
            std::cmp::Ordering::Greater => 1.0,
        }))
    });
    method(vm, sp, "isWellFormed", 0, |vm, c| {
        let s = this_str(vm, &c)?;
        Ok(Value::Bool(String::from_utf16(&s).is_ok()))
    });
    method(vm, sp, "toWellFormed", 0, |vm, c| {
        let s = this_str(vm, &c)?;
        let out: Vec<u16> = char::decode_utf16(s.iter().copied())
            .flat_map(|r| {
                let ch = r.unwrap_or('\u{FFFD}');
                let mut b = [0u16; 2];
                ch.encode_utf16(&mut b).to_vec()
            })
            .collect();
        Ok(Value::String(out.into()))
    });
    method(vm, sp, "normalize", 0, |vm, c| {
        let s = this_str(vm, &c)?;
        let f = if c.arg(0).is_undefined() { js_str("NFC") } else { vm.to_string(&c.arg(0))? };
        let form = String::from_utf16_lossy(&f);
        if !["NFC", "NFD", "NFKC", "NFKD"].contains(&form.as_str()) {
            return vm.range_error("The normalization form should be one of NFC, NFD, NFKC, NFKD.");
        }
        if s.iter().all(|&u| u < 0x300) {
            return Ok(Value::String(s));
        }
        vm.syntax_error("String.prototype.normalize beyond Latin-1 is not supported yet")
    });
    let si = vm.realm.sym_iterator.clone();
    method_sym(vm, sp, si, "[Symbol.iterator]", 0, |vm, c| {
        let s = this_str(vm, &c)?;
        let p = vm.realm.string_iterator_proto;
        Ok(Value::Object(vm.heap.alloc(Object::new(Some(p), Kind::StringIter { s, pos: 0, done: false }))))
    });
    for (name, regexp_sym) in [("match", 0), ("matchAll", 1), ("search", 2)] {
        let f: NativeFn = match regexp_sym {
            0 => |vm, c| regexp_dispatch(vm, c, 0),
            1 => |vm, c| regexp_dispatch(vm, c, 1),
            _ => |vm, c| regexp_dispatch(vm, c, 2),
        };
        method(vm, sp, name, 1, f);
    }
    // Annex B.2.2 HTML methods.
    for (name, tag, attr) in [
        ("anchor", "a", "name"),
        ("big", "big", ""),
        ("blink", "blink", ""),
        ("bold", "b", ""),
        ("fixed", "tt", ""),
        ("fontcolor", "font", "color"),
        ("fontsize", "font", "size"),
        ("italics", "i", ""),
        ("link", "a", "href"),
        ("small", "small", ""),
        ("strike", "strike", ""),
        ("sub", "sub", ""),
        ("sup", "sup", ""),
    ] {
        let m = realm::native_with(
            vm,
            name,
            if attr.is_empty() { 0 } else { 1 },
            html_method,
            false,
            vec![Value::str(tag), Value::str(attr)],
        );
        realm::value(vm, sp, name, Value::Object(m));
    }
}

/// CreateHTML (§B.2.2.2.1).
fn html_method(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let s = this_str(vm, &c)?;
    let (tag, attr) = match &vm.heap.obj(c.callee).kind {
        Kind::Native(n) => (n.slots[0].clone(), n.slots[1].clone()),
        _ => return vm.type_error("bad HTML method"),
    };
    let (Value::String(tag), Value::String(attr)) = (tag, attr) else {
        return vm.type_error("bad HTML method");
    };
    let mut out: Vec<u16> = vec![b'<' as u16];
    out.extend_from_slice(&tag);
    if !attr.is_empty() {
        let v = vm.to_string(&c.arg(0))?;
        out.push(b' ' as u16);
        out.extend_from_slice(&attr);
        out.extend("=\"".encode_utf16());
        for &u in v.iter() {
            if u == b'"' as u16 {
                out.extend("&quot;".encode_utf16());
            } else {
                out.push(u);
            }
        }
        out.push(b'"' as u16);
    }
    out.push(b'>' as u16);
    out.extend_from_slice(&s);
    out.extend("</".encode_utf16());
    out.extend_from_slice(&tag);
    out.push(b'>' as u16);
    Ok(Value::String(out.into()))
}

/// String.prototype.match (0), matchAll (1), search (2) (§22.1.3.13, .14, .23).
fn regexp_dispatch(vm: &mut Vm, c: NativeCall, which: u8) -> JsResult<Value> {
    if c.this.is_nullish() {
        return vm.type_error("String.prototype method called on null or undefined");
    }
    let r = c.arg(0);
    let sym = match which {
        0 => vm.realm.sym_match.clone(),
        1 => vm.realm.sym_match_all.clone(),
        _ => vm.realm.sym_search.clone(),
    };
    if !r.is_nullish() {
        if which == 1 && is_regexp(vm, &r)? {
            let flags = vm.get_value_key(&r, &PropKey::lit("flags"))?;
            if flags.is_nullish() {
                return vm.type_error("String.prototype.matchAll called with a RegExp without flags");
            }
            let f = vm.to_string(&flags)?;
            if !f.contains(&(b'g' as u16)) {
                return vm.type_error("String.prototype.matchAll called with a non-global RegExp argument");
            }
        }
        if let Some(f) = vm.get_method(&r, &PropKey::Sym(sym.clone()))? {
            return vm.call(&f, r, vec![c.this.clone()]);
        }
    }
    let s = vm.to_string(&c.this)?;
    let p = if r.is_undefined() { js_str("") } else { vm.to_string(&r)? };
    let flags = js_str(if which == 1 { "g" } else { "" });
    let rx = vm.regexp_create(p, flags)?;
    let f = vm.get(rx, &PropKey::Sym(sym))?;
    vm.call(&f, Value::Object(rx), vec![Value::String(s)])
}

fn pad(vm: &mut Vm, c: NativeCall, at_start: bool) -> JsResult<Value> {
    let s = this_str(vm, &c)?;
    let max = vm.to_length(&c.arg(0))?;
    if max <= s.len() as f64 {
        return Ok(Value::String(s));
    }
    let filler = if c.arg(1).is_undefined() { js_str(" ") } else { vm.to_string(&c.arg(1))? };
    if filler.is_empty() {
        return Ok(Value::String(s));
    }
    if max > super::super::value::MAX_STRING_LEN as f64 {
        return vm.range_error("Invalid string length");
    }
    let fill_len = max as usize - s.len();
    let fill: Vec<u16> = filler.iter().copied().cycle().take(fill_len).collect();
    let out: Vec<u16> = if at_start {
        fill.into_iter().chain(s.iter().copied()).collect()
    } else {
        s.iter().copied().chain(fill).collect()
    };
    Ok(Value::String(out.into()))
}

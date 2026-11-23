//! RegExp (§22.2.4–9): construction, exec, the flag accessors, and the Symbol.match /
//! matchAll / replace / search / split protocol, over our engine in `vm/regex`.

use super::super::heap::*;
use super::super::realm::{self, accessor, method, method_sym};
use super::super::regex::{Flags, Regex};
use super::super::value::{js_str, to_rust, JsString, PropKey, Value};
use super::super::{JsResult, Vm};
use std::rc::Rc;

pub struct RegExpData {
    pub source: JsString,
    pub flags: JsString,
    pub matcher: Rc<Regex>,
    /// [[Realm]] and [[LegacyFeaturesEnabled]] (proposal-regexp-legacy-features).
    pub realm: u32,
    pub legacy: bool,
}

fn data(vm: &Vm, v: &Value) -> Option<(JsString, JsString, Rc<Regex>)> {
    let Value::Object(o) = v else { return None };
    match &vm.heap.obj(*o).kind {
        Kind::RegExp(d) => Some((d.source.clone(), d.flags.clone(), d.matcher.clone())),
        _ => None,
    }
}

fn has(flags: &[u16], c: char) -> bool {
    flags.contains(&(c as u16))
}

/// Parse and check flags and pattern (§22.2.3.3 RegExpInitialize steps 5–8).
fn compile(vm: &mut Vm, p: &JsString, f: &JsString) -> JsResult<Regex> {
    let fs = to_rust(f);
    let mut seen = String::new();
    for ch in fs.chars() {
        if !"dgimsuvy".contains(ch) || seen.contains(ch) {
            return vm.syntax_error(&format!("Invalid regular expression flags '{fs}'"));
        }
        seen.push(ch);
    }
    if seen.contains('u') && seen.contains('v') {
        return vm.syntax_error("Invalid regular expression flags");
    }
    let body = String::from_utf16_lossy(p);
    if let Err(e) = crate::regexp::validate(&body, seen.contains('u'), seen.contains('v')) {
        return vm.syntax_error(&format!("Invalid regular expression: /{body}/: {e}"));
    }
    let flags = Flags {
        ignore_case: seen.contains('i'),
        multiline: seen.contains('m'),
        dot_all: seen.contains('s'),
        unicode: seen.contains('u') || seen.contains('v'),
        sets: seen.contains('v'),
    };
    Ok(Regex::new(p, flags))
}

impl Vm {
    /// RegExpCreate (§22.2.3.1) for literals and internal uses.
    pub fn regexp_create(&mut self, source: JsString, flags: JsString) -> JsResult<ObjRef> {
        let proto = self.realm.regexp_proto;
        self.regexp_alloc_init(proto, source, flags, true)
    }

    /// RegExpAlloc + RegExpInitialize; `legacy` is SameValue(newTarget, %RegExp%).
    fn regexp_alloc_init(
        &mut self,
        proto: ObjRef,
        source: JsString,
        flags: JsString,
        legacy: bool,
    ) -> JsResult<ObjRef> {
        let m = compile(self, &source, &flags)?;
        let realm = self.cur_realm;
        let o = self.heap.alloc(Object::new(
            Some(proto),
            Kind::RegExp(Box::new(RegExpData { source, flags, matcher: Rc::new(m), realm, legacy })),
        ));
        self.heap
            .obj_mut(o)
            .props
            .insert(PropKey::lit("lastIndex"), Prop::data(Value::Number(0.0), true, false, false));
        Ok(o)
    }

    /// RegExpBuiltinExec's legacy step: UpdateLegacyRegExpStaticProperties when `r` is a
    /// plain RegExp of the current realm, else InvalidateLegacyRegExpStaticProperties.
    fn update_legacy_statics(
        &mut self,
        r: ObjRef,
        s: &JsString,
        start: usize,
        end: usize,
        caps: &[Option<(usize, usize)>],
    ) {
        let (realm, legacy) = match &self.heap.obj(r).kind {
            Kind::RegExp(d) => (d.realm, d.legacy),
            _ => return,
        };
        if realm != self.cur_realm {
            return;
        }
        if !legacy {
            self.realm.regexp_statics = None;
            return;
        }
        let cap = |c: &Option<(usize, usize)>| -> JsString {
            match c {
                Some((x, y)) => s[*x..*y].into(),
                None => js_str(""),
            }
        };
        let last_paren = if caps.len() > 1 { cap(&caps[caps.len() - 1]) } else { js_str("") };
        let mut st: [JsString; 14] = std::array::from_fn(|_| js_str(""));
        st[0] = s.clone();
        st[1] = s[start..end].into();
        st[2] = last_paren;
        st[3] = s[..start].into();
        st[4] = s[end..].into();
        for i in 1..=9 {
            if let Some(c) = caps.get(i) {
                st[4 + i] = cap(c);
            }
        }
        self.realm.regexp_statics = Some(Box::new(st));
    }

    /// RegExpBuiltinExec (§22.2.7.2).
    fn regexp_builtin_exec(&mut self, r: ObjRef, s: &JsString) -> JsResult<Value> {
        let (_, flags, m) = data(self, &Value::Object(r)).expect("regexp");
        let li = self.get(r, &PropKey::lit("lastIndex"))?;
        let mut last = self.to_length(&li)?;
        let global = has(&flags, 'g');
        let sticky = has(&flags, 'y');
        let indices = has(&flags, 'd');
        let full = has(&flags, 'u') || has(&flags, 'v');
        if !global && !sticky {
            last = 0.0;
        }
        let caps = loop {
            if last > s.len() as f64 {
                if global || sticky {
                    self.set_or_throw(r, PropKey::lit("lastIndex"), Value::Number(0.0))?;
                }
                return Ok(Value::Null);
            }
            let mut budget = self.fuel.unwrap_or(u64::MAX / 2).max(1);
            let before = budget;
            let res = m.match_at(s, last as usize, &mut budget);
            if let Some(f) = &mut self.fuel {
                *f = f.saturating_sub(before - budget);
            }
            match res {
                Err(_) => {
                    self.terminated = true;
                    return Err(Value::str("execution terminated: regular expression ran out of steps"));
                }
                Ok(Some(c)) => break c,
                Ok(None) => {
                    if sticky {
                        self.set_or_throw(r, PropKey::lit("lastIndex"), Value::Number(0.0))?;
                        return Ok(Value::Null);
                    }
                    last = advance(s, last, full);
                }
            }
        };
        let (start, end) = caps[0].unwrap_or((last as usize, last as usize));
        if global || sticky {
            self.set_or_throw(r, PropKey::lit("lastIndex"), Value::Number(end as f64))?;
        }
        self.update_legacy_statics(r, s, start, end, &caps);
        let a = self.new_array(Vec::new());
        self.create_data_property_or_throw(a, PropKey::lit("index"), Value::Number(start as f64))?;
        self.create_data_property_or_throw(a, PropKey::lit("input"), Value::String(s.clone()))?;
        self.create_data_property_or_throw(a, PropKey::Index(0), Value::String(s[start..end].into()))?;
        let groups = if m.names.is_empty() { None } else { Some(self.new_object_with_proto(None)) };
        self.create_data_property_or_throw(
            a,
            PropKey::lit("groups"),
            groups.map(Value::Object).unwrap_or_default(),
        )?;
        let cap_val = |c: &Option<(usize, usize)>| match c {
            Some((x, y)) => Value::String(s[*x..*y].into()),
            None => Value::Undefined,
        };
        for (i, c) in caps.iter().enumerate().skip(1) {
            self.create_data_property_or_throw(a, PropKey::from_u32(i as u32), cap_val(c))?;
        }
        if let Some(g) = groups {
            // In pattern order; with duplicate names the participating group wins.
            let mut done: Vec<Vec<u16>> = Vec::new();
            for (name, idx) in &m.names {
                if done.contains(name) {
                    if caps[*idx].is_some() {
                        self.create_data_property_or_throw(
                            g,
                            PropKey::from_js(name.as_slice().into()),
                            cap_val(&caps[*idx]),
                        )?;
                    }
                    continue;
                }
                done.push(name.clone());
                self.create_data_property_or_throw(
                    g,
                    PropKey::from_js(name.as_slice().into()),
                    cap_val(&caps[*idx]),
                )?;
            }
        }
        if indices {
            // MakeMatchIndicesIndexPairArray (§22.2.7.8).
            let pairs: Vec<Value> = caps
                .iter()
                .map(|c| match c {
                    Some((x, y)) => Value::Object(
                        self.new_array(vec![Value::Number(*x as f64), Value::Number(*y as f64)]),
                    ),
                    None => Value::Undefined,
                })
                .collect();
            let arr = self.new_array(pairs.clone());
            let ig = if m.names.is_empty() {
                Value::Undefined
            } else {
                Value::Object(self.new_object_with_proto(None))
            };
            if let Value::Object(igo) = ig {
                let mut done: Vec<Vec<u16>> = Vec::new();
                for (name, idx) in &m.names {
                    if done.contains(name) && caps[*idx].is_none() {
                        continue;
                    }
                    done.push(name.clone());
                    self.create_data_property_or_throw(
                        igo,
                        PropKey::from_js(name.as_slice().into()),
                        pairs[*idx].clone(),
                    )?;
                }
            }
            self.create_data_property_or_throw(arr, PropKey::lit("groups"), ig)?;
            self.create_data_property_or_throw(a, PropKey::lit("indices"), Value::Object(arr))?;
        }
        Ok(Value::Object(a))
    }

    /// RegExpExec (§22.2.7.1).
    pub fn regexp_exec(&mut self, r: ObjRef, s: &JsString) -> JsResult<Value> {
        let exec = self.get(r, &PropKey::lit("exec"))?;
        if self.is_callable(&exec) {
            let res = self.call(&exec, Value::Object(r), vec![Value::String(s.clone())])?;
            if !matches!(res, Value::Object(_) | Value::Null) {
                return self.type_error("exec result must be an object or null");
            }
            return Ok(res);
        }
        if data(self, &Value::Object(r)).is_none() {
            return self.type_error("RegExp exec method called on an incompatible receiver");
        }
        self.regexp_builtin_exec(r, s)
    }
}

/// AdvanceStringIndex (§22.2.7.3).
fn advance(s: &[u16], i: f64, unicode: bool) -> f64 {
    if !unicode || i + 1.0 >= s.len() as f64 {
        return i + 1.0;
    }
    let k = i as usize;
    if (0xD800..0xDC00).contains(&s[k]) && (0xDC00..0xE000).contains(&s[k + 1]) {
        i + 2.0
    } else {
        i + 1.0
    }
}

fn this_obj(vm: &mut Vm, v: &Value) -> JsResult<ObjRef> {
    match v {
        Value::Object(o) => Ok(*o),
        _ => vm.type_error("RegExp method called on a non-object"),
    }
}

/// IsRegExp (§7.2.6).
pub(super) fn is_regexp(vm: &mut Vm, v: &Value) -> JsResult<bool> {
    let Value::Object(o) = v else { return Ok(false) };
    let mk = PropKey::Sym(vm.realm.sym_match.clone());
    let m = vm.get(*o, &mk)?;
    if !m.is_undefined() {
        return Ok(vm.to_boolean(&m));
    }
    Ok(matches!(vm.heap.obj(*o).kind, Kind::RegExp(_)))
}

/// The RegExp constructor (§22.2.4.1).
fn ctor(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let pattern = c.arg(0);
    let flags = c.arg(1);
    let pattern_is_regexp = is_regexp(vm, &pattern)?;
    let nt = match c.new_target {
        Some(nt) => nt,
        None => {
            let nt = c.callee;
            if pattern_is_regexp && flags.is_undefined() {
                let pc = vm.get_value_key(&pattern, &PropKey::lit("constructor"))?;
                if matches!(pc, Value::Object(p) if p == nt) {
                    return Ok(pattern);
                }
            }
            nt
        }
    };
    let (p, f) = if let Some((src, fl, _)) = data(vm, &pattern) {
        (Value::String(src), if flags.is_undefined() { Value::String(fl) } else { flags })
    } else if pattern_is_regexp {
        let src = vm.get_value_key(&pattern, &PropKey::lit("source"))?;
        let fl =
            if flags.is_undefined() { vm.get_value_key(&pattern, &PropKey::lit("flags"))? } else { flags };
        (src, fl)
    } else {
        (pattern, flags)
    };
    let proto = vm.prototype_from_ctor(nt, |r| r.regexp_proto)?;
    let ps = if p.is_undefined() { js_str("") } else { vm.to_string(&p)? };
    let fs = if f.is_undefined() { js_str("") } else { vm.to_string(&f)? };
    let legacy = nt == vm.realm.regexp_ctor;
    let o = vm.regexp_alloc_init(proto, ps, fs, legacy)?;
    Ok(Value::Object(o))
}

/// EscapeRegExpPattern (§22.2.6.13.1).
fn escape_source(s: &[u16]) -> JsString {
    if s.is_empty() {
        return js_str("(?:)");
    }
    let mut out = Vec::with_capacity(s.len());
    let mut in_class = false;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        match c {
            0x5C => {
                out.push(c);
                if let Some(&n) = s.get(i + 1) {
                    out.push(n);
                    i += 1;
                }
            }
            0x5B => {
                in_class = true;
                out.push(c);
            }
            0x5D => {
                in_class = false;
                out.push(c);
            }
            0x2F if !in_class => out.extend("\\/".encode_utf16()),
            0x0A => out.extend("\\n".encode_utf16()),
            0x0D => out.extend("\\r".encode_utf16()),
            0x2028 => out.extend("\\u2028".encode_utf16()),
            0x2029 => out.extend("\\u2029".encode_utf16()),
            _ => out.push(c),
        }
        i += 1;
    }
    out.into()
}

/// GetSubstitution (§22.1.3.19.1).
pub(super) fn substitution(
    vm: &mut Vm,
    matched: &[u16],
    s: &[u16],
    pos: usize,
    caps: &[Value],
    named: &Value,
    repl: &[u16],
) -> JsResult<Vec<u16>> {
    let m = caps.len();
    let mut out = Vec::new();
    let mut i = 0;
    let tail_pos = (pos + matched.len()).min(s.len());
    while i < repl.len() {
        let c = repl[i];
        if c != 0x24 || i + 1 >= repl.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let n = repl[i + 1];
        match n {
            0x24 => {
                out.push(0x24);
                i += 2;
            }
            0x26 => {
                out.extend_from_slice(matched);
                i += 2;
            }
            0x60 => {
                out.extend_from_slice(&s[..pos.min(s.len())]);
                i += 2;
            }
            0x27 => {
                out.extend_from_slice(&s[tail_pos..]);
                i += 2;
            }
            0x30..=0x39 => {
                let d1 = (n - 0x30) as usize;
                let two = repl
                    .get(i + 2)
                    .filter(|&&d| (0x30..=0x39).contains(&d))
                    .map(|&d| d1 * 10 + (d - 0x30) as usize);
                let (idx, len) = match two {
                    Some(k) if (1..=m).contains(&k) => (k, 3),
                    _ => (d1, 2),
                };
                if idx >= 1 && idx <= m {
                    if let Value::String(v) = &caps[idx - 1] {
                        out.extend_from_slice(v);
                    } else if !caps[idx - 1].is_undefined() {
                        let v = vm.to_string(&caps[idx - 1])?;
                        out.extend_from_slice(&v);
                    }
                    i += len;
                } else {
                    out.push(0x24);
                    i += 1;
                }
            }
            0x3C => {
                if named.is_undefined() {
                    out.push(0x24);
                    i += 1;
                    continue;
                }
                match repl[i + 2..].iter().position(|&x| x == 0x3E) {
                    None => {
                        out.push(0x24);
                        i += 1;
                    }
                    Some(k) => {
                        let name: JsString = repl[i + 2..i + 2 + k].into();
                        let v = vm.get_value_key(named, &PropKey::from_js(name))?;
                        if !v.is_undefined() {
                            out.extend_from_slice(&vm.to_string(&v)?);
                        }
                        i += k + 3;
                    }
                }
            }
            _ => {
                out.push(0x24);
                i += 1;
            }
        }
    }
    Ok(out)
}

fn flag_getter(vm: &mut Vm, c: &NativeCall, flag: char) -> JsResult<Value> {
    let Value::Object(o) = c.this else { return vm.type_error("RegExp flag getter called on a non-object") };
    match &vm.heap.obj(o).kind {
        Kind::RegExp(d) => Ok(Value::Bool(has(&d.flags, flag))),
        _ if o == vm.realm.regexp_proto => Ok(Value::Undefined),
        _ => vm.type_error("RegExp flag getter called on an incompatible receiver"),
    }
}

fn get_string(vm: &mut Vm, o: ObjRef, k: &str) -> JsResult<JsString> {
    let v = vm.get(o, &PropKey::named(k))?;
    vm.to_string(&v)
}

fn set_last_index(vm: &mut Vm, o: ObjRef, n: f64) -> JsResult<()> {
    vm.set_or_throw(o, PropKey::lit("lastIndex"), Value::Number(n))
}

fn match_string(vm: &mut Vm, res: &Value) -> JsResult<JsString> {
    let v = vm.get_value_key(res, &PropKey::Index(0))?;
    vm.to_string(&v)
}

/// RegExp.prototype[@@replace] (§22.2.6.11).
fn symbol_replace(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let rx = this_obj(vm, &c.this)?;
    let s = vm.to_string(&c.arg(0))?;
    let rv = c.arg(1);
    let functional = vm.is_callable(&rv);
    let repl_str = if functional { js_str("") } else { vm.to_string(&rv)? };
    let flags = get_string(vm, rx, "flags")?;
    let global = has(&flags, 'g');
    let full = has(&flags, 'u') || has(&flags, 'v');
    if global {
        set_last_index(vm, rx, 0.0)?;
    }
    let mut results = Vec::new();
    loop {
        let r = vm.regexp_exec(rx, &s)?;
        if matches!(r, Value::Null) {
            break;
        }
        results.push(r.clone());
        if !global {
            break;
        }
        let m = match_string(vm, &r)?;
        if m.is_empty() {
            let li = vm.get(rx, &PropKey::lit("lastIndex"))?;
            let li = vm.to_length(&li)?;
            set_last_index(vm, rx, advance(&s, li, full))?;
        }
    }
    let mut out: Vec<u16> = Vec::new();
    let mut next_pos = 0usize;
    for r in results {
        let Value::Object(ro) = r else { continue };
        let n = vm.length_of_array_like(ro)?;
        let ncaps = (n - 1.0).max(0.0) as usize;
        let matched = match_string(vm, &r)?;
        let pv = vm.get(ro, &PropKey::lit("index"))?;
        let pos = vm.to_integer_or_infinity(&pv)?.clamp(0.0, s.len() as f64) as usize;
        let mut caps = Vec::with_capacity(ncaps);
        for k in 1..=ncaps {
            let cv = vm.get(ro, &PropKey::from_u32(k as u32))?;
            caps.push(if cv.is_undefined() { cv } else { Value::String(vm.to_string(&cv)?) });
        }
        let named = vm.get(ro, &PropKey::lit("groups"))?;
        let replacement: Vec<u16> = if functional {
            let mut args = vec![Value::String(matched.clone())];
            args.extend(caps.iter().cloned());
            args.push(Value::Number(pos as f64));
            args.push(Value::String(s.clone()));
            if !named.is_undefined() {
                args.push(named.clone());
            }
            let v = vm.call(&rv, Value::Undefined, args)?;
            vm.to_string(&v)?.to_vec()
        } else {
            let named_obj = if named.is_undefined() { named } else { Value::Object(vm.to_object(&named)?) };
            substitution(vm, &matched, &s, pos, &caps, &named_obj, &repl_str)?
        };
        if pos >= next_pos {
            out.extend_from_slice(&s[next_pos..pos]);
            out.extend(replacement);
            next_pos = pos + matched.len();
        }
    }
    if next_pos < s.len() {
        out.extend_from_slice(&s[next_pos..]);
    }
    Ok(Value::String(out.into()))
}

/// RegExp.prototype[@@split] (§22.2.6.14).
fn symbol_split(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    let rx = this_obj(vm, &c.this)?;
    let s = vm.to_string(&c.arg(0))?;
    let rc = vm.realm.regexp_ctor;
    let ctor = vm.species_constructor(rx, rc)?;
    let flags = get_string(vm, rx, "flags")?;
    let full = has(&flags, 'u') || has(&flags, 'v');
    let mut nf = flags.to_vec();
    if !has(&flags, 'y') {
        nf.push(b'y' as u16);
    }
    let splitter = vm.construct(
        ctor.as_object().expect("constructor"),
        vec![Value::Object(rx), Value::String(nf.into())],
        None,
    )?;
    let Value::Object(sp) = splitter else { return vm.type_error("splitter is not an object") };
    let lim = if c.arg(1).is_undefined() { u32::MAX } else { vm.to_uint32(&c.arg(1))? };
    let mut out: Vec<Value> = Vec::new();
    if lim == 0 {
        return Ok(Value::Object(vm.new_array(out)));
    }
    if s.is_empty() {
        let z = vm.regexp_exec(sp, &s)?;
        if matches!(z, Value::Null) {
            out.push(Value::String(s));
        }
        return Ok(Value::Object(vm.new_array(out)));
    }
    let size = s.len();
    let (mut p, mut q) = (0usize, 0usize);
    while q < size {
        set_last_index(vm, sp, q as f64)?;
        let z = vm.regexp_exec(sp, &s)?;
        if matches!(z, Value::Null) {
            q = advance(&s, q as f64, full) as usize;
            continue;
        }
        let li = vm.get(sp, &PropKey::lit("lastIndex"))?;
        let e = (vm.to_length(&li)? as usize).min(size);
        if e == p {
            q = advance(&s, q as f64, full) as usize;
            continue;
        }
        out.push(Value::String(s[p..q].into()));
        if out.len() as u32 == lim {
            return Ok(Value::Object(vm.new_array(out)));
        }
        p = e;
        let Value::Object(zo) = z else { break };
        let ncaps = (vm.length_of_array_like(zo)? - 1.0).max(0.0) as u32;
        for i in 1..=ncaps {
            out.push(vm.get(zo, &PropKey::from_u32(i))?);
            if out.len() as u32 == lim {
                return Ok(Value::Object(vm.new_array(out)));
            }
        }
        q = p;
    }
    out.push(Value::String(s[p..].into()));
    Ok(Value::Object(vm.new_array(out)))
}

pub(super) fn install(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let rp = vm.new_object_with_proto(Some(op));
    vm.realm.regexp_proto = rp;
    let rc = realm::constructor(vm, "RegExp", 2, ctor, rp);
    vm.realm.regexp_ctor = rc;
    legacy_accessors(vm, rc);
    let species = PropKey::Sym(vm.realm.sym_species.clone());
    accessor(vm, rc, species, Some(|_, c| Ok(c.this)), None);
    method(vm, rc, "escape", 1, |vm, c| {
        // RegExp.escape (ES2025 §22.2.5.1).
        let Value::String(s) = c.arg(0) else { return vm.type_error("RegExp.escape requires a string") };
        let mut out: Vec<u16> = Vec::new();
        for (i, ch) in char::decode_utf16(s.iter().copied()).enumerate() {
            let cp = match ch {
                Ok(c) => c as u32,
                Err(e) => e.unpaired_surrogate() as u32,
            };
            let c8 = char::from_u32(cp);
            if i == 0 && c8.is_some_and(|c| c.is_ascii_alphanumeric()) {
                out.extend(format!("\\x{cp:02x}").encode_utf16());
                continue;
            }
            match c8 {
                Some(c) if "^$\\.*+?()[]{}|/".contains(c) => {
                    out.push(b'\\' as u16);
                    out.push(cp as u16);
                }
                Some(c) if ",-=<>#&!%:;@~'`\"".contains(c) => {
                    out.extend(format!("\\x{cp:02x}").encode_utf16())
                }
                Some('\t') => out.extend("\\t".encode_utf16()),
                Some('\n') => out.extend("\\n".encode_utf16()),
                Some('\u{b}') => out.extend("\\v".encode_utf16()),
                Some('\u{c}') => out.extend("\\f".encode_utf16()),
                Some('\r') => out.extend("\\r".encode_utf16()),
                _ if super::super::value::is_js_space(cp as u16) && cp <= 0xFFFF
                    || (0xD800..0xE000).contains(&cp) =>
                {
                    if cp <= 0xFF {
                        out.extend(format!("\\x{cp:02x}").encode_utf16());
                    } else {
                        out.extend(format!("\\u{cp:04x}").encode_utf16());
                    }
                }
                _ => {
                    let mut b = [0u16; 2];
                    out.extend_from_slice(
                        char::from_u32(cp).map(|c| c.encode_utf16(&mut b) as &[u16]).unwrap_or(&[]),
                    );
                }
            }
        }
        Ok(Value::String(out.into()))
    });
    method(vm, rp, "exec", 1, |vm, c| {
        let Value::Object(o) = c.this else {
            return vm.type_error("RegExp.prototype.exec called on a non-object");
        };
        if data(vm, &c.this).is_none() {
            return vm.type_error("RegExp.prototype.exec called on an incompatible receiver");
        }
        let s = vm.to_string(&c.arg(0))?;
        vm.regexp_builtin_exec(o, &s)
    });
    method(vm, rp, "test", 1, |vm, c| {
        let o = this_obj(vm, &c.this)?;
        let s = vm.to_string(&c.arg(0))?;
        Ok(Value::Bool(!matches!(vm.regexp_exec(o, &s)?, Value::Null)))
    });
    method(vm, rp, "toString", 0, |vm, c| {
        let o = this_obj(vm, &c.this)?;
        let s = get_string(vm, o, "source")?;
        let f = get_string(vm, o, "flags")?;
        let mut v: Vec<u16> = vec![b'/' as u16];
        v.extend_from_slice(&s);
        v.push(b'/' as u16);
        v.extend_from_slice(&f);
        Ok(Value::String(v.into()))
    });
    method(vm, rp, "compile", 2, |vm, c| {
        // Annex B.2.4.1.
        let Value::Object(o) = c.this else { return vm.type_error("not a RegExp") };
        let (realm, legacy) = match &vm.heap.obj(o).kind {
            Kind::RegExp(d) => (d.realm, d.legacy),
            _ => return vm.type_error("not a RegExp"),
        };
        // proposal-regexp-legacy-features: only plain RegExps of this realm.
        if realm != vm.cur_realm || !legacy {
            return vm.type_error("RegExp.prototype.compile called on a subclass or cross-realm RegExp");
        }
        let (p, f) = match data(vm, &c.arg(0)) {
            Some((src, fl, _)) => {
                if !c.arg(1).is_undefined() {
                    return vm.type_error("flags must be undefined when the pattern is a RegExp");
                }
                (src, fl)
            }
            None => {
                let p = if c.arg(0).is_undefined() { js_str("") } else { vm.to_string(&c.arg(0))? };
                let f = if c.arg(1).is_undefined() { js_str("") } else { vm.to_string(&c.arg(1))? };
                (p, f)
            }
        };
        let m = compile(vm, &p, &f)?;
        if let Kind::RegExp(d) = &mut vm.heap.obj_mut(o).kind {
            d.source = p;
            d.flags = f;
            d.matcher = Rc::new(m);
        }
        set_last_index(vm, o, 0.0)?;
        Ok(Value::Object(o))
    });
    accessor(
        vm,
        rp,
        PropKey::lit("source"),
        Some(|vm, c| {
            let Value::Object(o) = c.this else {
                return vm.type_error("RegExp.prototype.source getter called on a non-object");
            };
            match data(vm, &c.this) {
                Some((src, ..)) => Ok(Value::String(escape_source(&src))),
                None if o == vm.realm.regexp_proto => Ok(Value::str("(?:)")),
                None => vm.type_error("RegExp.prototype.source getter called on an incompatible receiver"),
            }
        }),
        None,
    );
    accessor(
        vm,
        rp,
        PropKey::lit("flags"),
        Some(|vm, c| {
            let o = this_obj(vm, &c.this)?;
            let mut out = String::new();
            for (name, ch) in [
                ("hasIndices", 'd'),
                ("global", 'g'),
                ("ignoreCase", 'i'),
                ("multiline", 'm'),
                ("dotAll", 's'),
                ("unicode", 'u'),
                ("unicodeSets", 'v'),
                ("sticky", 'y'),
            ] {
                let cond = vm.get(o, &PropKey::named(name))?;
                if vm.to_boolean(&cond) {
                    out.push(ch);
                }
            }
            Ok(Value::str(&out))
        }),
        None,
    );
    macro_rules! flags {
        ($($name:literal => $ch:literal),* $(,)?) => {
            $(accessor(vm, rp, PropKey::named($name), Some(|vm, c| flag_getter(vm, &c, $ch)), None);)*
        };
    }
    flags!("hasIndices" => 'd', "global" => 'g', "ignoreCase" => 'i', "multiline" => 'm', "dotAll" => 's', "unicode" => 'u', "unicodeSets" => 'v', "sticky" => 'y');

    let sm = vm.realm.sym_match.clone();
    method_sym(vm, rp, sm, "[Symbol.match]", 1, |vm, c| {
        let rx = this_obj(vm, &c.this)?;
        let s = vm.to_string(&c.arg(0))?;
        let flags = get_string(vm, rx, "flags")?;
        if !has(&flags, 'g') {
            return vm.regexp_exec(rx, &s);
        }
        let full = has(&flags, 'u') || has(&flags, 'v');
        set_last_index(vm, rx, 0.0)?;
        let mut out = Vec::new();
        loop {
            let r = vm.regexp_exec(rx, &s)?;
            if matches!(r, Value::Null) {
                return Ok(if out.is_empty() { Value::Null } else { Value::Object(vm.new_array(out)) });
            }
            let m = match_string(vm, &r)?;
            if m.is_empty() {
                let li = vm.get(rx, &PropKey::lit("lastIndex"))?;
                let li = vm.to_length(&li)?;
                set_last_index(vm, rx, advance(&s, li, full))?;
            }
            out.push(Value::String(m));
        }
    });
    let sma = vm.realm.sym_match_all.clone();
    method_sym(vm, rp, sma, "[Symbol.matchAll]", 1, |vm, c| {
        let r = this_obj(vm, &c.this)?;
        let s = vm.to_string(&c.arg(0))?;
        let rc = vm.realm.regexp_ctor;
        let ctor = vm.species_constructor(r, rc)?;
        let flags = get_string(vm, r, "flags")?;
        let matcher = vm.construct(
            ctor.as_object().expect("constructor"),
            vec![Value::Object(r), Value::String(flags.clone())],
            None,
        )?;
        let li = vm.get(r, &PropKey::lit("lastIndex"))?;
        let li = vm.to_length(&li)?;
        let Value::Object(mo) = matcher else { return vm.type_error("matcher is not an object") };
        set_last_index(vm, mo, li)?;
        let p = vm.realm.regexp_string_iterator_proto;
        let it = vm.heap.alloc(Object::new(
            Some(p),
            Kind::RegExpStringIter(Box::new(RegExpStringIter {
                matcher: mo,
                s,
                global: has(&flags, 'g'),
                unicode: has(&flags, 'u') || has(&flags, 'v'),
                done: false,
            })),
        ));
        Ok(Value::Object(it))
    });
    let sr = vm.realm.sym_replace.clone();
    method_sym(vm, rp, sr, "[Symbol.replace]", 2, symbol_replace);
    let ss = vm.realm.sym_search.clone();
    method_sym(vm, rp, ss, "[Symbol.search]", 1, |vm, c| {
        let rx = this_obj(vm, &c.this)?;
        let s = vm.to_string(&c.arg(0))?;
        let prev = vm.get(rx, &PropKey::lit("lastIndex"))?;
        if !super::super::value::same_value(&prev, &Value::Number(0.0)) {
            set_last_index(vm, rx, 0.0)?;
        }
        let r = vm.regexp_exec(rx, &s)?;
        let cur = vm.get(rx, &PropKey::lit("lastIndex"))?;
        if !super::super::value::same_value(&cur, &prev) {
            vm.set_or_throw(rx, PropKey::lit("lastIndex"), prev)?;
        }
        match r {
            Value::Null => Ok(Value::Number(-1.0)),
            r => vm.get_value_key(&r, &PropKey::lit("index")),
        }
    });
    let sp = vm.realm.sym_split.clone();
    method_sym(vm, rp, sp, "[Symbol.split]", 2, symbol_split);

    let ip = vm.realm.iterator_proto;
    let rsi = vm.new_object_with_proto(Some(ip));
    vm.realm.regexp_string_iterator_proto = rsi;
    method(vm, rsi, "next", 0, |vm, c| {
        let Value::Object(o) = c.this else { return vm.type_error("not a RegExp String Iterator") };
        let (m, s, global, unicode, done) = match &vm.heap.obj(o).kind {
            Kind::RegExpStringIter(it) => (it.matcher, it.s.clone(), it.global, it.unicode, it.done),
            _ => return vm.type_error("not a RegExp String Iterator"),
        };
        if done {
            return Ok(vm.iter_result(Value::Undefined, true));
        }
        let set_done = |vm: &mut Vm| {
            if let Kind::RegExpStringIter(it) = &mut vm.heap.obj_mut(o).kind {
                it.done = true;
            }
        };
        let r = vm.regexp_exec(m, &s)?;
        if matches!(r, Value::Null) {
            set_done(vm);
            return Ok(vm.iter_result(Value::Undefined, true));
        }
        if !global {
            set_done(vm);
            return Ok(vm.iter_result(r, false));
        }
        let ms = match_string(vm, &r)?;
        if ms.is_empty() {
            let li = vm.get(m, &PropKey::lit("lastIndex"))?;
            let li = vm.to_length(&li)?;
            set_last_index(vm, m, advance(&s, li, unicode))?;
        }
        Ok(vm.iter_result(r, false))
    });
    realm::to_string_tag(vm, rsi, "RegExp String Iterator");
}

pub struct RegExpStringIter {
    pub matcher: ObjRef,
    pub s: JsString,
    pub global: bool,
    pub unicode: bool,
    pub done: bool,
}

/// GetLegacyRegExpStaticProperty (proposal-regexp-legacy-features): slot `i`.
fn legacy_get(vm: &mut Vm, c: &NativeCall, i: usize) -> JsResult<Value> {
    if !matches!(c.this, Value::Object(o) if o == vm.realm.regexp_ctor) {
        return vm.type_error("RegExp legacy static accessed on a value other than RegExp");
    }
    match &vm.realm.regexp_statics {
        Some(st) => Ok(Value::String(st[i].clone())),
        None => vm.type_error("RegExp legacy statics are not available after a subclass match"),
    }
}

/// The legacy static accessors on %RegExp%: input/$_ (with a setter), lastMatch/$&,
/// lastParen/$+, leftContext/$`, rightContext/$', $1–$9.
fn legacy_accessors(vm: &mut Vm, rc: ObjRef) {
    macro_rules! getters {
        ($($i:literal: $($name:literal),+);* $(;)?) => {
            $($(
                realm::accessor(vm, rc, PropKey::lit($name), Some(|vm, c| legacy_get(vm, &c, $i)),
                    if $i == 0 { Some(legacy_set_input as NativeFn) } else { None });
            )+)*
        };
    }
    getters!(
        0: "input", "$_";
        1: "lastMatch", "$&";
        2: "lastParen", "$+";
        3: "leftContext", "$`";
        4: "rightContext", "$'";
        5: "$1"; 6: "$2"; 7: "$3"; 8: "$4"; 9: "$5"; 10: "$6"; 11: "$7"; 12: "$8"; 13: "$9";
    );
}

/// SetLegacyRegExpStaticProperty for [[RegExpInput]].
fn legacy_set_input(vm: &mut Vm, c: NativeCall) -> JsResult<Value> {
    if !matches!(c.this, Value::Object(o) if o == vm.realm.regexp_ctor) {
        return vm.type_error("RegExp.input set on a value other than RegExp");
    }
    let v = vm.to_string(&c.arg(0))?;
    if let Some(st) = &mut vm.realm.regexp_statics {
        st[0] = v;
    }
    Ok(Value::Undefined)
}

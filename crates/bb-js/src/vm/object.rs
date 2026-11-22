//! The essential internal methods of objects (ECMA-262 §10.1), the Array (§10.4.2) and
//! String (§10.4.3) exotic objects, and the object operations of §7.3 and iterators (§7.4).

use super::heap::*;
use super::value::{same_value, PropKey, Value};
use super::{JsResult, Vm};

/// A Property Descriptor (§6.2.6) with absent fields as `None`.
#[derive(Clone, Default, Debug)]
pub struct PropDesc {
    pub value: Option<Value>,
    pub writable: Option<bool>,
    /// `Some(None)`: present and undefined.
    pub get: Option<Option<ObjRef>>,
    pub set: Option<Option<ObjRef>>,
    pub enumerable: Option<bool>,
    pub configurable: Option<bool>,
}

impl PropDesc {
    pub fn data(v: Value, w: bool, e: bool, c: bool) -> PropDesc {
        PropDesc {
            value: Some(v),
            writable: Some(w),
            get: None,
            set: None,
            enumerable: Some(e),
            configurable: Some(c),
        }
    }
    pub fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }
    pub fn is_data(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }
    pub fn is_generic(&self) -> bool {
        !self.is_accessor() && !self.is_data()
    }
    pub fn is_empty(&self) -> bool {
        self.is_generic() && self.enumerable.is_none() && self.configurable.is_none()
    }
    pub fn from_prop(p: &Prop) -> PropDesc {
        match &p.slot {
            Slot::Data(v) => PropDesc::data(v.clone(), p.writable, p.enumerable, p.configurable),
            Slot::Accessor(g, s) => PropDesc {
                value: None,
                writable: None,
                get: Some(*g),
                set: Some(*s),
                enumerable: Some(p.enumerable),
                configurable: Some(p.configurable),
            },
        }
    }
    fn to_prop(&self) -> Prop {
        if self.is_accessor() {
            Prop {
                slot: Slot::Accessor(self.get.flatten(), self.set.flatten()),
                writable: false,
                enumerable: self.enumerable.unwrap_or(false),
                configurable: self.configurable.unwrap_or(false),
            }
        } else {
            Prop {
                slot: Slot::Data(self.value.clone().unwrap_or_default()),
                writable: self.writable.unwrap_or(false),
                enumerable: self.enumerable.unwrap_or(false),
                configurable: self.configurable.unwrap_or(false),
            }
        }
    }
}

impl Vm {
    pub fn new_object(&mut self) -> ObjRef {
        let p = self.realm.object_proto;
        self.heap.alloc(Object::new(Some(p), Kind::Ordinary))
    }

    pub fn new_object_with_proto(&mut self, proto: Option<ObjRef>) -> ObjRef {
        self.heap.alloc(Object::new(proto, Kind::Ordinary))
    }

    /// CreateArrayFromList (§7.3.18).
    pub fn new_array(&mut self, vals: Vec<Value>) -> ObjRef {
        let p = self.realm.array_proto;
        let a = self.heap.alloc(Object::new(Some(p), Kind::Array));
        let o = self.heap.obj_mut(a);
        let n = vals.len();
        for (i, v) in vals.into_iter().enumerate() {
            o.props.insert(PropKey::from_u32(i as u32), Prop::data(v, true, true, true));
        }
        o.props.insert(PropKey::named("length"), Prop::data(Value::Number(n as f64), true, false, false));
        a
    }

    /// An array's `length` (no user code runs).
    pub fn length_of_array(&self, a: ObjRef) -> u32 {
        match self.heap.obj(a).props.get(&PropKey::named("length")).map(|p| &p.slot) {
            Some(Slot::Data(Value::Number(n))) => *n as u32,
            _ => 0,
        }
    }

    pub fn set_array_length_raw(&mut self, a: ObjRef, n: u32) {
        if let Some(p) = self.heap.obj_mut(a).props.get_mut(&PropKey::named("length")) {
            p.slot = Slot::Data(Value::Number(n as f64));
        }
    }

    /// Append to an array being built (literals, spreads): CreateDataProperty at `length`.
    pub fn array_push(&mut self, a: ObjRef, v: Value) -> JsResult<()> {
        let n = self.length_of_array(a);
        if n == u32::MAX - 1 {
            return self.range_error("Invalid array length");
        }
        self.create_data_property_or_throw(a, PropKey::from_u32(n), v)?;
        self.set_array_length_raw(a, n + 1);
        Ok(())
    }

    // ---- [[GetPrototypeOf]] etc. ----

    pub fn get_prototype_of(&mut self, o: ObjRef) -> JsResult<Option<ObjRef>> {
        if self.is_proxy(o) {
            return self.proxy_get_prototype_of(o);
        }
        Ok(self.heap.obj(o).proto)
    }

    /// OrdinarySetPrototypeOf (§10.1.2.1).
    pub fn set_prototype_of(&mut self, o: ObjRef, v: Option<ObjRef>) -> JsResult<bool> {
        if self.is_proxy(o) {
            return self.proxy_set_prototype_of(o, v);
        }
        let cur = self.heap.obj(o).proto;
        if cur == v {
            return Ok(true);
        }
        if !self.heap.obj(o).extensible {
            return Ok(false);
        }
        // Immutable prototype exotic objects (%Object.prototype%, §10.4.7).
        if o == self.realm.object_proto {
            return Ok(false);
        }
        let mut p = v;
        while let Some(x) = p {
            if x == o {
                return Ok(false);
            }
            if self.is_proxy(x) {
                break;
            }
            p = self.heap.obj(x).proto;
        }
        self.heap.obj_mut(o).proto = v;
        Ok(true)
    }

    pub fn is_extensible(&mut self, o: ObjRef) -> JsResult<bool> {
        if self.is_proxy(o) {
            return self.proxy_is_extensible(o);
        }
        Ok(self.heap.obj(o).extensible)
    }

    pub fn prevent_extensions(&mut self, o: ObjRef) -> JsResult<bool> {
        if self.is_proxy(o) {
            return self.proxy_prevent_extensions(o);
        }
        self.heap.obj_mut(o).extensible = false;
        Ok(true)
    }

    /// [[GetOwnProperty]] (§10.1.5, §10.4.3.1).
    pub fn get_own_property(&mut self, o: ObjRef, k: &PropKey) -> JsResult<Option<PropDesc>> {
        if self.is_proxy(o) {
            return self.proxy_get_own_property(o, k);
        }
        let obj = self.heap.obj(o);
        if let Some(p) = obj.props.get(k) {
            return Ok(Some(PropDesc::from_prop(p)));
        }
        if let (Kind::String(s), PropKey::Index(i)) = (&obj.kind, k) {
            if let Some(&c) = s.get(*i as usize) {
                return Ok(Some(PropDesc::data(Value::String(vec![c].into()), false, true, false)));
            }
        }
        Ok(None)
    }

    /// [[DefineOwnProperty]] (§10.1.6, §10.4.2.1, §10.4.3.2).
    pub fn define_own_property(&mut self, o: ObjRef, k: PropKey, d: PropDesc) -> JsResult<bool> {
        match &self.heap.obj(o).kind {
            Kind::Proxy(_) => return self.proxy_define_own_property(o, k, d),
            Kind::Array => {
                if k.is_str("length") {
                    return self.array_set_length(o, d);
                }
                if let PropKey::Index(i) = k {
                    let (len, len_writable) = match self.heap.obj(o).props.get(&PropKey::named("length")) {
                        Some(Prop { slot: Slot::Data(Value::Number(n)), writable, .. }) => {
                            (*n as u32, *writable)
                        }
                        _ => (0, true),
                    };
                    if i >= len && !len_writable {
                        return Ok(false);
                    }
                    if !self.ordinary_define(o, k, d)? {
                        return Ok(false);
                    }
                    if i >= len {
                        self.set_array_length_raw(o, i + 1);
                    }
                    return Ok(true);
                }
            }
            Kind::String(s) => {
                if let PropKey::Index(i) = k {
                    if (i as usize) < s.len() {
                        let cur = self.get_own_property(o, &k)?;
                        return Ok(validate_and_apply(None, &k, false, &d, cur.as_ref()));
                    }
                }
            }
            _ => {}
        }
        self.ordinary_define(o, k, d)
    }

    fn ordinary_define(&mut self, o: ObjRef, k: PropKey, d: PropDesc) -> JsResult<bool> {
        let cur = self.heap.obj(o).props.get(&k).map(PropDesc::from_prop);
        let ext = self.heap.obj(o).extensible;
        let obj = self.heap.obj_mut(o);
        Ok(validate_and_apply(Some(&mut obj.props), &k, ext, &d, cur.as_ref()))
    }

    /// ArraySetLength (§10.4.2.4).
    fn array_set_length(&mut self, a: ObjRef, d: PropDesc) -> JsResult<bool> {
        let lk = PropKey::named("length");
        let Some(v) = d.value.clone() else {
            return self.ordinary_define(a, lk, d);
        };
        let new_len = self.to_uint32(&v)?;
        let number_len = self.to_number(&v)?;
        if new_len as f64 != number_len {
            return self.range_error("Invalid array length");
        }
        let mut nd = d.clone();
        nd.value = Some(Value::Number(new_len as f64));
        let (old_len, old_writable) = match self.heap.obj(a).props.get(&lk) {
            Some(Prop { slot: Slot::Data(Value::Number(n)), writable, .. }) => (*n as u32, *writable),
            _ => (0, true),
        };
        if new_len >= old_len {
            return self.ordinary_define(a, lk, nd);
        }
        if !old_writable {
            return Ok(false);
        }
        let new_writable = nd.writable != Some(false);
        if !new_writable {
            nd.writable = Some(true);
        }
        if !self.ordinary_define(a, lk.clone(), nd.clone())? {
            return Ok(false);
        }
        let mut idx: Vec<u32> =
            self.heap.obj(a).props.index_keys().into_iter().filter(|&i| i >= new_len).collect();
        idx.sort_unstable_by(|x, y| y.cmp(x));
        for i in idx {
            if !self.delete(a, &PropKey::Index(i))? {
                nd.value = Some(Value::Number(i as f64 + 1.0));
                if !new_writable {
                    nd.writable = Some(false);
                }
                self.ordinary_define(a, lk, nd)?;
                return Ok(false);
            }
        }
        if !new_writable {
            self.ordinary_define(a, lk, PropDesc { writable: Some(false), ..Default::default() })?;
        }
        Ok(true)
    }

    /// [[HasProperty]] (§10.1.7).
    pub fn has_property(&mut self, o: ObjRef, k: &PropKey) -> JsResult<bool> {
        self.burn()?;
        let mut cur = Some(o);
        while let Some(c) = cur {
            if self.is_proxy(c) {
                return self.proxy_has(c, k);
            }
            if self.get_own_property(c, k)?.is_some() {
                return Ok(true);
            }
            cur = self.get_prototype_of(c)?;
        }
        Ok(false)
    }

    pub fn has_own_property(&mut self, o: ObjRef, k: &PropKey) -> JsResult<bool> {
        Ok(self.get_own_property(o, k)?.is_some())
    }

    /// [[Get]] (§10.1.8).
    pub fn get(&mut self, o: ObjRef, k: &PropKey) -> JsResult<Value> {
        self.get_with_receiver(o, k, Value::Object(o))
    }

    pub fn get_with_receiver(&mut self, o: ObjRef, k: &PropKey, receiver: Value) -> JsResult<Value> {
        self.burn()?;
        let mut cur = o;
        loop {
            if self.is_proxy(cur) {
                return self.proxy_get(cur, k, receiver);
            }
            match self.get_own_property(cur, k)? {
                Some(d) => {
                    if let Some(v) = d.value {
                        return Ok(v);
                    }
                    return match d.get.flatten() {
                        Some(g) => self.call(&Value::Object(g), receiver, Vec::new()),
                        None => Ok(Value::Undefined),
                    };
                }
                None => match self.get_prototype_of(cur)? {
                    Some(p) => cur = p,
                    None => return Ok(Value::Undefined),
                },
            }
        }
    }

    /// [[Set]] (§10.1.9 OrdinarySet).
    pub fn set(&mut self, o: ObjRef, k: PropKey, v: Value, receiver: Value) -> JsResult<bool> {
        let mut cur = o;
        let own = loop {
            if self.is_proxy(cur) {
                return self.proxy_set(cur, k, v, receiver);
            }
            match self.get_own_property(cur, &k)? {
                Some(d) => break d,
                None => match self.get_prototype_of(cur)? {
                    Some(p) => cur = p,
                    None => break PropDesc::data(Value::Undefined, true, true, true),
                },
            }
        };
        if own.is_data() {
            if own.writable != Some(true) {
                return Ok(false);
            }
            let Value::Object(r) = receiver else { return Ok(false) };
            match self.get_own_property(r, &k)? {
                Some(e) => {
                    if e.is_accessor() || e.writable != Some(true) {
                        return Ok(false);
                    }
                    self.define_own_property(r, k, PropDesc { value: Some(v), ..Default::default() })
                }
                None => self.create_data_property(r, k, v),
            }
        } else {
            match own.set.flatten() {
                Some(s) => {
                    self.call(&Value::Object(s), receiver, vec![v])?;
                    Ok(true)
                }
                None => Ok(false),
            }
        }
    }

    /// [[Delete]] (§10.1.10).
    pub fn delete(&mut self, o: ObjRef, k: &PropKey) -> JsResult<bool> {
        if self.is_proxy(o) {
            return self.proxy_delete(o, k);
        }
        match self.get_own_property(o, k)? {
            None => Ok(true),
            Some(d) if d.configurable == Some(true) => {
                self.heap.obj_mut(o).props.remove(k);
                Ok(true)
            }
            Some(_) => Ok(false),
        }
    }

    /// [[OwnPropertyKeys]] (§10.1.11, §10.4.3.3).
    pub fn own_property_keys(&mut self, o: ObjRef) -> JsResult<Vec<PropKey>> {
        if self.is_proxy(o) {
            return self.proxy_own_keys(o);
        }
        let obj = self.heap.obj(o);
        let mut keys = obj.props.keys();
        if let Kind::String(s) = &obj.kind {
            let n = s.len() as u32;
            let mut out: Vec<PropKey> = (0..n).map(PropKey::Index).collect();
            out.extend(keys.drain(..).filter(|k| !matches!(k, PropKey::Index(i) if *i < n)));
            keys = out;
        }
        Ok(keys)
    }

    // ---- §7.3 operations ----

    pub fn create_data_property(&mut self, o: ObjRef, k: PropKey, v: Value) -> JsResult<bool> {
        self.define_own_property(o, k, PropDesc::data(v, true, true, true))
    }

    pub fn create_data_property_or_throw(&mut self, o: ObjRef, k: PropKey, v: Value) -> JsResult<()> {
        let d = k.display();
        if !self.create_data_property(o, k, v)? {
            return self.type_error(&format!("Cannot define property '{d}'"));
        }
        Ok(())
    }

    pub fn define_own_property_or_throw(&mut self, o: ObjRef, k: PropKey, d: PropDesc) -> JsResult<()> {
        let name = k.display();
        if !self.define_own_property(o, k, d)? {
            return self.type_error(&format!("Cannot redefine property: {name}"));
        }
        Ok(())
    }

    /// Set (§7.3.4) with `Throw` = true.
    pub fn set_or_throw(&mut self, o: ObjRef, k: PropKey, v: Value) -> JsResult<()> {
        let name = k.display();
        if !self.set(o, k, v, Value::Object(o))? {
            return self.type_error(&format!("Cannot assign to read only property '{name}'"));
        }
        Ok(())
    }

    pub fn delete_or_throw(&mut self, o: ObjRef, k: &PropKey) -> JsResult<()> {
        if !self.delete(o, k)? {
            return self.type_error(&format!("Cannot delete property '{}'", k.display()));
        }
        Ok(())
    }

    /// GetMethod (§7.3.11).
    pub fn get_method(&mut self, v: &Value, k: &PropKey) -> JsResult<Option<Value>> {
        let f = self.get_value_key(v, k)?;
        if f.is_nullish() {
            return Ok(None);
        }
        if !self.is_callable(&f) {
            return self.type_error(&format!("{} is not a function", k.display()));
        }
        Ok(Some(f))
    }

    /// GetValue on a property reference: `base[key]` with ToObject for primitives.
    pub fn get_value_prop(&mut self, base: &Value, key: &Value) -> JsResult<Value> {
        if base.is_nullish() {
            let k = self.to_property_key(key).map(|k| k.display()).unwrap_or_default();
            return self.type_error(&format!(
                "Cannot read properties of {} (reading '{k}')",
                super::ops::nullish_name(base)
            ));
        }
        let k = self.to_property_key(key)?;
        self.get_value_key(base, &k)
    }

    pub fn get_value_key(&mut self, base: &Value, k: &PropKey) -> JsResult<Value> {
        match base {
            Value::Object(o) => self.get(*o, k),
            Value::String(s) => {
                if let PropKey::Index(i) = k {
                    if let Some(&c) = s.get(*i as usize) {
                        return Ok(Value::String(vec![c].into()));
                    }
                }
                if k.is_str("length") {
                    return Ok(Value::Number(s.len() as f64));
                }
                let p = self.realm.string_proto;
                self.get_with_receiver(p, k, base.clone())
            }
            Value::Undefined | Value::Null => self.type_error(&format!(
                "Cannot read properties of {} (reading '{}')",
                super::ops::nullish_name(base),
                k.display()
            )),
            _ => {
                let o = self.to_object(base)?;
                self.get_with_receiver(o, k, base.clone())
            }
        }
    }

    /// PutValue on a property reference (§6.2.5.6).
    pub fn put_value(&mut self, base: &Value, k: PropKey, v: Value) -> JsResult<()> {
        let name = k.display();
        let ok = match base {
            Value::Object(o) => self.set(*o, k, v, base.clone())?,
            _ => {
                let o = self.to_object(base)?;
                self.set(o, k, v, base.clone())?
            }
        };
        if !ok && self.strict() {
            return self.type_error(&format!("Cannot assign to read only property '{name}'"));
        }
        Ok(())
    }

    /// LengthOfArrayLike (§7.3.19).
    pub fn length_of_array_like(&mut self, o: ObjRef) -> JsResult<f64> {
        let v = self.get(o, &PropKey::named("length"))?;
        self.to_length(&v)
    }

    /// CreateListFromArrayLike (§7.3.20).
    pub fn list_from_array_like(&mut self, v: &Value) -> JsResult<Vec<Value>> {
        let Value::Object(o) = v else {
            return self.type_error("CreateListFromArrayLike called on non-object");
        };
        let n = self.length_of_array_like(*o)?;
        if n > 1e7 {
            return self.range_error("Too many arguments");
        }
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n as u32 {
            out.push(self.get(*o, &PropKey::from_u32(i))?);
        }
        Ok(out)
    }

    /// The values of an array built by the VM (spread arguments).
    pub(super) fn array_values(&mut self, v: &Value) -> JsResult<Vec<Value>> {
        self.list_from_array_like(v)
    }

    /// CopyDataProperties (§7.3.25).
    pub fn copy_data_properties(
        &mut self,
        target: ObjRef,
        src: &Value,
        excluded: &[PropKey],
    ) -> JsResult<()> {
        if src.is_nullish() {
            return Ok(());
        }
        let from = self.to_object(src)?;
        for k in self.own_property_keys(from)? {
            if excluded.contains(&k) {
                continue;
            }
            if let Some(d) = self.get_own_property(from, &k)? {
                if d.enumerable == Some(true) {
                    let v = self.get(from, &k)?;
                    self.create_data_property_or_throw(target, k, v)?;
                }
            }
        }
        Ok(())
    }

    /// SetIntegrityLevel (§7.3.16): `frozen` or sealed.
    pub fn set_integrity(&mut self, o: ObjRef, frozen: bool) -> JsResult<bool> {
        if !self.prevent_extensions(o)? {
            return Ok(false);
        }
        for k in self.own_property_keys(o)? {
            let d = if frozen {
                match self.get_own_property(o, &k)? {
                    Some(cur) if cur.is_accessor() => {
                        PropDesc { configurable: Some(false), ..Default::default() }
                    }
                    Some(_) => {
                        PropDesc { configurable: Some(false), writable: Some(false), ..Default::default() }
                    }
                    None => continue,
                }
            } else {
                PropDesc { configurable: Some(false), ..Default::default() }
            };
            self.define_own_property_or_throw(o, k, d)?;
        }
        Ok(true)
    }

    /// TestIntegrityLevel (§7.3.17).
    pub fn test_integrity(&mut self, o: ObjRef, frozen: bool) -> JsResult<bool> {
        if self.is_extensible(o)? {
            return Ok(false);
        }
        for k in self.own_property_keys(o)? {
            if let Some(d) = self.get_own_property(o, &k)? {
                if d.configurable == Some(true) {
                    return Ok(false);
                }
                if frozen && d.is_data() && d.writable == Some(true) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    // ---- iterators (§7.4) ----

    /// GetIterator (§7.4.4), sync: an iterator record object.
    pub fn get_iterator(&mut self, v: &Value) -> JsResult<ObjRef> {
        let k = PropKey::Sym(self.realm.sym_iterator.clone());
        let Some(m) = self.get_method(v, &k)? else {
            return self.type_error(&format!("{} is not iterable", self.describe_short(v)));
        };
        self.get_iterator_from_method(v, &m)
    }

    pub fn get_iterator_from_method(&mut self, v: &Value, m: &Value) -> JsResult<ObjRef> {
        let it = self.call(m, v.clone(), Vec::new())?;
        if !matches!(it, Value::Object(_)) {
            return self.type_error("Result of the Symbol.iterator method is not an object");
        }
        let next = self.get_value_key(&it, &PropKey::named("next"))?;
        Ok(self.heap.alloc(Object::new(
            None,
            Kind::IterRecord(Box::new(IterRecord { iterator: it, next, done: false })),
        )))
    }

    pub(super) fn iter_done(&self, r: ObjRef) -> bool {
        match &self.heap.obj(r).kind {
            Kind::IterRecord(x) => x.done,
            _ => true,
        }
    }

    fn set_iter_done(&mut self, r: ObjRef) {
        if let Kind::IterRecord(x) = &mut self.heap.obj_mut(r).kind {
            x.done = true;
        }
    }

    /// IteratorStepValue (§7.4.10): `None` when done. Errors mark the record done.
    pub fn iter_step(&mut self, r: ObjRef) -> JsResult<Option<Value>> {
        let (it, next) = match &self.heap.obj(r).kind {
            Kind::IterRecord(x) => (x.iterator.clone(), x.next.clone()),
            _ => return Ok(None),
        };
        let res = match self.call(&next, it, Vec::new()) {
            Ok(v) => v,
            Err(e) => {
                self.set_iter_done(r);
                return Err(e);
            }
        };
        let Value::Object(ro) = res else {
            self.set_iter_done(r);
            return self.type_error("Iterator result is not an object");
        };
        let done = match self.get(ro, &PropKey::named("done")) {
            Ok(v) => v.truthy(),
            Err(e) => {
                self.set_iter_done(r);
                return Err(e);
            }
        };
        if done {
            self.set_iter_done(r);
            return Ok(None);
        }
        match self.get(ro, &PropKey::named("value")) {
            Ok(v) => Ok(Some(v)),
            Err(e) => {
                self.set_iter_done(r);
                Err(e)
            }
        }
    }

    /// IteratorClose (§7.4.11) for a normal completion.
    pub fn iterator_close(&mut self, r: ObjRef) -> JsResult<()> {
        let it = match &self.heap.obj(r).kind {
            Kind::IterRecord(x) => x.iterator.clone(),
            _ => return Ok(()),
        };
        self.set_iter_done(r);
        let ret = self.get_method(&it, &PropKey::named("return"))?;
        if let Some(f) = ret {
            let res = self.call(&f, it, Vec::new())?;
            if !matches!(res, Value::Object(_)) {
                return self.type_error("Iterator result is not an object");
            }
        }
        Ok(())
    }

    /// IteratorToList over GetIterator (spread).
    pub fn iterate_to_vec(&mut self, v: &Value) -> JsResult<Vec<Value>> {
        let r = self.get_iterator(v)?;
        let mut out = Vec::new();
        while let Some(x) = self.iter_step(r)? {
            out.push(x);
        }
        Ok(out)
    }

    /// CreateIterResultObject (§7.4.14).
    pub fn iter_result(&mut self, v: Value, done: bool) -> Value {
        let o = self.new_object();
        let obj = self.heap.obj_mut(o);
        obj.props.insert(PropKey::named("value"), Prop::data(v, true, true, true));
        obj.props.insert(PropKey::named("done"), Prop::data(Value::Bool(done), true, true, true));
        Value::Object(o)
    }

    /// The next key of a for-in enumeration (§14.7.5.10 EnumerateObjectProperties).
    pub(super) fn for_in_next(&mut self, it: ObjRef) -> JsResult<Option<Value>> {
        let Kind::ForIn(mut st) = std::mem::replace(&mut self.heap.obj_mut(it).kind, Kind::Ordinary) else {
            return Ok(None);
        };
        let r = self.for_in_step(&mut st);
        self.heap.obj_mut(it).kind = Kind::ForIn(st);
        r
    }

    fn for_in_step(&mut self, st: &mut ForIn) -> JsResult<Option<Value>> {
        loop {
            let Some(o) = st.object else { return Ok(None) };
            if !st.started {
                st.started = true;
                let keys = self.own_property_keys(o)?;
                st.queue = keys.into_iter().filter(|k| !matches!(k, PropKey::Sym(_))).collect();
            }
            while let Some(k) = st.queue.pop_front() {
                if st.visited.contains(&k) {
                    continue;
                }
                let Some(d) = self.get_own_property(o, &k)? else { continue };
                st.visited.insert(k.clone());
                if d.enumerable == Some(true) {
                    return Ok(Some(k.to_value()));
                }
            }
            st.object = self.get_prototype_of(o)?;
            st.started = false;
        }
    }
}

/// ValidateAndApplyPropertyDescriptor (§10.1.6.3). `props: None` only validates.
pub fn validate_and_apply(
    props: Option<&mut Props>,
    k: &PropKey,
    extensible: bool,
    d: &PropDesc,
    cur: Option<&PropDesc>,
) -> bool {
    let Some(cur) = cur else {
        if !extensible {
            return false;
        }
        if let Some(p) = props {
            p.insert(k.clone(), d.to_prop());
        }
        return true;
    };
    if d.is_empty() {
        return true;
    }
    if cur.configurable == Some(false) {
        if d.configurable == Some(true) {
            return false;
        }
        if d.enumerable.is_some() && d.enumerable != cur.enumerable {
            return false;
        }
        if !d.is_generic() && d.is_accessor() != cur.is_accessor() {
            return false;
        }
        if cur.is_accessor() {
            if d.get.is_some() && d.get != cur.get {
                return false;
            }
            if d.set.is_some() && d.set != cur.set {
                return false;
            }
        } else if cur.writable == Some(false) {
            if d.writable == Some(true) {
                return false;
            }
            if let Some(v) = &d.value {
                if !same_value(v, cur.value.as_ref().unwrap_or(&Value::Undefined)) {
                    return false;
                }
            }
        }
    }
    let Some(props) = props else { return true };
    let Some(p) = props.get_mut(k) else { return true };
    if cur.is_data() && d.is_accessor() {
        p.slot = Slot::Accessor(d.get.flatten(), d.set.flatten());
        p.writable = false;
    } else if cur.is_accessor() && d.is_data() {
        p.slot = Slot::Data(d.value.clone().unwrap_or_default());
        p.writable = d.writable.unwrap_or(false);
    } else {
        if let Some(v) = &d.value {
            p.slot = Slot::Data(v.clone());
        }
        if let Some(w) = d.writable {
            p.writable = w;
        }
        if let Slot::Accessor(g, s) = &mut p.slot {
            if let Some(ng) = d.get {
                *g = ng;
            }
            if let Some(ns) = d.set {
                *s = ns;
            }
        }
    }
    if let Some(e) = d.enumerable {
        p.enumerable = e;
    }
    if let Some(c) = d.configurable {
        p.configurable = c;
    }
    true
}

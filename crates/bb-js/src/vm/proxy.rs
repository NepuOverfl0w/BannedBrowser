//! Proxy exotic objects (§10.5): each internal method calls the handler's trap and checks
//! the invariants that keep the target's non-configurable and non-extensible facts true.

use super::heap::*;
use super::object::{validate_and_apply, PropDesc};
use super::value::{same_value, PropKey, Value};
use super::{JsResult, Vm};

pub struct ProxyData {
    /// `None` once revoked.
    pub target: Option<ObjRef>,
    pub handler: Option<ObjRef>,
    pub callable: bool,
    pub constructor: bool,
}

fn complete(mut d: PropDesc) -> PropDesc {
    // CompletePropertyDescriptor (§6.2.6.6).
    if d.is_generic() || d.is_data() {
        d.value.get_or_insert(Value::Undefined);
        d.writable.get_or_insert(false);
    } else {
        d.get.get_or_insert(None);
        d.set.get_or_insert(None);
    }
    d.enumerable.get_or_insert(false);
    d.configurable.get_or_insert(false);
    d
}

impl Vm {
    pub(super) fn is_proxy(&self, o: ObjRef) -> bool {
        matches!(self.heap.obj(o).kind, Kind::Proxy(_))
    }

    fn proxy_parts(&mut self, o: ObjRef) -> JsResult<(ObjRef, ObjRef)> {
        match &self.heap.obj(o).kind {
            Kind::Proxy(p) => match (p.target, p.handler) {
                (Some(t), Some(h)) => Ok((t, h)),
                _ => self.type_error("Cannot perform operation on a revoked proxy"),
            },
            _ => self.type_error("not a proxy"),
        }
    }

    fn trap(&mut self, h: ObjRef, name: &str) -> JsResult<Option<Value>> {
        self.get_method(&Value::Object(h), &PropKey::named(name))
    }

    pub(super) fn proxy_get_prototype_of(&mut self, o: ObjRef) -> JsResult<Option<ObjRef>> {
        let (t, h) = self.proxy_parts(o)?;
        let Some(trap) = self.trap(h, "getPrototypeOf")? else { return self.get_prototype_of(t) };
        let r = self.call(&trap, Value::Object(h), vec![Value::Object(t)])?;
        let p = match r {
            Value::Object(p) => Some(p),
            Value::Null => None,
            _ => return self.type_error("'getPrototypeOf' on proxy: trap returned neither object nor null"),
        };
        if self.is_extensible(t)? {
            return Ok(p);
        }
        if self.get_prototype_of(t)? != p {
            return self.type_error("'getPrototypeOf' on proxy: proxy target is non-extensible but the trap did not return its actual prototype");
        }
        Ok(p)
    }

    pub(super) fn proxy_set_prototype_of(&mut self, o: ObjRef, v: Option<ObjRef>) -> JsResult<bool> {
        let (t, h) = self.proxy_parts(o)?;
        let Some(trap) = self.trap(h, "setPrototypeOf")? else { return self.set_prototype_of(t, v) };
        let arg = v.map(Value::Object).unwrap_or(Value::Null);
        if !self.call(&trap, Value::Object(h), vec![Value::Object(t), arg])?.truthy() {
            return Ok(false);
        }
        if self.is_extensible(t)? {
            return Ok(true);
        }
        if self.get_prototype_of(t)? != v {
            return self.type_error("'setPrototypeOf' on proxy: trap returned truish for a non-extensible target with a different prototype");
        }
        Ok(true)
    }

    pub(super) fn proxy_is_extensible(&mut self, o: ObjRef) -> JsResult<bool> {
        let (t, h) = self.proxy_parts(o)?;
        let Some(trap) = self.trap(h, "isExtensible")? else { return self.is_extensible(t) };
        let r = self.call(&trap, Value::Object(h), vec![Value::Object(t)])?.truthy();
        if r != self.is_extensible(t)? {
            return self.type_error(
                "'isExtensible' on proxy: trap result does not reflect extensibility of proxy target",
            );
        }
        Ok(r)
    }

    pub(super) fn proxy_prevent_extensions(&mut self, o: ObjRef) -> JsResult<bool> {
        let (t, h) = self.proxy_parts(o)?;
        let Some(trap) = self.trap(h, "preventExtensions")? else { return self.prevent_extensions(t) };
        let r = self.call(&trap, Value::Object(h), vec![Value::Object(t)])?.truthy();
        if r && self.is_extensible(t)? {
            return self.type_error(
                "'preventExtensions' on proxy: trap returned truish but the proxy target is extensible",
            );
        }
        Ok(r)
    }

    pub(super) fn proxy_get_own_property(&mut self, o: ObjRef, k: &PropKey) -> JsResult<Option<PropDesc>> {
        let (t, h) = self.proxy_parts(o)?;
        let Some(trap) = self.trap(h, "getOwnPropertyDescriptor")? else {
            return self.get_own_property(t, k);
        };
        let r = self.call(&trap, Value::Object(h), vec![Value::Object(t), k.to_value()])?;
        if !matches!(r, Value::Object(_) | Value::Undefined) {
            return self.type_error(
                "'getOwnPropertyDescriptor' on proxy: trap returned neither object nor undefined",
            );
        }
        let target_desc = self.get_own_property(t, k)?;
        if r.is_undefined() {
            let Some(td) = target_desc else { return Ok(None) };
            if td.configurable == Some(false) {
                return self.type_error("'getOwnPropertyDescriptor' on proxy: trap returned undefined for a non-configurable property");
            }
            if !self.is_extensible(t)? {
                return self.type_error("'getOwnPropertyDescriptor' on proxy: trap returned undefined for a property of a non-extensible target");
            }
            return Ok(None);
        }
        let ext = self.is_extensible(t)?;
        let rd = complete(super::builtins::to_property_descriptor(self, &r)?);
        if !validate_and_apply(None, k, ext, &rd, target_desc.as_ref()) {
            return self.type_error("'getOwnPropertyDescriptor' on proxy: trap returned a descriptor incompatible with the target's");
        }
        if rd.configurable == Some(false) {
            match &target_desc {
                None => return self.type_error("'getOwnPropertyDescriptor' on proxy: trap reported non-configurability for a missing property"),
                Some(td) if td.configurable == Some(true) => {
                    return self.type_error("'getOwnPropertyDescriptor' on proxy: trap reported non-configurability for a configurable property")
                }
                Some(td) => {
                    if rd.writable == Some(false) && td.writable == Some(true) {
                        return self.type_error("'getOwnPropertyDescriptor' on proxy: trap reported non-writability for a writable property");
                    }
                }
            }
        }
        Ok(Some(rd))
    }

    pub(super) fn proxy_define_own_property(&mut self, o: ObjRef, k: PropKey, d: PropDesc) -> JsResult<bool> {
        let (t, h) = self.proxy_parts(o)?;
        let Some(trap) = self.trap(h, "defineProperty")? else { return self.define_own_property(t, k, d) };
        let dobj = super::builtins::from_property_descriptor(self, Some(d.clone()));
        if !self.call(&trap, Value::Object(h), vec![Value::Object(t), k.to_value(), dobj])?.truthy() {
            return Ok(false);
        }
        let td = self.get_own_property(t, &k)?;
        let ext = self.is_extensible(t)?;
        let setting_config_false = d.configurable == Some(false);
        match td {
            None => {
                if !ext {
                    return self.type_error("'defineProperty' on proxy: trap returned truish for adding a property to a non-extensible target");
                }
                if setting_config_false {
                    return self.type_error("'defineProperty' on proxy: trap returned truish for defining a non-configurable property that doesn't exist on the target");
                }
            }
            Some(td) => {
                if !validate_and_apply(None, &k, ext, &d, Some(&td)) {
                    return self.type_error("'defineProperty' on proxy: trap returned truish for a property incompatible with the target's");
                }
                if setting_config_false && td.configurable == Some(true) {
                    return self.type_error("'defineProperty' on proxy: trap returned truish for making a configurable property non-configurable");
                }
                if td.is_data()
                    && td.configurable == Some(false)
                    && td.writable == Some(true)
                    && d.writable == Some(false)
                {
                    return self.type_error("'defineProperty' on proxy: trap returned truish for making a non-configurable writable property non-writable");
                }
            }
        }
        Ok(true)
    }

    pub(super) fn proxy_has(&mut self, o: ObjRef, k: &PropKey) -> JsResult<bool> {
        let (t, h) = self.proxy_parts(o)?;
        let Some(trap) = self.trap(h, "has")? else { return self.has_property(t, k) };
        let r = self.call(&trap, Value::Object(h), vec![Value::Object(t), k.to_value()])?.truthy();
        if !r {
            if let Some(td) = self.get_own_property(t, k)? {
                if td.configurable == Some(false) {
                    return self
                        .type_error("'has' on proxy: trap returned falsish for a non-configurable property");
                }
                if !self.is_extensible(t)? {
                    return self.type_error(
                        "'has' on proxy: trap returned falsish for a property of a non-extensible target",
                    );
                }
            }
        }
        Ok(r)
    }

    pub(super) fn proxy_get(&mut self, o: ObjRef, k: &PropKey, receiver: Value) -> JsResult<Value> {
        let (t, h) = self.proxy_parts(o)?;
        let Some(trap) = self.trap(h, "get")? else { return self.get_with_receiver(t, k, receiver) };
        let r = self.call(&trap, Value::Object(h), vec![Value::Object(t), k.to_value(), receiver])?;
        if let Some(td) = self.get_own_property(t, k)? {
            if td.configurable == Some(false) {
                if td.is_data()
                    && td.writable == Some(false)
                    && !same_value(&r, td.value.as_ref().unwrap_or(&Value::Undefined))
                {
                    return self.type_error("'get' on proxy: property is a read-only and non-configurable data property but the trap returned a different value");
                }
                if td.is_accessor() && td.get == Some(None) && !r.is_undefined() {
                    return self.type_error("'get' on proxy: property is a non-configurable accessor without a getter but the trap did not return undefined");
                }
            }
        }
        Ok(r)
    }

    pub(super) fn proxy_set(&mut self, o: ObjRef, k: PropKey, v: Value, receiver: Value) -> JsResult<bool> {
        let (t, h) = self.proxy_parts(o)?;
        let Some(trap) = self.trap(h, "set")? else { return self.set(t, k, v, receiver) };
        if !self
            .call(&trap, Value::Object(h), vec![Value::Object(t), k.to_value(), v.clone(), receiver])?
            .truthy()
        {
            return Ok(false);
        }
        if let Some(td) = self.get_own_property(t, &k)? {
            if td.configurable == Some(false) {
                if td.is_data()
                    && td.writable == Some(false)
                    && !same_value(&v, td.value.as_ref().unwrap_or(&Value::Undefined))
                {
                    return self.type_error("'set' on proxy: trap returned truish for a read-only non-configurable property with a different value");
                }
                if td.is_accessor() && td.set == Some(None) {
                    return self.type_error("'set' on proxy: trap returned truish for a non-configurable accessor without a setter");
                }
            }
        }
        Ok(true)
    }

    pub(super) fn proxy_delete(&mut self, o: ObjRef, k: &PropKey) -> JsResult<bool> {
        let (t, h) = self.proxy_parts(o)?;
        let Some(trap) = self.trap(h, "deleteProperty")? else { return self.delete(t, k) };
        if !self.call(&trap, Value::Object(h), vec![Value::Object(t), k.to_value()])?.truthy() {
            return Ok(false);
        }
        let Some(td) = self.get_own_property(t, k)? else { return Ok(true) };
        if td.configurable == Some(false) {
            return self.type_error(
                "'deleteProperty' on proxy: trap returned truish for a non-configurable property",
            );
        }
        if !self.is_extensible(t)? {
            return self.type_error(
                "'deleteProperty' on proxy: trap returned truish for a property of a non-extensible target",
            );
        }
        Ok(true)
    }

    pub(super) fn proxy_own_keys(&mut self, o: ObjRef) -> JsResult<Vec<PropKey>> {
        let (t, h) = self.proxy_parts(o)?;
        let Some(trap) = self.trap(h, "ownKeys")? else { return self.own_property_keys(t) };
        let arr = self.call(&trap, Value::Object(h), vec![Value::Object(t)])?;
        let Value::Object(_) = arr else {
            return self.type_error("CreateListFromArrayLike called on non-object");
        };
        let vals = self.list_from_array_like(&arr)?;
        let mut keys: Vec<PropKey> = Vec::with_capacity(vals.len());
        for v in vals {
            let k = match v {
                Value::String(s) => PropKey::from_js(s),
                Value::Symbol(s) => PropKey::Sym(s),
                _ => return self.type_error("'ownKeys' on proxy: trap returned a non-property-key"),
            };
            if keys.contains(&k) {
                return self.type_error("'ownKeys' on proxy: trap returned duplicate entries");
            }
            keys.push(k);
        }
        let ext = self.is_extensible(t)?;
        let target_keys = self.own_property_keys(t)?;
        let mut configurable = Vec::new();
        let mut nonconfigurable = Vec::new();
        for k in target_keys {
            match self.get_own_property(t, &k)? {
                Some(d) if d.configurable == Some(false) => nonconfigurable.push(k),
                _ => configurable.push(k),
            }
        }
        if ext && nonconfigurable.is_empty() {
            return Ok(keys);
        }
        let mut unchecked = keys.clone();
        for k in &nonconfigurable {
            match unchecked.iter().position(|x| x == k) {
                Some(i) => {
                    unchecked.remove(i);
                }
                None => {
                    return self
                        .type_error("'ownKeys' on proxy: trap result did not include a non-configurable key")
                }
            }
        }
        if ext {
            return Ok(keys);
        }
        for k in &configurable {
            match unchecked.iter().position(|x| x == k) {
                Some(i) => {
                    unchecked.remove(i);
                }
                None => {
                    return self.type_error(
                        "'ownKeys' on proxy: trap result did not include a key of the non-extensible target",
                    )
                }
            }
        }
        if !unchecked.is_empty() {
            return self
                .type_error("'ownKeys' on proxy: trap returned extra keys for a non-extensible target");
        }
        Ok(keys)
    }

    /// [[Call]] and [[Construct]] (§10.5.12–13).
    pub(super) fn proxy_call(
        &mut self,
        o: ObjRef,
        this: Value,
        args: Vec<Value>,
        new_target: Option<ObjRef>,
    ) -> JsResult<Value> {
        let (t, h) = self.proxy_parts(o)?;
        match new_target {
            None => {
                let Some(trap) = self.trap(h, "apply")? else {
                    return self.call(&Value::Object(t), this, args);
                };
                let a = self.new_array(args);
                self.call(&trap, Value::Object(h), vec![Value::Object(t), this, Value::Object(a)])
            }
            Some(nt) => {
                let Some(trap) = self.trap(h, "construct")? else { return self.construct(t, args, Some(nt)) };
                let a = self.new_array(args);
                let r = self.call(
                    &trap,
                    Value::Object(h),
                    vec![Value::Object(t), Value::Object(a), Value::Object(nt)],
                )?;
                if !matches!(r, Value::Object(_)) {
                    return self.type_error("'construct' on proxy: trap returned non-object");
                }
                Ok(r)
            }
        }
    }
}

/// The Proxy constructor and Proxy.revocable (§28.2).
pub(super) fn install(vm: &mut Vm) {
    fn create(vm: &mut Vm, t: &Value, h: &Value) -> JsResult<ObjRef> {
        let (Value::Object(t), Value::Object(h)) = (t, h) else {
            return vm.type_error("Cannot create proxy with a non-object as target or handler");
        };
        let callable =
            vm.heap.obj(*t).is_callable() || matches!(&vm.heap.obj(*t).kind, Kind::Proxy(p) if p.callable);
        let constructor = vm.is_constructor(&Value::Object(*t));
        Ok(vm.heap.alloc(Object::new(
            None,
            Kind::Proxy(Box::new(ProxyData { target: Some(*t), handler: Some(*h), callable, constructor })),
        )))
    }
    let c = super::realm::native_with(
        vm,
        "Proxy",
        2,
        |vm, c| {
            if c.new_target.is_none() {
                return vm.type_error("Constructor Proxy requires 'new'");
            }
            Ok(Value::Object(create(vm, &c.arg(0), &c.arg(1))?))
        },
        true,
        Vec::new(),
    );
    let g = vm.realm.global;
    super::realm::value(vm, g, "Proxy", Value::Object(c));
    super::realm::method(vm, c, "revocable", 2, |vm, c| {
        let p = create(vm, &c.arg(0), &c.arg(1))?;
        let revoke = super::realm::native_with(
            vm,
            "",
            0,
            |vm, c| {
                let slot = match &vm.heap.obj(c.callee).kind {
                    Kind::Native(n) => n.slots.first().cloned(),
                    _ => None,
                };
                if let Some(Value::Object(p)) = slot {
                    if let Kind::Proxy(d) = &mut vm.heap.obj_mut(p).kind {
                        d.target = None;
                        d.handler = None;
                    }
                    if let Kind::Native(n) = &mut vm.heap.obj_mut(c.callee).kind {
                        n.slots[0] = Value::Null;
                    }
                }
                Ok(Value::Undefined)
            },
            false,
            vec![Value::Object(p)],
        );
        let o = vm.new_object();
        vm.create_data_property_or_throw(o, PropKey::lit("proxy"), Value::Object(p))?;
        vm.create_data_property_or_throw(o, PropKey::lit("revoke"), Value::Object(revoke))?;
        Ok(Value::Object(o))
    });
}

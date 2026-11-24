//! Host (platform) objects, for embedders such as a browser's Web APIs (WebIDL §3.7).
//!
//! A platform object is `Kind::Host`: the host's interface id (`class`), a payload (`id`,
//! e.g. a DOM node's index) and slots for values the host keeps on it (traced). Natives reach the host's state through `Vm::host`. Objects marked
//! `exotic` are WebIDL legacy platform objects (§3.9): their indexed and named properties come
//! from the host's [`HostExotic`] hooks before their ordinary own properties. Host tables that
//! hold objects (wrapper caches) report them to the collector through `Vm::host_trace`.
//!
//! Several realms of one VM may each have their own host state (a browser's frames, each with
//! a Window): a platform object belongs to the realm it was made in, `host_data` sees it only
//! from that realm (natives of another realm treat it as a foreign object), and the exotic
//! hooks run in the object's realm.

use super::heap::{Kind, ObjRef, Object};
use super::object::PropDesc;
use super::value::{PropKey, Value};
use super::{JsResult, Vm};

/// A platform object's host data.
pub struct HostObj {
    pub class: u32,
    pub id: u64,
    /// A legacy platform object with the host's [`HostExotic`] properties.
    pub exotic: bool,
    pub slots: Vec<Value>,
    /// The realm it was made in.
    pub realm: u32,
}

/// Exotic internal methods of legacy platform objects. Each returns `None` to let the
/// ordinary behaviour run.
pub trait HostExotic {
    /// LegacyPlatformObjectGetOwnProperty (§3.9.1) for a key the host supports.
    fn get_own(&self, vm: &mut Vm, o: ObjRef, class: u32, id: u64, k: &PropKey)
        -> JsResult<Option<PropDesc>>;
    /// The supported indexed then named property keys (§3.9.6 [[OwnPropertyKeys]]).
    fn own_keys(&self, vm: &mut Vm, o: ObjRef, class: u32, id: u64) -> JsResult<Vec<PropKey>>;
    /// [[DefineOwnProperty]] (§3.9.3) for a supported key.
    fn define(
        &self,
        vm: &mut Vm,
        o: ObjRef,
        class: u32,
        id: u64,
        k: &PropKey,
        d: &PropDesc,
    ) -> JsResult<Option<bool>>;
    /// [[Delete]] (§3.9.4) for a supported key.
    fn delete(&self, vm: &mut Vm, o: ObjRef, class: u32, id: u64, k: &PropKey) -> JsResult<Option<bool>>;
    /// [[Set]] (§3.9.2) through an indexed or named setter.
    fn set(
        &self,
        vm: &mut Vm,
        o: ObjRef,
        class: u32,
        id: u64,
        k: &PropKey,
        v: &Value,
    ) -> JsResult<Option<bool>>;
}

impl Vm {
    /// A new platform object with interface `class`, payload `id` and prototype `proto`.
    pub fn new_host_object(&mut self, proto: Option<ObjRef>, class: u32, id: u64, exotic: bool) -> ObjRef {
        self.heap.alloc(Object::new(
            proto,
            Kind::Host(Box::new(HostObj { class, id, exotic, slots: Vec::new(), realm: self.cur_realm })),
        ))
    }

    /// The (class, id) of a platform object of the current realm (None for other values,
    /// including platform objects of other realms, whose ids mean nothing to this realm's host).
    pub fn host_data(&self, v: &Value) -> Option<(u32, u64)> {
        match v {
            Value::Object(o) => match &self.heap.obj(*o).kind {
                Kind::Host(h) if h.realm == self.cur_realm => Some((h.class, h.id)),
                _ => None,
            },
            _ => None,
        }
    }

    /// The realm a platform object was made in.
    pub fn host_realm(&self, o: ObjRef) -> Option<u32> {
        match &self.heap.obj(o).kind {
            Kind::Host(h) => Some(h.realm),
            _ => None,
        }
    }

    /// The host state of the current realm, if it is a `T`: the realm's own, else the VM's.
    pub fn host<T: 'static>(&self) -> Option<std::rc::Rc<T>> {
        let h = match &self.realm.host {
            Some(w) => w.upgrade()?,
            None => self.host.clone()?,
        };
        h.downcast::<T>().ok()
    }

    /// The host state of realm `id` (None if it was dropped or has none of type `T`).
    pub fn realm_host<T: 'static>(&self, id: u32) -> Option<std::rc::Rc<T>> {
        if self.realm_dropped(id) {
            return None;
        }
        let r = if id == self.cur_realm { &self.realm } else { self.realms.get(id as usize)?.as_ref()? };
        let h = match &r.host {
            Some(w) => w.upgrade()?,
            None => self.host.clone()?,
        };
        h.downcast::<T>().ok()
    }

    /// Set the current realm's own host state (held weakly: the host owns it).
    pub fn set_realm_host(&mut self, h: std::rc::Weak<dyn std::any::Any>) {
        self.realm.host = Some(h);
    }

    /// An exotic platform object's hooks, (class, id) and realm (None if the realm is gone).
    pub(super) fn host_exotic_of(&self, o: ObjRef) -> Option<(std::rc::Rc<dyn HostExotic>, u32, u64, u32)> {
        match &self.heap.obj(o).kind {
            Kind::Host(h) if h.exotic && !self.realm_dropped(h.realm) => {
                Some((self.host_exotic.clone()?, h.class, h.id, h.realm))
            }
            _ => None,
        }
    }

    /// A platform object's host data.
    pub fn host_obj(&self, o: ObjRef) -> Option<&HostObj> {
        match &self.heap.obj(o).kind {
            Kind::Host(h) => Some(h),
            _ => None,
        }
    }

    pub fn host_obj_mut(&mut self, o: ObjRef) -> Option<&mut HostObj> {
        match &mut self.heap.obj_mut(o).kind {
            Kind::Host(h) => Some(h),
            _ => None,
        }
    }
}

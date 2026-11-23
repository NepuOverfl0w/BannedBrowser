//! Host (platform) objects, for embedders such as a browser's Web APIs (WebIDL §3.7).
//!
//! A platform object is `Kind::Host`: the host's interface id (`class`), a payload (`id`,
//! e.g. a DOM node's index) and slots for values the host keeps on it (traced). Natives reach the host's state through `Vm::host`. Objects marked
//! `exotic` are WebIDL legacy platform objects (§3.9): their indexed and named properties come
//! from the host's [`HostExotic`] hooks before their ordinary own properties. Host tables that
//! hold objects (wrapper caches) report them to the collector through `Vm::host_trace`.

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
        self.heap
            .alloc(Object::new(proto, Kind::Host(Box::new(HostObj { class, id, exotic, slots: Vec::new() }))))
    }

    /// The (class, id) of a platform object.
    pub fn host_data(&self, v: &Value) -> Option<(u32, u64)> {
        match v {
            Value::Object(o) => match &self.heap.obj(*o).kind {
                Kind::Host(h) => Some((h.class, h.id)),
                _ => None,
            },
            _ => None,
        }
    }

    /// The host state, if it is a `T`.
    pub fn host<T: 'static>(&self) -> Option<std::rc::Rc<T>> {
        self.host.clone()?.downcast::<T>().ok()
    }

    /// An exotic platform object's (class, id) and the host's hooks.
    pub(super) fn host_exotic_of(&self, o: ObjRef) -> Option<(std::rc::Rc<dyn HostExotic>, u32, u64)> {
        match &self.heap.obj(o).kind {
            Kind::Host(h) if h.exotic => Some((self.host_exotic.clone()?, h.class, h.id)),
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

//! Geometry Interfaces Module Level 1: DOMPoint(ReadOnly) (§2), DOMRect's static `fromRect`
//! (§3), DOMQuad (§5) and DOMMatrix(ReadOnly) (§6), including parsing a CSS transform list
//! into a matrix (§6.2 "parse a string into an abstract matrix").
//!
//! A point keeps (x, y, z, w) in its slots; a matrix keeps its 16 entries in the order
//! m11, m12, m13, m14, m21, … m44 (column by column, as toFloat64Array returns them) and its
//! is2D flag; a quad keeps its four DOMPoint objects.

use bb_js::vm::heap::{NativeCall, ObjRef};
use bb_js::vm::PropKey;
use bb_js::{JsResult, Value, Vm};

use crate::class::C;
use crate::exception::throw;
use crate::host::*;
use crate::idl::{attr_with, callee_slots, op};

type R = JsResult<Value>;

/// A 4×4 matrix: `m[c][r]` is column c, row r (so m11 = m[0][0], m21 = m[1][0], m41 = m[3][0]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat {
    pub m: [[f64; 4]; 4],
    pub is_2d: bool,
}

impl Mat {
    pub const IDENTITY: Mat = Mat {
        m: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]],
        is_2d: true,
    };

    pub fn from_2d(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Mat {
        let mut m = Mat::IDENTITY;
        m.m[0][0] = a;
        m.m[0][1] = b;
        m.m[1][0] = c;
        m.m[1][1] = d;
        m.m[3][0] = e;
        m.m[3][1] = f;
        m
    }

    /// From 16 values in column order (m11, m12, … m44).
    fn from_16(v: &[f64]) -> Mat {
        let mut m = Mat::IDENTITY;
        for (i, x) in v.iter().take(16).enumerate() {
            m.m[i / 4][i % 4] = *x;
        }
        m.is_2d = false;
        m
    }

    fn values(&self) -> [f64; 16] {
        let mut out = [0.0; 16];
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.m[i / 4][i % 4];
        }
        out
    }

    /// `self × o`.
    pub fn mul(&self, o: &Mat) -> Mat {
        let mut r = [[0.0; 4]; 4];
        for (c, col) in r.iter_mut().enumerate() {
            for (row, x) in col.iter_mut().enumerate() {
                *x = (0..4).map(|k| self.m[k][row] * o.m[c][k]).sum();
            }
        }
        Mat { m: r, is_2d: self.is_2d && o.is_2d }
    }

    fn translate(x: f64, y: f64, z: f64) -> Mat {
        let mut m = Mat::IDENTITY;
        m.m[3][0] = x;
        m.m[3][1] = y;
        m.m[3][2] = z;
        m.is_2d = z == 0.0;
        m
    }

    fn scale(x: f64, y: f64, z: f64) -> Mat {
        let mut m = Mat::IDENTITY;
        m.m[0][0] = x;
        m.m[1][1] = y;
        m.m[2][2] = z;
        m.is_2d = z == 1.0;
        m
    }

    /// CSS Transforms 2 §13 rotate3d(x, y, z, angle in degrees).
    fn rotate_axis(x: f64, y: f64, z: f64, deg: f64) -> Mat {
        let len = (x * x + y * y + z * z).sqrt();
        let mut m = Mat::IDENTITY;
        m.is_2d = x == 0.0 && y == 0.0;
        if len == 0.0 || !len.is_finite() {
            return m;
        }
        let (x, y, z) = (x / len, y / len, z / len);
        let a = deg.to_radians();
        let sc = (a / 2.0).sin() * (a / 2.0).cos();
        let sq = (a / 2.0).sin() * (a / 2.0).sin();
        m.m[0][0] = 1.0 - 2.0 * (y * y + z * z) * sq;
        m.m[0][1] = 2.0 * (x * y * sq + z * sc);
        m.m[0][2] = 2.0 * (x * z * sq - y * sc);
        m.m[1][0] = 2.0 * (x * y * sq - z * sc);
        m.m[1][1] = 1.0 - 2.0 * (x * x + z * z) * sq;
        m.m[1][2] = 2.0 * (y * z * sq + x * sc);
        m.m[2][0] = 2.0 * (x * z * sq + y * sc);
        m.m[2][1] = 2.0 * (y * z * sq - x * sc);
        m.m[2][2] = 1.0 - 2.0 * (x * x + y * y) * sq;
        m
    }

    fn skew(ax_deg: f64, ay_deg: f64) -> Mat {
        let mut m = Mat::IDENTITY;
        m.m[1][0] = ax_deg.to_radians().tan();
        m.m[0][1] = ay_deg.to_radians().tan();
        m
    }

    fn is_identity(&self) -> bool {
        self.m == Mat::IDENTITY.m
    }

    /// The inverse, or None when it isn't invertible.
    pub fn inverse(&self) -> Option<Mat> {
        if self.is_2d {
            let [a, b, c, d, e, f] =
                [self.m[0][0], self.m[0][1], self.m[1][0], self.m[1][1], self.m[3][0], self.m[3][1]];
            let det = a * d - b * c;
            if det == 0.0 || !det.is_finite() {
                return None;
            }
            return Some(Mat::from_2d(
                d / det,
                -b / det,
                -c / det,
                a / det,
                (c * f - d * e) / det,
                (b * e - a * f) / det,
            ));
        }
        // The adjugate divided by the determinant (cofactor expansion), on the 16 values in
        // column order.
        let m = self.values();
        let mut inv = [0.0f64; 16];
        inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15]
            + m[9] * m[7] * m[14]
            + m[13] * m[6] * m[11]
            - m[13] * m[7] * m[10];
        inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15]
            - m[8] * m[7] * m[14]
            - m[12] * m[6] * m[11]
            + m[12] * m[7] * m[10];
        inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15]
            + m[8] * m[7] * m[13]
            + m[12] * m[5] * m[11]
            - m[12] * m[7] * m[9];
        inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14]
            - m[8] * m[6] * m[13]
            - m[12] * m[5] * m[10]
            + m[12] * m[6] * m[9];
        inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15]
            - m[9] * m[3] * m[14]
            - m[13] * m[2] * m[11]
            + m[13] * m[3] * m[10];
        inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15]
            + m[8] * m[3] * m[14]
            + m[12] * m[2] * m[11]
            - m[12] * m[3] * m[10];
        inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15]
            - m[8] * m[3] * m[13]
            - m[12] * m[1] * m[11]
            + m[12] * m[3] * m[9];
        inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14]
            + m[8] * m[2] * m[13]
            + m[12] * m[1] * m[10]
            - m[12] * m[2] * m[9];
        inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15]
            + m[5] * m[3] * m[14]
            + m[13] * m[2] * m[7]
            - m[13] * m[3] * m[6];
        inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15]
            - m[4] * m[3] * m[14]
            - m[12] * m[2] * m[7]
            + m[12] * m[3] * m[6];
        inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15]
            + m[4] * m[3] * m[13]
            + m[12] * m[1] * m[7]
            - m[12] * m[3] * m[5];
        inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14]
            - m[4] * m[2] * m[13]
            - m[12] * m[1] * m[6]
            + m[12] * m[2] * m[5];
        inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11]
            - m[5] * m[3] * m[10]
            - m[9] * m[2] * m[7]
            + m[9] * m[3] * m[6];
        inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11]
            + m[4] * m[3] * m[10]
            + m[8] * m[2] * m[7]
            - m[8] * m[3] * m[6];
        inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11]
            - m[4] * m[3] * m[9]
            - m[8] * m[1] * m[7]
            + m[8] * m[3] * m[5];
        inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10]
            + m[4] * m[2] * m[9]
            + m[8] * m[1] * m[6]
            - m[8] * m[2] * m[5];
        let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let out: Vec<f64> = inv.iter().map(|x| x / det).collect();
        if out.iter().any(|v| !v.is_finite()) {
            return None;
        }
        Some(Mat::from_16(&out))
    }

    fn transform(&self, p: [f64; 4]) -> [f64; 4] {
        let mut out = [0.0; 4];
        for (r, o) in out.iter_mut().enumerate() {
            *o = (0..4).map(|c| self.m[c][r] * p[c]).sum();
        }
        out
    }
}

// ---- slots ----

fn nums(vm: &Vm, o: ObjRef, n: usize) -> Vec<f64> {
    let mut out = vec![0.0; n];
    if let Some(h) = vm.host_obj(o) {
        for (i, x) in out.iter_mut().enumerate() {
            if let Some(Value::Number(v)) = h.slots.get(i) {
                *x = *v;
            }
        }
    }
    out
}

fn set_nums(vm: &mut Vm, o: ObjRef, v: &[f64]) {
    if let Some(h) = vm.host_obj_mut(o) {
        h.slots = v.iter().map(|&x| Value::Number(x)).collect();
    }
}

fn point_of(vm: &mut Vm, v: &Value) -> JsResult<(ObjRef, [f64; 4])> {
    this_of(vm, v, C::DOMPointReadOnly)?;
    let Value::Object(o) = v else { return vm.type_error("Illegal invocation") };
    let n = nums(vm, *o, 4);
    Ok((*o, [n[0], n[1], n[2], n[3]]))
}

fn matrix_of(vm: &mut Vm, v: &Value) -> JsResult<(ObjRef, Mat)> {
    this_of(vm, v, C::DOMMatrixReadOnly)?;
    let Value::Object(o) = v else { return vm.type_error("Illegal invocation") };
    let n = nums(vm, *o, 16);
    let is_2d = vm.host_obj(*o).is_some_and(|h| matches!(h.slots.get(16), Some(Value::Bool(true))));
    let mut m = Mat::from_16(&n);
    m.is_2d = is_2d;
    Ok((*o, m))
}

fn store(vm: &mut Vm, o: ObjRef, m: &Mat) {
    if let Some(h) = vm.host_obj_mut(o) {
        h.slots = m.values().iter().map(|&x| Value::Number(x)).collect();
        h.slots.push(Value::Bool(m.is_2d));
    }
}

pub fn new_point(vm: &mut Vm, h: &Host, class: C, p: [f64; 4]) -> Value {
    let o = vm.new_host_object(Some(h.proto(class)), class as u32, 0, false);
    set_nums(vm, o, &p);
    Value::Object(o)
}

pub fn new_matrix(vm: &mut Vm, h: &Host, class: C, m: &Mat) -> Value {
    let o = vm.new_host_object(Some(h.proto(class)), class as u32, 0, false);
    store(vm, o, m);
    Value::Object(o)
}

fn new_rect(vm: &mut Vm, h: &Host, class: C, r: [f64; 4]) -> Value {
    let o = vm.new_host_object(Some(h.proto(class)), class as u32, 0, false);
    set_nums(vm, o, &r);
    Value::Object(o)
}

/// The class a constructor or static method was called on (DOMPoint vs DOMPointReadOnly).
fn class_of_callee(vm: &mut Vm, c: &NativeCall, ro: C, rw: C) -> C {
    match callee_slots(vm, c.callee).first() {
        Some(Value::Number(n)) if *n as u32 == ro as u32 => ro,
        Some(Value::Number(_)) => rw,
        _ => match host(vm) {
            Ok(h) if c.callee == h.ctor(ro) => ro,
            _ => rw,
        },
    }
}

fn num_or(vm: &mut Vm, v: &Value, d: f64) -> JsResult<f64> {
    if v.is_undefined() {
        Ok(d)
    } else {
        vm.to_number(v)
    }
}

/// A dictionary member as an unrestricted double, or None when absent.
fn member(vm: &mut Vm, d: &Value, k: &'static str) -> JsResult<Option<f64>> {
    match dict_get(vm, d, k)? {
        Value::Undefined => Ok(None),
        v => vm.to_number(&v).map(Some),
    }
}

fn dict_check(vm: &mut Vm, d: &Value) -> JsResult<()> {
    match d {
        Value::Undefined | Value::Null | Value::Object(_) => Ok(()),
        _ => vm.type_error("the argument must be a dictionary"),
    }
}

/// A DOMPointInit (§2).
fn point_init(vm: &mut Vm, d: &Value) -> JsResult<[f64; 4]> {
    dict_check(vm, d)?;
    Ok([
        member(vm, d, "x")?.unwrap_or(0.0),
        member(vm, d, "y")?.unwrap_or(0.0),
        member(vm, d, "z")?.unwrap_or(0.0),
        member(vm, d, "w")?.unwrap_or(1.0),
    ])
}

// ---- DOMPoint (§2) ----

pub fn point_ctor(vm: &mut Vm, c: NativeCall) -> R {
    if c.new_target.is_none() {
        return vm.type_error("DOMPoint requires 'new'");
    }
    let p = [
        num_or(vm, &c.arg(0), 0.0)?,
        num_or(vm, &c.arg(1), 0.0)?,
        num_or(vm, &c.arg(2), 0.0)?,
        num_or(vm, &c.arg(3), 1.0)?,
    ];
    let h = host(vm)?;
    let class = if c.callee == h.ctor(C::DOMPointReadOnly) { C::DOMPointReadOnly } else { C::DOMPoint };
    Ok(new_point(vm, &h, class, p))
}

pub fn setup_point_readonly(vm: &mut Vm, proto: ObjRef, ctor: ObjRef) {
    for (i, n) in ["x", "y", "z", "w"].iter().enumerate() {
        attr_with(
            vm,
            proto,
            n,
            |vm, c| {
                let (_, p) = point_of(vm, &c.this)?;
                let i = slot_index(vm, &c);
                Ok(Value::Number(p[i]))
            },
            None,
            vec![Value::Number(i as f64)],
        );
    }
    static_from_point(vm, ctor, C::DOMPointReadOnly);
    op(vm, proto, "matrixTransform", 0, |vm, c| {
        let (_, p) = point_of(vm, &c.this)?;
        let m = matrix_init(vm, &c.arg(0))?;
        let h = host(vm)?;
        Ok(new_point(vm, &h, C::DOMPoint, m.transform(p)))
    });
    op(vm, proto, "toJSON", 0, |vm, c| {
        let (_, p) = point_of(vm, &c.this)?;
        let o = vm.new_object();
        for (k, v) in ["x", "y", "z", "w"].iter().zip(p) {
            bb_js::vm::realm::value(vm, o, k, Value::Number(v));
        }
        Ok(Value::Object(o))
    });
}

fn static_from_point(vm: &mut Vm, ctor: ObjRef, class: C) {
    let f = bb_js::vm::realm::native_with(
        vm,
        "fromPoint",
        0,
        |vm, c| {
            let class = class_of_callee(vm, &c, C::DOMPointReadOnly, C::DOMPoint);
            let p = point_init(vm, &c.arg(0))?;
            let h = host(vm)?;
            Ok(new_point(vm, &h, class, p))
        },
        false,
        vec![Value::Number(class as u32 as f64)],
    );
    put_method(vm, ctor, "fromPoint", f);
}

fn put_method(vm: &mut Vm, on: ObjRef, name: &str, f: ObjRef) {
    vm.heap.obj_mut(on).props.insert(
        PropKey::named(name),
        bb_js::vm::heap::Prop {
            slot: bb_js::vm::heap::Slot::Data(Value::Object(f)),
            writable: true,
            enumerable: true,
            configurable: true,
        },
    );
}

fn slot_index(vm: &mut Vm, c: &NativeCall) -> usize {
    match callee_slots(vm, c.callee).first() {
        Some(Value::Number(n)) => *n as usize,
        _ => 0,
    }
}

pub fn setup_point(vm: &mut Vm, proto: ObjRef, ctor: ObjRef) {
    for (i, n) in ["x", "y", "z", "w"].iter().enumerate() {
        attr_with(
            vm,
            proto,
            n,
            |vm, c| {
                let (_, p) = point_of(vm, &c.this)?;
                let i = slot_index(vm, &c);
                Ok(Value::Number(p[i]))
            },
            Some(|vm, c| {
                let v = vm.to_number(&c.arg(0))?;
                let (o, mut p) = point_of(vm, &c.this)?;
                let i = slot_index(vm, &c);
                p[i] = v;
                set_nums(vm, o, &p);
                Ok(Value::Undefined)
            }),
            vec![Value::Number(i as f64)],
        );
    }
    static_from_point(vm, ctor, C::DOMPoint);
}

// ---- DOMRect.fromRect (§3) ----

pub fn setup_rect_statics(vm: &mut Vm, ctor: ObjRef, class: C) {
    let f = bb_js::vm::realm::native_with(
        vm,
        "fromRect",
        0,
        |vm, c| {
            let class = class_of_callee(vm, &c, C::DOMRectReadOnly, C::DOMRect);
            let r = rect_init(vm, &c.arg(0))?;
            let h = host(vm)?;
            Ok(new_rect(vm, &h, class, r))
        },
        false,
        vec![Value::Number(class as u32 as f64)],
    );
    put_method(vm, ctor, "fromRect", f);
}

fn rect_init(vm: &mut Vm, d: &Value) -> JsResult<[f64; 4]> {
    dict_check(vm, d)?;
    Ok([
        member(vm, d, "x")?.unwrap_or(0.0),
        member(vm, d, "y")?.unwrap_or(0.0),
        member(vm, d, "width")?.unwrap_or(0.0),
        member(vm, d, "height")?.unwrap_or(0.0),
    ])
}

/// NaN-safe min/max (§3: the rect's edges propagate NaN).
fn min4(v: [f64; 4]) -> f64 {
    if v.iter().any(|x| x.is_nan()) {
        return f64::NAN;
    }
    v.iter().copied().fold(f64::INFINITY, f64::min)
}

fn max4(v: [f64; 4]) -> f64 {
    if v.iter().any(|x| x.is_nan()) {
        return f64::NAN;
    }
    v.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

// ---- DOMQuad (§5) ----

fn quad_points(vm: &mut Vm, v: &Value) -> JsResult<[ObjRef; 4]> {
    this_of(vm, v, C::DOMQuad)?;
    let Value::Object(o) = v else { return vm.type_error("Illegal invocation") };
    let mut out = [*o; 4];
    if let Some(h) = vm.host_obj(*o) {
        for (i, p) in out.iter_mut().enumerate() {
            if let Some(Value::Object(x)) = h.slots.get(i) {
                *p = *x;
            }
        }
    }
    Ok(out)
}

fn new_quad(vm: &mut Vm, h: &Host, pts: [[f64; 4]; 4]) -> Value {
    let o = vm.new_host_object(Some(h.proto(C::DOMQuad)), C::DOMQuad as u32, 0, false);
    let points: Vec<Value> = pts.iter().map(|p| new_point(vm, h, C::DOMPoint, *p)).collect();
    if let Some(ho) = vm.host_obj_mut(o) {
        ho.slots = points;
    }
    Value::Object(o)
}

pub fn quad_ctor(vm: &mut Vm, c: NativeCall) -> R {
    if c.new_target.is_none() {
        return vm.type_error("DOMQuad requires 'new'");
    }
    let mut pts = [[0.0; 4]; 4];
    for (i, p) in pts.iter_mut().enumerate() {
        *p = point_init(vm, &c.arg(i))?;
    }
    let h = host(vm)?;
    Ok(new_quad(vm, &h, pts))
}

fn rect_quad(r: [f64; 4]) -> [[f64; 4]; 4] {
    let [x, y, w, h] = r;
    [[x, y, 0.0, 1.0], [x + w, y, 0.0, 1.0], [x + w, y + h, 0.0, 1.0], [x, y + h, 0.0, 1.0]]
}

pub fn setup_quad(vm: &mut Vm, proto: ObjRef, ctor: ObjRef) {
    for (i, n) in ["p1", "p2", "p3", "p4"].iter().enumerate() {
        attr_with(
            vm,
            proto,
            n,
            |vm, c| {
                let pts = quad_points(vm, &c.this)?;
                Ok(Value::Object(pts[slot_index(vm, &c)]))
            },
            None,
            vec![Value::Number(i as f64)],
        );
    }
    op(vm, ctor, "fromRect", 0, |vm, c| {
        let r = rect_init(vm, &c.arg(0))?;
        let h = host(vm)?;
        Ok(new_quad(vm, &h, rect_quad(r)))
    });
    op(vm, ctor, "fromQuad", 0, |vm, c| {
        let d = c.arg(0);
        dict_check(vm, &d)?;
        let mut pts = [[0.0; 4]; 4];
        for (i, k) in ["p1", "p2", "p3", "p4"].iter().enumerate() {
            let p = dict_get(vm, &d, k)?;
            pts[i] = point_init(vm, &p)?;
        }
        let h = host(vm)?;
        Ok(new_quad(vm, &h, pts))
    });
    op(vm, proto, "getBounds", 0, |vm, c| {
        let pts = quad_points(vm, &c.this)?;
        let p: Vec<[f64; 4]> = pts
            .iter()
            .map(|o| {
                let n = nums(vm, *o, 4);
                [n[0], n[1], n[2], n[3]]
            })
            .collect();
        let xs = [p[0][0], p[1][0], p[2][0], p[3][0]];
        let ys = [p[0][1], p[1][1], p[2][1], p[3][1]];
        let (l, t) = (min4(xs), min4(ys));
        let h = host(vm)?;
        Ok(new_rect(vm, &h, C::DOMRect, [l, t, max4(xs) - l, max4(ys) - t]))
    });
    op(vm, proto, "toJSON", 0, |vm, c| {
        let pts = quad_points(vm, &c.this)?;
        let o = vm.new_object();
        for (k, p) in ["p1", "p2", "p3", "p4"].iter().zip(pts) {
            bb_js::vm::realm::value(vm, o, k, Value::Object(p));
        }
        Ok(Value::Object(o))
    });
}

// ---- DOMMatrix (§6) ----

/// §6.1 "validate and fixup (2D)" a DOMMatrix2DInit, then the matrix it describes.
fn fixup_2d(vm: &mut Vm, d: &Value) -> JsResult<[f64; 6]> {
    dict_check(vm, d)?;
    let pairs = [
        ("a", "m11", 1.0),
        ("b", "m12", 0.0),
        ("c", "m21", 0.0),
        ("d", "m22", 1.0),
        ("e", "m41", 0.0),
        ("f", "m42", 0.0),
    ];
    let mut out = [0.0; 6];
    for (i, (short, long, def)) in pairs.iter().enumerate() {
        let s = member(vm, d, short)?;
        let l = member(vm, d, long)?;
        out[i] = match (s, l) {
            (Some(s), Some(l)) => {
                // SameValueZero.
                if !(s == l || (s.is_nan() && l.is_nan())) {
                    return vm.type_error(&format!("{short} and {long} disagree"));
                }
                l
            }
            (Some(s), None) => s,
            (None, Some(l)) => l,
            (None, None) => *def,
        };
    }
    Ok(out)
}

/// §6.1 "create a DOMMatrix from the dictionary" (a DOMMatrixInit, validated and fixed up).
fn matrix_init(vm: &mut Vm, d: &Value) -> JsResult<Mat> {
    let [a, b, c, dd, e, f] = fixup_2d(vm, d)?;
    let third = ["m13", "m14", "m23", "m24", "m31", "m32", "m34", "m43"];
    let mut extra = [0.0; 8];
    for (i, k) in third.iter().enumerate() {
        extra[i] = member(vm, d, k)?.unwrap_or(0.0);
    }
    let m33 = member(vm, d, "m33")?.unwrap_or(1.0);
    let m44 = member(vm, d, "m44")?.unwrap_or(1.0);
    let flat = extra.iter().all(|&x| x == 0.0) && m33 == 1.0 && m44 == 1.0;
    let is_2d = match dict_get(vm, d, "is2D")? {
        Value::Undefined => flat,
        v => {
            let b = vm.to_boolean(&v);
            if b && !flat {
                return vm.type_error("is2D is true but the matrix isn't 2D");
            }
            b
        }
    };
    let mut m = Mat::from_2d(a, b, c, dd, e, f);
    if !is_2d {
        m.m[0][2] = extra[0];
        m.m[0][3] = extra[1];
        m.m[1][2] = extra[2];
        m.m[1][3] = extra[3];
        m.m[2][0] = extra[4];
        m.m[2][1] = extra[5];
        m.m[2][3] = extra[6];
        m.m[3][2] = extra[7];
        m.m[2][2] = m33;
        m.m[3][3] = m44;
    }
    m.is_2d = is_2d;
    Ok(m)
}

/// A DOMMatrix2DInit as a matrix (for canvas and Path2D).
pub fn matrix_2d_init(vm: &mut Vm, d: &Value) -> JsResult<Mat> {
    let [a, b, c, dd, e, f] = fixup_2d(vm, d)?;
    Ok(Mat::from_2d(a, b, c, dd, e, f))
}

pub fn matrix_ctor(vm: &mut Vm, c: NativeCall) -> R {
    if c.new_target.is_none() {
        return vm.type_error("DOMMatrix requires 'new'");
    }
    let h = host(vm)?;
    let class = if c.callee == h.ctor(C::DOMMatrixReadOnly) { C::DOMMatrixReadOnly } else { C::DOMMatrix };
    let init = c.arg(0);
    let m = match &init {
        Value::Undefined => Mat::IDENTITY,
        Value::Object(o) if !vm.is_callable(&init) && is_iterable(vm, *o) => {
            let v = sequence(vm, &init)?;
            match v.len() {
                6 => Mat::from_2d(v[0], v[1], v[2], v[3], v[4], v[5]),
                16 => Mat::from_16(&v),
                n => return vm.type_error(&format!("a matrix needs 6 or 16 numbers, not {n}")),
            }
        }
        _ => {
            if h.workers.borrow().link.is_some() {
                return vm.type_error("DOMMatrix can't parse CSS in a worker");
            }
            let s = rstr(vm, &init)?;
            match parse_transform_list(&s) {
                Some(m) => m,
                None => return throw(vm, "SyntaxError", &format!("'{s}' is not a transform list")),
            }
        }
    };
    Ok(new_matrix(vm, &h, class, &m))
}

fn is_iterable(vm: &mut Vm, o: ObjRef) -> bool {
    let key = PropKey::Sym(vm.realm.sym_iterator.clone());
    vm.get(o, &key).is_ok_and(|f| vm.is_callable(&f))
}

fn sequence(vm: &mut Vm, v: &Value) -> JsResult<Vec<f64>> {
    let items = vm.iterate_to_vec(v)?;
    let mut out = Vec::with_capacity(items.len());
    for x in items {
        out.push(vm.to_number(&x)?);
    }
    Ok(out)
}

const FIELDS: [(&str, usize, usize); 22] = [
    ("a", 0, 0),
    ("b", 0, 1),
    ("c", 1, 0),
    ("d", 1, 1),
    ("e", 3, 0),
    ("f", 3, 1),
    ("m11", 0, 0),
    ("m12", 0, 1),
    ("m13", 0, 2),
    ("m14", 0, 3),
    ("m21", 1, 0),
    ("m22", 1, 1),
    ("m23", 1, 2),
    ("m24", 1, 3),
    ("m31", 2, 0),
    ("m32", 2, 1),
    ("m33", 2, 2),
    ("m34", 2, 3),
    ("m41", 3, 0),
    ("m42", 3, 1),
    ("m43", 3, 2),
    ("m44", 3, 3),
];

fn field_get(vm: &mut Vm, c: NativeCall) -> R {
    let (_, m) = matrix_of(vm, &c.this)?;
    let (_, col, row) = FIELDS[slot_index(vm, &c)];
    Ok(Value::Number(m.m[col][row]))
}

fn field_set(vm: &mut Vm, c: NativeCall) -> R {
    let v = vm.to_number(&c.arg(0))?;
    let (o, mut m) = matrix_of(vm, &c.this)?;
    let i = slot_index(vm, &c);
    let (name, col, row) = FIELDS[i];
    m.m[col][row] = v;
    // Setting a 3D-only entry away from its identity value makes the matrix 3D.
    let identity = if col == row { 1.0 } else { 0.0 };
    let only_3d =
        matches!(name, "m13" | "m14" | "m23" | "m24" | "m31" | "m32" | "m33" | "m34" | "m43" | "m44");
    if only_3d && v != identity {
        m.is_2d = false;
    }
    store(vm, o, &m);
    Ok(Value::Undefined)
}

/// The number arguments of a method, with defaults.
fn args(vm: &mut Vm, c: &NativeCall, defs: &[f64]) -> JsResult<Vec<f64>> {
    let mut out = Vec::with_capacity(defs.len());
    for (i, d) in defs.iter().enumerate() {
        out.push(num_or(vm, &c.arg(i), *d)?);
    }
    Ok(out)
}

/// The transform operations shared by the immutable methods (on a copy) and the `…Self`
/// methods (in place): (name, how to apply it).
type Apply = fn(&mut Vm, &NativeCall, Mat) -> JsResult<Mat>;

const OPS: [(&str, Apply); 13] = [
    ("translate", |vm, c, m| {
        let a = args(vm, c, &[0.0, 0.0, 0.0])?;
        Ok(m.mul(&Mat::translate(a[0], a[1], a[2])))
    }),
    ("scale", |vm, c, m| {
        let sx = num_or(vm, &c.arg(0), 1.0)?;
        let sy = num_or(vm, &c.arg(1), sx)?;
        let a = [
            sx,
            sy,
            num_or(vm, &c.arg(2), 1.0)?,
            num_or(vm, &c.arg(3), 0.0)?,
            num_or(vm, &c.arg(4), 0.0)?,
            num_or(vm, &c.arg(5), 0.0)?,
        ];
        Ok(scale_about(m, a))
    }),
    ("scale3d", |vm, c, m| {
        let a = args(vm, c, &[1.0, 0.0, 0.0, 0.0])?;
        Ok(scale_about(m, [a[0], a[0], a[0], a[1], a[2], a[3]]))
    }),
    ("rotate", |vm, c, m| {
        let mut a = args(vm, c, &[0.0, f64::NAN, f64::NAN])?;
        if c.arg(1).is_undefined() && c.arg(2).is_undefined() {
            a = vec![0.0, 0.0, a[0]];
        }
        let (x, y, z) = (
            a[0],
            if a[1].is_nan() && c.arg(1).is_undefined() { 0.0 } else { a[1] },
            if a[2].is_nan() && c.arg(2).is_undefined() { 0.0 } else { a[2] },
        );
        let r = m
            .mul(&Mat::rotate_axis(0.0, 0.0, 1.0, z))
            .mul(&Mat::rotate_axis(0.0, 1.0, 0.0, y))
            .mul(&Mat::rotate_axis(1.0, 0.0, 0.0, x));
        Ok(Mat { is_2d: m.is_2d && x == 0.0 && y == 0.0, ..r })
    }),
    ("rotateFromVector", |vm, c, m| {
        let a = args(vm, c, &[0.0, 0.0])?;
        let deg = if a[0] == 0.0 && a[1] == 0.0 { 0.0 } else { a[1].atan2(a[0]).to_degrees() };
        Ok(m.mul(&Mat::rotate_axis(0.0, 0.0, 1.0, deg)))
    }),
    ("rotateAxisAngle", |vm, c, m| {
        let a = args(vm, c, &[0.0, 0.0, 0.0, 0.0])?;
        Ok(m.mul(&Mat::rotate_axis(a[0], a[1], a[2], a[3])))
    }),
    ("skewX", |vm, c, m| {
        let a = args(vm, c, &[0.0])?;
        Ok(m.mul(&Mat::skew(a[0], 0.0)))
    }),
    ("skewY", |vm, c, m| {
        let a = args(vm, c, &[0.0])?;
        Ok(m.mul(&Mat::skew(0.0, a[0])))
    }),
    ("multiply", |vm, c, m| {
        let o = matrix_init(vm, &c.arg(0))?;
        Ok(m.mul(&o))
    }),
    ("preMultiply", |vm, c, m| {
        let o = matrix_init(vm, &c.arg(0))?;
        Ok(o.mul(&m))
    }),
    ("flipX", |_, _, m| Ok(m.mul(&Mat::from_2d(-1.0, 0.0, 0.0, 1.0, 0.0, 0.0)))),
    ("flipY", |_, _, m| Ok(m.mul(&Mat::from_2d(1.0, 0.0, 0.0, -1.0, 0.0, 0.0)))),
    ("invert", |_, _, m| Ok(m.inverse().unwrap_or(Mat { m: [[f64::NAN; 4]; 4], is_2d: false }))),
];

fn scale_about(m: Mat, a: [f64; 6]) -> Mat {
    let [sx, sy, sz, ox, oy, oz] = a;
    m.mul(&Mat::translate(ox, oy, oz)).mul(&Mat::scale(sx, sy, sz)).mul(&Mat::translate(-ox, -oy, -oz))
}

fn op_index(vm: &mut Vm, c: &NativeCall) -> usize {
    slot_index(vm, c)
}

pub fn setup_matrix_readonly(vm: &mut Vm, proto: ObjRef, ctor: ObjRef, worker: bool) {
    for (i, (name, ..)) in FIELDS.iter().enumerate() {
        attr_with(vm, proto, name, field_get, None, vec![Value::Number(i as f64)]);
    }
    crate::idl::attr(
        vm,
        proto,
        "is2D",
        |vm, c| {
            let (_, m) = matrix_of(vm, &c.this)?;
            Ok(Value::Bool(m.is_2d))
        },
        None,
    );
    crate::idl::attr(
        vm,
        proto,
        "isIdentity",
        |vm, c| {
            let (_, m) = matrix_of(vm, &c.this)?;
            Ok(Value::Bool(m.is_identity()))
        },
        None,
    );
    // The immutable operations return a new DOMMatrix (§6.5).
    for (i, (name, _)) in OPS.iter().enumerate() {
        let public = match *name {
            "invert" => "inverse",
            "preMultiply" => continue,
            n => n,
        };
        let f = bb_js::vm::realm::native_with(
            vm,
            public,
            0,
            |vm, c| {
                let (_, m) = matrix_of(vm, &c.this)?;
                let r = (OPS[op_index(vm, &c)].1)(vm, &c, m)?;
                let h = host(vm)?;
                Ok(new_matrix(vm, &h, C::DOMMatrix, &r))
            },
            false,
            vec![Value::Number(i as f64)],
        );
        put_method(vm, proto, public, f);
    }
    op(vm, proto, "scaleNonUniform", 0, |vm, c| {
        let (_, m) = matrix_of(vm, &c.this)?;
        let a = args(vm, &c, &[1.0, 1.0])?;
        let h = host(vm)?;
        Ok(new_matrix(vm, &h, C::DOMMatrix, &scale_about(m, [a[0], a[1], 1.0, 0.0, 0.0, 0.0])))
    });
    op(vm, proto, "transformPoint", 0, |vm, c| {
        let (_, m) = matrix_of(vm, &c.this)?;
        let p = point_init(vm, &c.arg(0))?;
        let h = host(vm)?;
        Ok(new_point(vm, &h, C::DOMPoint, m.transform(p)))
    });
    op(vm, proto, "toFloat32Array", 0, |vm, c| {
        let (_, m) = matrix_of(vm, &c.this)?;
        let bytes: Vec<u8> = m.values().iter().flat_map(|v| (*v as f32).to_le_bytes()).collect();
        typed_array(vm, "Float32Array", bytes)
    });
    op(vm, proto, "toFloat64Array", 0, |vm, c| {
        let (_, m) = matrix_of(vm, &c.this)?;
        let bytes: Vec<u8> = m.values().iter().flat_map(|v| v.to_le_bytes()).collect();
        typed_array(vm, "Float64Array", bytes)
    });
    op(vm, proto, "toJSON", 0, |vm, c| {
        let (_, m) = matrix_of(vm, &c.this)?;
        let o = vm.new_object();
        for (name, col, row) in FIELDS {
            bb_js::vm::realm::value(vm, o, name, Value::Number(m.m[col][row]));
        }
        bb_js::vm::realm::value(vm, o, "is2D", Value::Bool(m.is_2d));
        bb_js::vm::realm::value(vm, o, "isIdentity", Value::Bool(m.is_identity()));
        Ok(Value::Object(o))
    });
    if !worker {
        // §6.6 stringifier ([Exposed=Window]).
        op(vm, proto, "toString", 0, |vm, c| {
            let (_, m) = matrix_of(vm, &c.this)?;
            let v = m.values();
            if v.iter().any(|x| !x.is_finite()) {
                return throw(vm, "InvalidStateError", "a matrix with non-finite values has no string");
            }
            let num = bb_js::vm::value::number_to_string;
            let s = if m.is_2d {
                let [a, b, c2, d, e, f] = [m.m[0][0], m.m[0][1], m.m[1][0], m.m[1][1], m.m[3][0], m.m[3][1]];
                format!("matrix({}, {}, {}, {}, {}, {})", num(a), num(b), num(c2), num(d), num(e), num(f))
            } else {
                format!("matrix3d({})", v.iter().map(|x| num(*x)).collect::<Vec<_>>().join(", "))
            };
            Ok(js(&s))
        });
    }
    // Static constructors (§6.3).
    for (name, f) in [
        (
            "fromMatrix",
            (|vm, c| {
                let class = class_of_callee(vm, &c, C::DOMMatrixReadOnly, C::DOMMatrix);
                let m = matrix_init(vm, &c.arg(0))?;
                let h = host(vm)?;
                Ok(new_matrix(vm, &h, class, &m))
            }) as bb_js::vm::heap::NativeFn,
        ),
        ("fromFloat32Array", from_f32),
        ("fromFloat64Array", from_f64),
    ] {
        let f = bb_js::vm::realm::native_with(
            vm,
            name,
            1,
            f,
            false,
            vec![Value::Number(C::DOMMatrixReadOnly as u32 as f64)],
        );
        put_method(vm, ctor, name, f);
    }
}

fn from_f32(vm: &mut Vm, c: NativeCall) -> R {
    from_array(vm, c, 4)
}

fn from_f64(vm: &mut Vm, c: NativeCall) -> R {
    from_array(vm, c, 8)
}

fn from_array(vm: &mut Vm, c: NativeCall, size: usize) -> R {
    let class = class_of_callee(vm, &c, C::DOMMatrixReadOnly, C::DOMMatrix);
    let want = if size == 4 { "Float32Array" } else { "Float64Array" };
    let kind = if size == 4 { bb_js::vm::typed::TaKind::Float32 } else { bb_js::vm::typed::TaKind::Float64 };
    let vals = match c.arg(0) {
        Value::Object(o) => match vm.ta_view(o) {
            Some((k, ..)) if k == kind => {
                let n = vm.ta_length(o).unwrap_or(0);
                (0..n)
                    .map(|i| match vm.ta_get(o, i as f64) {
                        Value::Number(x) => x,
                        _ => f64::NAN,
                    })
                    .collect::<Vec<f64>>()
            }
            _ => return vm.type_error(&format!("the argument must be a {want}")),
        },
        _ => return vm.type_error(&format!("the argument must be a {want}")),
    };
    let m = match vals.len() {
        6 => Mat::from_2d(vals[0], vals[1], vals[2], vals[3], vals[4], vals[5]),
        16 => Mat::from_16(&vals),
        n => return vm.type_error(&format!("a matrix needs 6 or 16 numbers, not {n}")),
    };
    let h = host(vm)?;
    Ok(new_matrix(vm, &h, class, &m))
}

fn typed_array(vm: &mut Vm, ctor: &str, bytes: Vec<u8>) -> R {
    let buf = Value::Object(vm.new_array_buffer(bytes));
    let g = vm.realm.global;
    let f = vm.get(g, &PropKey::named(ctor))?;
    let Value::Object(fo) = f else { return vm.type_error("no typed array constructor") };
    vm.construct(fo, vec![buf], None)
}

pub fn setup_matrix(vm: &mut Vm, proto: ObjRef, ctor: ObjRef, worker: bool) {
    for (i, (name, ..)) in FIELDS.iter().enumerate() {
        attr_with(vm, proto, name, field_get, Some(field_set), vec![Value::Number(i as f64)]);
    }
    // The in-place operations (§6.7) return the matrix itself.
    for (i, (name, _)) in OPS.iter().enumerate() {
        let public = format!("{name}Self");
        let f = bb_js::vm::realm::native_with(
            vm,
            &public,
            0,
            |vm, c| {
                let (o, m) = matrix_of(vm, &c.this)?;
                let r = (OPS[op_index(vm, &c)].1)(vm, &c, m)?;
                store(vm, o, &r);
                Ok(c.this.clone())
            },
            false,
            vec![Value::Number(i as f64)],
        );
        put_method(vm, proto, &public, f);
    }
    if !worker {
        // [LegacyWindowAlias=WebKitCSSMatrix] (§6).
        let g = vm.realm.global;
        bb_js::vm::realm::value(vm, g, "WebKitCSSMatrix", Value::Object(ctor));
        op(vm, proto, "setMatrixValue", 1, |vm, c| {
            let s = rstr(vm, &c.arg(0))?;
            let (o, _) = matrix_of(vm, &c.this)?;
            match parse_transform_list(&s) {
                Some(m) => {
                    store(vm, o, &m);
                    Ok(c.this.clone())
                }
                None => throw(vm, "SyntaxError", &format!("'{s}' is not a transform list")),
            }
        });
    }
    for name in ["fromMatrix", "fromFloat32Array", "fromFloat64Array"] {
        let f = match name {
            "fromMatrix" => {
                (|vm, c| {
                    let m = matrix_init(vm, &c.arg(0))?;
                    let h = host(vm)?;
                    Ok(new_matrix(vm, &h, C::DOMMatrix, &m))
                }) as bb_js::vm::heap::NativeFn
            }
            "fromFloat32Array" => from_f32,
            _ => from_f64,
        };
        let f = bb_js::vm::realm::native_with(
            vm,
            name,
            1,
            f,
            false,
            vec![Value::Number(C::DOMMatrix as u32 as f64)],
        );
        put_method(vm, ctor, name, f);
    }
}

// ---- parsing a CSS transform list (§6.2; CSS Transforms 1 §13, CSS Transforms 2 §13) ----

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tok {
    Num(f64),
    Dim(f64, Unit),
    Pct(f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Unit {
    Px,
    Deg,
    Other(f64, bool), // (factor to px or degrees, is_angle)
    Bad,
}

fn unit(u: &str) -> Unit {
    match u.to_ascii_lowercase().as_str() {
        "px" => Unit::Px,
        "deg" => Unit::Deg,
        "rad" => Unit::Other(180.0 / std::f64::consts::PI, true),
        "grad" => Unit::Other(0.9, true),
        "turn" => Unit::Other(360.0, true),
        "cm" => Unit::Other(96.0 / 2.54, false),
        "mm" => Unit::Other(96.0 / 25.4, false),
        "q" => Unit::Other(96.0 / 101.6, false),
        "in" => Unit::Other(96.0, false),
        "pt" => Unit::Other(96.0 / 72.0, false),
        "pc" => Unit::Other(16.0, false),
        _ => Unit::Bad,
    }
}

impl Tok {
    /// A <length> in px (relative units can't be resolved without layout).
    fn length(self) -> Option<f64> {
        match self {
            Tok::Num(0.0) => Some(0.0),
            Tok::Dim(n, Unit::Px) => Some(n),
            Tok::Dim(n, Unit::Other(k, false)) => Some(n * k),
            _ => None,
        }
    }
    /// An <angle> in degrees (a bare 0 too).
    fn angle(self) -> Option<f64> {
        match self {
            Tok::Num(0.0) => Some(0.0),
            Tok::Dim(n, Unit::Deg) => Some(n),
            Tok::Dim(n, Unit::Other(k, true)) => Some(n * k),
            _ => None,
        }
    }
    fn number(self) -> Option<f64> {
        match self {
            Tok::Num(n) => Some(n),
            _ => None,
        }
    }
    /// <number> | <percentage> (scale functions).
    fn factor(self) -> Option<f64> {
        match self {
            Tok::Num(n) => Some(n),
            Tok::Pct(p) => Some(p / 100.0),
            _ => None,
        }
    }
}

/// Read a CSS <number-token>/<dimension-token>/<percentage-token> at the start of `s`.
fn read_number(s: &str) -> Option<(Tok, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > start
    };
    let int = digits(&mut i);
    let mut frac = false;
    if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
        i += 1;
        frac = digits(&mut i);
    }
    if !int && !frac {
        return None;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            i = j;
            digits(&mut i);
        }
    }
    let n: f64 = s[..i].parse().ok()?;
    let rest = &s[i..];
    if let Some(r) = rest.strip_prefix('%') {
        return Some((Tok::Pct(n), r));
    }
    let ulen = rest.bytes().take_while(|c| c.is_ascii_alphabetic()).count();
    if ulen > 0 {
        return Some((Tok::Dim(n, unit(&rest[..ulen])), &rest[ulen..]));
    }
    Some((Tok::Num(n), rest))
}

/// Parse a <transform-list> (or "none", or the empty string) into a matrix: §6.2 "parse a
/// string into an abstract matrix". None for syntax errors and lengths that need layout.
pub fn parse_transform_list(s: &str) -> Option<Mat> {
    if s.is_empty() {
        return Some(Mat::IDENTITY);
    }
    let ws = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c');
    let uncommented = strip_comments(s);
    let t = uncommented.trim_matches(ws);
    if t.eq_ignore_ascii_case("none") {
        return Some(Mat::IDENTITY);
    }
    if t.is_empty() {
        return None;
    }
    let mut rest = t;
    let mut m = Mat::IDENTITY;
    while !rest.is_empty() {
        let open = rest.find('(')?;
        let name = rest[..open].to_ascii_lowercase();
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let close = matching_paren(rest, open)?;
        let body = &rest[open + 1..close];
        rest = rest[close + 1..].trim_start_matches(ws);
        // Arguments: tokens separated by commas (with optional whitespace).
        let mut toks = Vec::new();
        let mut b = body.trim_matches(ws);
        if !b.is_empty() {
            loop {
                let (tok, r) = read_value(b)?;
                toks.push(tok);
                let r = r.trim_start_matches(ws);
                if r.is_empty() {
                    break;
                }
                b = r.strip_prefix(',')?.trim_start_matches(ws);
            }
        }
        m = m.mul(&function(&name, &toks)?);
    }
    Some(m)
}

fn function(name: &str, a: &[Tok]) -> Option<Mat> {
    let n = a.len();
    let len = |i: usize| a.get(i).and_then(|t| t.length());
    let ang = |i: usize| a.get(i).and_then(|t| t.angle());
    let num = |i: usize| a.get(i).and_then(|t| t.number());
    let fac = |i: usize| a.get(i).and_then(|t| t.factor());
    Some(match (name, n) {
        ("matrix", 6) => Mat::from_2d(num(0)?, num(1)?, num(2)?, num(3)?, num(4)?, num(5)?),
        ("matrix3d", 16) => {
            let v: Option<Vec<f64>> = (0..16).map(num).collect();
            Mat::from_16(&v?)
        }
        ("translate", 1) => Mat::translate(len(0)?, 0.0, 0.0),
        ("translate", 2) => Mat::translate(len(0)?, len(1)?, 0.0),
        ("translatex", 1) => Mat::translate(len(0)?, 0.0, 0.0),
        ("translatey", 1) => Mat::translate(0.0, len(0)?, 0.0),
        ("translatez", 1) => Mat { is_2d: false, ..Mat::translate(0.0, 0.0, len(0)?) },
        ("translate3d", 3) => Mat { is_2d: false, ..Mat::translate(len(0)?, len(1)?, len(2)?) },
        ("scale", 1) => Mat::scale(fac(0)?, fac(0)?, 1.0),
        ("scale", 2) => Mat::scale(fac(0)?, fac(1)?, 1.0),
        ("scalex", 1) => Mat::scale(fac(0)?, 1.0, 1.0),
        ("scaley", 1) => Mat::scale(1.0, fac(0)?, 1.0),
        ("scalez", 1) => Mat { is_2d: false, ..Mat::scale(1.0, 1.0, fac(0)?) },
        ("scale3d", 3) => Mat { is_2d: false, ..Mat::scale(fac(0)?, fac(1)?, fac(2)?) },
        ("rotate" | "rotatez", 1) => {
            Mat { is_2d: name == "rotate", ..Mat::rotate_axis(0.0, 0.0, 1.0, ang(0)?) }
        }
        ("rotatex", 1) => Mat::rotate_axis(1.0, 0.0, 0.0, ang(0)?),
        ("rotatey", 1) => Mat::rotate_axis(0.0, 1.0, 0.0, ang(0)?),
        ("rotate3d", 4) => Mat { is_2d: false, ..Mat::rotate_axis(num(0)?, num(1)?, num(2)?, ang(3)?) },
        ("skew", 1) => Mat::skew(ang(0)?, 0.0),
        ("skew", 2) => Mat::skew(ang(0)?, ang(1)?),
        ("skewx", 1) => Mat::skew(ang(0)?, 0.0),
        ("skewy", 1) => Mat::skew(0.0, ang(0)?),
        ("perspective", 1) => {
            let d = len(0)?;
            if d < 0.0 {
                return None;
            }
            let mut m = Mat::IDENTITY;
            m.is_2d = false;
            if d > 0.0 {
                m.m[2][3] = -1.0 / d;
            }
            m
        }
        _ => return None,
    })
}

/// CSS comments become whitespace (CSS Syntax §4.3.2).
fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        out.push(' ');
        rest = match rest[i + 2..].find("*/") {
            Some(j) => &rest[i + 2 + j + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// The index of the `)` closing the `(` at `open`.
fn matching_paren(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// A number, dimension or percentage, or a `calc()` of them (CSS Values 4 §10).
fn read_value(s: &str) -> Option<(Tok, &str)> {
    if s.len() >= 5 && s[..5].eq_ignore_ascii_case("calc(") {
        let close = matching_paren(s, 4)?;
        let v = calc(&s[5..close])?;
        return Some((v, &s[close + 1..]));
    }
    read_number(s)
}

/// A calc() value's type: plain numbers, lengths (px), angles (deg) or percentages.
fn canon(t: Tok) -> Option<(f64, u8)> {
    Some(match t {
        Tok::Num(n) => (n, 0),
        Tok::Pct(p) => (p, 3),
        Tok::Dim(n, Unit::Px) => (n, 1),
        Tok::Dim(n, Unit::Deg) => (n, 2),
        Tok::Dim(n, Unit::Other(k, angle)) => (n * k, if angle { 2 } else { 1 }),
        Tok::Dim(_, Unit::Bad) => return None,
    })
}

fn uncanon(v: f64, kind: u8) -> Tok {
    match kind {
        0 => Tok::Num(v),
        1 => Tok::Dim(v, Unit::Px),
        2 => Tok::Dim(v, Unit::Deg),
        _ => Tok::Pct(v),
    }
}

/// Evaluate a calc() sum: products joined by " + " and " - ", products of values (and
/// nested parentheses or calc()s) joined by * and /.
fn calc(s: &str) -> Option<Tok> {
    fn sum(s: &str) -> Option<(f64, u8)> {
        let ws = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c');
        let mut rest = s.trim_matches(ws);
        let (mut acc, kind) = product(&mut rest)?;
        loop {
            rest = rest.trim_start_matches(ws);
            if rest.is_empty() {
                return Some((acc, kind));
            }
            let neg = match rest.as_bytes()[0] {
                b'+' => false,
                b'-' => true,
                _ => return None,
            };
            rest = &rest[1..];
            let (v, k) = product(&mut rest)?;
            if k != kind {
                return None;
            }
            acc = if neg { acc - v } else { acc + v };
        }
    }
    fn product(rest: &mut &str) -> Option<(f64, u8)> {
        let ws = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c');
        let (mut acc, mut kind) = term(rest)?;
        loop {
            let t = rest.trim_start_matches(ws);
            let op = match t.as_bytes().first() {
                Some(b'*') => '*',
                Some(b'/') => '/',
                _ => return Some((acc, kind)),
            };
            *rest = &t[1..];
            let (v, k) = term(rest)?;
            match op {
                '*' if kind == 0 => {
                    acc *= v;
                    kind = k;
                }
                '*' if k == 0 => acc *= v,
                '/' if k == 0 => acc /= v,
                _ => return None,
            }
        }
    }
    fn term(rest: &mut &str) -> Option<(f64, u8)> {
        let ws = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c');
        let t = rest.trim_start_matches(ws);
        if t.starts_with('(') {
            let close = matching_paren(t, 0)?;
            let v = sum(&t[1..close])?;
            *rest = &t[close + 1..];
            return Some(v);
        }
        let (tok, r) = read_value(t)?;
        *rest = r;
        canon(tok)
    }
    let (v, k) = sum(s)?;
    Some(uncanon(v, k))
}

// ---- structured clone (§2–§6 "serialization steps") ----

const SERIALIZABLE: [C; 7] = [
    C::DOMPointReadOnly,
    C::DOMPoint,
    C::DOMRectReadOnly,
    C::DOMRect,
    C::DOMQuad,
    C::DOMMatrixReadOnly,
    C::DOMMatrix,
];

/// A geometry object's numbers for structured clone (None for other objects).
pub fn serialize(vm: &Vm, o: ObjRef, class: u32) -> Option<bb_js::vm::clone::HostSer> {
    let c = *SERIALIZABLE.iter().find(|c| **c as u32 == class)?;
    let numbers = match c {
        C::DOMQuad => {
            let pts: Vec<ObjRef> = vm
                .host_obj(o)?
                .slots
                .iter()
                .filter_map(|v| match v {
                    Value::Object(p) => Some(*p),
                    _ => None,
                })
                .collect();
            pts.iter().flat_map(|p| nums(vm, *p, 4)).collect()
        }
        C::DOMMatrixReadOnly | C::DOMMatrix => {
            let mut n = nums(vm, o, 16);
            let is_2d = vm.host_obj(o).is_some_and(|h| matches!(h.slots.get(16), Some(Value::Bool(true))));
            if is_2d {
                // §6.8: a 2D matrix serializes only a–f.
                let m = Mat::from_2d(n[0], n[1], n[4], n[5], n[12], n[13]);
                n = m.values().to_vec();
            }
            n.push(if is_2d { 1.0 } else { 0.0 });
            n
        }
        _ => nums(vm, o, 4),
    };
    Some(bb_js::vm::clone::HostSer { class, numbers, ..Default::default() })
}

/// Recreate a serialized geometry object (None for other classes).
pub fn deserialize(vm: &mut Vm, h: &Host, s: &bb_js::vm::clone::HostSer) -> Option<Value> {
    let c = *SERIALIZABLE.iter().find(|c| **c as u32 == s.class)?;
    let n = |i: usize| s.numbers.get(i).copied().unwrap_or(0.0);
    Some(match c {
        C::DOMQuad => {
            let pts: [[f64; 4]; 4] = std::array::from_fn(|p| std::array::from_fn(|i| n(p * 4 + i)));
            new_quad(vm, h, pts)
        }
        C::DOMMatrixReadOnly | C::DOMMatrix => {
            let v: Vec<f64> = (0..16).map(n).collect();
            let mut m = Mat::from_16(&v);
            m.is_2d = n(16) == 1.0;
            new_matrix(vm, h, c, &m)
        }
        C::DOMPointReadOnly | C::DOMPoint => new_point(vm, h, c, [n(0), n(1), n(2), n(3)]),
        _ => new_rect(vm, h, c, [n(0), n(1), n(2), n(3)]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_lists_parse() {
        let m = parse_transform_list("translate(10px, 20px) scale(2)").unwrap();
        assert_eq!(m, Mat::from_2d(2.0, 0.0, 0.0, 2.0, 10.0, 20.0));
        assert!(parse_transform_list("translate(1em)").is_none(), "relative lengths need layout");
        assert!(parse_transform_list("rotate(1px)").is_none());
        assert!(!parse_transform_list("translateZ(0)").unwrap().is_2d);
        let r = parse_transform_list("rotate(90deg)").unwrap();
        assert!((r.m[0][1] - 1.0).abs() < 1e-12 && r.m[0][0].abs() < 1e-12);
        assert_eq!(parse_transform_list("none"), Some(Mat::IDENTITY));
        assert_eq!(parse_transform_list("/**/none"), Some(Mat::IDENTITY));
        assert!(parse_transform_list(" ").is_none());
        assert_eq!(
            parse_transform_list("translateX(calc(2 * 2.5px)) translateY(calc((1px + 1px) / 2))"),
            Some(Mat::from_2d(1.0, 0.0, 0.0, 1.0, 5.0, 1.0))
        );
    }

    #[test]
    fn inverses() {
        let m = Mat::from_2d(2.0, 0.0, 0.0, 4.0, 6.0, 8.0);
        assert_eq!(m.mul(&m.inverse().unwrap()), Mat::IDENTITY);
        assert!(Mat::from_2d(0.0, 0.0, 0.0, 0.0, 0.0, 0.0).inverse().is_none());
        let mut m3 = Mat::rotate_axis(1.0, 1.0, 0.0, 30.0).mul(&Mat::translate(1.0, 2.0, 3.0));
        m3.is_2d = false;
        let p = m3.mul(&m3.inverse().unwrap());
        for c in 0..4 {
            for r in 0..4 {
                assert!((p.m[c][r] - if c == r { 1.0 } else { 0.0 }).abs() < 1e-9);
            }
        }
    }
}

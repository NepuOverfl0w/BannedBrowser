//! Built-ins written in JavaScript (`selfhost.js`), mostly the async ones whose spec steps
//! are an AsyncFunctionStart closure (Array.fromAsync, …). The script is compiled once per
//! thread and run in every new realm with an object of native helpers; the functions it
//! defines look like built-ins (`[native code]`, %Function.prototype%, no `prototype`).
//!
//! The JavaScript must not reach anything user code can patch: it calls functions through
//! `I.call`, never uses spread, `for…of` or methods of the built-in prototypes.

use super::super::code::Code;
use super::super::compiler::{Compiler, Unit};
use super::super::heap::*;
use super::super::realm::native;
use super::super::value::{PropKey, Value};
use super::super::{ErrorKind, JsResult, Vm};
use std::cell::OnceCell;
use std::rc::Rc;

/// The referrer of self-hosted code; `Function.prototype.toString` reports it as native.
pub const MARK: &str = "\u{0}builtin";

/// The self-hosted scripts, run in this order with the same helpers.
const SOURCES: [&str; 2] = [include_str!("selfhost.js"), include_str!("temporal.js")];

fn code() -> Vec<Rc<Code>> {
    thread_local! {
        static CODE: OnceCell<Vec<Rc<Code>>> = const { OnceCell::new() };
    }
    CODE.with(|c| {
        c.get_or_init(|| {
            SOURCES
                .iter()
                .filter_map(|src| {
                    let prog = crate::parse_script(src).ok()?;
                    let source: Rc<[char]> = src.chars().collect::<Vec<_>>().into();
                    Some(Rc::new(Compiler::compile_unit(Unit::Script(&prog), source, Some(MARK.into()))))
                })
                .collect()
        })
        .clone()
    })
}

fn obj_arg(vm: &mut Vm, c: &NativeCall, i: usize) -> JsResult<ObjRef> {
    match c.arg(i) {
        Value::Object(o) => Ok(o),
        _ => vm.type_error("value is not an object"),
    }
}

fn helper(vm: &mut Vm, on: ObjRef, name: &str, f: NativeFn) {
    let m = native(vm, name, 0, f);
    vm.heap.obj_mut(on).props.insert(PropKey::named(name), Prop::data(Value::Object(m), false, false, false));
}

pub(super) fn install(vm: &mut Vm) {
    let codes = code();
    debug_assert_eq!(codes.len(), SOURCES.len(), "a self-hosted script doesn't parse");
    let i = vm.heap.alloc(Object::new(None, Kind::Ordinary));
    // Internal slots for self-hosted classes (Temporal): an array only the JavaScript sees.
    helper(vm, i, "setSlots", |vm, c| {
        let o = obj_arg(vm, &c, 0)?;
        let brand = vm.to_number(&c.arg(1))? as u32;
        let arr = obj_arg(vm, &c, 2)?;
        let obj = vm.heap.obj_mut(o);
        if !matches!(obj.kind, Kind::Ordinary) {
            return vm.type_error("the object already has internal slots");
        }
        obj.kind = Kind::Slots(brand, arr);
        Ok(Value::Undefined)
    });
    helper(vm, i, "slots", |vm, c| {
        let brand = vm.to_number(&c.arg(1))? as u32;
        if let Value::Object(o) = c.arg(0) {
            if let Kind::Slots(b, arr) = vm.heap.obj(o).kind {
                if b == brand {
                    return Ok(Value::Object(arr));
                }
            }
        }
        let what = super::to_rust(&vm.to_string(&c.arg(2))?);
        vm.type_error(&format!("the receiver is not a {what}"))
    });
    helper(vm, i, "brand", |vm, c| {
        Ok(Value::Number(match c.arg(0) {
            Value::Object(o) => match vm.heap.obj(o).kind {
                Kind::Slots(b, _) => b as f64,
                _ => -1.0,
            },
            _ => -1.0,
        }))
    });
    helper(vm, i, "objectCreate", |vm, c| {
        let proto = c.arg(0).as_object();
        Ok(Value::Object(vm.heap.alloc(Object::new(proto, Kind::Ordinary))))
    });
    // defineValue(obj, key, value, writable, enumerable, configurable)
    helper(vm, i, "defineValue", |vm, c| {
        let o = obj_arg(vm, &c, 0)?;
        let k = vm.to_property_key(&c.arg(1))?;
        let p = Prop::data(
            c.arg(2),
            vm.to_boolean(&c.arg(3)),
            vm.to_boolean(&c.arg(4)),
            vm.to_boolean(&c.arg(5)),
        );
        vm.heap.obj_mut(o).props.insert(k, p);
        Ok(Value::Undefined)
    });
    helper(vm, i, "nowNs", |_, _| {
        let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        use super::super::bigint::BigInt;
        let ns = BigInt::from_u64(d.as_secs())
            .mul(&BigInt::from_u64(1_000_000_000))
            .add(&BigInt::from_u64(d.subsec_nanos() as u64));
        Ok(Value::BigInt(Rc::new(ns)))
    });
    // The time zone database (vm/tz.rs). Instants are whole seconds since the epoch as
    // Numbers (exact: Temporal's range is within ±2^53 s); Temporal adds the nanoseconds.
    helper(vm, i, "tzId", |vm, c| {
        let n = vm.to_string(&c.arg(0))?;
        Ok(match super::super::tz::lookup(&String::from_utf16_lossy(&n)) {
            Some((id, primary)) => Value::Object(vm.new_array(vec![Value::str(id), Value::str(primary)])),
            None => Value::Undefined,
        })
    });
    fn tz_of(vm: &mut Vm, c: &NativeCall) -> JsResult<(&'static super::super::tz::Zone, i64)> {
        let n = vm.to_string(&c.arg(0))?;
        let t = vm.to_number(&c.arg(1))?;
        match super::super::tz::zone(&String::from_utf16_lossy(&n)) {
            // Clamped far beyond Temporal's range (±8.64e12 s) so tz.rs's date math stays small.
            Some(z) if t.is_finite() => Ok((z, t.clamp(-1e14, 1e14) as i64)),
            _ => vm.range_error("unknown time zone"),
        }
    }
    helper(vm, i, "tzOffset", |vm, c| {
        let (z, t) = tz_of(vm, &c)?;
        Ok(Value::Number(z.offset_at(t) as f64))
    });
    helper(vm, i, "tzPossible", |vm, c| {
        let (z, t) = tz_of(vm, &c)?;
        let v = z.possible(t).into_iter().map(|x| Value::Number(x as f64)).collect();
        Ok(Value::Object(vm.new_array(v)))
    });
    helper(vm, i, "tzNext", |vm, c| {
        let (z, t) = tz_of(vm, &c)?;
        Ok(z.next_transition(t).map_or(Value::Null, |x| Value::Number(x as f64)))
    });
    helper(vm, i, "tzPrevious", |vm, c| {
        let (z, t) = tz_of(vm, &c)?;
        Ok(z.previous_transition(t).map_or(Value::Null, |x| Value::Number(x as f64)))
    });
    helper(vm, i, "tzSystem", |vm, _| Ok(Value::str(vm.time_zone())));
    helper(vm, i, "thisTimeValue", |vm, c| Ok(Value::Number(super::date::this_time(vm, &c.arg(0))?)));
    helper(vm, i, "codeUnit", |vm, c| {
        let s = vm.to_string(&c.arg(0))?;
        let k = vm.to_number(&c.arg(1))?;
        Ok(match s.get(k as usize) {
            Some(&u) if k >= 0.0 => Value::Number(u as f64),
            _ => Value::Number(f64::NAN),
        })
    });
    helper(vm, i, "substring", |vm, c| {
        let s = vm.to_string(&c.arg(0))?;
        let a = (vm.to_number(&c.arg(1))?.max(0.0) as usize).min(s.len());
        let b = if c.arg(2).is_undefined() {
            s.len()
        } else {
            (vm.to_number(&c.arg(2))?.max(0.0) as usize).min(s.len())
        };
        Ok(Value::String(s[a.min(b)..b.max(a)].into()))
    });
    helper(vm, i, "toPrimitiveValue", |vm, c| {
        let hint = match c.arg(1) {
            Value::String(s) if s.iter().copied().eq("string".encode_utf16()) => {
                super::super::ops::Hint::String
            }
            _ => super::super::ops::Hint::Number,
        };
        vm.to_primitive(&c.arg(0), hint)
    });
    // bigDiv(n, d): the exact quotient of two BigInts, correctly rounded to a Number.
    helper(vm, i, "bigDiv", |vm, c| {
        use super::super::bigint::BigInt;
        let (Value::BigInt(n), Value::BigInt(d)) = (c.arg(0), c.arg(1)) else {
            return vm.type_error("bigDiv needs BigInts");
        };
        if d.is_zero() {
            return vm.range_error("division by zero");
        }
        let neg = (n.to_f64() < 0.0) != (d.to_f64() < 0.0);
        let abs = |x: &BigInt| if x.to_f64() < 0.0 { x.neg() } else { x.clone() };
        let (n, d) = (abs(&n), abs(&d));
        if n.is_zero() {
            return Ok(Value::Number(0.0));
        }
        // A quotient of 66+ bits with a sticky bit rounds once, correctly.
        let shift = (66 + d.bit_len()).saturating_sub(n.bit_len());
        let Some((q, r)) = n.shl(shift).divrem(&d) else { return Ok(Value::Number(f64::NAN)) };
        let q = q.shl(1).add(&BigInt::from_u64(u64::from(!r.is_zero())));
        let mut v = q.to_f64();
        let mut e = shift + 1;
        while e > 0 {
            let s = e.min(1000);
            v *= 2f64.powi(-(s as i32));
            e -= s;
        }
        Ok(Value::Number(if neg { -v } else { v }))
    });
    helper(vm, i, "asciiLower", |vm, c| {
        let s = vm.to_string(&c.arg(0))?;
        let v: Vec<u16> = s.iter().map(|&u| if (65..=90).contains(&u) { u + 32 } else { u }).collect();
        Ok(Value::String(v.into()))
    });
    helper(vm, i, "asciiUpper", |vm, c| {
        let s = vm.to_string(&c.arg(0))?;
        let v: Vec<u16> = s.iter().map(|&u| if (97..=122).contains(&u) { u - 32 } else { u }).collect();
        Ok(Value::String(v.into()))
    });
    helper(vm, i, "call", |vm, c| {
        let f = c.arg(0);
        let this = c.arg(1);
        vm.call(&f, this, c.args.get(2..).map(<[Value]>::to_vec).unwrap_or_default())
    });
    helper(vm, i, "construct", |vm, c| {
        let f = obj_arg(vm, &c, 0)?;
        vm.construct(f, c.args.get(1..).map(<[Value]>::to_vec).unwrap_or_default(), None)
    });
    helper(vm, i, "isCallable", |vm, c| Ok(Value::Bool(vm.is_callable(&c.arg(0)))));
    helper(vm, i, "isConstructor", |vm, c| Ok(Value::Bool(vm.is_constructor(&c.arg(0)))));
    helper(vm, i, "toObject", |vm, c| vm.to_object(&c.arg(0)).map(Value::Object));
    helper(vm, i, "lengthOfArrayLike", |vm, c| {
        let o = obj_arg(vm, &c, 0)?;
        vm.length_of_array_like(o).map(Value::Number)
    });
    helper(vm, i, "arrayCreate", |vm, c| {
        let n = vm.to_number(&c.arg(0))?;
        super::array::array_create(vm, n, None).map(Value::Object)
    });
    helper(vm, i, "createDataPropertyOrThrow", |vm, c| {
        let o = obj_arg(vm, &c, 0)?;
        let k = vm.to_property_key(&c.arg(1))?;
        vm.create_data_property_or_throw(o, k, c.arg(2))?;
        Ok(Value::Undefined)
    });
    helper(vm, i, "asyncStackTake", super::super::dispose::async_stack_take);
    helper(vm, i, "suppressedError", |vm, c| Ok(vm.suppressed_error(c.arg(0), c.arg(1))));
    helper(vm, i, "asyncFromSyncIterator", |vm, c| Ok(Value::Object(vm.async_from_sync(c.arg(0), c.arg(1)))));
    // defineMethod(target, key, fn): a writable, configurable, non-enumerable built-in.
    helper(vm, i, "defineMethod", |vm, c| {
        let o = obj_arg(vm, &c, 0)?;
        let k = vm.to_property_key(&c.arg(1))?;
        let f = obj_arg(vm, &c, 2)?;
        let fp = vm.realm.function_proto;
        let name = match &k {
            PropKey::Sym(s) => {
                format!(
                    "[{}]",
                    s.0.description.as_deref().map(super::super::value::to_rust).unwrap_or_default()
                )
            }
            k => k.display(),
        };
        let fo = vm.heap.obj_mut(f);
        fo.proto = Some(fp);
        // An anonymous function is named after its key (SetFunctionName).
        let anonymous = matches!(fo.props.get(&PropKey::lit("name")).map(|p| &p.slot),
            Some(Slot::Data(Value::String(s))) if s.is_empty());
        if anonymous {
            fo.props.insert(PropKey::lit("name"), Prop::data(Value::str(&name), false, false, true));
        }
        vm.heap.obj_mut(o).props.insert(k, Prop::method(Value::Object(f)));
        Ok(Value::Undefined)
    });
    // Intrinsics, read before any user code runs.
    let mut vals: Vec<(&str, Value)> = vec![
        ("Array", Value::Object(vm.realm.array_ctor)),
        ("Promise", Value::Object(vm.realm.promise_ctor)),
        ("ObjectPrototype", Value::Object(vm.realm.object_proto)),
        ("FunctionPrototype", Value::Object(vm.realm.function_proto)),
        ("global", Value::Object(vm.realm.global)),
        ("DatePrototype", Value::Object(vm.realm.date_proto)),
        ("AsyncDisposableStackPrototype", Value::Object(vm.realm.async_disposable_stack_proto)),
        ("AsyncIteratorPrototype", Value::Object(vm.realm.async_iterator_proto)),
    ];
    for (name, k) in [("TypeError", ErrorKind::Type), ("RangeError", ErrorKind::Range)] {
        let p = vm.realm.error_proto(k);
        vals.push((name, vm.get(p, &PropKey::lit("constructor")).unwrap_or(Value::Undefined)));
    }
    for (name, sym) in vm.realm.well_known() {
        vals.push((name, Value::Symbol(sym)));
    }
    for (name, v) in vals {
        vm.heap.obj_mut(i).props.insert(PropKey::named(name), Prop::data(v, false, false, false));
    }
    for code in codes {
        match vm.run_global_code(code) {
            Ok(f) => {
                if let Err(e) = vm.call(&f, Value::Undefined, vec![Value::Object(i)]) {
                    let d = vm.describe(&e);
                    debug_assert!(false, "a self-hosted script threw: {d}");
                }
            }
            Err(_) => debug_assert!(false, "a self-hosted script failed to run"),
        }
    }
}

//! Realms a host drops (a browser frame that went away), and platform objects tied to the
//! realm they were made in.

use bb_js::{Value, Vm};

#[test]
fn dropped_realms_no_longer_run() {
    let mut vm = Vm::new();
    let id = vm.create_realm();
    let f = vm.in_realm(id, |vm| vm.run_script("(function () { return 41 + 1 })")).unwrap();
    let parse_int = vm.in_realm(id, |vm| vm.run_script("parseInt")).unwrap();
    // Kept alive across collections like a host's values.
    vm.roots.push(f.clone());
    vm.roots.push(parse_int.clone());
    assert!(matches!(vm.call(&f, Value::Undefined, vec![]), Ok(Value::Number(n)) if n == 42.0));
    vm.drop_realm(id);
    assert!(vm.realm_dropped(id));
    // Neither its closures nor its built-ins run, now or after a collection.
    assert!(vm.call(&f, Value::Undefined, vec![]).is_err());
    assert!(vm.call(&parse_int, Value::Undefined, vec![Value::str("7")]).is_err());
    vm.collect_garbage();
    assert!(!vm.realm_alive(id));
    assert!(vm.call(&f, Value::Undefined, vec![]).is_err());
    assert!(vm.try_in_realm(id, |_| ()).is_none());
    // The first realm can't be dropped.
    vm.drop_realm(0);
    assert!(matches!(vm.run_script("1 + 1"), Ok(Value::Number(n)) if n == 2.0));
}

#[test]
fn platform_objects_belong_to_their_realm() {
    let mut vm = Vm::new();
    let id = vm.create_realm();
    let o = vm.in_realm(id, |vm| vm.new_host_object(None, 7, 99, false));
    let v = Value::Object(o);
    assert_eq!(vm.host_data(&v), None, "another realm's platform object");
    assert_eq!(vm.host_realm(o), Some(id));
    assert_eq!(vm.in_realm(id, |vm| vm.host_data(&v)), Some((7, 99)));
}

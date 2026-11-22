//! Interpreter behaviour test262 doesn't cover: garbage collection, termination of runaway
//! scripts, and recursion limits that hold on an ordinary 8 MB thread.

use bb_js::{Value, Vm};

fn run(src: &str) -> (Vm, Result<Value, Value>) {
    let mut vm = Vm::new();
    let r = vm.run_script(src);
    (vm, r)
}

fn number(src: &str) -> f64 {
    match run(src).1 {
        Ok(Value::Number(n)) => n,
        Ok(v) => panic!("{src}: not a number: {v:?}"),
        Err(e) => {
            let mut vm = Vm::new();
            panic!("{src}: threw {}", vm.describe(&e))
        }
    }
}

#[test]
fn garbage_is_collected() {
    let (vm, r) = run("var keep = []; for (var i = 0; i < 300000; i++) { var o = {i: i, next: {i: i}}; if (i % 1000 == 0) keep.push(o) } keep.length");
    assert!(matches!(r, Ok(Value::Number(n)) if n == 300.0));
    // 600 000 objects were made; the live ones are the realm, `keep` and its 300 entries.
    assert!(vm.heap.live_objects() < 100_000, "{} objects still live", vm.heap.live_objects());
}

#[test]
fn closures_survive_collection() {
    let n = number(
        "function counter() { var c = 0; return function () { return ++c } }
         var f = counter();
         for (var i = 0; i < 400000; i++) { [i, {i}]; f() }
         f()",
    );
    assert_eq!(n, 400001.0);
}

#[test]
fn fuel_terminates_infinite_loops_even_inside_try() {
    for src in [
        "while (true) {}",
        "for (;;) { try { while (true) {} } catch (e) {} finally { continue } }",
        "function f() { try { f() } finally { f() } } f()",
    ] {
        let mut vm = Vm::new();
        vm.fuel = Some(2_000_000);
        let r = vm.run_script(src);
        assert!(r.is_err() && vm.terminated, "{src}");
    }
}

#[test]
fn fuel_reaches_natives_looping_over_huge_array_likes() {
    let mut vm = Vm::new();
    vm.fuel = Some(1_000_000);
    let r = vm.run_script("Array.prototype.indexOf.call({length: 2 ** 53 - 1}, 1)");
    assert!(r.is_err() && vm.terminated);
}

#[test]
fn deep_recursion_is_a_range_error_on_a_normal_stack() {
    let srcs = [
        "function f() { return f() } f()",
        "({ get x() { return this.x } }).x",
        "var o = {}; o.toString = function () { return String(o) }; String(o)",
        "var a = []; a[0] = a; String(a)",
        "function g() { return [0].map(g) } g()",
    ];
    // An 8 MB stack, the usual main-thread default.
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            for src in srcs {
                let (mut vm, r) = run(src);
                let e = r.expect_err(src);
                assert!(vm.describe(&e).starts_with("RangeError"), "{src}: {}", vm.describe(&e));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn completion_values() {
    let (_, r) = run("1; if (true) {}");
    assert!(matches!(r, Ok(Value::Undefined)));
    assert_eq!(number("2; var x = 3;"), 2.0);
    assert_eq!(number("eval('4; {}')"), 4.0);
    assert_eq!(number("var s = 0; for (var i = 0; i < 4; i++) s += i"), 6.0);
}

#[test]
fn syntax_errors_and_uncaught_exceptions_reach_the_host() {
    let (mut vm, r) = run("var = 1");
    let e = r.unwrap_err();
    assert!(vm.describe(&e).starts_with("SyntaxError"));
    let (mut vm, r) = run("throw new TypeError('nope')");
    assert_eq!(vm.describe(&r.unwrap_err()), "TypeError: nope");
}

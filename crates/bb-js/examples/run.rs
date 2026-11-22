//! Run a script file (or `-e code`) and print its completion value or the exception.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let src = if args.get(1).map(String::as_str) == Some("-e") {
        args.get(2).cloned().unwrap_or_default()
    } else {
        std::fs::read_to_string(args.get(1).expect("usage: run FILE | -e CODE")).expect("read")
    };
    let mut vm = bb_js::Vm::new();
    let r = vm.run_script(&src);
    for line in std::mem::take(&mut vm.output) {
        println!("{line}");
    }
    match r {
        Ok(v) => println!("=> {}", vm.describe(&v)),
        Err(e) => {
            println!("Uncaught {}", vm.describe(&e));
            std::process::exit(1);
        }
    }
}

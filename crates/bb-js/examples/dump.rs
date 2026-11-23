//! Print the bytecode of a script and of each function in it (a development aid).
fn main() {
    let path = std::env::args().nth(1).expect("usage: dump FILE");
    let src = std::fs::read_to_string(path).expect("read");
    let prog = bb_js::parse_script(&src).expect("parse");
    let source: std::rc::Rc<[char]> = src.chars().collect::<Vec<_>>().into();
    let code =
        bb_js::vm::compiler::Compiler::compile_unit(bb_js::vm::compiler::Unit::Script(&prog), source, None);
    dump(&code, "script");
}

fn dump(code: &bb_js::vm::code::Code, name: &str) {
    println!("== {name}");
    for (i, op) in code.ops.iter().enumerate() {
        let s = code.slots.get(i).copied().unwrap_or(u32::MAX);
        let slot = if s == u32::MAX { String::new() } else { format!("  @{}:{}", s >> 16, s & 0xFFFF) };
        println!("{i:4} {op:?}{slot}");
    }
    for f in &code.funcs {
        let c = bb_js::vm::compiler::Compiler::compile_function(f);
        dump(&c, f.ast.name.as_deref().unwrap_or("<anon>"));
    }
}

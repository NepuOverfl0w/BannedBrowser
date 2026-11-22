//! `cargo run -p bb-js --example parse -- [--module] [--strict] FILE`: parse a file and print the
//! result or the syntax error.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let module = args.iter().any(|a| a == "--module");
    let strict = args.iter().any(|a| a == "--strict");
    let Some(file) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("usage: parse [--module] [--strict] FILE");
        std::process::exit(2);
    };
    let mut src = std::fs::read_to_string(file).unwrap_or_default();
    if strict {
        src = format!("\"use strict\";\n{src}");
    }
    let r = if module { bb_js::parse_module(&src) } else { bb_js::parse_script(&src) };
    match r {
        Ok(_) => println!("ok"),
        Err(e) => {
            let line =
                src[..src.char_indices().nth(e.pos).map_or(src.len(), |(i, _)| i)].matches('\n').count() + 1;
            println!("SyntaxError at line {line}: {}", e.msg);
        }
    }
}

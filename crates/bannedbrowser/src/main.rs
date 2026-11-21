//! BannedBrowser — entry point.
#![forbid(unsafe_code)]

mod cli;
mod headless;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args = match cli::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(msg) => {
            println!("{msg}");
            std::process::exit(if msg.starts_with("unknown") || msg.contains("needs a value") {
                2
            } else {
                0
            });
        }
    };
    if args.headless || args.dump_dom || args.dump_style || args.dump_layout {
        if let Err(e) = headless::run(&args) {
            eprintln!("error: {e:#}");
            std::process::exit(1);
        }
        return;
    }
    eprintln!("GUI mode is not built yet in this revision; use --headless --screenshot out.png");
}

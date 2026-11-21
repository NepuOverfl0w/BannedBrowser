//! `cargo run -p bb-net --example fetch -- <url>`: print status, headers and body size.
fn main() {
    let url = std::env::args().nth(1).unwrap_or_else(|| "https://example.com/".into());
    match bb_net::get(&url) {
        Ok(r) => {
            println!("{} {} (final URL {}, secure={})", r.status, r.status_text, r.url, r.secure);
            for (k, v) in r.headers.iter() {
                println!("{k}: {v}");
            }
            println!(
                "\n{} bytes; first 300:\n{}",
                r.body.len(),
                String::from_utf8_lossy(&r.body[..r.body.len().min(300)])
            );
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

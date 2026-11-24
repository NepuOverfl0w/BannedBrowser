//! Command-line parsing.

use bb_url::Url;

#[derive(Debug, Clone, Default)]
pub struct Args {
    pub headless: bool,
    pub screenshot: Option<String>,
    pub window_size: Option<(u32, u32)>,
    pub full_page: bool,
    pub dump_dom: bool,
    pub dump_style: bool,
    pub dump_layout: bool,
    pub extra_ca: Vec<std::path::PathBuf>,
    pub dark: Option<bool>,
    pub scale: Option<f32>,
    pub private_window: bool,
    pub new_tab: Vec<String>,
    pub profile: Option<std::path::PathBuf>,
    pub urls: Vec<String>,
    pub no_images: bool,
    pub user_agent: Option<String>,
    /// Headless: how long the page's scripts may keep running after load (ms).
    pub script_wait: Option<u64>,
}

pub const HELP: &str = "BannedBrowser — a web browser written from scratch in Rust.

USAGE:
    bannedbrowser [OPTIONS] [URL]...

OPTIONS:
    --headless                 Render without a window
    --screenshot <FILE.png>    Write a PNG of the page (implies --headless)
    --window-size <WxH>        Window / viewport size in CSS px (default 1280x800)
    --full-page                Screenshot the whole document, not just the viewport
    --scale <N>                Device pixel ratio for screenshots (default 1)
    --dump-dom                 Print the DOM (html5lib format) and exit
    --dump-style               Print computed styles and exit
    --dump-layout              Print the layout/fragment tree and exit
    --dark | --light           Force the color scheme
    --extra-ca <FILE.pem>      Trust an extra CA bundle (repeatable)
    --user-agent <STRING>      Override the User-Agent header
    --private-window           Open a private window
    --new-tab <URL>            Open URL in a new tab
    --profile <DIR>            Use DIR as the profile directory
    --no-images                Don't load images
    --script-wait <MS>         Headless: let the page's timers and tasks run this long after
                               load, or until nothing is pending (default 1000)
    -h, --help                 Show this help
    -V, --version              Show the version
";

pub fn parse(argv: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = argv.peekable();
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--headless" => a.headless = true,
            "--screenshot" => {
                a.screenshot = Some(val("--screenshot")?);
                a.headless = true;
            }
            "--window-size" => {
                let v = val("--window-size")?;
                let (w, h) = v.split_once(['x', ',', 'X']).ok_or("--window-size expects WxH")?;
                a.window_size = Some((
                    w.trim().parse().map_err(|_| "bad width")?,
                    h.trim().parse().map_err(|_| "bad height")?,
                ));
            }
            "--full-page" => a.full_page = true,
            "--dump-dom" => a.dump_dom = true,
            "--dump-style" => a.dump_style = true,
            "--dump-layout" => a.dump_layout = true,
            "--dark" => a.dark = Some(true),
            "--light" => a.dark = Some(false),
            "--scale" => a.scale = Some(val("--scale")?.parse().map_err(|_| "bad --scale")?),
            "--extra-ca" => a.extra_ca.push(val("--extra-ca")?.into()),
            "--user-agent" => a.user_agent = Some(val("--user-agent")?),
            "--private-window" | "--private" => a.private_window = true,
            "--new-tab" => a.new_tab.push(val("--new-tab")?),
            "--profile" => a.profile = Some(val("--profile")?.into()),
            "--no-images" => a.no_images = true,
            "--script-wait" => {
                a.script_wait = Some(val("--script-wait")?.parse().map_err(|_| "bad --script-wait")?)
            }
            "-h" | "--help" => return Err(HELP.to_string()),
            "-V" | "--version" => return Err(format!("BannedBrowser {}", env!("CARGO_PKG_VERSION"))),
            s if s.starts_with("--") => return Err(format!("unknown option {s}\n\n{HELP}")),
            _ => a.urls.push(arg),
        }
    }
    Ok(a)
}

/// Turn user input (URL bar text or a CLI argument) into a URL.
pub fn fixup_url(input: &str) -> Option<Url> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    // Existing local files.
    let p = std::path::Path::new(s);
    if !s.contains("://") && p.exists() {
        if let Ok(abs) = p.canonicalize() {
            return Url::from_file_path(&abs.to_string_lossy());
        }
    }
    if let Ok(u) = Url::parse(s) {
        let scheme = u.scheme();
        if matches!(scheme, "http" | "https" | "file" | "about" | "data" | "view-source") {
            return Some(u);
        }
        // "localhost:8000" parses as scheme "localhost" — treat as host:port.
        if !s.contains("://")
            && s.split(':').nth(1).is_some_and(|r| r.chars().next().is_some_and(|c| c.is_ascii_digit()))
        {
            return Url::parse(&format!("http://{s}")).ok();
        }
        return Some(u);
    }
    let looks_like_host = !s.contains(' ') && (s.contains('.') || s.starts_with("localhost"));
    if looks_like_host {
        let scheme = if s.starts_with("localhost") || s.starts_with("127.") { "http" } else { "https" };
        return Url::parse(&format!("{scheme}://{s}")).ok();
    }
    // Otherwise: a search.
    let q = bb_url::form_urlencode(&[("q".to_string(), s.to_string())]);
    Url::parse(&format!("https://duckduckgo.com/html/?{q}")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixups() {
        assert_eq!(fixup_url("example.com").unwrap().href(), "https://example.com/");
        assert_eq!(fixup_url("localhost:8000/x").unwrap().href(), "http://localhost:8000/x");
        assert_eq!(fixup_url("about:blank").unwrap().href(), "about:blank");
        assert!(fixup_url("rust programming")
            .unwrap()
            .href()
            .starts_with("https://duckduckgo.com/html/?q=rust+programming"));
        assert!(fixup_url("").is_none());
    }

    #[test]
    fn args() {
        let a = parse(
            ["--screenshot", "o.png", "--window-size", "800x600", "x.html"].iter().map(|s| s.to_string()),
        )
        .unwrap();
        assert!(a.headless);
        assert_eq!(a.window_size, Some((800, 600)));
        assert_eq!(a.urls, vec!["x.html"]);
    }
}

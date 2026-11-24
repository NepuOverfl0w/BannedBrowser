//! Headless mode: render a page to PNG or dump pipeline stages.

use crate::cli::{fixup_url, Args};
use anyhow::{anyhow, Context, Result};
use bb_engine::{Page, PageConfig};

pub fn run(args: &Args) -> Result<()> {
    let url_s = args.urls.first().cloned().unwrap_or_else(|| "about:newtab".into());
    let url = fixup_url(&url_s).ok_or_else(|| anyhow!("can't make a URL from {url_s:?}"))?;
    let (w, h) = args.window_size.unwrap_or((1280, 800));
    let client = bb_engine::new_client_with(args.extra_ca.clone(), args.user_agent.clone());
    let config = PageConfig {
        viewport: (w as f32, h as f32),
        prefers_dark: args.dark.unwrap_or(false),
        load_images: !args.no_images,
        ..Default::default()
    };
    let t0 = std::time::Instant::now();
    let mut page = Page::load(&client, &url, config);
    // Let the page's scripts finish what they started at load (timers, requests, frames),
    // as a window would before anyone looks at it.
    let wait_until =
        std::time::Instant::now() + std::time::Duration::from_millis(args.script_wait.unwrap_or(1000));
    while let Some(d) = page.script_wakeup() {
        let now = std::time::Instant::now();
        if now >= wait_until {
            break;
        }
        std::thread::sleep(d.min(wait_until - now).min(std::time::Duration::from_millis(50)));
        page.run_scripts();
    }
    log::info!(
        "loaded {} in {:?} (fetch {}ms, parse {}ms, subresources {}ms [{} sheets, {} images, {} failed], style {}ms, layout {}ms)",
        page.url,
        t0.elapsed(),
        page.stats.fetch_ms,
        page.stats.parse_ms,
        page.stats.subresources_ms,
        page.stats.stylesheets,
        page.stats.images,
        page.stats.images_failed,
        page.stats.style_ms,
        page.stats.layout_ms
    );
    if let Some(e) = &page.error {
        log::warn!("load error: {e}");
    }
    if args.dump_dom {
        print!("{}", bb_html::dump(&page.doc));
    }
    if args.dump_style {
        print!("{}", bb_style::dump_styles(&page.doc, &page.styles));
    }
    if args.dump_layout {
        print!("{}", bb_layout_dump(&page));
    }
    if let Some(out) = &args.screenshot {
        let scale = args.scale.unwrap_or(1.0);
        let css_h = if args.full_page { page.content_height().max(h as f32) } else { h as f32 };
        let (pw, ph) = ((w as f32 * scale).round() as u32, (css_h * scale).round().min(32760.0) as u32);
        let canvas = page.render(0.0, scale, pw, ph);
        std::fs::write(out, canvas.encode_png()).with_context(|| format!("writing {out}"))?;
        println!(
            "{out}: {pw}x{ph} — {}",
            if page.title.is_empty() { page.url.href() } else { page.title.clone() }
        );
    }
    Ok(())
}

fn bb_layout_dump(page: &Page) -> String {
    bb_layout::dump(&page.layout)
}

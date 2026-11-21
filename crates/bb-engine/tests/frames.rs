//! Frames: framesets, iframes, link targets and frame scrolling (HTML §15.6, §4.8.5, §7.3.1.7).

use bb_engine::{new_client, LinkDisposition, Page, PageConfig, MAX_FRAME_DEPTH};
use bb_url::Url;

fn page_url(name: &str) -> Url {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/pages/frames").join(name);
    Url::from_file_path(&p.canonicalize().unwrap().to_string_lossy()).unwrap()
}

fn load(name: &str, w: f32, h: f32) -> Page {
    let client = new_client(Vec::new());
    Page::load(&client, &page_url(name), PageConfig { viewport: (w, h), ..Default::default() })
}

fn titles(p: &Page) -> Vec<String> {
    p.frames.iter().map(|f| f.page.title.clone()).collect()
}

#[test]
fn frameset_loads_nested_frames_with_rects() {
    let p = load("index.html", 900.0, 500.0);
    assert!(p.frameset.is_some());
    assert_eq!(titles(&p), ["banner", "nav", "Welcome"]);
    let r: Vec<_> = p.frames.iter().map(|f| (f.rect.x, f.rect.y, f.rect.w, f.rect.h)).collect();
    // rows="64,*" with a 6px border; inner cols="180,*".
    assert_eq!(r[0], (0.0, 0.0, 900.0, 64.0));
    assert_eq!(r[1], (0.0, 70.0, 180.0, 430.0));
    assert_eq!(r[2], (186.0, 70.0, 714.0, 430.0));
    assert_eq!(p.frames[1].name, "nav");
    assert!(!p.frames[0].scrolling);
    assert_eq!(p.content_height(), 500.0, "a frameset never scrolls itself");
    // Resizing relayouts the frames.
    let mut p = p;
    p.set_viewport(1000.0, 600.0);
    assert_eq!(p.frames[2].rect.w, 814.0);
    assert_eq!(p.frames[2].page.config.viewport, (814.0, 530.0));
}

#[test]
fn targeted_links_navigate_frames() {
    let client = new_client(Vec::new());
    let mut p = Page::load(
        &client,
        &page_url("index.html"),
        PageConfig { viewport: (900.0, 500.0), ..Default::default() },
    );
    // "Cool Links" in the nav frame (target="main"). Find it via hit testing.
    let nav = p.frames[1].rect;
    let hit = (0..400)
        .step_by(4)
        .find_map(|y| {
            let h = p.link_hit_at(nav.x + 60.0, nav.y + y as f32)?;
            h.url.href().ends_with("links.html").then_some(h)
        })
        .expect("Cool Links link is hit-testable inside the nav frame");
    assert_eq!(hit.target, "main");
    assert_eq!(hit.frame_path, vec![1]);
    assert_eq!(p.follow_link(&client, hit), LinkDisposition::Frame);
    assert_eq!(titles(&p), ["banner", "nav", "Links"]);
    // A link without target inside a frame navigates that frame.
    let main = p.frames[2].rect;
    let hit = (0..300)
        .step_by(4)
        .find_map(|y| {
            let h = p.link_hit_at(main.x + 80.0, main.y + y as f32)?;
            h.url.href().ends_with("welcome.html").then_some(h)
        })
        .unwrap();
    assert_eq!(hit.target, "");
    assert_eq!(p.follow_link(&client, hit), LinkDisposition::Frame);
    assert_eq!(titles(&p)[2], "Welcome");
    // target="_top" leaves the frameset.
    let hit = (0..400)
        .step_by(4)
        .find_map(|y| p.link_hit_at(nav.x + 50.0, nav.y + y as f32).filter(|h| h.target == "_top"))
        .unwrap();
    assert!(
        matches!(p.follow_link(&client, hit), LinkDisposition::Top(u) if u.href().ends_with("welcome.html"))
    );
    // Unknown names and _blank open a new tab.
    let mut h = p.link_hit_at(nav.x + 60.0, nav.y + 40.0).unwrap_or(bb_engine::LinkHit {
        url: page_url("links.html"),
        target: String::new(),
        frame_path: vec![1],
    });
    h.target = "_blank".into();
    assert!(matches!(p.follow_link(&client, h.clone()), LinkDisposition::NewTab(_)));
    h.target = "nonexistent".into();
    assert!(matches!(p.follow_link(&client, h), LinkDisposition::NewTab(_)));
}

#[test]
fn frames_scroll_independently() {
    let mut p = load("index.html", 900.0, 500.0);
    let main = p.frames[2].rect;
    assert!(p.scroll_frame_at(main.x + 10.0, main.y + 10.0, 200.0));
    assert_eq!(p.frames[2].scroll_y, 200.0);
    // scrolling="no" banner doesn't consume the wheel.
    assert!(!p.scroll_frame_at(10.0, 10.0, 100.0));
    // Scrolling up past the top is not consumed.
    assert!(p.scroll_frame_at(main.x + 10.0, main.y + 10.0, -1000.0));
    assert!(!p.scroll_frame_at(main.x + 10.0, main.y + 10.0, -10.0));
    // The rendered frame shows the scrolled content (no panic, right size).
    let c = p.render(0.0, 1.0, 900, 500);
    assert_eq!(c.to_rgba().len(), 900 * 500 * 4);
}

#[test]
fn iframes_load_src_and_srcdoc_and_accept_targets() {
    let client = new_client(Vec::new());
    let mut p = Page::load(
        &client,
        &page_url("inline.html"),
        PageConfig { viewport: (900.0, 400.0), ..Default::default() },
    );
    assert!(p.frameset.is_none());
    assert_eq!(p.frames.len(), 2);
    assert_eq!(p.frames[0].page.title, "Links");
    assert_eq!(p.frames[0].name, "inner");
    assert_eq!((p.frames[0].rect.w, p.frames[0].rect.h), (320.0, 120.0));
    assert!(p.frames[1].page.doc.text_content(bb_dom::Document::ROOT).contains("srcdoc content"));
    let hit = (0..400)
        .step_by(3)
        .find_map(|y| p.link_hit_at(60.0, y as f32).filter(|h| h.target == "inner"))
        .expect("link targeting the iframe");
    assert_eq!(p.follow_link(&client, hit), LinkDisposition::Frame);
    assert_eq!(p.frames[0].page.title, "Welcome");
}

#[test]
fn self_framing_page_stops_at_depth_limit() {
    let p = load("recursive.html", 800.0, 600.0);
    fn depth(p: &Page) -> u8 {
        p.frames.iter().map(|f| 1 + depth(&f.page)).max().unwrap_or(0)
    }
    assert_eq!(depth(&p), MAX_FRAME_DEPTH);
}

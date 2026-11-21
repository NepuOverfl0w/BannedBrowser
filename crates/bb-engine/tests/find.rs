//! Find in page.

use bb_engine::{new_client, Page, PageConfig};
use bb_url::Url;

fn page(html: &str) -> Page {
    let client = new_client(Vec::new());
    let url = Url::parse("about:blank").unwrap();
    Page::from_html(
        &client,
        html,
        &url,
        PageConfig { viewport: (600.0, 400.0), ..Default::default() },
        200,
        false,
    )
}

#[test]
fn finds_case_insensitively_and_across_elements() {
    let mut p = page("<p>Hello world. HELLO again.</p><p>foo<b>bar</b> baz</p>");
    assert_eq!(p.find("hello", 0.0), (2, Some(0)));
    assert_eq!(p.find("foobar", 0.0).0, 1, "match spanning <b>");
    assert_eq!(p.find_matches[0].len(), 2, "one rect per text run");
    assert_eq!(p.find("nothing here", 0.0), (0, None));
    assert_eq!(p.find("", 0.0), (0, None));
}

#[test]
fn rects_cover_the_matched_text() {
    let mut p = page("<body style='margin:0;font:20px sans-serif'><p style='margin:0'>aaa needle bbb</p>");
    p.find("needle", 0.0);
    let r = p.find_active_rect().unwrap();
    let mut q = page("<body style='margin:0;font:20px sans-serif'><p style='margin:0'>aaa needle bbb</p>");
    q.find("aaa ", 0.0);
    let prefix = q.find_active_rect().unwrap();
    // The match starts after "aaa" plus one space (trailing spaces hang outside the piece's glyphs).
    assert!(r.x > prefix.right() + 2.0 && r.x < prefix.right() + 10.0, "{r:?} vs {prefix:?}");
    assert!(r.w > 30.0 && r.w < 90.0, "{r:?}");
    assert!(r.y >= 0.0 && r.h > 15.0 && r.h < 40.0, "{r:?}");
}

#[test]
fn stepping_wraps_and_from_y_picks_the_first_visible() {
    let mut p =
        page("<p>x</p><div style='height:1000px'></div><p>x</p><div style='height:1000px'></div><p>x</p>");
    assert_eq!(p.find("x", 0.0), (3, Some(0)));
    assert_eq!(p.find_step(true), (3, Some(1)));
    assert_eq!(p.find_step(true), (3, Some(2)));
    assert_eq!(p.find_step(true), (3, Some(0)));
    assert_eq!(p.find_step(false), (3, Some(2)));
    assert_eq!(p.find("x", 500.0), (3, Some(1)));
    assert!(p.find_active_rect().unwrap().y > 1000.0);
    // Matches follow relayout.
    let before = p.find_active_rect().unwrap();
    p.set_viewport(300.0, 400.0);
    assert_eq!(p.find_matches.len(), 3);
    assert!(p.find_active_rect().is_some() && p.find_active_rect().unwrap().y >= before.y - 1.0);
    p.find_clear();
    assert!(p.find_matches.is_empty() && p.find_active.is_none());
}

#[test]
fn highlights_are_painted() {
    let mut p =
        page("<body style='margin:0;background:white'><p style='margin:0;font:40px sans-serif'>MMMM</p>");
    let plain = p.render(0.0, 1.0, 200, 60).to_rgba();
    p.find("mm", 0.0);
    assert_eq!(p.find_matches.len(), 2);
    let lit = p.render(0.0, 1.0, 200, 60).to_rgba();
    assert_ne!(plain, lit);
}

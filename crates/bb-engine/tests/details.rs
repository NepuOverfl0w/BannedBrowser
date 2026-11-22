//! `<details>`/`<summary>` (HTML §4.11.1-2): a closed details element shows only its summary,
//! clicking the summary toggles `open`, and only the first summary of a details element
//! toggles it.

use bb_engine::{Page, PageConfig};
use bb_url::Url;

fn page(html: &str) -> Page {
    let client = bb_engine::new_client(Vec::new());
    let url = Url::parse("about:blank").unwrap();
    Page::from_html(
        &client,
        html,
        &url,
        PageConfig { viewport: (800.0, 600.0), ..Default::default() },
        200,
        false,
    )
}

const DOC: &str = "<!DOCTYPE html><style>body{margin:0;font:20px/30px sans-serif} p{margin:0}</style>\
    <details id=d><summary id=s>More</summary><p>one</p><p>two</p><p>three</p></details>\
    <p id=after>after</p>";

fn rect(p: &Page, id: &str) -> [f32; 4] {
    p.element_rects_by_id().into_iter().find(|(i, _)| i == id).map(|(_, r)| r).expect(id)
}

#[test]
fn closed_details_shows_only_the_summary_and_toggles_on_click() {
    let mut p = page(DOC);
    let closed_after = rect(&p, "after")[1];
    assert!(p.doc.attr(p.doc.get_element_by_id("d").unwrap(), "open").is_none());
    // Click the summary: it opens and the hidden paragraphs take up space.
    let s = rect(&p, "s");
    assert!(p.activate_at(s[0] + 5.0, s[1] + s[3] / 2.0));
    assert!(p.doc.attr(p.doc.get_element_by_id("d").unwrap(), "open").is_some());
    let open_after = rect(&p, "after")[1];
    assert!(open_after >= closed_after + 3.0 * 29.0, "closed {closed_after}, open {open_after}");
    // Click again: closed.
    let s = rect(&p, "s");
    assert!(p.activate_at(s[0] + 5.0, s[1] + s[3] / 2.0));
    assert_eq!(rect(&p, "after")[1], closed_after);
}

#[test]
fn clicks_elsewhere_and_second_summaries_do_nothing() {
    let mut p = page(
        "<!DOCTYPE html><style>body{margin:0;font:20px/30px sans-serif}</style>\
         <details open id=d><summary>first</summary><summary id=s2>second</summary></details><p id=x>x</p>",
    );
    let x = rect(&p, "x");
    assert!(!p.activate_at(x[0] + 2.0, x[1] + 2.0));
    let s2 = rect(&p, "s2");
    assert!(!p.activate_at(s2[0] + 2.0, s2[1] + 2.0));
    assert!(p.doc.attr(p.doc.get_element_by_id("d").unwrap(), "open").is_some());
}

#[test]
fn details_without_summary_gets_a_default_one() {
    let p = page("<!DOCTYPE html><details><p id=hidden>hidden</p></details>");
    let text = bb_layout::dump(&p.layout);
    assert!(text.contains("Details"), "{text}");
    assert!(!p.element_rects_by_id().iter().any(|(i, _)| i == "hidden"));
}

//! `bb-style` — the CSS cascade, inheritance, computed values, the UA
//! stylesheet and HTML presentational hints.
#![forbid(unsafe_code)]

mod cascade;
pub mod computed;
pub mod hints;
pub mod properties;

pub use cascade::{DomElement, FontFaceRule, Origin, PseudoKind, Sheet, StyleEnv, StyleMap, Stylist};
pub use computed::*;

use std::rc::Rc;

/// The HTML Standard-derived user-agent stylesheet.
pub const UA_CSS: &str = include_str!("../../../resources/ua.css");
/// Extra UA rules for quirks-mode documents.
pub const QUIRKS_CSS: &str = include_str!("../../../resources/quirks.css");

thread_local! {
    static UA_SHEET: Rc<bb_css::Stylesheet> = Rc::new(bb_css::Stylesheet::parse(UA_CSS));
    static QUIRKS_SHEET: Rc<bb_css::Stylesheet> = Rc::new(bb_css::Stylesheet::parse(QUIRKS_CSS));
}

/// The parsed UA stylesheet(s) for a document's mode.
pub fn ua_sheets(quirks: bool) -> Vec<Sheet> {
    let mut v = vec![Sheet {
        origin: Origin::UserAgent,
        stylesheet: UA_SHEET.with(|s| s.clone()),
        media: bb_css::MediaQueryList::default(),
    }];
    if quirks {
        v.push(Sheet {
            origin: Origin::UserAgent,
            stylesheet: QUIRKS_SHEET.with(|s| s.clone()),
            media: bb_css::MediaQueryList::default(),
        });
    }
    v
}

/// Human-readable dump of computed styles (for `--dump-style`).
pub fn dump_styles(doc: &bb_dom::Document, map: &StyleMap) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let mut stack: Vec<(bb_dom::NodeId, usize)> = vec![(bb_dom::Document::ROOT, 0)];
    while let Some((n, depth)) = stack.pop() {
        if let (Some(e), Some(s)) = (doc.element(n), map.get(n)) {
            let pad = "  ".repeat(depth.saturating_sub(1));
            let _ = writeln!(
                out,
                "{pad}<{}> display={:?} color=#{:02x}{:02x}{:02x} bg=#{:02x}{:02x}{:02x}{:02x} font={}px/{:?} w{} {:?} family={:?} margin=[{}] padding=[{}] border=[{:?}] width={:?} text-align={:?}",
                e.name, s.display, s.color.r, s.color.g, s.color.b,
                s.background_color.r, s.background_color.g, s.background_color.b, s.background_color.a,
                s.font_size, s.line_height, s.font_weight, s.font_style, s.font_family,
                s.margin.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join(" "),
                s.padding.iter().map(|m| format!("{m:?}")).collect::<Vec<_>>().join(" "),
                s.border_width, s.width, s.text_align,
            );
        }
        let kids: Vec<_> = doc.children(n).collect();
        for &k in kids.iter().rev() {
            stack.push((k, depth + 1));
        }
    }
    out
}

/// Convenience: compute styles for a document with only the UA sheet plus `author_css`.
pub fn style_document(doc: &bb_dom::Document, author_css: &[&str], env: &StyleEnv) -> StyleMap {
    let mut sheets = ua_sheets(doc.quirks_mode == bb_dom::QuirksMode::Quirks);
    for css in author_css {
        sheets.push(Sheet {
            origin: Origin::Author,
            stylesheet: Rc::new(bb_css::Stylesheet::parse(css)),
            media: bb_css::MediaQueryList::default(),
        });
    }
    let media = bb_css::MediaContext {
        width: env.viewport.0,
        height: env.viewport.1,
        dark: env.prefers_dark,
        print: false,
        dppx: 1.0,
    };
    Stylist::new(&sheets, media).compute(doc, env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bb_css::Rgba;

    fn styled(html: &str, css: &str) -> (bb_dom::Document, StyleMap) {
        let doc = bb_html::parse_document(html);
        let map = style_document(&doc, &[css], &StyleEnv::default());
        (doc, map)
    }

    fn by_id(doc: &bb_dom::Document, map: &StyleMap, id: &str) -> Rc<ComputedStyle> {
        map.get(doc.get_element_by_id(id).unwrap()).unwrap().clone()
    }

    #[test]
    fn cascade_specificity_and_importance() {
        let (d, m) = styled(
            "<p id=a class=c>x</p>",
            "#a { color: red } .c { color: blue !important } p { color: green }",
        );
        assert_eq!(by_id(&d, &m, "a").color, Rgba::rgb(0, 0, 255));
        let (d, m) = styled("<p id=a style='color: lime'>x</p>", "#a { color: red }");
        assert_eq!(by_id(&d, &m, "a").color, Rgba::rgb(0, 255, 0));
        let (d, m) = styled("<p id=a style='color: lime'>x</p>", "#a { color: red !important }");
        assert_eq!(by_id(&d, &m, "a").color, Rgba::rgb(255, 0, 0));
        // Later rule with equal specificity wins.
        let (d, m) = styled("<p id=a>x</p>", "p { color: red } p { color: blue }");
        assert_eq!(by_id(&d, &m, "a").color, Rgba::rgb(0, 0, 255));
    }

    #[test]
    fn inheritance_and_em() {
        let (d, m) = styled(
            "<div id=d><span id=s>x</span></div>",
            "#d { font-size: 20px; color: red; margin: 1em; padding: 2em } #s { font-size: 1.5em }",
        );
        let s = by_id(&d, &m, "s");
        assert_eq!(s.font_size, 30.0);
        assert_eq!(s.color, Rgba::rgb(255, 0, 0));
        assert!(s.padding[0].is_zero(), "padding is not inherited");
        let dv = by_id(&d, &m, "d");
        assert_eq!(dv.margin[0], LPA::LP(LP::Px(20.0)));
        assert_eq!(dv.display, Display::Block);
    }

    #[test]
    fn ua_defaults() {
        let (d, m) = styled("<h1 id=h>x</h1><a id=a href=#>y</a><ul><li id=l>z</ul>", "");
        let h = by_id(&d, &m, "h");
        assert_eq!(h.font_size, 32.0);
        assert_eq!(h.font_weight, 700);
        let a = by_id(&d, &m, "a");
        assert_eq!(a.color, Rgba::rgb(0, 0, 0xee));
        assert!(a.text_decoration_line.underline);
        assert_eq!(by_id(&d, &m, "l").display, Display::ListItem);
    }

    #[test]
    fn custom_properties() {
        let (d, m) = styled(
            "<div id=p><p id=c>x</p></div>",
            ":root { --main: #00f; --pad: 4px } #p { --main: red } #c { color: var(--main); padding: var(--pad) var(--missing, 8px) }",
        );
        let c = by_id(&d, &m, "c");
        assert_eq!(c.color, Rgba::rgb(255, 0, 0));
        assert_eq!(c.padding[0], LP::Px(4.0));
        assert_eq!(c.padding[1], LP::Px(8.0));
    }

    #[test]
    fn example_com_light_dark() {
        let css = "html{color-scheme:light dark;background:light-dark(#eee,#222)}body{font:16px/1.6 system-ui,sans-serif;max-width:26em;margin:auto;padding:25vh 2em 2em;text-align:center}";
        let doc = bb_html::parse_document("<!doctype html><html><body><p>x</p></body></html>");
        let light = style_document(&doc, &[css], &StyleEnv::default());
        assert_eq!(light.canvas_background, Rgba::rgb(0xee, 0xee, 0xee));
        let body = light.get(doc.body().unwrap()).unwrap();
        assert_eq!(body.line_height, LineHeight::Number(1.6));
        assert_eq!(body.font_family[0], "system-ui");
        assert_eq!(body.max_width, Some(LP::Px(416.0)));
        assert_eq!(body.margin[1], LPA::Auto);
        assert_eq!(body.padding[0], LP::Px(192.0));
        assert_eq!(body.color, Rgba::BLACK);
        let dark = style_document(&doc, &[css], &StyleEnv { prefers_dark: true, ..Default::default() });
        assert_eq!(dark.canvas_background, Rgba::rgb(0x22, 0x22, 0x22));
        let body = dark.get(doc.body().unwrap()).unwrap();
        assert!(body.color.brightness() > 0.8, "CanvasText is light in dark scheme");
    }

    #[test]
    fn legacy_hints_apply() {
        let (d, m) = styled(
            "<body id=b bgcolor=black text=lime link=yellow><font id=f size=6 color=maroon>a</font><a id=a href=x>l</a>\
             <center><table id=t align=center width=50%></table></center>",
            "",
        );
        let b = by_id(&d, &m, "b");
        assert_eq!(b.background_color, Rgba::BLACK);
        assert_eq!(b.color, Rgba::rgb(0, 255, 0));
        let f = by_id(&d, &m, "f");
        assert_eq!(f.font_size, 32.0);
        assert_eq!(f.color, Rgba::rgb(128, 0, 0));
        assert_eq!(by_id(&d, &m, "a").color, Rgba::rgb(255, 255, 0));
        let t = by_id(&d, &m, "t");
        assert_eq!(t.margin[1], LPA::Auto);
        assert_eq!(t.width, LPA::LP(LP::Pct(50.0)));
    }

    #[test]
    fn quirks_table_font_reset() {
        let (d, m) = styled("<font size=7><table><tr><td id=c>x</table></font>", "");
        assert_eq!(by_id(&d, &m, "c").font_size, 16.0);
        let (d, m) = styled("<!doctype html><font size=7><table><tr><td id=c>x</table></font>", "");
        assert_eq!(by_id(&d, &m, "c").font_size, 48.0);
    }

    #[test]
    fn media_rules() {
        let (d, m) = styled("<p id=a>x</p>", "@media (max-width: 500px) { #a { color: red } } @media (min-width: 500px) { #a { color: blue } }");
        assert_eq!(by_id(&d, &m, "a").color, Rgba::rgb(0, 0, 255));
    }

    #[test]
    fn display_fixups() {
        let (d, m) = styled("<span id=s style='float:left'>x</span><span id=p style='position:absolute;display:inline-table'>y</span>", "");
        assert_eq!(by_id(&d, &m, "s").display, Display::Block);
        assert_eq!(by_id(&d, &m, "p").display, Display::Table);
    }
}

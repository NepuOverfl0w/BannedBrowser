use proptest::prelude::*;

const PIECES: &[&str] = &[
    "a",
    "{",
    "}",
    ";",
    ":",
    "(",
    ")",
    "[",
    "]",
    "@media",
    "@import",
    "url(",
    "\"",
    "'",
    "\\",
    "/*",
    "*/",
    "!important",
    "#fff",
    "1.5em",
    "-",
    "+",
    ">",
    "~",
    ",",
    ".x",
    ":not(",
    "::before",
    "calc(",
    "100%",
    "rgb(",
    "@supports",
    "--v",
    "var(",
    " ",
    "\n",
    "e3",
    "@font-face",
    "<!--",
    "-->",
    "nth-child(",
    "2n+1",
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]
    #[test]
    fn css_soup_never_panics(idx in proptest::collection::vec(0usize..PIECES.len(), 0..80)) {
        let css: String = idx.iter().map(|&i| PIECES[i]).collect();
        let ss = bb_css::Stylesheet::parse(&css);
        let _ = ss.style_rule_count();
        let _ = bb_css::parse_declaration_list(&css);
        let cvs = bb_css::parse_component_values(&css);
        let _ = bb_css::values::parse_color(&cvs);
        let _ = bb_css::MediaQueryList::parse(&cvs).evaluate(&Default::default());
        let _ = bb_css::selectors::parse_selector_list(&cvs);
    }

    #[test]
    fn arbitrary_css_never_panics(s in "\\PC{0,300}") {
        let _ = bb_css::Stylesheet::parse(&s);
        let _ = bb_css::values::parse_legacy_color(&s);
    }
}

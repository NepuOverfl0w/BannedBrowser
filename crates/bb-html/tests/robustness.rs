//! Randomized robustness: the parser must never panic and must always
//! produce a document with an <html> element containing <head> and <body>
//! (or <frameset>).
use proptest::prelude::*;

const PIECES: &[&str] = &[
    "<",
    ">",
    "</",
    "/>",
    "<!--",
    "-->",
    "<!DOCTYPE html>",
    "<html>",
    "<head>",
    "<body>",
    "<table>",
    "<tr>",
    "<td>",
    "</td>",
    "</table>",
    "<p>",
    "</p>",
    "<b>",
    "</b>",
    "<i>",
    "</i>",
    "<a href=x>",
    "</a>",
    "<svg>",
    "<math>",
    "<mi>",
    "<foreignObject>",
    "</svg>",
    "<template>",
    "</template>",
    "<select>",
    "<option>",
    "<frameset>",
    "<frame>",
    "<script>",
    "</script>",
    "<style>",
    "<textarea>",
    "<plaintext>",
    "&amp;",
    "&#x110000;",
    "&notin",
    "\0",
    "text",
    " ",
    "\n",
    "<![CDATA[",
    "]]>",
    "<li>",
    "<dd>",
    "<h1>",
    "</h2>",
    "<form>",
    "</form>",
    "<caption>",
    "<colgroup>",
    "<col>",
    "<tbody>",
    "<nobr>",
    "<marquee>",
    "</object>",
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]
    #[test]
    fn soup_never_panics(idx in proptest::collection::vec(0usize..PIECES.len(), 0..60)) {
        let input: String = idx.iter().map(|&i| PIECES[i]).collect();
        let doc = bb_html::parse_document(&input);
        let html = doc.document_element();
        prop_assert!(html.is_some());
        let _ = bb_html::dump(&doc);
        let _ = bb_html::parse_fragment(&input, bb_dom::Namespace::Html, "td");
    }

    #[test]
    fn arbitrary_text_never_panics(s in "\\PC{0,200}") {
        let doc = bb_html::parse_document(&s);
        prop_assert!(doc.body().is_some());
    }
}

#[test]
fn deep_nesting_is_fine() {
    let input = "<div>".repeat(20_000) + &"<b>".repeat(5_000) + "x";
    let doc = bb_html::parse_document(&input);
    assert!(doc.body().is_some());
}

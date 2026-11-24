//! The XML parser and serializer: well-formed documents, fatal errors, entities, namespaces.

use bb_dom::{Document, Namespace, NodeData};
use bb_xml::serialize::serialize;
use bb_xml::{parse, parse_with_decl};

fn root_xml(src: &str) -> String {
    let d = parse(src).unwrap_or_else(|e| panic!("{src}: {e}"));
    serialize(&d, Document::ROOT, false).unwrap()
}

fn fails(src: &str) -> String {
    match parse(src) {
        Ok(_) => panic!("parsed, but should fail: {src}"),
        Err(e) => e.message,
    }
}

#[test]
fn elements_text_and_attributes() {
    let d = parse("<a x='1' y=\"2\">hi <b/> there</a>").unwrap();
    let a = d.document_element().unwrap();
    let e = d.element(a).unwrap();
    assert_eq!(e.name, "a");
    assert_eq!(e.ns, Namespace::None);
    assert_eq!(d.attr(a, "x"), Some("1"));
    assert_eq!(d.children(a).count(), 3);
    assert_eq!(d.text_content(a), "hi  there");
    assert_eq!(root_xml("<a x='1'>hi <b/> there</a>"), "<a x=\"1\">hi <b/> there</a>");
}

#[test]
fn prolog_doctype_comments_and_pis() {
    let (d, decl) =
        parse_with_decl("<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n<!-- c --><?pi data?><!DOCTYPE r SYSTEM 'r.dtd'><r/>").unwrap();
    assert_eq!(decl.encoding.as_deref(), Some("UTF-8"));
    assert_eq!(decl.standalone, Some(true));
    let kinds: Vec<&str> = d
        .children(Document::ROOT)
        .map(|k| match &d.node(k).data {
            NodeData::Comment(_) => "comment",
            NodeData::ProcessingInstruction { .. } => "pi",
            NodeData::Doctype { .. } => "doctype",
            NodeData::Element(_) => "element",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["comment", "pi", "doctype", "element"]);
}

#[test]
fn references_and_cdata() {
    let d = parse("<a>&lt;&amp;&#65;&#x42;<![CDATA[<raw>&]]></a>").unwrap();
    let a = d.document_element().unwrap();
    let kids: Vec<_> = d.children(a).collect();
    assert_eq!(kids.len(), 2);
    assert_eq!(d.node(kids[0]).as_text(), Some("<&AB"));
    assert!(d.is_cdata(kids[1]));
    assert_eq!(d.node(kids[1]).as_text(), Some("<raw>&"));
    assert_eq!(root_xml("<a>&lt;x<![CDATA[y]]></a>"), "<a>&lt;x<![CDATA[y]]></a>");
}

#[test]
fn line_ends_and_attribute_normalization() {
    let d = parse("<a v='x\r\ny\tz'>1\r2\r\n3</a>").unwrap();
    let a = d.document_element().unwrap();
    assert_eq!(d.attr(a, "v"), Some("x y z"));
    assert_eq!(d.text_content(a), "1\n2\n3");
}

#[test]
fn internal_subset_entities_and_defaults() {
    let src = "<!DOCTYPE r [\
        <!ENTITY who 'world'>\
        <!ENTITY greet 'hello &who;'>\
        <!ENTITY el '<b>bold</b>'>\
        <!ATTLIST r lang CDATA 'en' tok NMTOKENS #IMPLIED>\
        <!ELEMENT r (#PCDATA|b)*>\
        ]><r tok='  a   b '>&greet; &el;</r>";
    let d = parse(src).unwrap();
    let r = d.document_element().unwrap();
    assert_eq!(d.attr(r, "lang"), Some("en"));
    assert_eq!(d.attr(r, "tok"), Some("a b"));
    assert_eq!(d.text_content(r), "hello world bold");
    assert_eq!(d.element_children(r).count(), 1);
}

#[test]
fn namespaces() {
    let src = "<r xmlns='urn:d' xmlns:p='urn:p'><p:a p:x='1' y='2'/><b xmlns=''/></r>";
    let d = parse(src).unwrap();
    let r = d.document_element().unwrap();
    assert_eq!(d.element(r).unwrap().ns.url(), "urn:d");
    let kids: Vec<_> = d.element_children(r).collect();
    let a = d.element(kids[0]).unwrap();
    assert_eq!((a.ns.url(), a.prefix.as_deref(), a.name.as_str()), ("urn:p", Some("p"), "a"));
    assert_eq!(a.attrs[0].ns.url(), "urn:p");
    assert_eq!(a.attrs[1].ns, Namespace::None);
    assert_eq!(d.element(kids[1]).unwrap().ns, Namespace::None);
    assert_eq!(root_xml(src), src.replace('\'', "\""));
}

#[test]
fn fatal_errors() {
    for (src, why) in [
        ("", "document element"),
        ("<a>", "isn't closed"),
        ("<a></b>", "doesn't match"),
        ("<a x='1' x='2'/>", "twice"),
        ("<a x='<'/>", "'<'"),
        ("<a>&nope;</a>", "isn't declared"),
        ("<a/><b/>", "after the document element"),
        ("<a><!-- x -- y --></a>", "'--'"),
        ("<a>]]></a>", "]]>"),
        ("<p:a/>", "prefix"),
        ("<a xmlns:p=''/>", "empty namespace"),
        ("<a>&#0;</a>", "character"),
        ("<a>\u{1}</a>", "not allowed"),
        (" <?xml version='1.0'?><a/>", "XML declaration"),
        ("<!DOCTYPE a [<!ENTITY e '&e;'>]><a>&e;</a>", "itself"),
        ("<!DOCTYPE a [<!ENTITY e '<b>'>]><a>&e;</a>", "isn't closed"),
    ] {
        let m = fails(src);
        assert!(m.contains(why), "{src}: {m}");
    }
}

#[test]
fn errors_have_positions() {
    let e = parse("<a>\n  <b>\n</a>").unwrap_err();
    assert_eq!(e.line, 3);
    assert!(e.message.contains("doesn't match"), "{e}");
}

#[test]
fn billion_laughs_is_refused() {
    let mut src = String::from("<!DOCTYPE a [<!ENTITY l0 'lollollollollollollollollollol'>");
    for i in 1..12 {
        let p = i - 1;
        src.push_str(&format!(
            "<!ENTITY l{i} '&l{p};&l{p};&l{p};&l{p};&l{p};&l{p};&l{p};&l{p};&l{p};&l{p};'>"
        ));
    }
    src.push_str("]><a>&l11;</a>");
    assert!(fails(&src).contains("too much"));
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    let src = "<a>".repeat(100_000);
    assert!(fails(&src).contains("too deeply"));
    // Just under the limit parses, without deep recursion (Windows threads have small stacks).
    let ok = format!("{}{}", "<a>".repeat(2000), "</a>".repeat(2000));
    let d = parse(&ok).unwrap();
    assert_eq!(d.descendants(Document::ROOT).count(), 2001);
    // Elements inside entities nest and close like any others.
    let e = parse("<!DOCTYPE r [<!ENTITY e '<b><c/></b>'>]><r>&e;&e;</r>").unwrap();
    assert_eq!(e.descendants(Document::ROOT).count(), 7, "root, doctype, r and two b and c");
    assert!(fails("<!DOCTYPE r [<!ENTITY e '<b>'>]><r>&e;</b></r>").contains("isn't closed"));
}

#[test]
fn undeclared_entities_with_an_external_subset_are_skipped() {
    let d = parse("<!DOCTYPE a SYSTEM 'a.dtd'><a>x&ext;y</a>").unwrap();
    assert_eq!(d.text_content(d.document_element().unwrap()), "xy");
}

#[test]
fn serializer_declares_missing_prefixes() {
    // An element in a namespace with no declaration in scope gets one.
    let mut d = Document::new();
    let e = d.create_element(Namespace::from_url("urn:x"), "e", Vec::new());
    d.append_child(Document::ROOT, e);
    assert_eq!(serialize(&d, Document::ROOT, false).unwrap(), "<e xmlns=\"urn:x\"/>");
    let a = bb_dom::Attribute {
        ns: Namespace::from_url("urn:y"),
        prefix: None,
        name: "k".into(),
        value: "v".into(),
    };
    if let Some(el) = d.element_mut(e) {
        el.attrs.push(a);
    }
    assert_eq!(
        serialize(&d, Document::ROOT, false).unwrap(),
        "<e xmlns=\"urn:x\" xmlns:ns1=\"urn:y\" ns1:k=\"v\"/>"
    );
}

proptest::proptest! {
    #[test]
    fn never_panics(s in "[<>a-z&;#x0-9:='\"!\\[\\]?/ -]{0,80}") {
        let _ = parse(&s);
        if let Ok(d) = parse(&s) {
            let _ = serialize(&d, Document::ROOT, true);
        }
    }
}

#[test]
fn fragments_use_the_context_namespaces() {
    let scope = vec![(None, "urn:d".to_string()), (Some("p".to_string()), "urn:p".to_string())];
    let d = bb_xml::parse_fragment("text<a/><p:b>&amp;</p:b>", &scope).unwrap();
    let kids: Vec<_> = d.children(Document::ROOT).collect();
    assert_eq!(kids.len(), 3);
    assert_eq!(d.element(kids[1]).unwrap().ns.url(), "urn:d");
    assert_eq!(d.element(kids[2]).unwrap().ns.url(), "urn:p");
    assert!(bb_xml::parse_fragment("<a>", &scope).is_err());
    assert!(bb_xml::parse_fragment("</a>", &scope).is_err());
    assert!(bb_xml::parse_fragment("<q:a/>", &scope).is_err());
}

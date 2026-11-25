//! Forms (HTML §4.10): editing controls with the mouse and keyboard, sequential focus
//! navigation, selects, form owners, and form submission (entry list, encodings, methods).

use bb_engine::forms::{entry_list, multipart, text_plain, urlencode, EntryValue};
use bb_engine::{new_client, FocusKind, KeyMods, LinkDisposition, Page, PageConfig, PageKey};
use bb_url::Url;

fn page(html: &str) -> Page {
    page_at(html, "http://example.test/dir/page.html")
}

fn page_at(html: &str, url: &str) -> Page {
    let client = new_client(Vec::new());
    let url = Url::parse(url).unwrap();
    Page::from_html(
        &client,
        html,
        &url,
        PageConfig { viewport: (800.0, 600.0), ..Default::default() },
        200,
        false,
    )
}

/// The centre of the element with this id (page coordinates).
fn centre(p: &Page, id: &str) -> (f32, f32) {
    let (_, [x, y, w, h]) =
        p.element_rects_by_id().into_iter().find(|(i, _)| i == id).unwrap_or_else(|| panic!("no #{id}"));
    (x + w / 2.0, y + h / 2.0)
}

fn click(p: &mut Page, id: &str) -> bb_engine::Outcome {
    let (x, y) = centre(p, id);
    p.click(x, y)
}

fn by_id(p: &Page, id: &str) -> bb_dom::NodeId {
    p.doc.get_element_by_id(id).unwrap()
}

fn key(p: &mut Page, k: PageKey) -> bb_engine::Outcome {
    p.key(k, KeyMods::default())
}

fn shift(p: &mut Page, k: PageKey) -> bb_engine::Outcome {
    p.key(k, KeyMods { shift: true, ..Default::default() })
}

fn submitted_url(o: &bb_engine::Outcome) -> String {
    o.submit.as_ref().expect("a submission").0.request.url.href()
}

const SEARCH: &str = r#"<form action="/search" id=f>
  <input id=q name=q value="old">
  <input type=hidden name=lang value=en>
  <input type=checkbox id=c name=safe value=1>
  <select id=s name=where><option>web<option value=img>Images<option disabled>off</select>
  <textarea id=t name=notes rows=3>line</textarea>
  <input type=submit id=go name=btn value=Go>
  <input type=submit id=other name=btn value=Other formaction="/alt" formmethod=post>
</form>"#;

#[test]
fn typing_and_implicit_submission() {
    let mut p = page(SEARCH);
    let out = click(&mut p, "q");
    assert!(out.repaint);
    assert_eq!(p.focused().map(|f| f.0), Some(FocusKind::Text));
    // Select all and type over it.
    p.key(PageKey::SelectAll, KeyMods::default());
    p.insert_text("rust browsers");
    key(&mut p, PageKey::Backspace);
    p.insert_text("s & co");
    assert_eq!(p.doc.control_value(by_id(&p, "q")), "rust browsers & co");
    // Enter submits with the default button (the first submit button) as submitter.
    let out = key(&mut p, PageKey::Enter);
    assert_eq!(
        submitted_url(&out),
        "http://example.test/search?q=rust+browsers+%26+co&lang=en&where=web&notes=line&btn=Go"
    );
}

#[test]
fn checkbox_select_textarea_and_submitter() {
    let mut p = page(SEARCH);
    click(&mut p, "c");
    assert!(p.doc.checkedness(by_id(&p, "c")));
    // Clicking a select opens its drop-down; disabled options can't be chosen.
    let out = click(&mut p, "s");
    let popup = out.popup.expect("drop-down");
    assert_eq!(popup.items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["web", "Images", "off"]);
    assert!(popup.items[0].selected && !popup.items[2].enabled);
    assert!(!p.choose_option(&[], popup.select, 2));
    assert!(p.choose_option(&[], popup.select, 1));
    // Keyboard on a focused select: Up moves back to "web", Down to "Images" (skipping
    // nothing), End stops at the last enabled option.
    key(&mut p, PageKey::Up);
    assert!(p.doc.option_selectedness(p.doc.list_of_options(popup.select)[0]));
    key(&mut p, PageKey::End);
    assert!(p.doc.option_selectedness(p.doc.list_of_options(popup.select)[1]));
    // Textarea: Enter inserts a line break; the value is submitted with CR LF.
    click(&mut p, "t");
    p.key(PageKey::End, KeyMods { primary: true, ..Default::default() });
    key(&mut p, PageKey::Enter);
    p.insert_text("two");
    assert_eq!(p.doc.control_value(by_id(&p, "t")), "line\ntwo");
    // The second button posts to its formaction.
    let out = click(&mut p, "other");
    let (s, path) = out.submit.expect("submission");
    assert!(path.is_empty());
    assert_eq!(s.request.url.href(), "http://example.test/alt");
    let post = s.request.post.expect("POST");
    assert_eq!(post.content_type, "application/x-www-form-urlencoded");
    assert_eq!(
        String::from_utf8(post.body).unwrap(),
        "q=old&lang=en&safe=1&where=img&notes=line%0D%0Atwo&btn=Other"
    );
}

#[test]
fn radio_groups_by_mouse_and_keyboard() {
    let mut p = page(
        r#"<form><input type=radio id=a name=r value=a checked><input type=radio id=b name=r value=b>
        <input type=radio id=c name=r value=c disabled><input type=radio id=x name=other></form>"#,
    );
    click(&mut p, "b");
    assert!(!p.doc.checkedness(by_id(&p, "a")) && p.doc.checkedness(by_id(&p, "b")));
    // Arrows move through the group (skipping the disabled one) and check as they go.
    key(&mut p, PageKey::Down);
    assert!(p.doc.checkedness(by_id(&p, "a")));
    key(&mut p, PageKey::Up);
    assert!(p.doc.checkedness(by_id(&p, "b")));
    assert!(!p.doc.checkedness(by_id(&p, "x")));
}

#[test]
fn labels_activate_their_controls() {
    let mut p = page(
        r#"<form><label id=l1><input type=checkbox id=c> Remember me</label>
        <label for=n id=l2>Name</label> <input id=n></form>"#,
    );
    // The label's text, not the box.
    let (_, [x, y, w, h]) = p.element_rects_by_id().into_iter().find(|(i, _)| i == "l1").unwrap();
    p.click(x + w - 5.0, y + h / 2.0);
    assert!(p.doc.checkedness(by_id(&p, "c")));
    click(&mut p, "l2");
    assert_eq!(p.focused().map(|f| f.0), Some(FocusKind::Text));
    p.insert_text("Ada");
    assert_eq!(p.doc.control_value(by_id(&p, "n")), "Ada");
}

#[test]
fn tab_order_skips_disabled_hidden_and_negative_tabindex() {
    let mut p = page(
        r#"<a href="/x" id=link>x</a>
        <input id=one><input id=two disabled><input id=three tabindex=-1>
        <div style="display:none"><input id=hidden></div>
        <button id=btn>b</button><input id=first tabindex=1>
        <details><summary id=sum>More</summary>text</details>"#,
    );
    let mut order = Vec::new();
    for _ in 0..6 {
        let out = key(&mut p, PageKey::Tab);
        if out.focus_exit.is_some() {
            order.push("exit".to_string());
            continue;
        }
        let n = p.focus.node.unwrap();
        order.push(p.doc.attr(n, "id").unwrap_or("?").to_string());
    }
    assert_eq!(order, ["first", "link", "one", "btn", "sum", "exit"]);
    // Shift+Tab from nothing goes to the last one.
    shift(&mut p, PageKey::Tab);
    assert_eq!(p.focus.node, Some(by_id(&p, "sum")));
    // Space on a focused summary toggles its details; Enter on a link follows it.
    key(&mut p, PageKey::Space);
    assert!(p.doc.attr(p.doc.elements_by_tag("details")[0], "open").is_some());
    p.focus_node(Some(by_id(&p, "link")));
    let out = key(&mut p, PageKey::Enter);
    assert_eq!(out.follow.map(|h| h.url.href()).as_deref(), Some("http://example.test/x"));
}

#[test]
fn required_fields_block_submission_and_take_focus() {
    let mut p = page(
        r#"<form action="/s"><input id=a name=a required><input id=b name=b><input type=submit id=go></form>"#,
    );
    let out = click(&mut p, "go");
    assert!(out.submit.is_none());
    assert_eq!(p.focus.node, Some(by_id(&p, "a")));
    p.insert_text("ok");
    let out = click(&mut p, "go");
    assert_eq!(submitted_url(&out), "http://example.test/s?a=ok&b=");
    // novalidate skips the check.
    let mut p =
        page(r#"<form action="/s" novalidate><input name=a required><input type=submit id=go></form>"#);
    assert_eq!(submitted_url(&click(&mut p, "go")), "http://example.test/s?a=");
}

#[test]
fn constraint_validation_blocks_submission_with_a_message() {
    let mut p = page(
        r#"<style>:invalid { color: rgb(255, 0, 0) } :valid { color: rgb(0, 128, 0) }</style>
        <form action="/s"><input id=e type=email name=e value="not an address">
        <input id=c name=c pattern="[A-Z]{3}" value="ABC" title="Three capitals">
        <input id=n type=number name=n max=5 value=3><input type=submit id=go></form>"#,
    );
    let red = bb_css::Rgba { r: 255, g: 0, b: 0, a: 255 };
    let green = bb_css::Rgba { r: 0, g: 128, b: 0, a: 255 };
    assert_eq!(p.styles.get(by_id(&p, "e")).unwrap().color, red);
    assert_eq!(p.styles.get(by_id(&p, "c")).unwrap().color, green);
    let out = click(&mut p, "go");
    assert!(out.submit.is_none());
    assert_eq!(p.focus.node, Some(by_id(&p, "e")));
    let (_, msg) = p.validation_report.take().expect("a validation message");
    assert_eq!(msg, "Please enter an email address.");
    // Fix the address; the pattern now fails.
    p.doc.set_control_value(by_id(&p, "e"), "a@b.example");
    p.doc.set_control_value(by_id(&p, "c"), "abc");
    assert!(click(&mut p, "go").submit.is_none());
    assert_eq!(p.validation_report.take().unwrap().1, "Please match the requested format: Three capitals.");
    p.doc.set_control_value(by_id(&p, "c"), "XYZ");
    p.doc.set_control_value(by_id(&p, "n"), "9");
    assert!(click(&mut p, "go").submit.is_none());
    assert_eq!(p.validation_report.take().unwrap().1, "Please select a value that is no more than 5.");
    p.doc.set_control_value(by_id(&p, "n"), "4");
    assert_eq!(submitted_url(&click(&mut p, "go")), "http://example.test/s?e=a%40b.example&c=XYZ&n=4");
}

#[test]
fn implicit_submission_rules() {
    // One text field and no button: Enter submits.
    let mut p = page(r#"<form action="/one"><input id=q name=q></form>"#);
    click(&mut p, "q");
    p.insert_text("x");
    assert_eq!(submitted_url(&key(&mut p, PageKey::Enter)), "http://example.test/one?q=x");
    // Two text fields and no button: Enter does nothing.
    let mut p = page(r#"<form action="/two"><input id=q name=q><input name=r></form>"#);
    click(&mut p, "q");
    assert!(key(&mut p, PageKey::Enter).submit.is_none());
    // A disabled default button blocks implicit submission.
    let mut p = page(r#"<form><input id=q><input type=submit disabled></form>"#);
    click(&mut p, "q");
    assert!(key(&mut p, PageKey::Enter).submit.is_none());
}

#[test]
fn reset_restores_defaults() {
    let mut p = page(
        r#"<form><input id=q value=start><input type=checkbox id=c checked>
        <select id=s><option>a<option selected>b</select><input type=reset id=r></form>"#,
    );
    click(&mut p, "q");
    p.insert_text("more");
    click(&mut p, "c");
    let s = by_id(&p, "s");
    let a = p.doc.list_of_options(s)[0];
    p.choose_option(&[], s, 0);
    assert!(p.doc.option_selectedness(a));
    click(&mut p, "r");
    assert_eq!(p.doc.control_value(by_id(&p, "q")), "start");
    assert!(p.doc.checkedness(by_id(&p, "c")));
    assert!(!p.doc.option_selectedness(a));
}

#[test]
fn parser_form_owner_inside_tables() {
    // The classic old-web layout: the form is closed by the table, the input is in a cell.
    let mut p = page(
        r#"<table><form action="/find"><tr><td><input id=q name=q value=z><td><input type=submit id=go></table>"#,
    );
    let form = p.doc.elements_by_tag("form")[0];
    let q = by_id(&p, "q");
    assert!(!p.doc.ancestors(q).any(|a| a == form), "the input isn't inside the form");
    assert_eq!(p.doc.form_owner(q), Some(form));
    assert_eq!(submitted_url(&click(&mut p, "go")), "http://example.test/find?q=z");
}

#[test]
fn form_attribute_and_targets() {
    let mut p = page(
        r#"<form id=f action="/t" target=_blank></form><input name=a value=1 form=f><button id=b form=f>Go</button>"#,
    );
    let out = click(&mut p, "b");
    let (s, _) = out.submit.unwrap();
    assert_eq!(s.request.url.href(), "http://example.test/t?a=1");
    assert_eq!(s.target, "_blank");
    // A GET keeps the action's fragment and replaces its query.
    let mut p =
        page(r#"<form action="/p?old=1#frag"><input name=n value="é"><input type=submit id=go></form>"#);
    assert_eq!(submitted_url(&click(&mut p, "go")), "http://example.test/p?n=%C3%A9#frag");
    // Unsupported schemes (mailto:, javascript:) and method=dialog don't navigate.
    for action in [r#"action="mailto:a@b.c""#, r#"action="javascript:void(0)""#, r#"method=dialog"#] {
        let mut p = page(&format!(r#"<form {action}><input type=submit id=go></form>"#));
        assert!(click(&mut p, "go").submit.is_none(), "{action}");
    }
}

#[test]
fn image_buttons_and_charset() {
    let mut p = page(
        r#"<form action="/i"><input type=hidden name=_charset_><input type=image id=img name=pos width=40 height=20 alt=go></form>"#,
    );
    let (_, [x, y, _, _]) = p.element_rects_by_id().into_iter().find(|(i, _)| i == "img").unwrap();
    let out = p.click(x + 7.0, y + 3.0);
    assert_eq!(submitted_url(&out), "http://example.test/i?_charset_=UTF-8&pos.x=7&pos.y=3");
}

#[test]
fn legacy_encodings() {
    // The document's encoding is used; characters it can't represent become &#N; references.
    let enc = encoding_rs::WINDOWS_1252;
    let mut p = page(r#"<form id=f><input name=a value="café €5 日"></form>"#);
    p.encoding = enc;
    let f = by_id(&p, "f");
    let entries = entry_list(&p.doc, f, None, None, enc);
    assert_eq!(urlencode(&entries, enc), "a=caf%E9+%805+%26%2326085%3B");
    // accept-charset wins over the document's encoding.
    p.doc.set_attr(f, "accept-charset", "bogus utf-8");
    assert_eq!(bb_engine::forms::form_encoding(&p.doc, f, enc), encoding_rs::UTF_8);
}

#[test]
fn multipart_and_text_plain() {
    let entries = vec![
        ("na\"me\n".to_string(), EntryValue::Text("v1\nv2".into())),
        ("file".to_string(), EntryValue::empty_file("")),
    ];
    let body = String::from_utf8(multipart(&entries, encoding_rs::UTF_8, "XyZ")).unwrap();
    assert_eq!(
        body,
        "--XyZ\r\nContent-Disposition: form-data; name=\"na%22me%0D%0A\"\r\n\r\nv1\r\nv2\r\n\
         --XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"\"\r\nContent-Type: application/octet-stream\r\n\r\n\r\n\
         --XyZ--\r\n"
    );
    let plain = String::from_utf8(text_plain(&entries[..1], encoding_rs::UTF_8)).unwrap();
    assert_eq!(plain, "na\"me\r\n=v1\r\nv2\r\n");
}

#[test]
fn multipart_post_from_a_page() {
    let mut p = page(
        r#"<form method=POST enctype="multipart/form-data" action="/up"><input name=a value=b><input type=file name=f><button id=go>Send</button></form>"#,
    );
    let (s, _) = click(&mut p, "go").submit.unwrap();
    let post = s.request.post.unwrap();
    let boundary = post.content_type.strip_prefix("multipart/form-data; boundary=").unwrap().to_string();
    let body = String::from_utf8(post.body).unwrap();
    assert!(
        body.starts_with(&format!("--{boundary}\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nb\r\n"))
    );
    assert!(body.ends_with(&format!("--{boundary}--\r\n")));
}

#[test]
fn maxlength_readonly_and_password() {
    let mut p =
        page(r#"<input id=m maxlength=3><input id=r readonly value=fixed><input type=password id=pw>"#);
    click(&mut p, "m");
    p.insert_text("abcdef");
    assert_eq!(p.doc.control_value(by_id(&p, "m")), "abc");
    click(&mut p, "r");
    p.insert_text("x");
    key(&mut p, PageKey::Backspace);
    assert_eq!(p.doc.control_value(by_id(&p, "r")), "fixed");
    // Copy works in a readonly field.
    p.key(PageKey::SelectAll, KeyMods::default());
    assert_eq!(key(&mut p, PageKey::Copy).copied.as_deref(), Some("fixed"));
    click(&mut p, "pw");
    p.insert_text("secret");
    assert_eq!(p.doc.control_value(by_id(&p, "pw")), "secret");
}

#[test]
fn clicks_place_the_caret() {
    let mut p = page(r#"<input id=q value="abcdefghij" style="font: 16px monospace">"#);
    let (_, [x, y, _, h]) = p.element_rects_by_id().into_iter().find(|(i, _)| i == "q").unwrap();
    p.click(x + 3.0, y + h / 2.0);
    p.insert_text("<");
    assert!(p.doc.control_value(by_id(&p, "q")).starts_with('<'), "{}", p.doc.control_value(by_id(&p, "q")));
    // A click far right puts the caret at the end.
    p.click(x + 150.0, y + h / 2.0);
    p.insert_text(">");
    assert!(p.doc.control_value(by_id(&p, "q")).ends_with('>'));
}

#[test]
fn forms_in_frames_submit_into_the_frame() {
    let html = r#"<iframe id=fr srcdoc="<form action='/inner'><input id=q name=q></form>" width=300 height=100></iframe>"#;
    let mut p = page(html);
    let r = p.frames[0].rect;
    // The input sits at the frame's top left; click into it.
    p.click(r.x + 20.0, r.y + 15.0);
    assert_eq!(p.focused().map(|f| f.0), Some(FocusKind::Text));
    p.insert_text("in frame");
    let out = key(&mut p, PageKey::Enter);
    let (s, path) = out.submit.expect("submission from the frame");
    assert_eq!(path, vec![0]);
    assert_eq!(s.request.url.href(), "http://example.test/inner?q=in+frame");
    // Following it with the frame path navigates the frame, not the tab.
    let client = new_client(Vec::new());
    let hit = bb_engine::LinkHit { url: s.request.url, target: s.target, frame_path: path, post: None };
    assert!(matches!(p.follow_link(&client, hit), LinkDisposition::Frame));
}

#[test]
fn caret_shows_only_with_window_focus() {
    let mut p = page(r#"<input id=q value=abc>"#);
    click(&mut p, "q");
    let items_focused = p.display_list.items.len();
    assert!(p.set_window_focus(false));
    assert!(p.display_list.items.len() < items_focused, "caret and focus ring hidden");
    assert!(!p.set_window_focus(false));
}

#[test]
fn sanitized_values_keep_showing_what_was_typed() {
    // The DOM value is sanitized ("" for an invalid number, trimmed e-mail), but the field
    // shows and edits the typed text, and the caret never lands inside a character.
    let mut p = page(r#"<input type=number id=n><input type=email id=e>"#);
    click(&mut p, "n");
    p.insert_text("1é2");
    assert_eq!(p.doc.control_value(by_id(&p, "n")), "");
    key(&mut p, PageKey::Left);
    key(&mut p, PageKey::Backspace);
    p.insert_text("5");
    assert_eq!(p.doc.control_value(by_id(&p, "n")), "152");
    click(&mut p, "e");
    p.insert_text(" é@x.y ");
    key(&mut p, PageKey::Home);
    key(&mut p, PageKey::Right);
    p.key(PageKey::Right, KeyMods { shift: true, ..Default::default() });
    assert_eq!(p.doc.control_value(by_id(&p, "e")), "é@x.y");
    let (x, y) = centre(&p, "e");
    p.click(x, y);
    assert!(p.focused().is_some());
}

#[test]
fn textarea_editing_near_multibyte_line_ends() {
    // Found by forms_robustness: click into a textarea ending in "é\n", Shift+Right,
    // Backspace, Enter must not slice inside a character.
    let mut p = page("<textarea name=ta>a\nbé\n</textarea>");
    for (x, y) in [(39.645912, 34.739983), (10.0, 12.0), (30.0, 25.0), (60.0, 40.0)] {
        p.click(x, y);
        p.key(PageKey::Right, KeyMods { shift: true, ..Default::default() });
        p.key(PageKey::Backspace, KeyMods::default());
        p.key(PageKey::Enter, KeyMods::default());
        p.key(PageKey::Up, KeyMods::default());
        p.key(PageKey::End, KeyMods::default());
        p.key(PageKey::Down, KeyMods { shift: true, ..Default::default() });
        p.insert_text("é");
    }
}

#[test]
fn a_named_button_submits_its_value() {
    let mut p = page(
        r#"<form action="/s"><input name=q value=x><button id=go name=act value=save>Save</button></form>"#,
    );
    let o = click(&mut p, "go");
    assert!(submitted_url(&o).ends_with("/s?q=x&act=save"), "{}", submitted_url(&o));
}

#[test]
fn form_events_reach_scripts() {
    let mut p = page(
        r#"<form id=f action="/go"><input id=c type=checkbox><input id=t name=t>
        <select id=s><option>a<option>b</select><button id=r type=reset>R</button>
        <button id=b>Go</button></form><p id=log></p>
        <script>var log = [], f = document.getElementById('f'), cancel = true;
        function rec(e) { log.push(e.type + ':' + e.target.id + (e.submitter ? '/' + e.submitter.id : '')); }
        ['input', 'change', 'reset', 'submit'].forEach(function (t) { f.addEventListener(t, rec); });
        f.addEventListener('submit', function (e) { if (cancel) e.preventDefault(); cancel = false; });
        f.addEventListener('reset', function (e) { e.preventDefault(); });</script>"#,
    );
    click(&mut p, "c");
    assert!(p.doc.checkedness(by_id(&p, "c")));
    click(&mut p, "t");
    p.insert_text("hi");
    click(&mut p, "c");
    let s = by_id(&p, "s");
    assert!(p.choose_option(&[], s, 1));
    p.doc.set_control_value(by_id(&p, "t"), "kept");
    click(&mut p, "r");
    assert_eq!(p.doc.control_value(by_id(&p, "t")), "kept", "a canceled reset keeps the values");
    // The first submission is canceled, the second goes ahead.
    assert!(click(&mut p, "b").submit.is_none());
    let out = click(&mut p, "b");
    assert!(submitted_url(&out).starts_with("http://example.test/go?t=kept"));
    let log = |p: &mut Page| {
        p.with_window(|w| w.run_script("document.getElementById('log').textContent = log.join(' ')"));
        p.doc.text_content(by_id(p, "log"))
    };
    assert_eq!(
        log(&mut p),
        "input:c change:c change:t input:c change:c input:s change:s reset:f submit:f/b submit:f/b"
    );
    // form.submit() asks the page for a submission, without a submit event.
    p.with_window(|w| w.run_script("f.submit()"));
    let s = p.take_script_submission().expect("form.submit() submits");
    assert!(s.request.url.href().starts_with("http://example.test/go?t=kept"));
}

#[test]
fn script_clicks_have_activation_behavior() {
    let mut p = page(
        r#"<input id=c type=checkbox><label id=l for=r>R</label><input id=r type=radio name=g>
        <input id=d type=checkbox disabled><a id=a href="/next">next</a><p id=log></p>
        <script>var log = [];
        document.addEventListener('change', function (e) { log.push('change:' + e.target.id); });
        var c = document.getElementById('c');
        c.click();
        c.addEventListener('click', function (e) { e.preventDefault(); });
        c.click();
        document.getElementById('l').click();
        document.getElementById('d').click();
        log.push(c.checked, document.getElementById('r').checked, document.getElementById('d').checked);
        document.getElementById('log').textContent = log.join(' ');
        document.getElementById('a').click();</script>"#,
    );
    // The canceled second click puts the checkbox back.
    assert_eq!(p.doc.text_content(by_id(&p, "log")), "change:c change:r true true false");
    assert!(p.doc.checkedness(by_id(&p, "c")));
    assert_eq!(p.take_script_navigation().map(|u| u.href()).as_deref(), Some("http://example.test/next"));
}

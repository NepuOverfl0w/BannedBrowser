//! Randomized interaction with random form markup: clicks anywhere, keys, typed text, option
//! picks and focus changes must never panic, and values stay valid UTF-8 slices.

use bb_engine::{new_client, KeyMods, Page, PageConfig, PageKey};
use bb_url::Url;
use proptest::prelude::*;

const PIECES: &[&str] = &[
    "<form action=/a>",
    "<form method=post enctype=multipart/form-data action=/b>",
    "</form>",
    "<input name=t value='é日x'>",
    "<input type=number name=n>",
    "<input type=email name=e value=' a@b '>",
    "<input type=password name=p>",
    "<input type=checkbox name=c>",
    "<input type=radio name=r><input type=radio name=r checked>",
    "<input type=submit name=s value=Go>",
    "<input type=image name=i width=10 height=10>",
    "<input type=reset>",
    "<input maxlength=2 name=m>",
    "<input readonly name=ro value=ro>",
    "<input disabled name=d>",
    "<textarea name=ta>a\nbé\n</textarea>",
    "<select name=sel><option>a<optgroup label=g><option disabled>b<option>c</optgroup></select>",
    "<select multiple name=ms><option>x<option selected>y</select>",
    "<button>B</button><button type=reset>R</button>",
    "<label>L <input name=l></label><label for=nope>x</label>",
    "<fieldset disabled><legend><input name=in_legend></legend><input name=out></fieldset>",
    "<table><form><tr><td><input name=tf><td><input type=submit></table>",
    "<details><summary>s</summary>x</details>",
    "<a href=/x>link</a>",
    "<iframe srcdoc='<form><input name=f></form>' width=100 height=40></iframe>",
    "<p>text</p>",
];

#[derive(Debug, Clone)]
enum Act {
    Click(f32, f32),
    Key(u8, bool, bool),
    Text(String),
    Choose(usize),
    WindowFocus(bool),
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        (0f32..820.0, 0f32..400.0).prop_map(|(x, y)| Act::Click(x, y)),
        (0u8..18, any::<bool>(), any::<bool>()).prop_map(|(k, s, w)| Act::Key(k, s, w)),
        "[a-zé日 \\n]{0,6}".prop_map(Act::Text),
        (0usize..5).prop_map(Act::Choose),
        any::<bool>().prop_map(Act::WindowFocus),
    ]
}

const KEYS: [PageKey; 18] = [
    PageKey::Tab,
    PageKey::Enter,
    PageKey::Space,
    PageKey::Escape,
    PageKey::Backspace,
    PageKey::Delete,
    PageKey::Left,
    PageKey::Right,
    PageKey::Up,
    PageKey::Down,
    PageKey::Home,
    PageKey::End,
    PageKey::Open,
    PageKey::SelectAll,
    PageKey::Undo,
    PageKey::Redo,
    PageKey::Copy,
    PageKey::Cut,
];

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn interactions_never_panic(pieces in prop::collection::vec(0..PIECES.len(), 1..10), acts in prop::collection::vec(act(), 0..40)) {
        let html: String = pieces.iter().map(|&i| PIECES[i]).collect();
        let client = new_client(Vec::new());
        let url = Url::parse("http://example.test/").unwrap();
        let mut p = Page::from_html(&client, &html, &url, PageConfig { viewport: (800.0, 400.0), ..Default::default() }, 200, false);
        let mut popup = None;
        for a in acts {
            match a {
                Act::Click(x, y) => {
                    let out = p.click(x, y);
                    if out.popup.is_some() {
                        popup = out.popup;
                    }
                }
                Act::Key(k, shift, word) => {
                    let out = p.key(KEYS[k as usize], KeyMods { shift, word, primary: word && shift });
                    if out.popup.is_some() {
                        popup = out.popup;
                    }
                }
                Act::Text(t) => {
                    p.insert_text(&t);
                }
                Act::Choose(i) => {
                    if let Some(pp) = &popup {
                        p.choose_option(&pp.frame_path, pp.select, i);
                    }
                }
                Act::WindowFocus(on) => {
                    p.set_window_focus(on);
                }
            }
            let _ = p.focused();
        }
        // And it still renders.
        let _ = p.render(0.0, 1.0, 200, 100);
    }
}

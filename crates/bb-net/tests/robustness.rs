//! Randomized robustness (AGENTS.md rule 4): cookie and Public Suffix List code never panics on
//! hostile input, and the jar's invariants hold.

use bb_net::cookies::{parse_cookie_date, parse_set_cookie, CookieContext, CookieJar};
use bb_url::Url;
use proptest::prelude::*;

fn header() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        Just("a=b".to_string()),
        Just("; ".to_string()),
        Just("Domain=".to_string()),
        Just("Path=/".to_string()),
        Just("Max-Age=".to_string()),
        Just("Expires=Wed, 21 Oct 2015 07:28:00 GMT".to_string()),
        Just("Secure".to_string()),
        Just("SameSite=None".to_string()),
        Just("__Host-".to_string()),
        Just("=".to_string()),
        "[ -~]{0,12}",
        ".{0,6}",
    ];
    prop::collection::vec(piece, 0..12).prop_map(|v| v.concat())
}

proptest! {
    #[test]
    fn set_cookie_parsing_never_panics(h in header(), now in any::<i64>()) {
        let _ = parse_set_cookie(&h, now);
    }

    #[test]
    fn cookie_dates_never_panic(s in ".{0,40}") {
        let _ = parse_cookie_date(&s);
    }

    #[test]
    fn jar_never_panics_and_never_sends_ascii_control_characters(
        headers in prop::collection::vec(header(), 0..20),
        host in prop_oneof![Just("www.example.co.uk"), Just("127.0.0.1"), Just("x.github.io"), Just("[::1]")],
        path in "(/[a-z]{0,3}){0,3}",
    ) {
        let jar = CookieJar::new();
        let url = Url::parse(&format!("https://{host}{path}")).unwrap();
        for h in &headers {
            jar.store(&url, h, &CookieContext::default());
        }
        if let Some(c) = jar.cookie_header(&url, &CookieContext::default()) {
            // No ASCII control characters (CR, LF, NUL… would allow header injection).
            prop_assert!(!c.chars().any(|ch| (ch < ' ' && ch != '\t') || ch == '\x7f'));
        }
        // Never more than one cookie with the same (name, domain, path).
        let all = jar.all();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                prop_assert!(!(a.name == b.name && a.domain == b.domain && a.path == b.path && a.host_only == b.host_only));
            }
        }
    }

    #[test]
    fn public_suffix_list_never_panics(labels in prop::collection::vec("[a-z0-9-]{0,8}|\\*|xn--[a-z0-9]{1,6}", 0..6)) {
        let d = labels.join(".");
        let _ = bb_net::psl::public_suffix(&d);
        let _ = bb_net::psl::registrable_domain(&d);
    }
}

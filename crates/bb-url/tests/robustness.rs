use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]
    #[test]
    fn never_panics(s in "\\PC{0,64}", b in "[a-z:/\\\\?#@\\[\\]%.0-9]{0,40}") {
        let base = bb_url::Url::parse("http://example.com/a/b?c#d").unwrap();
        let _ = bb_url::Url::parse(&s);
        let _ = base.join(&s);
        let _ = base.join(&b);
        if let Ok(u) = bb_url::Url::parse(&b) {
            // Serialization must be idempotent.
            let again = bb_url::Url::parse(&u.href()).unwrap();
            prop_assert_eq!(again.href(), u.href());
        }
    }
}

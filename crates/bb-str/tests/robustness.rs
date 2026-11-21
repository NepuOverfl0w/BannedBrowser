//! Randomized checks: WTF-8 is a lossless encoding of any UTF-16 code unit sequence, and
//! concatenation behaves like concatenating the UTF-16.

use bb_str::Wtf8Buf;
use proptest::prelude::*;

/// UTF-16 units biased towards surrogates, so pairs and lone halves both show up.
fn units() -> impl Strategy<Value = Vec<u16>> {
    prop::collection::vec(prop_oneof![0xD800u16..0xE000, 0u16..0x80, any::<u16>()], 0..40)
}

proptest! {
    #[test]
    fn utf16_round_trips(u in units()) {
        prop_assert_eq!(Wtf8Buf::from_utf16(&u).to_utf16(), u);
    }

    #[test]
    fn concatenation_matches_utf16(a in units(), b in units()) {
        let mut s = Wtf8Buf::from_utf16(&a);
        s.push_wtf8(&Wtf8Buf::from_utf16(&b));
        let joined: Vec<u16> = a.iter().chain(b.iter()).copied().collect();
        prop_assert_eq!(s.clone(), Wtf8Buf::from_utf16(&joined));
        prop_assert_eq!(s.to_utf16(), joined);
    }

    #[test]
    fn lossy_matches_std(u in units()) {
        prop_assert_eq!(Wtf8Buf::from_utf16(&u).into_string_lossy(), String::from_utf16_lossy(&u));
    }

    #[test]
    fn code_points_rebuild(u in units()) {
        let s = Wtf8Buf::from_utf16(&u);
        prop_assert_eq!(Wtf8Buf::from_code_points(s.code_points()), s);
    }
}

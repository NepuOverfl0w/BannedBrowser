//! Unicode's normalization conformance test (UAX #15, NormalizationTest.txt from
//! scripts/fetch-spec-tests.sh): the invariants of every listed line, and that every other
//! code point is unchanged by all four forms.

use bb_js::vm::normalize::{normalize, Form};
use std::path::PathBuf;

fn utf16(cps: &[u32]) -> Vec<u16> {
    let mut out = Vec::new();
    for &c in cps {
        match char::from_u32(c) {
            Some(ch) => out.extend_from_slice(ch.encode_utf16(&mut [0; 2])),
            None => out.push(c as u16),
        }
    }
    out
}

#[test]
fn normalization_conformance() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/ucd/NormalizationTest.txt");
    let text = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e} (run scripts/fetch-spec-tests.sh)", p.display()));
    let mut listed = std::collections::HashSet::new();
    let mut part1 = false;
    let (mut lines, mut failures) = (0, Vec::new());
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('@') {
            part1 = line == "@Part1";
            continue;
        }
        let cols: Vec<Vec<u16>> = line
            .split(';')
            .take(5)
            .map(|c| {
                let cps: Vec<u32> =
                    c.split_whitespace().map(|h| u32::from_str_radix(h, 16).expect("hex")).collect();
                if part1 && cps.len() == 1 {
                    listed.insert(cps[0]);
                }
                utf16(&cps)
            })
            .collect();
        lines += 1;
        let [c1, c2, c3, c4, c5] = [&cols[0], &cols[1], &cols[2], &cols[3], &cols[4]];
        let checks = [
            (Form::Nfc, [c1, c2, c3], c2),
            (Form::Nfc, [c4, c5, c4], c4),
            (Form::Nfd, [c1, c2, c3], c3),
            (Form::Nfd, [c4, c5, c5], c5),
            (Form::Nfkc, [c1, c2, c3], c4),
            (Form::Nfkc, [c4, c5, c5], c4),
            (Form::Nfkd, [c1, c2, c3], c5),
            (Form::Nfkd, [c4, c5, c5], c5),
        ];
        for (form, inputs, want) in checks {
            for input in inputs {
                if &normalize(input, form) != want && failures.len() < 20 {
                    failures.push(format!("{form:?}({input:x?}) != {want:x?}"));
                }
            }
        }
    }
    // Part 1's complement: every other code point is its own normalization in each form.
    for c in (0..=0x10FFFFu32).filter(|c| !listed.contains(c) && !(0xD800..0xE000).contains(c)) {
        let s = utf16(&[c]);
        for form in [Form::Nfc, Form::Nfd, Form::Nfkc, Form::Nfkd] {
            if normalize(&s, form) != s && failures.len() < 20 {
                failures.push(format!("{form:?}(U+{c:04X}) changed"));
            }
        }
    }
    assert!(lines > 19000, "only {lines} test lines");
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

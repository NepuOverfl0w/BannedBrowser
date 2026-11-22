//! Public suffixes and registrable domains, from the Public Suffix List
//! (<https://publicsuffix.org/list/>; `resources/psl/public_suffix_list.dat`, MPL-2.0, shipped
//! unmodified). The matching follows the list's published algorithm: the prevailing rule is an
//! exception rule if one matches, else the matching rule with the most labels, else the
//! implicit `*` rule; an exception rule's public suffix drops its leftmost label.
//!
//! Domains are compared in ASCII (punycode) form, lowercase, without a trailing dot.

use std::collections::HashSet;
use std::sync::OnceLock;

const LIST: &str = include_str!("../../../resources/psl/public_suffix_list.dat");

struct Rules {
    /// Normal rules ("com", "co.uk"), and wildcard rules stored without their "*." ("ck"
    /// for "*.ck").
    normal: HashSet<String>,
    wildcard: HashSet<String>,
    /// Exception rules without their "!" ("www.ck").
    exception: HashSet<String>,
}

fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        let mut r = Rules { normal: HashSet::new(), wildcard: HashSet::new(), exception: HashSet::new() };
        for line in LIST.lines() {
            // A rule is the first whitespace-separated token of a non-comment line.
            let Some(rule) = line.split_whitespace().next() else { continue };
            if rule.starts_with("//") {
                continue;
            }
            let (set, body) = if let Some(b) = rule.strip_prefix('!') {
                (&mut r.exception, b)
            } else if let Some(b) = rule.strip_prefix("*.") {
                (&mut r.wildcard, b)
            } else {
                (&mut r.normal, rule)
            };
            // Rules may be written in Unicode; match on the ASCII form.
            if let Some(ascii) = bb_url::idna::domain_to_ascii(body) {
                set.insert(ascii);
            }
        }
        r
    })
}

fn canonical(domain: &str) -> String {
    domain.trim_end_matches('.').to_ascii_lowercase()
}

/// The public suffix of `domain` (e.g. "co.uk" for "www.example.co.uk"). An unknown TLD is its
/// own public suffix (the implicit `*` rule).
pub fn public_suffix(domain: &str) -> String {
    let d = canonical(domain);
    let labels: Vec<&str> = d.split('.').collect();
    let r = rules();
    // Candidates from the longest (whole domain) to the shortest (TLD).
    let mut best: Option<usize> = None; // number of labels of the prevailing rule's suffix
    for i in 0..labels.len() {
        let cand = labels[i..].join(".");
        let n = labels.len() - i;
        if r.exception.contains(&cand) {
            // An exception rule prevails; its suffix drops the leftmost label.
            return labels[i + 1..].join(".");
        }
        if best.is_none() {
            if r.normal.contains(&cand) {
                best = Some(n);
            } else if i + 1 < labels.len() && r.wildcard.contains(&labels[i + 1..].join(".")) {
                // "*.ck" matches "<anything>.ck".
                best = Some(n);
            }
        }
    }
    let n = best.unwrap_or(1).min(labels.len());
    labels[labels.len() - n..].join(".")
}

/// Whether `domain` is itself a public suffix.
pub fn is_public_suffix(domain: &str) -> bool {
    canonical(domain) == public_suffix(domain)
}

/// The registrable domain ("eTLD+1"), e.g. "example.co.uk"; `None` if `domain` is a public
/// suffix itself.
pub fn registrable_domain(domain: &str) -> Option<String> {
    let d = canonical(domain);
    let suffix = public_suffix(&d);
    if d == suffix {
        return None;
    }
    let prefix = d.strip_suffix(&suffix)?.strip_suffix('.')?;
    let label = prefix.rsplit('.').next()?;
    Some(format!("{label}.{suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixes() {
        assert_eq!(public_suffix("www.example.com"), "com");
        assert_eq!(public_suffix("a.b.example.co.uk"), "co.uk");
        assert_eq!(public_suffix("user.github.io"), "github.io");
        assert_eq!(public_suffix("example.unknowntld"), "unknowntld");
        assert!(is_public_suffix("co.uk"));
        assert!(is_public_suffix("COM."));
        assert!(!is_public_suffix("example.com"));
    }

    #[test]
    fn registrable_domains() {
        assert_eq!(registrable_domain("www.example.co.uk").as_deref(), Some("example.co.uk"));
        assert_eq!(registrable_domain("example.com").as_deref(), Some("example.com"));
        assert_eq!(registrable_domain("a.b.c.example.com").as_deref(), Some("example.com"));
        assert_eq!(registrable_domain("com"), None);
        assert_eq!(registrable_domain("github.io"), None);
        assert_eq!(registrable_domain("someone.github.io").as_deref(), Some("someone.github.io"));
    }

    // The list's own test vectors (publicsuffix.org "test_psl.txt"), a representative subset.
    #[test]
    fn official_vectors() {
        let cases = [
            ("example.com", Some("example.com")),
            ("b.example.com", Some("example.com")),
            ("a.b.example.com", Some("example.com")),
            ("uk.com", None),
            ("example.uk.com", Some("example.uk.com")),
            ("b.example.uk.com", Some("example.uk.com")),
            ("test.ac", Some("test.ac")),
            ("mm", None),
            ("c.mm", None),
            ("b.c.mm", Some("b.c.mm")),
            ("a.b.c.mm", Some("b.c.mm")),
            ("jp", None),
            ("test.jp", Some("test.jp")),
            ("www.test.jp", Some("test.jp")),
            ("ac.jp", None),
            ("test.ac.jp", Some("test.ac.jp")),
            ("kyoto.jp", None),
            ("test.kyoto.jp", Some("test.kyoto.jp")),
            ("ide.kyoto.jp", None),
            ("b.ide.kyoto.jp", Some("b.ide.kyoto.jp")),
            ("c.kobe.jp", None),
            ("b.c.kobe.jp", Some("b.c.kobe.jp")),
            ("city.kobe.jp", Some("city.kobe.jp")),
            ("www.city.kobe.jp", Some("city.kobe.jp")),
            ("ck", None),
            ("test.ck", None),
            ("b.test.ck", Some("b.test.ck")),
            ("www.ck", Some("www.ck")),
            ("www.www.ck", Some("www.ck")),
            ("us", None),
            ("test.us", Some("test.us")),
            ("ak.us", None),
            ("test.ak.us", Some("test.ak.us")),
            ("k12.ak.us", None),
            ("test.k12.ak.us", Some("test.k12.ak.us")),
            // IDN rules, in punycode: 食狮.com.cn and 公司.cn.
            ("xn--85x722f.com.cn", Some("xn--85x722f.com.cn")),
            ("xn--55qx5d.cn", None),
            ("www.xn--85x722f.xn--55qx5d.cn", Some("xn--85x722f.xn--55qx5d.cn")),
        ];
        for (domain, want) in cases {
            assert_eq!(registrable_domain(domain).as_deref(), want, "{domain}");
        }
    }
}

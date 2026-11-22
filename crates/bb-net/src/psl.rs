//! Public suffixes and registrable domains, from the Public Suffix List
//! (<https://publicsuffix.org/list/>, MPL-2.0, used unmodified). The matching follows the list's published algorithm
//! (<https://github.com/publicsuffix/list/wiki/Format#formal-algorithm>): the prevailing rule is
//! an exception rule if one matches, else the matching rule with the most labels, else the
//! implicit `*` rule; an exception rule's public suffix drops its leftmost label.
//!
//! Domains are compared in ASCII (punycode) form, lowercase. A trailing dot is kept in the
//! answer ("example.com." has the public suffix "com."); a domain with an empty label (".com",
//! "a..com") has no public suffix at all.
//!
//! The list changes often, and its maintainers ask browsers not to rely on a static copy. It is
//! not kept in the repository: `build.rs` embeds the current list at build time, and
//! [`refresh`] downloads it again at the browser's first start and then at most once a day into
//! a cache directory, [`install`]ing it when it is newer.
//!
//! Without any list (an offline build before the first download), [`available`] is false:
//! nothing is a known public suffix and every host is its own site; cookies then ignore
//! `Domain` attributes that name a parent domain (they stay host-only), which is safe.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The list embedded by `build.rs`; empty if the build couldn't get one.
const BUNDLED: &str = include_str!(concat!(env!("OUT_DIR"), "/public_suffix_list.dat"));

/// The only supported download location (publicsuffix.org asks for exactly this URL).
pub const LIST_URL: &str = "https://publicsuffix.org/list/public_suffix_list.dat";

/// How often [`refresh`] may download the list (publicsuffix.org: "at most once per day").
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

const CACHE_FILE: &str = "public_suffix_list.dat";
const STAMP_FILE: &str = "last-check";

struct Rules {
    /// `VERSION` from the list's header ("2026-11-21_04-33-45_UTC"; sorts by time).
    version: String,
    normal: HashSet<String>,
    /// Wildcard rules stored without their "*." ("ck" for "*.ck").
    wildcard: HashSet<String>,
    /// Exception rules without their "!" ("www.ck").
    exception: HashSet<String>,
}

/// Why a downloaded or cached list was not installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// It doesn't look like the Public Suffix List (markers, version or rules missing).
    Invalid(&'static str),
    /// It is not newer than the list in use.
    NotNewer,
}

fn parse(text: &str) -> Result<Rules, InstallError> {
    for marker in [
        "// ===BEGIN ICANN DOMAINS===",
        "// ===END ICANN DOMAINS===",
        "// ===BEGIN PRIVATE DOMAINS===",
        "// ===END PRIVATE DOMAINS===",
    ] {
        if !text.contains(marker) {
            return Err(InstallError::Invalid("section markers missing"));
        }
    }
    let version = text
        .lines()
        .find_map(|l| l.strip_prefix("// VERSION:"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or(InstallError::Invalid("no VERSION line"))?;
    let mut r =
        Rules { version, normal: HashSet::new(), wildcard: HashSet::new(), exception: HashSet::new() };
    for line in text.lines() {
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
    // A sanity floor far below the real list's size (~10 000 rules), so a truncated or wrong
    // download is refused.
    if r.normal.len() < 1000 || !r.normal.contains("com") || !r.normal.contains("co.uk") {
        return Err(InstallError::Invalid("too few rules"));
    }
    Ok(r)
}

fn current() -> &'static RwLock<Option<Arc<Rules>>> {
    static RULES: OnceLock<RwLock<Option<Arc<Rules>>>> = OnceLock::new();
    RULES.get_or_init(|| RwLock::new(parse(BUNDLED).ok().map(Arc::new)))
}

fn rules() -> Option<Arc<Rules>> {
    match current().read() {
        Ok(g) => g.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Whether a list is in use (embedded at build time, cached or downloaded).
pub fn available() -> bool {
    rules().is_some()
}

/// The `VERSION` of the list in use (empty if there is none).
pub fn version() -> String {
    rules().map(|r| r.version.clone()).unwrap_or_default()
}

/// The list embedded at build time (empty if the build couldn't get one).
pub fn embedded_list() -> &'static str {
    BUNDLED
}

/// Use `text` as the list from now on if it is a valid list newer than the one in use.
/// Returns the installed version.
pub fn install(text: &str) -> Result<String, InstallError> {
    let new = parse(text)?;
    let mut guard = match current().write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.as_ref().is_some_and(|old| new.version <= old.version) {
        return Err(InstallError::NotNewer);
    }
    let version = new.version.clone();
    *guard = Some(Arc::new(new));
    Ok(version)
}

/// What [`refresh`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refresh {
    /// Checked less than a day ago; the cached copy (if newer) is in use.
    Fresh,
    /// Downloaded and installed this version.
    Updated(String),
    /// Downloaded, but it wasn't newer than the list in use.
    Unchanged,
    /// The download failed or wasn't a valid list; the next try is in a day.
    Failed(String),
}

/// Load the cached list from `cache_dir` and, if the last check was a day or more ago, download
/// the current one from [`LIST_URL`] (or `$BB_PSL_URL`). Blocking: run it on a worker thread.
/// Requests carry no cookies.
pub fn refresh(client: &crate::Client, cache_dir: &Path) -> Refresh {
    let url = std::env::var("BB_PSL_URL").unwrap_or_else(|_| LIST_URL.to_string());
    refresh_from(client, &url, cache_dir, SystemTime::now())
}

/// [`refresh`] from another URL and at a given time (for tests).
pub fn refresh_from(client: &crate::Client, url: &str, cache_dir: &Path, now: SystemTime) -> Refresh {
    refresh_with(cache_dir, now, || {
        let url = bb_url::Url::parse(url).map_err(|e| format!("bad URL: {e:?}"))?;
        let mut req = crate::Request::get(url);
        req.credentials = false;
        let resp = client.fetch(req).map_err(|e| e.to_string())?;
        if resp.status != 200 {
            return Err(format!("HTTP {}", resp.status));
        }
        String::from_utf8(resp.body).map_err(|_| "not UTF-8".to_string())
    })
}

/// [`refresh`] with the clock and the download passed in (for tests).
pub fn refresh_with(
    cache_dir: &Path,
    now: SystemTime,
    download: impl FnOnce() -> Result<String, String>,
) -> Refresh {
    let cache = cache_dir.join(CACHE_FILE);
    let stamp = cache_dir.join(STAMP_FILE);
    if let Ok(text) = std::fs::read_to_string(&cache) {
        let _ = install(&text);
    }
    let now_secs = now.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let last = std::fs::read_to_string(&stamp).ok().and_then(|s| s.trim().parse::<u64>().ok());
    // A stamp from the future (the clock moved back) doesn't count as recent.
    if let Some(last) = last {
        if last <= now_secs && now_secs - last < REFRESH_INTERVAL.as_secs() {
            return Refresh::Fresh;
        }
    }
    // Record the attempt first, so a failing download is also tried at most once a day.
    let _ = std::fs::create_dir_all(cache_dir);
    let _ = std::fs::write(&stamp, now_secs.to_string());
    let text = match download() {
        Ok(t) => t,
        Err(e) => return Refresh::Failed(e),
    };
    match install(&text) {
        Ok(v) => {
            // Write to a temporary file and rename, so a crash never leaves half a list.
            let tmp = cache_dir.join(format!("{CACHE_FILE}.tmp"));
            if std::fs::write(&tmp, &text).is_ok() {
                let _ = std::fs::rename(&tmp, &cache);
            }
            Refresh::Updated(v)
        }
        Err(InstallError::NotNewer) => Refresh::Unchanged,
        Err(InstallError::Invalid(why)) => Refresh::Failed(why.to_string()),
    }
}

/// Lowercase, split off a trailing dot, and reject empty labels.
fn canonical(domain: &str) -> Option<(String, bool)> {
    let lower = domain.to_ascii_lowercase();
    let (body, dot) = match lower.strip_suffix('.') {
        Some(b) => (b.to_string(), true),
        None => (lower, false),
    };
    if body.is_empty() || body.split('.').any(str::is_empty) {
        return None;
    }
    Some((body, dot))
}

/// The number of labels of `domain`'s public suffix (`domain` canonical, without trailing dot).
fn suffix_labels(r: &Rules, labels: &[&str]) -> usize {
    let mut best: Option<usize> = None;
    // Candidates from the longest (whole domain) to the shortest (TLD).
    for i in 0..labels.len() {
        let cand = labels[i..].join(".");
        let n = labels.len() - i;
        if r.exception.contains(&cand) {
            // An exception rule prevails; its suffix drops the leftmost label.
            return n - 1;
        }
        if best.is_none() {
            if r.normal.contains(&cand) {
                best = Some(n);
            } else if i + 1 < labels.len() && r.wildcard.contains(&labels[i + 1..].join(".")) {
                // "*.ck" matches "<anything>.ck" (but not "ck" itself).
                best = Some(n);
            }
        }
    }
    best.unwrap_or(1).min(labels.len())
}

/// The public suffix of `domain` (e.g. "co.uk" for "www.example.co.uk"); an unknown TLD is its
/// own public suffix (the implicit `*` rule). A trailing dot is kept. `None` for an empty
/// domain, one with an empty label, or when no list is [`available`].
pub fn public_suffix(domain: &str) -> Option<String> {
    let r = rules()?;
    let (d, dot) = canonical(domain)?;
    let labels: Vec<&str> = d.split('.').collect();
    let n = suffix_labels(&r, &labels);
    let mut s = labels[labels.len() - n..].join(".");
    if dot {
        s.push('.');
    }
    Some(s)
}

/// Whether `domain` is itself a public suffix (always false without a list).
pub fn is_public_suffix(domain: &str) -> bool {
    let Some(r) = rules() else { return false };
    let Some((d, _)) = canonical(domain) else { return false };
    let labels: Vec<&str> = d.split('.').collect();
    suffix_labels(&r, &labels) == labels.len()
}

/// The registrable domain ("eTLD+1"), e.g. "example.co.uk"; a trailing dot is kept. `None` if
/// `domain` is a public suffix itself, empty, has an empty label, or no list is [`available`]
/// (callers then treat the host as its own site).
pub fn registrable_domain(domain: &str) -> Option<String> {
    let r = rules()?;
    let (d, dot) = canonical(domain)?;
    let labels: Vec<&str> = d.split('.').collect();
    let n = suffix_labels(&r, &labels);
    if n >= labels.len() {
        return None;
    }
    let mut s = labels[labels.len() - n - 1..].join(".");
    if dot {
        s.push('.');
    }
    Some(s)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The embedded list; the tests need one (the build downloads it, or set `BB_PSL_FILE`).
    pub(crate) fn list() -> &'static str {
        assert!(
            valid_embedded(),
            "no Public Suffix List was embedded: build online, or set BB_PSL_FILE to a copy of {LIST_URL}"
        );
        BUNDLED
    }

    fn valid_embedded() -> bool {
        parse(BUNDLED).is_ok()
    }

    #[test]
    fn suffixes() {
        list();
        assert_eq!(public_suffix("www.example.com").as_deref(), Some("com"));
        assert_eq!(public_suffix("a.b.example.co.uk").as_deref(), Some("co.uk"));
        assert_eq!(public_suffix("user.github.io").as_deref(), Some("github.io"));
        assert_eq!(public_suffix("example.unknowntld").as_deref(), Some("unknowntld"));
        assert!(is_public_suffix("co.uk"));
        assert!(is_public_suffix("COM."));
        assert!(!is_public_suffix("example.com"));
    }

    #[test]
    fn registrable_domains() {
        list();
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
        list();
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

    // More of test_psl.txt: case, leading dots (empty labels) and the trailing-dot rule.
    #[test]
    fn case_empty_labels_and_trailing_dots() {
        list();
        assert_eq!(registrable_domain("COM"), None);
        assert_eq!(registrable_domain("example.COM").as_deref(), Some("example.com"));
        assert_eq!(registrable_domain("WwW.Example.COM").as_deref(), Some("example.com"));
        for bad in [
            "",
            ".",
            ".com",
            ".example",
            ".example.com",
            ".example.example",
            "a..com",
            "example..",
            "..",
            "com..",
        ] {
            assert_eq!(registrable_domain(bad), None, "{bad:?}");
            assert_eq!(public_suffix(bad), None, "{bad:?}");
            assert!(!is_public_suffix(bad), "{bad:?}");
        }
        assert_eq!(public_suffix("example.com.").as_deref(), Some("com."));
        assert_eq!(registrable_domain("www.example.com.").as_deref(), Some("example.com."));
        assert_eq!(registrable_domain("com."), None);
        assert!(is_public_suffix("co.uk."));
        // A wildcard rule doesn't imply its parent ("*.ck" → "ck" via the implicit rule only).
        assert_eq!(public_suffix("ck").as_deref(), Some("ck"));
        assert_eq!(public_suffix("a.b.ck").as_deref(), Some("b.ck"));
    }

    #[test]
    fn embedded_list_is_valid() {
        let r = parse(list()).expect("embedded list parses");
        assert!(r.normal.len() > 5000);
        assert!(r.wildcard.contains("ck") && r.exception.contains("www.ck"));
    }

    fn newer_list(version: &str) -> String {
        let old = parse(list()).expect("embedded list parses").version;
        list().replacen(&format!("// VERSION: {old}"), &format!("// VERSION: {version}"), 1)
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("bb-psl-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn install_refuses_invalid_and_older_lists() {
        assert!(matches!(install(""), Err(InstallError::Invalid(_))));
        assert!(matches!(install("<html>Not here</html>"), Err(InstallError::Invalid(_))));
        // Truncated: the markers are gone.
        let full = list();
        let mut cut = full.len() / 2;
        while !full.is_char_boundary(cut) {
            cut -= 1;
        }
        assert!(matches!(install(&full[..cut]), Err(InstallError::Invalid(_))));
        let no_version = full.replacen("// VERSION:", "// VERSIO:", 1);
        assert_eq!(install(&no_version), Err(InstallError::Invalid("no VERSION line")));
        assert_eq!(install(&newer_list("2000-01-01_00-00-00_UTC")), Err(InstallError::NotNewer));
        assert_eq!(install(full), Err(InstallError::NotNewer));
    }

    #[test]
    fn refresh_downloads_at_most_once_a_day() {
        let dir = temp_dir("daily");
        let t0 = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        let v = "9998-01-01_00-00-00_UTC";
        let mut calls = 0;
        let out = refresh_with(&dir, t0, || {
            calls += 1;
            Ok(newer_list(v))
        });
        assert_eq!(out, Refresh::Updated(v.to_string()));
        assert!(version().as_str() >= v);
        assert!(std::fs::read_to_string(dir.join(CACHE_FILE)).unwrap().contains(v));
        // Within a day: no download.
        let out = refresh_with(&dir, t0 + Duration::from_secs(23 * 3600), || {
            calls += 1;
            Ok(String::new())
        });
        assert_eq!(out, Refresh::Fresh);
        assert_eq!(calls, 1);
        // A day later: download again; the same version is "unchanged".
        let out = refresh_with(&dir, t0 + REFRESH_INTERVAL, || {
            calls += 1;
            Ok(newer_list(v))
        });
        assert_eq!(out, Refresh::Unchanged);
        assert_eq!(calls, 2);
        // The clock moved back: a stamp from the future isn't "recent".
        let out = refresh_with(&dir, t0 - Duration::from_secs(3600), || {
            calls += 1;
            Err("offline".into())
        });
        assert_eq!(out, Refresh::Failed("offline".into()));
        assert_eq!(calls, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_or_invalid_downloads_keep_the_list_and_wait_a_day() {
        let dir = temp_dir("failed");
        let t0 = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        let before = version();
        let out = refresh_with(&dir, t0, || Ok("<html>captive portal</html>".into()));
        assert_eq!(out, Refresh::Failed("section markers missing".into()));
        assert!(version() >= before);
        assert!(!dir.join(CACHE_FILE).exists());
        assert_eq!(
            refresh_with(&dir, t0 + Duration::from_secs(60), || Ok(newer_list("9999-x"))),
            Refresh::Fresh
        );
        assert_eq!(registrable_domain("www.example.co.uk").as_deref(), Some("example.co.uk"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cached_list_is_loaded_even_when_fresh() {
        let dir = temp_dir("cached");
        std::fs::create_dir_all(&dir).unwrap();
        let v = "9997-01-01_00-00-00_UTC";
        std::fs::write(dir.join(CACHE_FILE), newer_list(v)).unwrap();
        let t0 = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        std::fs::write(dir.join(STAMP_FILE), (2_000_000_000u64 - 10).to_string()).unwrap();
        assert_eq!(refresh_with(&dir, t0, || panic!("must not download")), Refresh::Fresh);
        assert!(version().as_str() >= v);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

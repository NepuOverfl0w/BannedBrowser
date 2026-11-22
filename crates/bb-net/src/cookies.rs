//! HTTP cookies (RFC 6265bis, "Cookies: HTTP State Management Mechanism", draft-ietf-httpbis-rfc6265bis).
//!
//! * [`parse_set_cookie`]: §5.6 parsing of a `Set-Cookie` header, including §5.1.1 cookie dates.
//! * [`CookieJar::store`]: the §5.7 storage model (domain, path, Secure, HttpOnly, SameSite,
//!   prefixes, the Public Suffix List, eviction).
//! * [`CookieJar::cookie_header`]: the §5.8.3 retrieval algorithm and `Cookie` header.
//!
//! Times are Unix seconds. Private windows use their own jar (their own `Client`).

use crate::psl;
use bb_url::{Host, Url};
use std::sync::Mutex;

/// Maximum cookies kept in total and per registrable domain (oldest-accessed evicted first).
pub const MAX_COOKIES: usize = 3000;
pub const MAX_PER_DOMAIN: usize = 180;
/// §5.6: a name+value or attribute value longer than this is ignored.
const MAX_NAME_VALUE: usize = 4096;
const MAX_ATTRIBUTE_VALUE: usize = 1024;
/// §5.6 step 4.1.6: expiry is capped at 400 days.
const MAX_AGE_CAP: i64 = 400 * 24 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    Strict,
    Lax,
    None,
    /// No (or an unknown) `SameSite` attribute: enforced like `Lax` (§5.6.7, "lax by default").
    Default,
}

/// A parsed `Set-Cookie` header (before the storage model applies).
#[derive(Debug, Clone, PartialEq)]
pub struct SetCookie {
    pub name: String,
    pub value: String,
    pub expires: Option<i64>,
    /// Absolute expiry from Max-Age (takes precedence over Expires).
    pub max_age_expiry: Option<i64>,
    pub domain: Option<String>,
    pub path: Option<String>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
}

/// A stored cookie (§5.7 storage model).
#[derive(Debug, Clone, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// The cookie's domain (a host for host-only cookies).
    pub domain: String,
    pub host_only: bool,
    pub path: String,
    /// Unix seconds; `None` for session cookies.
    pub expires: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
    pub creation: i64,
    pub last_access: i64,
    /// For ordering cookies created in the same second; assigned by the jar (`load` and
    /// `store` overwrite it), so code outside the jar can leave it 0.
    pub seq: u64,
}

/// The context of a request, for SameSite (§5.2) and for "non-HTTP" APIs.
#[derive(Debug, Clone, PartialEq)]
pub struct CookieContext {
    /// The site of the top-level document ("site for cookies"); `None` means the request is
    /// a top-level navigation the user started (always same-site).
    pub site_for_cookies: Option<Url>,
    /// A top-level navigation (the address bar, a link in the top-level document).
    pub top_level_navigation: bool,
    /// The request method (Lax cookies go with cross-site top-level GETs only).
    pub method: String,
    /// `document.cookie` and other non-HTTP APIs can't see or set HttpOnly cookies.
    pub http_api: bool,
}

impl Default for CookieContext {
    fn default() -> Self {
        CookieContext {
            site_for_cookies: None,
            top_level_navigation: true,
            method: "GET".into(),
            http_api: true,
        }
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Days since 1970-01-01 for a civil date (H. Hinnant's days_from_civil).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// RFC 6265bis §5.1.1 cookie-date parsing. Returns Unix seconds.
pub fn parse_cookie_date(s: &str) -> Option<i64> {
    let is_delim = |c: char| {
        let b = c as u32;
        b == 0x09
            || (0x20..=0x2F).contains(&b)
            || (0x3B..=0x40).contains(&b)
            || (0x5B..=0x60).contains(&b)
            || (0x7B..=0x7E).contains(&b)
    };
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    for token in s.split(is_delim).filter(|t| !t.is_empty()) {
        // time: 1*2DIGIT ":" 1*2DIGIT ":" 1*2DIGIT ( non-digit *OCTET )
        if time.is_none() {
            if let Some(t) = parse_time(token) {
                time = Some(t);
                continue;
            }
        }
        // day-of-month: 1*2DIGIT ( non-digit *OCTET )
        if day.is_none() {
            if let Some(d) = leading_digits(token, 1, 2) {
                day = Some(d);
                continue;
            }
        }
        // month: the first three characters name a month (case-insensitive).
        if month.is_none() && token.len() >= 3 {
            let m = match token.get(..3).map(|t| t.to_ascii_lowercase()).as_deref() {
                Some("jan") => Some(1),
                Some("feb") => Some(2),
                Some("mar") => Some(3),
                Some("apr") => Some(4),
                Some("may") => Some(5),
                Some("jun") => Some(6),
                Some("jul") => Some(7),
                Some("aug") => Some(8),
                Some("sep") => Some(9),
                Some("oct") => Some(10),
                Some("nov") => Some(11),
                Some("dec") => Some(12),
                _ => None,
            };
            if m.is_some() {
                month = m;
                continue;
            }
        }
        // year: 2*4DIGIT ( non-digit *OCTET )
        if year.is_none() {
            if let Some(y) = leading_digits(token, 2, 4) {
                year = Some(y);
                continue;
            }
        }
    }
    let mut year = year?;
    // Two-digit years.
    if (70..=99).contains(&year) {
        year += 1900;
    } else if (0..=69).contains(&year) {
        year += 2000;
    }
    let (day, month, (h, mi, sec)) = (day?, month?, time?);
    if !(1..=31).contains(&day) || year < 1601 || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    // A day that doesn't exist in that month (e.g. 31 Feb) fails.
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let dim = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][month as usize - 1];
    if day > dim {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + h * 3600 + mi * 60 + sec)
}

/// `min..=max` leading ASCII digits, then either the end or a non-digit.
fn leading_digits(token: &str, min: usize, max: usize) -> Option<i64> {
    let n = token.bytes().take_while(u8::is_ascii_digit).count();
    if n < min || n > max {
        return None;
    }
    token[..n].parse().ok()
}

fn parse_time(token: &str) -> Option<(i64, i64, i64)> {
    let mut parts = token.splitn(3, ':');
    let h = leading_digits(parts.next()?, 1, 2)?;
    let m = parts.next()?;
    if m.is_empty() || m.len() > 2 || !m.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let s = leading_digits(parts.next()?, 1, 2)?;
    Some((h, m.parse().ok()?, s))
}

/// RFC 6265bis §5.6: parse a `Set-Cookie` header value. `None` if the cookie must be ignored.
pub fn parse_set_cookie(header: &str, now: i64) -> Option<SetCookie> {
    // Step 1: a control character other than HTAB aborts.
    if header.chars().any(|c| (c < ' ' && c != '\t') || c == '\x7F') {
        return None;
    }
    let (pair, attrs) = match header.find(';') {
        Some(i) => (&header[..i], &header[i..]),
        None => (header, ""),
    };
    let (name, value) = match pair.find('=') {
        Some(i) => (pair[..i].trim_matches([' ', '\t']), pair[i + 1..].trim_matches([' ', '\t'])),
        None => ("", pair.trim_matches([' ', '\t'])),
    };
    if name.len() + value.len() > MAX_NAME_VALUE || (name.is_empty() && value.is_empty()) {
        return None;
    }
    let mut c = SetCookie {
        name: name.to_string(),
        value: value.to_string(),
        expires: None,
        max_age_expiry: None,
        domain: None,
        path: None,
        secure: false,
        http_only: false,
        same_site: SameSite::Default,
    };
    for av in attrs.split(';').skip(1) {
        let (k, v) = match av.find('=') {
            Some(i) => (av[..i].trim_matches([' ', '\t']), av[i + 1..].trim_matches([' ', '\t'])),
            None => (av.trim_matches([' ', '\t']), ""),
        };
        if v.len() > MAX_ATTRIBUTE_VALUE {
            continue;
        }
        match k.to_ascii_lowercase().as_str() {
            "expires" => {
                if let Some(t) = parse_cookie_date(v) {
                    c.expires = Some(t.min(now + MAX_AGE_CAP));
                }
            }
            "max-age" => {
                let ok = v.bytes().next().is_some_and(|b| b.is_ascii_digit() || b == b'-')
                    && v.bytes().skip(1).all(|b| b.is_ascii_digit());
                if ok {
                    let delta: i64 = v.parse().unwrap_or(if v.starts_with('-') { -1 } else { i64::MAX });
                    c.max_age_expiry =
                        Some(if delta <= 0 { i64::MIN } else { now.saturating_add(delta.min(MAX_AGE_CAP)) });
                }
            }
            "domain" => {
                if !v.is_empty() {
                    c.domain = Some(v.trim_start_matches('.').to_ascii_lowercase());
                }
            }
            "path" => {
                c.path = if v.starts_with('/') { Some(v.to_string()) } else { None };
            }
            "secure" => c.secure = true,
            "httponly" => c.http_only = true,
            "samesite" => {
                c.same_site = match v.to_ascii_lowercase().as_str() {
                    "strict" => SameSite::Strict,
                    "lax" => SameSite::Lax,
                    "none" => SameSite::None,
                    _ => SameSite::Default,
                }
            }
            _ => {}
        }
    }
    Some(c)
}

/// §5.1.4 default-path of a request URL.
pub fn default_path(url: &Url) -> String {
    let p = url.pathname();
    if !p.starts_with('/') {
        return "/".into();
    }
    match p.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => p[..i].to_string(),
    }
}

/// §5.1.4 path-match.
pub fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    request_path == cookie_path
        || (request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/') || request_path[cookie_path.len()..].starts_with('/')))
}

/// §5.1.3 domain-match (the host is canonical; IP addresses only match exactly).
pub fn domain_matches(host: &str, domain: &str, host_is_ip: bool) -> bool {
    host == domain
        || (!host_is_ip && host.ends_with(domain) && host[..host.len() - domain.len()].ends_with('.'))
}

fn host_of(url: &Url) -> Option<(String, bool)> {
    match url.host()? {
        Host::Domain(d) => Some((d.trim_end_matches('.').to_string(), false)),
        h @ (Host::Ipv4(_) | Host::Ipv6(_)) => Some((h.serialize(), true)),
        _ => None,
    }
}

/// Is the URL's origin "potentially trustworthy" for Secure cookies (https, or loopback)?
fn secure_origin(url: &Url) -> bool {
    url.scheme() == "https" || url.host().is_some_and(|h| h.is_loopback())
}

/// A URL's schemeful site (HTML §7.1.1): scheme and registrable domain (or host, or the
/// serialized origin for URLs without a host).
pub(crate) fn site_of(u: &Url) -> String {
    match host_of(u) {
        Some((h, ip)) => {
            let host = if ip { h } else { psl::registrable_domain(&h).unwrap_or(h) };
            format!("{}://{host}", u.scheme())
        }
        None => u.origin().serialize(),
    }
}

/// Schemeful same-site (HTML §7.1.1): same scheme and same registrable domain (or host).
pub fn same_site(a: &Url, b: &Url) -> bool {
    let site = |u: &Url| {
        let (h, ip) = host_of(u)?;
        let host = if ip { h } else { psl::registrable_domain(&h).unwrap_or(h) };
        Some((u.scheme().to_string(), host))
    };
    match (site(a), site(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// A cookie jar, shared by the fetches of one browsing profile (thread-safe).
#[derive(Debug, Default)]
pub struct CookieJar {
    inner: Mutex<JarInner>,
}

#[derive(Debug, Default)]
struct JarInner {
    cookies: Vec<Cookie>,
    seq: u64,
    /// Bumped by every change that should be saved (stores, removals, evictions, clear).
    generation: u64,
}

impl CookieJar {
    pub fn new() -> Self {
        Self::default()
    }

    /// All live cookies (for persistence and inspection).
    pub fn all(&self) -> Vec<Cookie> {
        let t = now();
        self.inner
            .lock()
            .map(|j| j.cookies.iter().filter(|c| c.expires.is_none_or(|e| e > t)).cloned().collect())
            .unwrap_or_default()
    }

    /// Add previously stored cookies (e.g. loaded from the profile), oldest first. Expired ones
    /// are skipped. This doesn't count as a change to save.
    pub fn load(&self, mut cookies: Vec<Cookie>) {
        let t = now();
        cookies.sort_by_key(|c| c.creation);
        if let Ok(mut j) = self.inner.lock() {
            for mut c in cookies {
                if c.expires.is_some_and(|e| e <= t) {
                    continue;
                }
                j.seq += 1;
                c.seq = j.seq;
                j.cookies.retain(|o| {
                    !(o.name == c.name
                        && o.domain == c.domain
                        && o.host_only == c.host_only
                        && o.path == c.path)
                });
                j.cookies.push(c);
            }
        }
    }

    pub fn clear(&self) {
        if let Ok(mut j) = self.inner.lock() {
            j.cookies.clear();
            j.generation += 1;
        }
    }

    /// A counter that changes whenever cookies are added, replaced or removed (the profile saves
    /// when it moves).
    pub fn generation(&self) -> u64 {
        self.inner.lock().map(|j| j.generation).unwrap_or(0)
    }

    /// The cookies a profile keeps across restarts: live persistent (non-session) cookies.
    pub fn persistent(&self) -> Vec<Cookie> {
        self.all().into_iter().filter(|c| c.expires.is_some()).collect()
    }

    /// §5.7: process a `Set-Cookie` header received for `url`. Returns whether it was stored
    /// (or removed an existing cookie).
    pub fn store(&self, url: &Url, header: &str, ctx: &CookieContext) -> bool {
        self.store_with(url, header, ctx, psl::available())
    }

    /// [`store`](Self::store), told whether a Public Suffix List is available. Without one,
    /// nothing is known to be a public suffix, so a `Domain` attribute that names a parent
    /// domain is ignored (the cookie stays host-only) instead of trusted.
    fn store_with(&self, url: &Url, header: &str, ctx: &CookieContext, have_psl: bool) -> bool {
        let t = now();
        let Some(sc) = parse_set_cookie(header, t) else { return false };
        let Some((host, host_is_ip)) = host_of(url) else { return false };
        // Step 4-6: domain attribute and public suffixes.
        let mut domain = sc.domain.clone().unwrap_or_default();
        if !domain.is_empty() {
            match bb_url::idna::domain_to_ascii(&domain) {
                Some(d) => domain = d,
                None => return false,
            }
            if !host_is_ip && have_psl && psl::is_public_suffix(&domain) {
                if domain == host {
                    domain.clear(); // a public suffix setting a cookie for itself: host-only
                } else {
                    return false;
                }
            } else if !host_is_ip && !have_psl && domain != host {
                if !domain_matches(&host, &domain, false) {
                    return false;
                }
                domain.clear();
            }
        }
        let (domain, host_only) = if domain.is_empty() {
            (host.clone(), true)
        } else {
            if !domain_matches(&host, &domain, host_is_ip) {
                return false;
            }
            (domain, false)
        };
        let path = sc.path.clone().unwrap_or_else(|| default_path(url));
        let secure_url = secure_origin(url);
        // Secure cookies only from secure origins.
        if sc.secure && !secure_url {
            return false;
        }
        // HttpOnly cookies can't be set from non-HTTP APIs.
        if sc.http_only && !ctx.http_api {
            return false;
        }
        // SameSite=None requires Secure.
        if sc.same_site == SameSite::None && !sc.secure {
            return false;
        }
        // Cookies with SameSite restrictions can't be set from cross-site subresources.
        if sc.same_site != SameSite::None && !is_same_site_request(url, ctx) && !ctx.top_level_navigation {
            return false;
        }
        // Cookie name prefixes (§4.1.3).
        let lname = sc.name.to_ascii_lowercase();
        if lname.starts_with("__secure-") && !(sc.secure && secure_url) {
            return false;
        }
        if lname.starts_with("__host-")
            && !(sc.secure && secure_url && host_only && sc.domain.is_none() && path == "/")
        {
            return false;
        }
        // A nameless cookie whose value looks like a prefixed name is refused too.
        let lvalue = sc.value.to_ascii_lowercase();
        if sc.name.is_empty() && (lvalue.starts_with("__secure-") || lvalue.starts_with("__host-")) {
            return false;
        }
        let expires = sc.max_age_expiry.or(sc.expires);
        let Ok(mut j) = self.inner.lock() else { return false };
        // A non-secure origin can't overwrite (shadow) a Secure cookie (§5.7 step 16).
        if !sc.secure && !secure_url {
            let shadows = j.cookies.iter().any(|o| {
                o.secure
                    && o.name == sc.name
                    && (domain_matches(&domain, &o.domain, host_is_ip)
                        || domain_matches(&o.domain, &domain, host_is_ip))
                    && path_matches(&path, &o.path)
            });
            if shadows {
                return false;
            }
        }
        // Replace an existing cookie with the same name, domain, host-only-ness and path.
        let mut creation = t;
        if let Some(i) = j.cookies.iter().position(|o| {
            o.name == sc.name && o.domain == domain && o.host_only == host_only && o.path == path
        }) {
            if j.cookies[i].http_only && !ctx.http_api {
                return false;
            }
            creation = j.cookies[i].creation;
            j.cookies.remove(i);
            j.generation += 1;
        }
        if expires.is_some_and(|e| e <= t) {
            return true; // expired: the old cookie (if any) is now gone
        }
        j.generation += 1;
        j.seq += 1;
        let seq = j.seq;
        j.cookies.push(Cookie {
            name: sc.name,
            value: sc.value,
            domain: domain.clone(),
            host_only,
            path,
            expires,
            secure: sc.secure,
            http_only: sc.http_only,
            same_site: sc.same_site,
            creation,
            last_access: t,
            seq,
        });
        evict(&mut j, &domain, host_is_ip, t);
        true
    }

    /// §5.8.3: the `Cookie` header value for a request to `url`, or `None` if no cookie applies.
    pub fn cookie_header(&self, url: &Url, ctx: &CookieContext) -> Option<String> {
        let (host, host_is_ip) = host_of(url)?;
        let path = url.pathname();
        let secure_url = secure_origin(url);
        let same_site = is_same_site_request(url, ctx);
        let lax_ok =
            ctx.top_level_navigation && matches!(ctx.method.as_str(), "GET" | "HEAD" | "OPTIONS" | "TRACE");
        let t = now();
        let mut j = self.inner.lock().ok()?;
        j.cookies.retain(|c| c.expires.is_none_or(|e| e > t));
        let mut matching: Vec<usize> = Vec::new();
        for (i, c) in j.cookies.iter().enumerate() {
            let domain_ok =
                if c.host_only { host == c.domain } else { domain_matches(&host, &c.domain, host_is_ip) };
            if !domain_ok || !path_matches(&path, &c.path) {
                continue;
            }
            if c.secure && !secure_url {
                continue;
            }
            if c.http_only && !ctx.http_api {
                continue;
            }
            if !same_site {
                match c.same_site {
                    SameSite::Strict => continue,
                    SameSite::Lax | SameSite::Default if !lax_ok => continue,
                    _ => {}
                }
            }
            matching.push(i);
        }
        if matching.is_empty() {
            return None;
        }
        // Longer paths first, then earlier creation.
        matching.sort_by(|&a, &b| {
            let (x, y) = (&j.cookies[a], &j.cookies[b]);
            y.path.len().cmp(&x.path.len()).then(x.creation.cmp(&y.creation)).then(x.seq.cmp(&y.seq))
        });
        let mut out = String::new();
        for &i in &matching {
            j.cookies[i].last_access = t;
            let c = &j.cookies[i];
            if !out.is_empty() {
                out.push_str("; ");
            }
            if c.name.is_empty() {
                out.push_str(&c.value);
            } else {
                out.push_str(&c.name);
                out.push('=');
                out.push_str(&c.value);
            }
        }
        Some(out)
    }
}

fn is_same_site_request(url: &Url, ctx: &CookieContext) -> bool {
    ctx.site_for_cookies.as_ref().is_none_or(|top| same_site(top, url))
}

/// Keep at most MAX_PER_DOMAIN cookies per registrable domain and MAX_COOKIES in total,
/// evicting expired cookies first, then the least recently accessed.
fn evict(j: &mut JarInner, domain: &str, is_ip: bool, t: i64) {
    j.cookies.retain(|c| c.expires.is_none_or(|e| e > t));
    let base = |d: &str| {
        if is_ip {
            d.to_string()
        } else {
            psl::registrable_domain(d).unwrap_or_else(|| d.to_string())
        }
    };
    let b = base(domain);
    let same: Vec<usize> =
        j.cookies.iter().enumerate().filter(|(_, c)| base(&c.domain) == b).map(|(i, _)| i).collect();
    if same.len() > MAX_PER_DOMAIN {
        let mut v: Vec<(i64, u64, usize)> =
            same.iter().map(|&i| (j.cookies[i].last_access, j.cookies[i].seq, i)).collect();
        v.sort();
        let drop: Vec<usize> = v[..same.len() - MAX_PER_DOMAIN].iter().map(|x| x.2).collect();
        let mut i = 0;
        j.cookies.retain(|_| {
            let keep = !drop.contains(&i);
            i += 1;
            keep
        });
    }
    if j.cookies.len() > MAX_COOKIES {
        j.cookies.sort_by_key(|c| (c.last_access, c.seq));
        let n = j.cookies.len() - MAX_COOKIES;
        j.cookies.drain(..n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn top() -> CookieContext {
        // These tests assume the real list (public suffixes, registrable domains).
        crate::psl::tests::list();
        CookieContext::default()
    }

    #[test]
    fn without_a_public_suffix_list_domain_attributes_stay_host_only() {
        let jar = CookieJar::new();
        let ctx = CookieContext::default();
        let url = u("https://www.example.co.uk/");
        // A parent domain is ignored (it might be a public suffix): host-only.
        assert!(jar.store_with(&url, "a=1; Domain=co.uk", &ctx, false));
        assert!(jar.store_with(&url, "b=2; Domain=example.co.uk", &ctx, false));
        assert!(jar.store_with(&url, "c=3; Domain=www.example.co.uk", &ctx, false));
        // A domain the host isn't in is still refused.
        assert!(!jar.store_with(&url, "d=4; Domain=other.co.uk", &ctx, false));
        assert_eq!(jar.cookie_header(&u("https://other.example.co.uk/"), &ctx), None);
        assert_eq!(jar.cookie_header(&url, &ctx).as_deref(), Some("a=1; b=2; c=3"));
        // __Host- still refuses any Domain attribute.
        assert!(!jar.store_with(&url, "__Host-x=1; Secure; Path=/; Domain=example.co.uk", &ctx, false));
    }

    #[test]
    fn cookie_dates() {
        // RFC 6265bis examples and common formats.
        assert_eq!(parse_cookie_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784_111_777));
        assert_eq!(parse_cookie_date("Sunday, 06-Nov-94 08:49:37 GMT"), Some(784_111_777));
        assert_eq!(parse_cookie_date("Sun Nov  6 08:49:37 1994"), Some(784_111_777));
        assert_eq!(parse_cookie_date("Wed, 21 Oct 2015 07:28:00 GMT"), Some(1_445_412_480));
        assert_eq!(parse_cookie_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_cookie_date("1 Jan 2069 00:00:00"), parse_cookie_date("1 Jan 69 00:00:00"));
        assert_eq!(parse_cookie_date("31 Feb 2020 00:00:00"), None);
        assert_eq!(parse_cookie_date("1 Jan 1600 00:00:00"), None);
        assert_eq!(parse_cookie_date("1 Jan 2020 24:00:00"), None);
        assert_eq!(parse_cookie_date("garbage"), None);
    }

    #[test]
    fn parsing() {
        let c =
            parse_set_cookie(" sid = abc ; Path=/app; Secure; HttpOnly; SameSite=Strict; Max-Age=60", 1000)
                .unwrap();
        assert_eq!((c.name.as_str(), c.value.as_str()), ("sid", "abc"));
        assert_eq!(c.path.as_deref(), Some("/app"));
        assert!(c.secure && c.http_only);
        assert_eq!(c.same_site, SameSite::Strict);
        assert_eq!(c.max_age_expiry, Some(1060));
        // No "=": a nameless cookie.
        let c = parse_set_cookie("justvalue", 0).unwrap();
        assert_eq!((c.name.as_str(), c.value.as_str()), ("", "justvalue"));
        // Control characters abort; empty name and value is ignored.
        assert!(parse_set_cookie("a=b\x01", 0).is_none());
        assert!(parse_set_cookie("=", 0).is_none());
        // Max-Age 0 or negative expires immediately; bad Max-Age is ignored.
        assert_eq!(parse_set_cookie("a=b; Max-Age=0", 5).unwrap().max_age_expiry, Some(i64::MIN));
        assert_eq!(parse_set_cookie("a=b; Max-Age=1x", 5).unwrap().max_age_expiry, None);
        // Domain loses its leading dot; a relative Path is ignored.
        let c = parse_set_cookie("a=b; Domain=.Example.COM; Path=rel", 0).unwrap();
        assert_eq!(c.domain.as_deref(), Some("example.com"));
        assert_eq!(c.path, None);
    }

    #[test]
    fn paths() {
        assert_eq!(default_path(&u("http://h/a/b/c")), "/a/b");
        assert_eq!(default_path(&u("http://h/a")), "/");
        assert!(path_matches("/a/b", "/a"));
        assert!(path_matches("/a/", "/a/"));
        assert!(!path_matches("/ab", "/a"));
    }

    #[test]
    fn store_and_send() {
        let jar = CookieJar::new();
        let url = u("https://www.example.com/shop/cart");
        assert!(jar.store(&url, "a=1", &top()));
        assert!(jar.store(&url, "b=2; Domain=example.com; Path=/", &top()));
        assert!(jar.store(&url, "c=3; Path=/shop/cart", &top()));
        // Host-only a (path /shop) doesn't go to other subdomains; b does.
        assert_eq!(jar.cookie_header(&u("https://api.example.com/"), &top()).as_deref(), Some("b=2"));
        // Longest path first.
        assert_eq!(jar.cookie_header(&url, &top()).as_deref(), Some("c=3; a=1; b=2"));
        // Replace and delete.
        assert!(jar.store(&url, "a=new", &top()));
        assert!(jar.store(&url, "c=gone; Path=/shop/cart; Max-Age=0", &top()));
        assert_eq!(jar.cookie_header(&url, &top()).as_deref(), Some("a=new; b=2"));
    }

    #[test]
    fn public_suffixes_and_foreign_domains_are_refused() {
        let jar = CookieJar::new();
        let url = u("https://www.example.co.uk/");
        assert!(!jar.store(&url, "x=1; Domain=co.uk", &top()));
        assert!(!jar.store(&url, "x=1; Domain=other.co.uk", &top()));
        assert!(!jar.store(&u("https://evil.github.io/"), "x=1; Domain=github.io", &top()));
        assert!(jar.store(&url, "x=1; Domain=example.co.uk", &top()));
        assert_eq!(jar.cookie_header(&u("https://example.co.uk/"), &top()).as_deref(), Some("x=1"));
    }

    #[test]
    fn secure_httponly_and_prefixes() {
        let jar = CookieJar::new();
        let http = u("http://example.com/");
        let https = u("https://example.com/");
        assert!(!jar.store(&http, "s=1; Secure", &top()), "Secure from an insecure origin");
        assert!(jar.store(&https, "s=1; Secure", &top()));
        assert_eq!(jar.cookie_header(&http, &top()), None, "Secure cookies only over https");
        assert!(!jar.store(&http, "s=evil", &top()), "can't shadow a Secure cookie from http");
        assert!(!jar.store(&https, "__Secure-a=1", &top()));
        assert!(jar.store(&https, "__Secure-a=1; Secure", &top()));
        assert!(!jar.store(&https, "__Host-b=1; Secure; Domain=example.com; Path=/", &top()));
        assert!(jar.store(&https, "__Host-b=1; Secure; Path=/", &top()));
        // HttpOnly: invisible to and unsettable by non-HTTP APIs.
        assert!(jar.store(&https, "h=1; HttpOnly", &top()));
        let script = CookieContext { http_api: false, ..top() };
        assert!(!jar.cookie_header(&https, &script).unwrap_or_default().contains("h=1"));
        assert!(!jar.store(&https, "h=2", &script));
        // Loopback counts as secure.
        assert!(jar.store(&u("http://127.0.0.1:8000/"), "l=1; Secure", &top()));
    }

    #[test]
    fn same_site_rules() {
        let jar = CookieJar::new();
        let site = u("https://shop.example/");
        for h in
            ["strict=1; SameSite=Strict", "lax=1; SameSite=Lax", "dflt=1", "none=1; SameSite=None; Secure"]
        {
            assert!(jar.store(&site, h, &top()));
        }
        assert!(!jar.store(&site, "insecure=1; SameSite=None", &top()), "None needs Secure");
        let from_elsewhere = Some(u("https://news.example/"));
        // A cross-site image: only SameSite=None.
        let sub =
            CookieContext { site_for_cookies: from_elsewhere.clone(), top_level_navigation: false, ..top() };
        assert_eq!(jar.cookie_header(&site, &sub).as_deref(), Some("none=1"));
        // A cross-site top-level GET navigation: Lax and default too, not Strict.
        let nav =
            CookieContext { site_for_cookies: from_elsewhere.clone(), top_level_navigation: true, ..top() };
        assert_eq!(jar.cookie_header(&site, &nav).as_deref(), Some("lax=1; dflt=1; none=1"));
        // A cross-site top-level POST: only None.
        let post = CookieContext { method: "POST".into(), ..nav };
        assert_eq!(jar.cookie_header(&site, &post).as_deref(), Some("none=1"));
        // Same site (subdomain of the same registrable domain): everything.
        let same = CookieContext {
            site_for_cookies: Some(u("https://www.shop.example/")),
            top_level_navigation: false,
            ..top()
        };
        assert_eq!(jar.cookie_header(&site, &same).as_deref(), Some("strict=1; lax=1; dflt=1; none=1"));
        assert!(same_site(&u("https://a.example.co.uk/"), &u("https://b.example.co.uk/x")));
        assert!(!same_site(&u("http://example.com/"), &u("https://example.com/")), "schemeful");
    }

    #[test]
    fn eviction_limits() {
        let jar = CookieJar::new();
        let url = u("https://example.com/");
        for i in 0..MAX_PER_DOMAIN + 20 {
            assert!(jar.store(&url, &format!("c{i}=v"), &top()));
        }
        assert_eq!(jar.all().len(), MAX_PER_DOMAIN);
    }
}

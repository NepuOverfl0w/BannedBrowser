//! The HTTP cache (RFC 9111): a private cache, partitioned by top-level site.
//!
//! - **Storing** (§3): `GET` responses without `no-store` (request or response), not `Vary: *`,
//!   with explicit freshness or a heuristically cacheable status; `Set-Cookie` is never stored
//!   (cookies are only ever taken from the network).
//! - **Freshness** (§4.2): `max-age`, else `Expires` − `Date`, else a heuristic of 10 % of the
//!   time since `Last-Modified` (at most a week); the age calculation of §4.2.3.
//! - **Validation** (§4.3): a stale entry is revalidated with `If-None-Match` /
//!   `If-Modified-Since`; a 304 freshens the stored headers (§4.3.4) and the stored body is used.
//! - **Vary** (§4.1): the request headers it names must match.
//! - **Invalidation** (§4.4): a non-error response to an unsafe method removes the target URI and
//!   its same-origin `Location` / `Content-Location`.
//! - **Request modes** (Fetch §4.5 cache mode): `default`, `no-store`, `reload` (hard reload),
//!   `no-cache` (reload: always revalidate) and `force-cache` (history navigation: stale is fine).
//!
//! Entries are kept on disk (one file per entry in the cache directory, written atomically) or,
//! for private windows, in memory only. The cache is bounded and evicts the least recently used
//! entries. A damaged entry file reads as a miss.

use crate::{Headers, Request, Response};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// How a request uses the cache (Fetch §4.5 "cache mode").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CacheMode {
    /// Use a fresh entry, revalidate a stale one.
    #[default]
    Default,
    /// Neither read nor write the cache.
    NoStore,
    /// Ignore the cache, but store the response (hard reload).
    Reload,
    /// Always revalidate an entry before using it (reload).
    NoCache,
    /// Use any entry, even a stale one; the network only on a miss (Back/Forward).
    ForceCache,
}

/// Where a response came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CacheStatus {
    #[default]
    Network,
    /// A fresh (or force-cache) entry; no request was made.
    Hit,
    /// A stale entry the server confirmed with 304 Not Modified.
    Revalidated,
}

/// Status codes that are heuristically cacheable (RFC 9110 §15.1).
const HEURISTIC: &[u16] = &[200, 203, 204, 206, 300, 301, 308, 404, 405, 410, 414, 501];
/// The heuristic freshness cap.
const MAX_HEURISTIC: i64 = 7 * 24 * 3600;
/// File format magic + version.
const MAGIC: &[u8; 8] = b"BBCACHE1";

/// Request headers the HTTP layer adds when a request doesn't set them (for Vary matching).
pub(crate) type DefaultHeader<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The `Cache-Control` directives of a header list: lowercase names, unquoted values.
pub fn directives(h: &Headers) -> HashMap<String, Option<String>> {
    let mut out = HashMap::new();
    for v in h.get_all("cache-control") {
        for part in v.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (name, val) = match part.split_once('=') {
                Some((n, v)) => (n.trim(), Some(v.trim().trim_matches('"').to_string())),
                None => (part, None),
            };
            out.entry(name.to_ascii_lowercase()).or_insert(val);
        }
    }
    out
}

/// RFC 9111 §1.2.2 delta-seconds (invalid → `None`; huge values saturate).
fn delta_seconds(v: &Option<String>) -> Option<i64> {
    let v = v.as_deref()?;
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(v.parse::<i64>().unwrap_or(i64::MAX / 4).min(i64::MAX / 4))
}

fn http_date(h: &Headers, name: &str) -> Option<i64> {
    h.get(name).and_then(crate::cookies::parse_cookie_date)
}

/// A stored response (without its body).
#[derive(Clone, Debug)]
pub(crate) struct Meta {
    pub url: String,
    pub status: u16,
    pub status_text: String,
    pub headers: Headers,
    /// The request header values named by `Vary`, at store time (`None` = absent).
    pub vary: Vec<(String, Option<String>)>,
    pub request_time: i64,
    pub response_time: i64,
    pub secure: bool,
    pub body_len: u64,
    last_used: u64,
}

impl Meta {
    /// §4.2.1 freshness lifetime.
    fn freshness_lifetime(&self) -> i64 {
        let cc = directives(&self.headers);
        if let Some(Some(a)) = cc.get("max-age").map(delta_seconds) {
            return a;
        }
        if self.headers.contains("expires") {
            let date = http_date(&self.headers, "date").unwrap_or(self.response_time);
            // An invalid Expires (e.g. "0") means already expired.
            return http_date(&self.headers, "expires").map_or(0, |e| (e - date).max(0));
        }
        if HEURISTIC.contains(&self.status) || cc.contains_key("public") {
            if let Some(lm) = http_date(&self.headers, "last-modified") {
                let date = http_date(&self.headers, "date").unwrap_or(self.response_time);
                return ((date - lm).max(0) / 10).min(MAX_HEURISTIC);
            }
            if matches!(self.status, 301 | 308) {
                return 24 * 3600;
            }
        }
        0
    }

    /// §4.2.3 current age.
    fn current_age(&self, now: i64) -> i64 {
        let age_value =
            self.headers.get("age").and_then(|a| delta_seconds(&Some(a.trim().to_string()))).unwrap_or(0);
        let date = http_date(&self.headers, "date").unwrap_or(self.response_time);
        let apparent_age = (self.response_time - date).max(0);
        let response_delay = (self.response_time - self.request_time).max(0);
        let corrected_age_value = age_value.saturating_add(response_delay);
        let corrected_initial_age = apparent_age.max(corrected_age_value);
        let resident_time = (now - self.response_time).max(0);
        corrected_initial_age.saturating_add(resident_time)
    }

    /// Fresh, and allowed to be used without validation.
    fn usable_fresh(&self, now: i64) -> bool {
        let cc = directives(&self.headers);
        !cc.contains_key("no-cache") && self.current_age(now) < self.freshness_lifetime()
    }

    fn has_validator(&self) -> bool {
        self.headers.contains("etag") || self.headers.contains("last-modified")
    }
}

/// The partition (top-level site) and URL of a request.
pub(crate) fn key(req: &Request) -> String {
    let top = req.cookie_context.site_for_cookies.as_ref().unwrap_or(&req.url);
    format!("{} {}", crate::cookies::site_of(top), req.url.href_without_fragment())
}

/// §3 whether a response may be stored.
pub(crate) fn storable(req: &Request, resp: &Response) -> bool {
    if req.method != "GET" || resp.status < 200 || resp.status == 206 || resp.status == 304 || !resp.complete
    {
        return false;
    }
    let rc = directives(&req.headers);
    let cc = directives(&resp.headers);
    if rc.contains_key("no-store") || cc.contains_key("no-store") {
        return false;
    }
    if resp.headers.get_all("vary").any(|v| v.split(',').any(|f| f.trim() == "*")) {
        return false;
    }
    cc.contains_key("max-age")
        || cc.contains_key("public")
        || cc.contains_key("private")
        || resp.headers.contains("expires")
        || HEURISTIC.contains(&resp.status)
}

/// FNV-1a 64 (a stable file name for a key).
fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

struct Entry {
    meta: Meta,
    /// Memory-only caches keep the body here.
    body: Option<Arc<Vec<u8>>>,
}

#[derive(Default)]
struct Index {
    entries: HashMap<String, Entry>,
    total: u64,
    loaded: bool,
    clock: u64,
}

/// An HTTP cache: on disk in `dir`, or in memory only.
pub struct HttpCache {
    dir: Option<PathBuf>,
    max_bytes: u64,
    index: Mutex<Index>,
}

impl std::fmt::Debug for HttpCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpCache").field("dir", &self.dir).field("max_bytes", &self.max_bytes).finish()
    }
}

// ----- entry files -----

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.i.checked_add(n)?;
        let s = self.b.get(self.i..end)?;
        self.i = end;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

/// Header section of an entry file (everything but the body).
fn encode_meta(key: &str, m: &Meta) -> Vec<u8> {
    let mut out = Vec::new();
    put_str(&mut out, key);
    put_str(&mut out, &m.url);
    put_u64(&mut out, u64::from(m.status));
    put_str(&mut out, &m.status_text);
    put_u64(&mut out, m.request_time as u64);
    put_u64(&mut out, m.response_time as u64);
    put_u64(&mut out, u64::from(m.secure));
    put_u64(&mut out, m.headers.len() as u64);
    for (k, v) in m.headers.iter() {
        put_str(&mut out, k);
        put_str(&mut out, v);
    }
    put_u64(&mut out, m.vary.len() as u64);
    for (k, v) in &m.vary {
        put_str(&mut out, k);
        put_u64(&mut out, u64::from(v.is_some()));
        put_str(&mut out, v.as_deref().unwrap_or(""));
    }
    put_u64(&mut out, m.body_len);
    out
}

fn decode_meta(b: &[u8]) -> Option<(String, Meta)> {
    let mut c = Cursor { b, i: 0 };
    let key = c.str()?;
    let url = c.str()?;
    let status = u16::try_from(c.u64()?).ok()?;
    let status_text = c.str()?;
    let request_time = c.u64()? as i64;
    let response_time = c.u64()? as i64;
    let secure = c.u64()? != 0;
    let n = c.u64()?;
    if n > 10_000 {
        return None;
    }
    let mut headers = Headers::new();
    for _ in 0..n {
        let k = c.str()?;
        let v = c.str()?;
        headers.append(&k, &v);
    }
    let nv = c.u64()?;
    if nv > 1000 {
        return None;
    }
    let mut vary = Vec::new();
    for _ in 0..nv {
        let k = c.str()?;
        let some = c.u64()? != 0;
        let v = c.str()?;
        vary.push((k, some.then_some(v)));
    }
    let body_len = c.u64()?;
    Some((
        key,
        Meta {
            url,
            status,
            status_text,
            headers,
            vary,
            request_time,
            response_time,
            secure,
            body_len,
            last_used: 0,
        },
    ))
}

impl HttpCache {
    /// A disk cache in `dir` holding at most `max_bytes` of bodies.
    pub fn on_disk(dir: &Path, max_bytes: u64) -> HttpCache {
        HttpCache { dir: Some(dir.to_path_buf()), max_bytes, index: Mutex::new(Index::default()) }
    }

    /// A memory-only cache (private windows): nothing is written to disk.
    pub fn in_memory(max_bytes: u64) -> HttpCache {
        HttpCache { dir: None, max_bytes, index: Mutex::new(Index { loaded: true, ..Default::default() }) }
    }

    fn file_for(&self, key: &str) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join(format!("{:016x}.entry", fnv(key))))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Index> {
        let mut g = match self.index.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if !g.loaded {
            g.loaded = true;
            self.load_index(&mut g);
        }
        g
    }

    /// Read every entry file's header section (once, on first use).
    fn load_index(&self, idx: &mut Index) {
        let Some(dir) = &self.dir else { return };
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut found: Vec<(String, Meta)> = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("entry") {
                continue;
            }
            match read_meta(&p) {
                Some((key, meta)) if self.file_for(&key).as_deref() == Some(p.as_path()) => {
                    found.push((key, meta))
                }
                _ => {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        // Least recently stored first, so the clock orders them.
        found.sort_by_key(|(_, m)| m.response_time);
        for (key, mut meta) in found {
            idx.clock += 1;
            meta.last_used = idx.clock;
            idx.total += meta.body_len;
            idx.entries.insert(key, Entry { meta, body: None });
        }
        self.evict(idx);
    }

    fn evict(&self, idx: &mut Index) {
        if idx.total <= self.max_bytes {
            return;
        }
        let mut order: Vec<(u64, String)> =
            idx.entries.iter().map(|(k, e)| (e.meta.last_used, k.clone())).collect();
        order.sort();
        let target = self.max_bytes / 10 * 9;
        for (_, k) in order {
            if idx.total <= target {
                break;
            }
            self.remove_locked(idx, &k);
        }
    }

    fn remove_locked(&self, idx: &mut Index, key: &str) {
        if let Some(e) = idx.entries.remove(key) {
            idx.total = idx.total.saturating_sub(e.meta.body_len);
            if let Some(f) = self.file_for(key) {
                let _ = std::fs::remove_file(f);
            }
        }
    }

    /// Remove `key` (invalidation).
    pub(crate) fn remove(&self, key: &str) {
        let mut idx = self.lock();
        self.remove_locked(&mut idx, key);
    }

    /// Forget everything (and delete the files).
    pub fn clear(&self) {
        let mut idx = self.lock();
        let keys: Vec<String> = idx.entries.keys().cloned().collect();
        for k in keys {
            self.remove_locked(&mut idx, &k);
        }
    }

    /// Number of entries and total body bytes.
    pub fn stats(&self) -> (usize, u64) {
        let idx = self.lock();
        (idx.entries.len(), idx.total)
    }

    /// The stored response for `key` if its Vary headers match `req`.
    pub(crate) fn lookup(&self, key: &str, req: &Request, defaults: DefaultHeader) -> Option<Meta> {
        let mut idx = self.lock();
        idx.clock += 1;
        let clock = idx.clock;
        let e = idx.entries.get_mut(key)?;
        let matches = e.meta.vary.iter().all(|(name, stored)| {
            let now = req.headers.get(name).map(str::to_string).or_else(|| defaults(name));
            now.as_deref().map(str::trim) == stored.as_deref().map(str::trim)
        });
        if !matches {
            return None;
        }
        e.meta.last_used = clock;
        Some(e.meta.clone())
    }

    /// The body of an entry (from memory or its file); `None` if it's gone or damaged.
    pub(crate) fn body(&self, key: &str) -> Option<Vec<u8>> {
        let (mem, len) = {
            let idx = self.lock();
            let e = idx.entries.get(key)?;
            (e.body.clone(), e.meta.body_len)
        };
        if let Some(b) = mem {
            return Some(b.as_ref().clone());
        }
        let path = self.file_for(key)?;
        let body = read_body(&path, len);
        if body.is_none() {
            self.remove(key);
        }
        body
    }

    /// Store a response (headers without `Set-Cookie`).
    pub(crate) fn store(
        &self,
        key: &str,
        req: &Request,
        resp: &Response,
        request_time: i64,
        response_time: i64,
        defaults: DefaultHeader,
    ) {
        let len = resp.body.len() as u64;
        if len > self.max_bytes / 8 {
            self.remove(key);
            return;
        }
        let mut headers = Headers::new();
        for (k, v) in resp.headers.iter() {
            if !k.eq_ignore_ascii_case("set-cookie") && !k.eq_ignore_ascii_case("set-cookie2") {
                headers.append(k, v);
            }
        }
        let mut vary = Vec::new();
        for v in resp.headers.get_all("vary") {
            for f in v.split(',').map(|f| f.trim().to_ascii_lowercase()).filter(|f| !f.is_empty()) {
                let val = req.headers.get(&f).map(str::to_string).or_else(|| defaults(&f));
                vary.push((f, val));
            }
        }
        let meta = Meta {
            url: resp.url.href(),
            status: resp.status,
            status_text: resp.status_text.clone(),
            headers,
            vary,
            request_time,
            response_time,
            secure: resp.secure,
            body_len: len,
            last_used: 0,
        };
        self.put(key, meta, &resp.body);
    }

    fn put(&self, key: &str, mut meta: Meta, body: &[u8]) {
        let mut mem = None;
        if let Some(path) = self.file_for(key) {
            if !write_entry(&path, key, &meta, body) {
                self.remove(key);
                return;
            }
        } else {
            mem = Some(Arc::new(body.to_vec()));
        }
        let mut idx = self.lock();
        if let Some(old) = idx.entries.remove(key) {
            idx.total = idx.total.saturating_sub(old.meta.body_len);
        }
        idx.clock += 1;
        meta.last_used = idx.clock;
        idx.total += meta.body_len;
        idx.entries.insert(key.to_string(), Entry { meta, body: mem });
        self.evict(&mut idx);
    }

    /// §4.3.4 freshen a stored response with a 304's headers; returns the updated metadata.
    pub(crate) fn freshen(
        &self,
        key: &str,
        not_modified: &Headers,
        request_time: i64,
        response_time: i64,
    ) -> Option<Meta> {
        let (mut meta, mem) = {
            let idx = self.lock();
            let e = idx.entries.get(key)?;
            (e.meta.clone(), e.body.clone())
        };
        const KEEP: &[&str] = &[
            "content-length",
            "content-encoding",
            "transfer-encoding",
            "connection",
            "keep-alive",
            "proxy-connection",
            "te",
            "upgrade",
            "set-cookie",
            "set-cookie2",
        ];
        let mut names: Vec<String> = Vec::new();
        for (k, _) in not_modified.iter() {
            let k = k.to_ascii_lowercase();
            if !KEEP.contains(&k.as_str()) && !names.contains(&k) {
                names.push(k);
            }
        }
        for n in &names {
            meta.headers.remove(n);
            for v in not_modified.get_all(n) {
                meta.headers.append(n, v);
            }
        }
        meta.request_time = request_time;
        meta.response_time = response_time;
        let body = match mem {
            Some(b) => b.as_ref().clone(),
            None => self.body(key)?,
        };
        self.put(key, meta.clone(), &body);
        Some(meta)
    }
}

fn read_meta(path: &Path) -> Option<(String, Meta)> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut magic = [0u8; 8];
    f.read_exact(&mut magic).ok()?;
    if &magic != MAGIC {
        return None;
    }
    let mut len = [0u8; 4];
    f.read_exact(&mut len).ok()?;
    let len = u32::from_le_bytes(len) as usize;
    if len > 4 * 1024 * 1024 {
        return None;
    }
    let mut b = vec![0u8; len];
    f.read_exact(&mut b).ok()?;
    let (key, meta) = decode_meta(&b)?;
    // The file must hold the whole body.
    let size = f.metadata().ok()?.len();
    (size == 12 + len as u64 + meta.body_len).then_some((key, meta))
}

fn read_body(path: &Path, len: u64) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    if &head[..8] != MAGIC {
        return None;
    }
    let mlen = u32::from_le_bytes(head[8..12].try_into().ok()?) as u64;
    if f.metadata().ok()?.len() != 12 + mlen + len {
        return None;
    }
    std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(12 + mlen)).ok()?;
    let mut body = Vec::with_capacity(len as usize);
    f.read_to_end(&mut body).ok()?;
    (body.len() as u64 == len).then_some(body)
}

/// Write an entry file atomically (a temporary file renamed into place).
fn write_entry(path: &Path, key: &str, meta: &Meta, body: &[u8]) -> bool {
    let Some(dir) = path.parent() else { return false };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let head = encode_meta(key, meta);
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let ok = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(MAGIC)?;
        f.write_all(&(head.len() as u32).to_le_bytes())?;
        f.write_all(&head)?;
        f.write_all(body)?;
        f.flush()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if ok.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

/// The response built from a stored entry.
pub(crate) fn response_from(
    meta: &Meta,
    body: Vec<u8>,
    req: &Request,
    now: i64,
    status: CacheStatus,
) -> Response {
    let mut headers = meta.headers.clone();
    headers.set("Age", &meta.current_age(now).to_string());
    Response {
        url: req.url.clone(),
        url_list: vec![req.url.clone()],
        status: meta.status,
        status_text: meta.status_text.clone(),
        headers,
        body,
        secure: meta.secure,
        cache: status,
        complete: true,
    }
}

/// What the cache layer decides for a GET.
pub(crate) enum Plan {
    /// Use the stored response.
    Use(Meta),
    /// Revalidate it with these conditional headers.
    Validate(Vec<(&'static str, String)>),
    /// Go to the network.
    Network,
}

pub(crate) fn plan(
    cache: &HttpCache,
    key: &str,
    req: &Request,
    mode: CacheMode,
    now: i64,
    defaults: DefaultHeader,
) -> Plan {
    if matches!(mode, CacheMode::NoStore | CacheMode::Reload) {
        return Plan::Network;
    }
    let Some(meta) = cache.lookup(key, req, defaults) else { return Plan::Network };
    let rc = directives(&req.headers);
    let request_no_cache = rc.contains_key("no-cache")
        || req.headers.get("pragma").is_some_and(|p| p.to_ascii_lowercase().contains("no-cache"))
        || rc.get("max-age").and_then(delta_seconds).is_some_and(|a| meta.current_age(now) > a);
    if mode == CacheMode::ForceCache {
        return Plan::Use(meta);
    }
    if mode == CacheMode::Default && !request_no_cache && meta.usable_fresh(now) {
        return Plan::Use(meta);
    }
    if !meta.has_validator() {
        return Plan::Network;
    }
    let mut cond = Vec::new();
    if let Some(e) = meta.headers.get("etag") {
        cond.push(("If-None-Match", e.to_string()));
    }
    if let Some(lm) = meta.headers.get("last-modified") {
        cond.push(("If-Modified-Since", lm.to_string()));
    }
    Plan::Validate(cond)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(headers: &[(&str, &str)], status: u16, request_time: i64, response_time: i64) -> Meta {
        let mut h = Headers::new();
        for (k, v) in headers {
            h.append(k, v);
        }
        Meta {
            url: "http://a/".into(),
            status,
            status_text: String::new(),
            headers: h,
            vary: Vec::new(),
            request_time,
            response_time,
            secure: false,
            body_len: 0,
            last_used: 0,
        }
    }

    const T: i64 = 1_800_000_000;

    fn date(t: i64) -> String {
        // IMF-fixdate for a Unix time (only what the tests need).
        let days = t.div_euclid(86_400) + 719_468;
        let era = days.div_euclid(146_097);
        let doe = days.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        let secs = t.rem_euclid(86_400);
        let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        format!(
            "Mon, {d:02} {} {y} {:02}:{:02}:{:02} GMT",
            months[(m - 1) as usize],
            secs / 3600,
            secs % 3600 / 60,
            secs % 60
        )
    }

    #[test]
    fn freshness_lifetime_precedence() {
        let d = date(T);
        assert_eq!(
            meta(&[("Cache-Control", "max-age=60"), ("Expires", &date(T + 999))], 200, T, T)
                .freshness_lifetime(),
            60
        );
        assert_eq!(meta(&[("Date", &d), ("Expires", &date(T + 300))], 200, T, T).freshness_lifetime(), 300);
        assert_eq!(meta(&[("Date", &d), ("Expires", "0")], 200, T, T).freshness_lifetime(), 0);
        // Heuristic: 10 % of the time since Last-Modified, capped at a week.
        assert_eq!(
            meta(&[("Date", &d), ("Last-Modified", &date(T - 1000))], 200, T, T).freshness_lifetime(),
            100
        );
        assert_eq!(
            meta(&[("Date", &d), ("Last-Modified", &date(T - 100 * 86_400))], 200, T, T).freshness_lifetime(),
            MAX_HEURISTIC
        );
        // Not heuristically cacheable: no lifetime without explicit freshness.
        assert_eq!(
            meta(&[("Date", &d), ("Last-Modified", &date(T - 1000))], 302, T, T).freshness_lifetime(),
            0
        );
        assert_eq!(meta(&[("Cache-Control", "max-age=abc")], 200, T, T).freshness_lifetime(), 0);
        assert_eq!(meta(&[], 301, T, T).freshness_lifetime(), 86_400);
    }

    #[test]
    fn age_calculation() {
        // Date 10 s before the response arrived, Age 5, request took 2 s, resident 20 s.
        let m = meta(&[("Date", &date(T - 10)), ("Age", "5")], 200, T - 2, T);
        // apparent 10, corrected age value 5 + 2 = 7 → initial 10, + 20 resident.
        assert_eq!(m.current_age(T + 20), 30);
        let fresh = meta(&[("Date", &date(T)), ("Cache-Control", "max-age=60")], 200, T, T);
        assert!(fresh.usable_fresh(T + 59));
        assert!(!fresh.usable_fresh(T + 60));
        let nc = meta(&[("Cache-Control", "max-age=60, no-cache")], 200, T, T);
        assert!(!nc.usable_fresh(T));
    }

    #[test]
    fn directives_parse() {
        let mut h = Headers::new();
        h.append("Cache-Control", "Max-Age=\"10\", no-cache");
        h.append("Cache-Control", "private");
        let d = directives(&h);
        assert_eq!(d.get("max-age"), Some(&Some("10".to_string())));
        assert!(d.contains_key("no-cache") && d.contains_key("private"));
    }

    #[test]
    fn entry_format_roundtrip_and_damage() {
        let m = meta(&[("ETag", "\"x\""), ("Content-Type", "text/plain")], 200, T, T + 1);
        let enc = encode_meta("k", &m);
        let (k, d) = decode_meta(&enc).unwrap();
        assert_eq!(k, "k");
        assert_eq!(d.headers.get("etag"), Some("\"x\""));
        assert_eq!(d.response_time, T + 1);
        for cut in 0..enc.len() {
            let _ = decode_meta(&enc[..cut]);
        }
    }
}

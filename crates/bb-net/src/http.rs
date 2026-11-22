//! HTTP/1.1 (RFC 9112) over TCP or TLS, with a keep-alive connection pool.

use crate::{Client, Headers, NetError, NetErrorKind, NetResult, Request, Response};
use rustls::pki_types::ServerName;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// Upper bound on decoded bodies (protects against decompression bombs).
const MAX_BODY: usize = 512 * 1024 * 1024;
const MAX_HEAD: usize = 256 * 1024;
/// Poll interval for cancellation / deadline checks while blocked on reads.
const POLL: Duration = Duration::from_millis(200);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(t) => &t.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(t) => match t.read(buf) {
                // Servers that close without close_notify: treat as EOF.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
                r => r,
            },
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(t) => t.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(t) => t.flush(),
        }
    }
}

/// Idle keep-alive connections keyed by `scheme://host:port[|proxy]`.
#[derive(Default)]
pub(crate) struct Pool {
    idle: HashMap<String, Vec<(Stream, Instant)>>,
}

impl Pool {
    fn take(&mut self, key: &str) -> Option<Stream> {
        let list = self.idle.get_mut(key)?;
        while let Some((s, t)) = list.pop() {
            if t.elapsed() < IDLE_TIMEOUT {
                return Some(s);
            }
        }
        None
    }

    fn put(&mut self, key: String, s: Stream) {
        let list = self.idle.entry(key).or_default();
        if list.len() < 6 {
            list.push((s, Instant::now()));
        }
    }
}

/// A buffered reader that checks a deadline and cancellation while waiting.
struct Reader<'a> {
    stream: &'a mut Stream,
    buf: Vec<u8>,
    pos: usize,
    deadline: Instant,
    req: &'a Request,
    /// Bytes read from the socket (for "did we get anything" decisions).
    total: usize,
}

impl<'a> Reader<'a> {
    fn fill(&mut self) -> NetResult<usize> {
        if self.pos > 0 && self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
        let mut tmp = [0u8; 16 * 1024];
        loop {
            if self.req.cancel.is_cancelled() {
                return Err(NetError::new(NetErrorKind::Cancelled, "cancelled"));
            }
            if Instant::now() > self.deadline {
                return Err(NetError::new(NetErrorKind::Timeout, "the server took too long to respond"));
            }
            match self.stream.read(&mut tmp) {
                Ok(n) => {
                    self.buf.extend_from_slice(&tmp[..n]);
                    self.total += n;
                    return Ok(n);
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(io_err(e)),
            }
        }
    }

    fn available(&self) -> &[u8] {
        &self.buf[self.pos..]
    }

    /// Read a line terminated by LF (CR stripped). `None` at EOF with no data.
    fn read_line(&mut self, limit: usize) -> NetResult<Option<Vec<u8>>> {
        loop {
            if let Some(i) = self.available().iter().position(|&b| b == b'\n') {
                let mut line = self.buf[self.pos..self.pos + i].to_vec();
                self.pos += i + 1;
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(line));
            }
            if self.available().len() > limit {
                return Err(NetError::new(NetErrorKind::Protocol, "header line too long"));
            }
            if self.fill()? == 0 {
                if self.available().is_empty() {
                    return Ok(None);
                }
                let line = self.buf[self.pos..].to_vec();
                self.pos = self.buf.len();
                return Ok(Some(line));
            }
        }
    }

    /// Read up to `n` bytes into `out`; returns false if EOF came first.
    fn read_exact_into(
        &mut self,
        mut n: usize,
        out: &mut Vec<u8>,
        progress: &mut dyn FnMut(usize),
    ) -> NetResult<bool> {
        while n > 0 {
            if self.available().is_empty() && self.fill()? == 0 {
                return Ok(false);
            }
            let take = n.min(self.available().len());
            out.extend_from_slice(&self.buf[self.pos..self.pos + take]);
            self.pos += take;
            n -= take;
            progress(out.len());
            if out.len() > MAX_BODY {
                return Err(NetError::new(NetErrorKind::Protocol, "response too large"));
            }
        }
        Ok(true)
    }

    fn read_to_end_into(&mut self, out: &mut Vec<u8>, progress: &mut dyn FnMut(usize)) -> NetResult<()> {
        loop {
            out.extend_from_slice(self.available());
            self.pos = self.buf.len();
            progress(out.len());
            if out.len() > MAX_BODY {
                return Err(NetError::new(NetErrorKind::Protocol, "response too large"));
            }
            if self.fill()? == 0 {
                return Ok(());
            }
        }
    }
}

fn io_err(e: io::Error) -> NetError {
    let kind = match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => NetErrorKind::Timeout,
        io::ErrorKind::ConnectionRefused => NetErrorKind::Connect,
        _ => NetErrorKind::Io,
    };
    // rustls certificate errors surface as InvalidData wrapping rustls::Error.
    if let Some(inner) = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) {
        return tls_err(inner.clone());
    }
    NetError::new(kind, e.to_string())
}

fn tls_err(e: rustls::Error) -> NetError {
    match e {
        rustls::Error::InvalidCertificate(c) => {
            NetError::new(NetErrorKind::Certificate, format!("certificate not trusted: {c:?}"))
        }
        other => NetError::new(NetErrorKind::Tls, format!("TLS error: {other}")),
    }
}

fn connect_tcp(host: &str, port: u16, deadline: Instant) -> NetResult<TcpStream> {
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| NetError::new(NetErrorKind::Dns, format!("could not resolve {host}: {e}")))?
        .collect();
    if addrs.is_empty() {
        return Err(NetError::new(NetErrorKind::Dns, format!("no addresses for {host}")));
    }
    let mut last = None;
    for addr in addrs {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(&addr, remaining.min(Duration::from_secs(15))) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                let _ = s.set_read_timeout(Some(POLL));
                let _ = s.set_write_timeout(Some(Duration::from_secs(30)));
                return Ok(s);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) if e.kind() == io::ErrorKind::TimedOut => {
            NetError::new(NetErrorKind::Timeout, format!("connection to {host} timed out"))
        }
        Some(e) => NetError::new(NetErrorKind::Connect, format!("could not connect to {host}:{port}: {e}")),
        None => NetError::new(NetErrorKind::Timeout, format!("connection to {host} timed out")),
    })
}

/// Read a response head: status line + headers.
fn read_head(r: &mut Reader) -> NetResult<Option<(String, u16, String, Headers)>> {
    let mut line = match r.read_line(MAX_HEAD)? {
        Some(l) => l,
        None => return Ok(None),
    };
    // Tolerate stray blank lines before the status line.
    while line.is_empty() {
        line = match r.read_line(MAX_HEAD)? {
            Some(l) => l,
            None => return Ok(None),
        };
    }
    let status_line = String::from_utf8_lossy(&line).into_owned();
    if !status_line.starts_with("HTTP/") {
        return Err(NetError::new(NetErrorKind::Protocol, "HTTP/0.9-style response"));
    }
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or("HTTP/1.0").to_string();
    let status: u16 = parts
        .next()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| NetError::new(NetErrorKind::Protocol, format!("bad status line {status_line:?}")))?;
    let reason = parts.next().unwrap_or("").trim().to_string();
    let mut headers = Headers::new();
    let mut size = 0;
    while let Some(l) = r.read_line(MAX_HEAD)? {
        if l.is_empty() {
            break;
        }
        size += l.len();
        if size > MAX_HEAD {
            return Err(NetError::new(NetErrorKind::Protocol, "response headers too large"));
        }
        let text = String::from_utf8_lossy(&l).into_owned();
        // obs-fold: continuation line appends to the previous header.
        if text.starts_with(' ') || text.starts_with('\t') {
            if let Some((k, v)) = headers.iter().last().map(|(k, v)| (k.to_string(), v.to_string())) {
                headers.remove(&k);
                headers.append(&k, &format!("{v} {}", text.trim()));
            }
            continue;
        }
        if let Some((k, v)) = text.split_once(':') {
            let k = k.trim();
            if !k.is_empty() && !k.contains(' ') {
                headers.append(k, v.trim());
            }
        }
    }
    Ok(Some((version, status, reason, headers)))
}

fn decode_chunked(r: &mut Reader, out: &mut Vec<u8>, progress: &mut dyn FnMut(usize)) -> NetResult<bool> {
    loop {
        let Some(line) = r.read_line(4096)? else { return Ok(false) };
        let text = String::from_utf8_lossy(&line);
        let size_str = text.split(';').next().unwrap_or("").trim();
        if size_str.is_empty() {
            continue; // tolerate stray CRLF between chunks
        }
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| NetError::new(NetErrorKind::Protocol, format!("bad chunk size {size_str:?}")))?;
        if size == 0 {
            // Trailer section.
            while let Some(l) = r.read_line(MAX_HEAD)? {
                if l.is_empty() {
                    break;
                }
            }
            return Ok(true);
        }
        if !r.read_exact_into(size, out, progress)? {
            return Ok(false);
        }
        // CRLF after chunk data (read lazily by the next read_line).
        if r.available().is_empty() {
            r.fill()?;
        }
        if r.available().starts_with(b"\r\n") {
            r.pos += 2;
        } else if r.available().starts_with(b"\n") {
            r.pos += 1;
        }
    }
}

/// Remove content codings (RFC 9110 §8.4.1), innermost last.
pub(crate) fn decode_content(headers: &Headers, body: Vec<u8>) -> Vec<u8> {
    let Some(ce) = headers.get_combined("content-encoding") else { return body };
    let codings: Vec<String> = ce
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty() && s != "identity")
        .collect();
    let mut data = body;
    for coding in codings.iter().rev() {
        let decoded = match coding.as_str() {
            "gzip" | "x-gzip" => read_limited(flate2::read::MultiGzDecoder::new(&data[..])),
            "deflate" => read_limited(flate2::read::ZlibDecoder::new(&data[..]))
                .or_else(|_| read_limited(flate2::read::DeflateDecoder::new(&data[..]))),
            "br" => read_limited(brotli_decompressor::Decompressor::new(&data[..], 4096)),
            other => {
                log::warn!("unsupported content-encoding {other}");
                return data;
            }
        };
        match decoded {
            Ok(d) => data = d,
            Err(partial) if !partial.is_empty() => {
                log::warn!("content-encoding {coding}: truncated or corrupt stream");
                data = partial
            }
            Err(_) => {
                log::warn!("content-encoding {coding}: could not decode; using raw bytes");
                return data;
            }
        }
    }
    data
}

/// Read a decoder fully; on error return what was decoded so far.
fn read_limited<R: Read>(mut r: R) -> Result<Vec<u8>, Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 32 * 1024];
    loop {
        match r.read(&mut buf) {
            Ok(0) => return Ok(out),
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > MAX_BODY {
                    return Err(out);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(out),
        }
    }
}

fn open_connection(client: &Client, req: &Request, deadline: Instant) -> NetResult<(Stream, bool)> {
    let url = &req.url;
    let host = url.host().ok_or_else(|| NetError::new(NetErrorKind::InvalidUrl, "URL has no host"))?;
    let host_name = host.connect_name();
    let port = url.port_or_default().unwrap_or(80);
    let https = url.scheme() == "https";
    let proxy = client.proxies.proxy_for(url);
    let mut via_http_proxy = false;
    let tcp = match proxy {
        Some(p) if https => {
            let mut s = connect_tcp(&p.host, p.port, deadline)?;
            let authority = format!("{}:{port}", host.connect_name_for_authority());
            let mut msg = format!(
                "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nUser-Agent: {}\r\n",
                client.config.user_agent()
            );
            if let Some(a) = &p.authorization {
                msg.push_str(&format!("Proxy-Authorization: {a}\r\n"));
            }
            msg.push_str("\r\n");
            s.write_all(msg.as_bytes()).map_err(io_err)?;
            let mut stream = Stream::Plain(s);
            let mut r = Reader { stream: &mut stream, buf: Vec::new(), pos: 0, deadline, req, total: 0 };
            let head = read_head(&mut r)?;
            match head {
                Some((_, 200, _, _)) => {}
                Some((_, status, reason, _)) => {
                    let kind = if status == 403 || status == 407 {
                        NetErrorKind::Blocked
                    } else {
                        NetErrorKind::Connect
                    };
                    return Err(NetError::new(
                        kind,
                        format!("proxy refused tunnel to {authority}: {status} {reason}"),
                    ));
                }
                None => return Err(NetError::new(NetErrorKind::Connect, "proxy closed the connection")),
            }
            match stream {
                Stream::Plain(s) => s,
                Stream::Tls(_) => unreachable!("proxy stream is plain"),
            }
        }
        Some(p) => {
            via_http_proxy = true;
            connect_tcp(&p.host, p.port, deadline)?
        }
        None => connect_tcp(&host_name, port, deadline)?,
    };
    if !https {
        return Ok((Stream::Plain(tcp), via_http_proxy));
    }
    let cfg = client.tls.config.clone().ok_or_else(|| NetError::new(NetErrorKind::Tls, "TLS unavailable"))?;
    let server_name = ServerName::try_from(host_name.clone())
        .map_err(|_| NetError::new(NetErrorKind::InvalidUrl, format!("bad TLS server name {host_name}")))?;
    let conn = rustls::ClientConnection::new(cfg, server_name).map_err(tls_err)?;
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    // Drive the handshake now so certificate errors are reported precisely.
    while tls.conn.is_handshaking() {
        if Instant::now() > deadline {
            return Err(NetError::new(NetErrorKind::Timeout, "TLS handshake timed out"));
        }
        match tls.conn.complete_io(&mut tls.sock) {
            Ok(_) => {}
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
            Err(e) => {
                let mut err = io_err(e);
                if err.kind == NetErrorKind::Certificate {
                    // The verifier recorded which certificate failed and why.
                    if let Some(p) = client.tls.take_failure(&host_name) {
                        err.message = format!("{} ({})", p.reason, p.code);
                        err.certificate = Some(Box::new(p));
                    }
                }
                return Err(err);
            }
        }
    }
    Ok((Stream::Tls(Box::new(tls)), false))
}

trait AuthorityName {
    fn connect_name_for_authority(&self) -> String;
}

impl AuthorityName for bb_url::Host {
    fn connect_name_for_authority(&self) -> String {
        match self {
            bb_url::Host::Ipv6(_) => self.serialize(),
            other => other.connect_name(),
        }
    }
}

fn build_request(client: &Client, req: &Request, absolute_form: bool) -> Vec<u8> {
    let url = &req.url;
    let target = if absolute_form { url.href_without_fragment() } else { url.request_target() };
    let mut s = format!("{} {} HTTP/1.1\r\n", req.method, target);
    let mut push = |k: &str, v: &str| {
        s.push_str(k);
        s.push_str(": ");
        s.push_str(v);
        s.push_str("\r\n");
    };
    push("Host", &url.host_with_port());
    if !req.headers.contains("user-agent") {
        push("User-Agent", client.config.user_agent());
    }
    for (k, v) in DEFAULT_HEADERS {
        if !req.headers.contains(k) {
            push(k, v);
        }
    }
    if client.config.do_not_track {
        push("DNT", "1");
        push("Sec-GPC", "1");
    }
    for (k, v) in req.headers.iter() {
        if k.eq_ignore_ascii_case("host") || k.eq_ignore_ascii_case("content-length") {
            continue;
        }
        push(k, v);
    }
    if let Some(b) = &req.body {
        push("Content-Length", &b.len().to_string());
    } else if req.method == "POST" || req.method == "PUT" {
        push("Content-Length", "0");
    }
    push("Connection", "keep-alive");
    s.push_str("\r\n");
    let mut bytes = s.into_bytes();
    if let Some(b) = &req.body {
        bytes.extend_from_slice(b);
    }
    bytes
}

/// Request headers sent when the request doesn't set them (besides `User-Agent`).
pub(crate) const DEFAULT_HEADERS: [(&str, &str); 3] = [
    (
        "Accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,image/png,image/*;q=0.8,*/*;q=0.5",
    ),
    ("Accept-Language", "en-US,en;q=0.5"),
    ("Accept-Encoding", "gzip, deflate, br"),
];

/// One HTTP exchange (no redirect handling).
pub(crate) fn fetch(client: &Client, req: &Request, deadline: Instant) -> NetResult<Response> {
    let proxy_key =
        client.proxies.proxy_for(&req.url).map(|p| format!("|{}:{}", p.host, p.port)).unwrap_or_default();
    let key = format!("{}://{}{proxy_key}", req.url.scheme(), req.url.host_with_port());
    // Try a pooled connection first; on early failure retry with a fresh one.
    let pooled = client.pool.lock().ok().and_then(|mut p| p.take(&key));
    if let Some(stream) = pooled {
        let abs = !proxy_key.is_empty() && req.url.scheme() == "http";
        match exchange(client, req, deadline, stream, abs, &key) {
            Ok(r) => return Ok(r),
            Err((e, got_bytes)) => {
                if got_bytes || matches!(e.kind, NetErrorKind::Cancelled | NetErrorKind::Timeout) {
                    return Err(e);
                }
                log::debug!("stale pooled connection ({e}); reconnecting");
            }
        }
    }
    let (stream, abs) = open_connection(client, req, deadline)?;
    exchange(client, req, deadline, stream, abs, &key).map_err(|(e, _)| e)
}

fn exchange(
    client: &Client,
    req: &Request,
    deadline: Instant,
    mut stream: Stream,
    absolute_form: bool,
    key: &str,
) -> Result<Response, (NetError, bool)> {
    let secure = matches!(stream, Stream::Tls(_));
    let bytes = build_request(client, req, absolute_form);
    stream.write_all(&bytes).and_then(|_| stream.flush()).map_err(|e| (io_err(e), false))?;
    let _ = stream.tcp().set_read_timeout(Some(POLL));
    let mut r = Reader { stream: &mut stream, buf: Vec::new(), pos: 0, deadline, req, total: 0 };
    // Skip 1xx interim responses.
    let (version, status, reason, headers) = loop {
        let head = read_head(&mut r).map_err(|e| {
            let got = r.total > 0;
            (e, got)
        })?;
        match head {
            None => {
                let got = r.total > 0;
                return Err((
                    NetError::new(NetErrorKind::Protocol, "server closed connection without a response"),
                    got,
                ));
            }
            Some((_, s, _, _)) if (100..200).contains(&s) && s != 101 => continue,
            Some(h) => break h,
        }
    };
    let total_len: Option<u64> = headers.get("content-length").and_then(|v| v.trim().parse().ok());
    let progress_cb = req.progress.clone();
    let mut progress = |n: usize| {
        if let Some(p) = &progress_cb {
            p(n as u64, total_len);
        }
    };
    let mut body = Vec::new();
    let no_body = req.method == "HEAD" || (100..200).contains(&status) || status == 204 || status == 304;
    let chunked = headers.get_combined("transfer-encoding").is_some_and(|te| {
        te.to_ascii_lowercase().split(',').next_back().is_some_and(|c| c.trim() == "chunked")
    });
    let mut reusable = if version == "HTTP/1.0" {
        headers.has_token("connection", "keep-alive")
    } else {
        !headers.has_token("connection", "close")
    };
    let complete = if no_body {
        true
    } else if chunked {
        decode_chunked(&mut r, &mut body, &mut progress).map_err(|e| (e, true))?
    } else if let Some(len) = total_len {
        r.read_exact_into(len as usize, &mut body, &mut progress).map_err(|e| (e, true))?
    } else {
        reusable = false;
        r.read_to_end_into(&mut body, &mut progress).map_err(|e| (e, true))?;
        true
    };
    let leftover = !r.available().is_empty();
    drop(r);
    if !complete {
        log::warn!("{}: body truncated", req.url);
    }
    if complete && reusable && !leftover {
        if let Ok(mut pool) = client.pool.lock() {
            pool.put(key.to_string(), stream);
        }
    }
    let body = decode_content(&headers, body);
    Ok(Response {
        url: req.url.clone(),
        url_list: vec![req.url.clone()],
        status,
        status_text: reason,
        headers,
        body,
        secure,
        cache: crate::cache::CacheStatus::Network,
        complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_and_deflate_roundtrip() {
        let text = b"hello hello hello hello".repeat(50);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&text).unwrap();
        let gz = gz.finish().unwrap();
        let mut h = Headers::new();
        h.set("Content-Encoding", "gzip");
        assert_eq!(decode_content(&h, gz), text);

        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&text).unwrap();
        h.set("Content-Encoding", "deflate");
        assert_eq!(decode_content(&h, z.finish().unwrap()), text);

        let mut raw = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        raw.write_all(&text).unwrap();
        assert_eq!(decode_content(&h, raw.finish().unwrap()), text);
    }

    #[test]
    fn corrupt_encoding_does_not_panic() {
        let mut h = Headers::new();
        for ce in ["gzip", "deflate", "br", "gzip, br"] {
            h.set("Content-Encoding", ce);
            let _ = decode_content(&h, b"\x1f\x8b\x08garbage".to_vec());
            let _ = decode_content(&h, Vec::new());
        }
    }
}

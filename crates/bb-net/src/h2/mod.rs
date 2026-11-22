//! HTTP/2 (RFC 9113), negotiated with ALPN `h2` over TLS.
//!
//! One `Connection` multiplexes the requests of every thread fetching from an origin. There is
//! no I/O thread (leader/follower): one waiting thread at a time takes the socket out of the
//! shared state and reads (with a short timeout), then puts it back, hands the frames to their
//! streams and wakes the others; threads with frames to send go first, so a long download never
//! holds up another request's HEADERS.
//!
//! Implemented: the connection preface and SETTINGS exchange; HEADERS/CONTINUATION with HPACK;
//! DATA with padding; stream and connection flow control in both directions (§5.2, §6.9);
//! RST_STREAM, PING, GOAWAY (unprocessed streams are retried on a new connection),
//! SETTINGS_MAX_CONCURRENT_STREAMS, SETTINGS_INITIAL_WINDOW_SIZE changes, trailers, 1xx
//! responses; malformed-message checks (§8.1.1). Server push is disabled (ENABLE_PUSH = 0).
//! Not used: priorities (PRIORITY frames are ignored, RFC 9113 §5.3.2), extended CONNECT.

pub mod hpack;
mod tables;

use crate::http::Stream;
use crate::{Headers, NetError, NetErrorKind, NetResult, Request, Response};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The client connection preface (§3.4).
pub const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

pub const DATA: u8 = 0x0;
pub const HEADERS: u8 = 0x1;
pub const PRIORITY: u8 = 0x2;
pub const RST_STREAM: u8 = 0x3;
pub const SETTINGS: u8 = 0x4;
pub const PUSH_PROMISE: u8 = 0x5;
pub const PING: u8 = 0x6;
pub const GOAWAY: u8 = 0x7;
pub const WINDOW_UPDATE: u8 = 0x8;
pub const CONTINUATION: u8 = 0x9;

pub const END_STREAM: u8 = 0x1;
pub const ACK: u8 = 0x1;
pub const END_HEADERS: u8 = 0x4;
pub const PADDED: u8 = 0x8;
pub const PRIORITY_FLAG: u8 = 0x20;

// Error codes (§7).
pub const NO_ERROR: u32 = 0x0;
pub const PROTOCOL_ERROR: u32 = 0x1;
pub const FLOW_CONTROL_ERROR: u32 = 0x3;
pub const STREAM_CLOSED: u32 = 0x5;
pub const FRAME_SIZE_ERROR: u32 = 0x6;
pub const REFUSED_STREAM: u32 = 0x7;
pub const CANCEL: u32 = 0x8;
pub const COMPRESSION_ERROR: u32 = 0x9;

// Settings (§6.5.2).
const SETTINGS_HEADER_TABLE_SIZE: u16 = 0x1;
const SETTINGS_ENABLE_PUSH: u16 = 0x2;
const SETTINGS_MAX_CONCURRENT_STREAMS: u16 = 0x3;
const SETTINGS_INITIAL_WINDOW_SIZE: u16 = 0x4;
const SETTINGS_MAX_FRAME_SIZE: u16 = 0x5;
const SETTINGS_MAX_HEADER_LIST_SIZE: u16 = 0x6;

/// The largest frame we accept (the default; we don't raise it).
const OUR_MAX_FRAME: usize = 16_384;
/// Our per-stream receive window, and the connection window we grow to.
const OUR_STREAM_WINDOW: u32 = 4 * 1024 * 1024;
const OUR_CONN_WINDOW: u32 = 16 * 1024 * 1024;
const OUR_HEADER_TABLE: usize = 4096;
const OUR_MAX_HEADER_LIST: usize = 256 * 1024;
/// Upper bound on a response body (as for HTTP/1.1).
const MAX_BODY: usize = 512 * 1024 * 1024;
/// How long one thread reads before giving others a turn.
const READ_SLICE: Duration = Duration::from_millis(20);
const MAX_WINDOW: i64 = (1 << 31) - 1;

/// A frame header + payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub flags: u8,
    pub stream: u32,
    pub payload: Vec<u8>,
}

/// Serialize a frame (§4.1).
pub fn frame_bytes(kind: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let len = payload.len() as u32;
    let mut out = Vec::with_capacity(9 + payload.len());
    out.extend_from_slice(&len.to_be_bytes()[1..]);
    out.push(kind);
    out.push(flags);
    out.extend_from_slice(&(stream & 0x7fff_ffff).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Parse one frame from the start of `buf`: `Ok(None)` if more bytes are needed.
pub fn parse_frame(buf: &[u8], max_size: usize) -> Result<Option<(Frame, usize)>, u32> {
    if buf.len() < 9 {
        return Ok(None);
    }
    let len = (usize::from(buf[0]) << 16) | (usize::from(buf[1]) << 8) | usize::from(buf[2]);
    if len > max_size {
        return Err(FRAME_SIZE_ERROR);
    }
    if buf.len() < 9 + len {
        return Ok(None);
    }
    let stream = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) & 0x7fff_ffff;
    Ok(Some((Frame { kind: buf[3], flags: buf[4], stream, payload: buf[9..9 + len].to_vec() }, 9 + len)))
}

/// Strip PADDED padding (and the PRIORITY fields of HEADERS); `None` if malformed.
fn unpad(f: &Frame) -> Option<&[u8]> {
    let mut p = &f.payload[..];
    let mut pad = 0usize;
    if f.flags & PADDED != 0 {
        pad = usize::from(*p.first()?);
        p = &p[1..];
    }
    if f.kind == HEADERS && f.flags & PRIORITY_FLAG != 0 {
        p = p.get(5..)?;
    }
    if pad > p.len() {
        return None;
    }
    Some(&p[..p.len() - pad])
}

fn conn_err(msg: impl Into<String>) -> NetError {
    NetError::new(NetErrorKind::Protocol, format!("HTTP/2: {}", msg.into()))
}

/// Why a request failed, and whether it can safely be sent again on a new connection (the
/// server didn't process it: GOAWAY above its id, REFUSED_STREAM, or a dead idle connection).
pub(crate) struct Failure {
    pub error: NetError,
    pub retry: bool,
}

#[derive(Default)]
struct StreamState {
    status: Option<u16>,
    headers: Headers,
    body: Vec<u8>,
    done: bool,
    error: Option<(NetError, bool)>,
    send_window: i64,
    /// Request body bytes still to send, and whether END_STREAM has been sent.
    pending: Vec<u8>,
    end_sent: bool,
    /// Bytes received since our last WINDOW_UPDATE for this stream.
    unacked: u32,
    /// The method may be sent again if the server never answered (RFC 9110 §9.2.2).
    idempotent: bool,
}

struct State {
    /// The socket; `None` while a thread is reading from it.
    io: Option<Stream>,
    /// Threads waiting to write: they go before the next read.
    writers: usize,
    buf: Vec<u8>,
    decoder: hpack::Decoder,
    next_id: u32,
    streams: HashMap<u32, StreamState>,
    peer_max_streams: u32,
    peer_initial_window: i64,
    peer_max_frame: usize,
    conn_send_window: i64,
    conn_unacked: u32,
    /// A header block in progress: (stream, block so far, END_STREAM).
    continuation: Option<(u32, Vec<u8>, bool)>,
    /// GOAWAY received: streams above this id were not processed.
    goaway: Option<u32>,
    /// A connection error: nothing more can be done on it.
    broken: Option<NetError>,
    got_settings: bool,
    last_used: Instant,
}

/// An HTTP/2 client connection.
pub(crate) struct Connection {
    state: Mutex<State>,
    /// Signalled whenever the socket comes back or frames were handled.
    cv: Condvar,
    pub secure: bool,
}

impl Connection {
    /// Start HTTP/2 on a TLS stream that negotiated `h2`: send the preface and our SETTINGS.
    pub fn start(mut io: Stream) -> NetResult<Connection> {
        let mut out = PREFACE.to_vec();
        let mut settings = Vec::new();
        for (id, v) in [
            (SETTINGS_ENABLE_PUSH, 0u32),
            (SETTINGS_INITIAL_WINDOW_SIZE, OUR_STREAM_WINDOW),
            (SETTINGS_HEADER_TABLE_SIZE, OUR_HEADER_TABLE as u32),
            (SETTINGS_MAX_HEADER_LIST_SIZE, OUR_MAX_HEADER_LIST as u32),
        ] {
            settings.extend_from_slice(&id.to_be_bytes());
            settings.extend_from_slice(&v.to_be_bytes());
        }
        out.extend(frame_bytes(SETTINGS, 0, 0, &settings));
        out.extend(frame_bytes(WINDOW_UPDATE, 0, 0, &(OUR_CONN_WINDOW - 65_535).to_be_bytes()));
        io.write_all(&out).and_then(|_| io.flush()).map_err(crate::http::io_err)?;
        let _ = io.tcp().set_read_timeout(Some(READ_SLICE));
        Ok(Connection {
            cv: Condvar::new(),
            state: Mutex::new(State {
                io: Some(io),
                writers: 0,
                buf: Vec::new(),
                decoder: hpack::Decoder::new(OUR_HEADER_TABLE, OUR_MAX_HEADER_LIST),
                next_id: 1,
                streams: HashMap::new(),
                peer_max_streams: 100,
                peer_initial_window: 65_535,
                peer_max_frame: 16_384,
                conn_send_window: 65_535,
                conn_unacked: 0,
                continuation: None,
                goaway: None,
                broken: None,
                got_settings: false,
                last_used: Instant::now(),
            }),
            secure: true,
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    /// Whether new requests may use this connection.
    pub fn usable(&self, idle_timeout: Duration) -> bool {
        let s = self.lock();
        s.broken.is_none()
            && s.goaway.is_none()
            && s.next_id < 0x7fff_0000
            && (!s.streams.is_empty() || s.last_used.elapsed() < idle_timeout)
    }

    /// Wait until the socket is free (writers go before the next reader).
    fn wait_for_io<'a>(&'a self, mut s: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        s.writers += 1;
        while s.io.is_none() {
            s = match self.cv.wait(s) {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
        }
        s.writers -= 1;
        s
    }

    /// Take a turn reading if nobody else is (and nobody wants to write); otherwise wait for
    /// whoever is reading to hand out what arrived.
    fn read_turn<'a>(&'a self, mut s: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        if s.io.is_none() || s.writers > 0 || s.broken.is_some() {
            return match self.cv.wait_timeout(s, READ_SLICE * 3) {
                Ok((g, _)) => g,
                Err(p) => p.into_inner().0,
            };
        }
        let Some(mut io) = s.io.take() else { return s };
        drop(s);
        let mut tmp = vec![0u8; 32 * 1024];
        let r = io.read(&mut tmp);
        let mut s = self.lock();
        s.io = Some(io);
        match r {
            Ok(n) => {
                if let Err(e) = s.ingest(&tmp[..n]) {
                    s.fail_all(&e);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
                ) => {}
            Err(e) => {
                let e = crate::http::io_err(e);
                s.fail_all(&e);
            }
        }
        self.cv.notify_all();
        s
    }

    /// Say goodbye (GOAWAY NO_ERROR) before dropping an idle connection.
    pub fn close(&self) {
        let s = self.lock();
        let mut s = self.wait_for_io(s);
        let last = s.next_id.saturating_sub(2);
        let mut p = last.to_be_bytes().to_vec();
        p.extend_from_slice(&NO_ERROR.to_be_bytes());
        let _ = s.write(&frame_bytes(GOAWAY, 0, 0, &p));
        s.broken = Some(conn_err("closed"));
    }

    /// One request/response exchange.
    pub fn request(
        &self,
        req: &Request,
        fields: Vec<(String, String)>,
        deadline: Instant,
    ) -> Result<Response, Failure> {
        let fail = |error: NetError, retry: bool| Failure { error, retry };
        // Open a stream once the peer allows another. Ids must go out in increasing order, so
        // the id is taken and its HEADERS sent while holding the socket.
        let refs: Vec<(&str, &str)> = fields.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let block = hpack::encode(&refs);
        let end = req.body.as_ref().is_none_or(|b| b.is_empty());
        let id = loop {
            let mut s = self.lock();
            if s.broken.is_none()
                && s.goaway.is_none()
                && (s.streams.len() as u32) < s.peer_max_streams.max(1)
            {
                s = self.wait_for_io(s);
            }
            if let Some(e) = &s.broken {
                // Nothing was sent: safe to try another connection.
                return Err(fail(e.clone(), true));
            }
            if s.goaway.is_some() {
                return Err(fail(conn_err("the server is closing the connection"), true));
            }
            if s.io.is_some() && (s.streams.len() as u32) < s.peer_max_streams.max(1) {
                let id = s.next_id;
                s.next_id += 2;
                let window = s.peer_initial_window;
                s.streams.insert(
                    id,
                    StreamState {
                        send_window: window,
                        pending: req.body.clone().unwrap_or_default(),
                        idempotent: matches!(
                            req.method.as_str(),
                            "GET" | "HEAD" | "OPTIONS" | "TRACE" | "PUT" | "DELETE"
                        ),
                        end_sent: end,
                        ..Default::default()
                    },
                );
                if let Err(e) = s.send_headers(id, &block, end) {
                    s.streams.remove(&id);
                    s.fail_all(&e);
                    return Err(fail(e, true));
                }
                break id;
            }
            // Wait for a slot: read (which may complete other streams).
            let s = self.read_turn(s);
            drop(s);
            if let Some(e) = timeout_or_cancel(req, deadline) {
                return Err(fail(e, false));
            }
        };
        // Send the body (as flow control allows) and wait for the response.
        let mut s = self.lock();
        loop {
            if s.streams.get(&id).is_some_and(|st| !st.end_sent && st.error.is_none()) {
                s = self.wait_for_io(s);
                if let Err(e) = s.flush_body(id) {
                    s.fail_all(&e);
                }
            }
            if let Some(st) = s.streams.get(&id) {
                if let Some((e, retry)) = st.error.clone() {
                    s.streams.remove(&id);
                    return Err(fail(e, retry));
                }
                if st.done {
                    let st = s.streams.remove(&id).unwrap_or_default();
                    s.last_used = Instant::now();
                    return Ok(build_response(req, st, self.secure));
                }
            } else {
                return Err(fail(conn_err("stream lost"), false));
            }
            if let Some(e) = s.broken.clone() {
                s.streams.remove(&id);
                return Err(fail(e, false));
            }
            if let Some(e) = timeout_or_cancel(req, deadline) {
                s = self.wait_for_io(s);
                s.reset(id, CANCEL);
                s.streams.remove(&id);
                return Err(fail(e, false));
            }
            s = self.read_turn(s);
        }
    }
}

fn timeout_or_cancel(req: &Request, deadline: Instant) -> Option<NetError> {
    if req.cancel.is_cancelled() {
        return Some(NetError::new(NetErrorKind::Cancelled, "cancelled"));
    }
    if Instant::now() > deadline {
        return Some(NetError::new(NetErrorKind::Timeout, "the server took too long to respond"));
    }
    None
}

fn build_response(req: &Request, st: StreamState, secure: bool) -> Response {
    let body = crate::http::decode_content(&st.headers, st.body);
    Response {
        url: req.url.clone(),
        url_list: vec![req.url.clone()],
        status: st.status.unwrap_or(0),
        status_text: String::new(),
        headers: st.headers,
        body,
        secure,
        cache: crate::cache::CacheStatus::Network,
        complete: true,
        version: "HTTP/2",
    }
}

impl State {
    fn write(&mut self, bytes: &[u8]) -> NetResult<()> {
        let io = self.io.as_mut().ok_or_else(|| conn_err("socket busy"))?;
        io.write_all(bytes).and_then(|_| io.flush()).map_err(crate::http::io_err)
    }

    fn send_headers(&mut self, id: u32, block: &[u8], end_stream: bool) -> NetResult<()> {
        let max = self.peer_max_frame;
        let mut out = Vec::new();
        let mut chunks = block.chunks(max).peekable();
        let first = chunks.next().unwrap_or(&[]);
        let mut flags = if end_stream { END_STREAM } else { 0 };
        if chunks.peek().is_none() {
            flags |= END_HEADERS;
        }
        out.extend(frame_bytes(HEADERS, flags, id, first));
        while let Some(c) = chunks.next() {
            let f = if chunks.peek().is_none() { END_HEADERS } else { 0 };
            out.extend(frame_bytes(CONTINUATION, f, id, c));
        }
        self.write(&out)
    }

    fn reset(&mut self, id: u32, code: u32) {
        let _ = self.write(&frame_bytes(RST_STREAM, 0, id, &code.to_be_bytes()));
    }

    /// A connection error: tell the peer (GOAWAY) and fail every stream.
    fn connection_error(&mut self, code: u32, msg: &str) -> NetError {
        let mut p = 0u32.to_be_bytes().to_vec();
        p.extend_from_slice(&code.to_be_bytes());
        let _ = self.write(&frame_bytes(GOAWAY, 0, 0, &p));
        conn_err(msg)
    }

    fn fail_all(&mut self, e: &NetError) {
        if self.broken.is_none() {
            self.broken = Some(e.clone());
        }
        for st in self.streams.values_mut() {
            if !st.done && st.error.is_none() {
                st.error = Some((e.clone(), false));
            }
        }
    }

    /// Send as much of the stream's pending body as the windows allow.
    fn flush_body(&mut self, id: u32) -> NetResult<()> {
        let max = self.peer_max_frame as i64;
        loop {
            let conn_window = self.conn_send_window;
            let Some(st) = self.streams.get_mut(&id) else { return Ok(()) };
            if st.end_sent {
                return Ok(());
            }
            let n = (st.pending.len() as i64).min(st.send_window).min(conn_window).min(max).max(0) as usize;
            if n == 0 && !st.pending.is_empty() {
                return Ok(()); // wait for WINDOW_UPDATE
            }
            let chunk: Vec<u8> = st.pending.drain(..n).collect();
            let last = st.pending.is_empty();
            st.send_window -= n as i64;
            if last {
                st.end_sent = true;
            }
            self.conn_send_window -= n as i64;
            self.write(&frame_bytes(DATA, if last { END_STREAM } else { 0 }, id, &chunk))?;
            if last {
                return Ok(());
            }
        }
    }

    /// Handle bytes read from the socket (empty = the server closed the connection).
    fn ingest(&mut self, data: &[u8]) -> NetResult<()> {
        if self.broken.is_some() {
            return Ok(());
        }
        if data.is_empty() {
            // The server closed the connection: unfinished streams fail; ones it never answered
            // can be retried if their method allows it.
            let e = conn_err("the server closed the connection");
            self.broken = Some(e.clone());
            let goaway = self.goaway.is_some();
            for st in self.streams.values_mut() {
                if !st.done && st.error.is_none() {
                    let untouched = st.status.is_none() && st.body.is_empty();
                    st.error = Some((e.clone(), untouched && (goaway || st.idempotent)));
                }
            }
            return Ok(());
        }
        self.buf.extend_from_slice(data);
        loop {
            let parsed = match parse_frame(&self.buf, OUR_MAX_FRAME) {
                Ok(p) => p,
                Err(code) => return Err(self.connection_error(code, "frame too large")),
            };
            let Some((frame, used)) = parsed else { return Ok(()) };
            self.buf.drain(..used);
            self.handle(frame)?;
        }
    }

    fn handle(&mut self, f: Frame) -> NetResult<()> {
        if !self.got_settings && !(f.kind == SETTINGS && f.flags & ACK == 0) {
            return Err(self.connection_error(PROTOCOL_ERROR, "the server's first frame wasn't SETTINGS"));
        }
        if let Some((sid, _, _)) = &self.continuation {
            if f.kind != CONTINUATION || f.stream != *sid {
                return Err(self.connection_error(PROTOCOL_ERROR, "expected CONTINUATION"));
            }
        }
        match f.kind {
            DATA => self.on_data(f),
            HEADERS => {
                if f.stream == 0 {
                    return Err(self.connection_error(PROTOCOL_ERROR, "HEADERS on stream 0"));
                }
                let Some(block) = unpad(&f) else {
                    return Err(self.connection_error(PROTOCOL_ERROR, "bad HEADERS padding"));
                };
                let block = block.to_vec();
                let end_stream = f.flags & END_STREAM != 0;
                if f.flags & END_HEADERS != 0 {
                    self.on_header_block(f.stream, &block, end_stream)
                } else {
                    self.continuation = Some((f.stream, block, end_stream));
                    Ok(())
                }
            }
            CONTINUATION => {
                let Some((sid, mut block, end_stream)) = self.continuation.take() else {
                    return Err(self.connection_error(PROTOCOL_ERROR, "unexpected CONTINUATION"));
                };
                block.extend_from_slice(&f.payload);
                if block.len() > OUR_MAX_HEADER_LIST {
                    return Err(self.connection_error(PROTOCOL_ERROR, "header block too large"));
                }
                if f.flags & END_HEADERS != 0 {
                    self.on_header_block(sid, &block, end_stream)
                } else {
                    self.continuation = Some((sid, block, end_stream));
                    Ok(())
                }
            }
            PRIORITY => Ok(()),
            RST_STREAM => {
                if f.payload.len() != 4 {
                    return Err(self.connection_error(FRAME_SIZE_ERROR, "bad RST_STREAM"));
                }
                let code = u32::from_be_bytes([f.payload[0], f.payload[1], f.payload[2], f.payload[3]]);
                if let Some(st) = self.streams.get_mut(&f.stream) {
                    if !st.done {
                        let retry = code == REFUSED_STREAM;
                        st.error =
                            Some((conn_err(format!("stream reset by the server (error {code:#x})")), retry));
                    }
                }
                Ok(())
            }
            SETTINGS => self.on_settings(f),
            PUSH_PROMISE => Err(self.connection_error(PROTOCOL_ERROR, "PUSH_PROMISE with push disabled")),
            PING => {
                if f.payload.len() != 8 || f.stream != 0 {
                    return Err(self.connection_error(FRAME_SIZE_ERROR, "bad PING"));
                }
                if f.flags & ACK == 0 {
                    self.write(&frame_bytes(PING, ACK, 0, &f.payload))?;
                }
                Ok(())
            }
            GOAWAY => {
                if f.payload.len() < 8 || f.stream != 0 {
                    return Err(self.connection_error(FRAME_SIZE_ERROR, "bad GOAWAY"));
                }
                let last = u32::from_be_bytes([f.payload[0], f.payload[1], f.payload[2], f.payload[3]])
                    & 0x7fff_ffff;
                self.goaway = Some(last);
                for (&id, st) in self.streams.iter_mut() {
                    if id > last && !st.done {
                        st.error = Some((conn_err("the server is closing the connection"), true));
                    }
                }
                Ok(())
            }
            WINDOW_UPDATE => {
                if f.payload.len() != 4 {
                    return Err(self.connection_error(FRAME_SIZE_ERROR, "bad WINDOW_UPDATE"));
                }
                let inc = i64::from(
                    u32::from_be_bytes([f.payload[0], f.payload[1], f.payload[2], f.payload[3]])
                        & 0x7fff_ffff,
                );
                if f.stream == 0 {
                    if inc == 0 {
                        return Err(self.connection_error(PROTOCOL_ERROR, "zero WINDOW_UPDATE"));
                    }
                    self.conn_send_window += inc;
                    if self.conn_send_window > MAX_WINDOW {
                        return Err(self.connection_error(FLOW_CONTROL_ERROR, "window overflow"));
                    }
                } else if let Some(st) = self.streams.get_mut(&f.stream) {
                    st.send_window += inc;
                    if inc == 0 || st.send_window > MAX_WINDOW {
                        st.error = Some((conn_err("bad stream WINDOW_UPDATE"), false));
                        let code = if inc == 0 { PROTOCOL_ERROR } else { FLOW_CONTROL_ERROR };
                        self.reset(f.stream, code);
                    }
                }
                Ok(())
            }
            // §4.1: unknown frame types are ignored.
            _ => Ok(()),
        }
    }

    fn on_settings(&mut self, f: Frame) -> NetResult<()> {
        if f.stream != 0 {
            return Err(self.connection_error(PROTOCOL_ERROR, "SETTINGS on a stream"));
        }
        if f.flags & ACK != 0 {
            if !f.payload.is_empty() {
                return Err(self.connection_error(FRAME_SIZE_ERROR, "SETTINGS ACK with a payload"));
            }
            return Ok(());
        }
        if f.payload.len() % 6 != 0 {
            return Err(self.connection_error(FRAME_SIZE_ERROR, "bad SETTINGS length"));
        }
        for c in f.payload.chunks(6) {
            let id = u16::from_be_bytes([c[0], c[1]]);
            let v = u32::from_be_bytes([c[2], c[3], c[4], c[5]]);
            match id {
                SETTINGS_ENABLE_PUSH if v > 1 => {
                    return Err(self.connection_error(PROTOCOL_ERROR, "bad ENABLE_PUSH"));
                }
                SETTINGS_MAX_CONCURRENT_STREAMS => self.peer_max_streams = v,
                SETTINGS_INITIAL_WINDOW_SIZE => {
                    if i64::from(v) > MAX_WINDOW {
                        return Err(self.connection_error(FLOW_CONTROL_ERROR, "bad INITIAL_WINDOW_SIZE"));
                    }
                    // §6.9.2: existing streams' windows move by the difference.
                    let delta = i64::from(v) - self.peer_initial_window;
                    self.peer_initial_window = i64::from(v);
                    for st in self.streams.values_mut() {
                        st.send_window += delta;
                    }
                }
                SETTINGS_MAX_FRAME_SIZE => {
                    if !(16_384..=16_777_215).contains(&v) {
                        return Err(self.connection_error(PROTOCOL_ERROR, "bad MAX_FRAME_SIZE"));
                    }
                    self.peer_max_frame = v as usize;
                }
                // Our encoder doesn't use the dynamic table, so its size doesn't matter.
                _ => {}
            }
        }
        self.got_settings = true;
        self.write(&frame_bytes(SETTINGS, ACK, 0, &[]))
    }

    fn on_data(&mut self, f: Frame) -> NetResult<()> {
        if f.stream == 0 {
            return Err(self.connection_error(PROTOCOL_ERROR, "DATA on stream 0"));
        }
        let len = f.payload.len() as u32;
        // Flow control counts the whole payload, padding included; give it back right away.
        self.conn_unacked += len;
        if self.conn_unacked >= OUR_CONN_WINDOW / 4 {
            let n = std::mem::take(&mut self.conn_unacked);
            self.write(&frame_bytes(WINDOW_UPDATE, 0, 0, &n.to_be_bytes()))?;
        }
        let Some(data) = unpad(&f).map(<[u8]>::to_vec) else {
            return Err(self.connection_error(PROTOCOL_ERROR, "bad DATA padding"));
        };
        let end = f.flags & END_STREAM != 0;
        let mut update = None;
        match self.streams.get_mut(&f.stream) {
            Some(st) if !st.done && st.error.is_none() => {
                if st.status.is_none() {
                    st.error = Some((conn_err("DATA before HEADERS"), false));
                    self.reset(f.stream, PROTOCOL_ERROR);
                    return Ok(());
                }
                st.body.extend_from_slice(&data);
                if st.body.len() > MAX_BODY {
                    st.error = Some((NetError::new(NetErrorKind::Protocol, "response too large"), false));
                    self.reset(f.stream, CANCEL);
                    return Ok(());
                }
                if end {
                    st.done = true;
                    check_length(st);
                } else {
                    st.unacked += len;
                    if st.unacked >= OUR_STREAM_WINDOW / 4 {
                        update = Some(std::mem::take(&mut st.unacked));
                    }
                }
            }
            // A stream we reset or gave up on: ignore (the connection window was returned).
            _ => {}
        }
        if let Some(n) = update {
            self.write(&frame_bytes(WINDOW_UPDATE, 0, f.stream, &n.to_be_bytes()))?;
        }
        Ok(())
    }

    fn on_header_block(&mut self, sid: u32, block: &[u8], end_stream: bool) -> NetResult<()> {
        // Always decode, so the HPACK state stays in sync, even for streams we dropped.
        let fields = match self.decoder.decode(block) {
            Ok(f) => f,
            Err(e) => return Err(self.connection_error(COMPRESSION_ERROR, e.0)),
        };
        let Some(st) = self.streams.get_mut(&sid) else { return Ok(()) };
        if st.done || st.error.is_some() {
            return Ok(());
        }
        if st.status.is_none() {
            // Response headers: :status first, then regular fields (§8.3.2).
            let mut status = None;
            let mut headers = Headers::new();
            let mut regular = false;
            let mut malformed = None;
            for (n, v) in fields {
                let name = String::from_utf8_lossy(&n).into_owned();
                let value = String::from_utf8_lossy(&v).into_owned();
                if name.bytes().any(|b| b.is_ascii_uppercase()) {
                    malformed = Some("uppercase header name");
                } else if let Some(pseudo) = name.strip_prefix(':') {
                    if regular || pseudo != "status" || status.is_some() {
                        malformed = Some("bad pseudo-header");
                    }
                    status = value.parse::<u16>().ok().filter(|s| (100..1000).contains(s));
                } else {
                    regular = true;
                    if matches!(
                        name.as_str(),
                        "connection" | "keep-alive" | "proxy-connection" | "transfer-encoding" | "upgrade"
                    ) {
                        malformed = Some("connection-specific header");
                    }
                    headers.append(&name, &value);
                }
            }
            let Some(code) = status.filter(|_| malformed.is_none()) else {
                st.error = Some((conn_err(malformed.unwrap_or("missing :status")), false));
                self.reset(sid, PROTOCOL_ERROR);
                return Ok(());
            };
            if (100..200).contains(&code) {
                // Interim response: wait for the final one.
                if end_stream {
                    st.error = Some((conn_err("END_STREAM on a 1xx response"), false));
                    self.reset(sid, PROTOCOL_ERROR);
                }
                return Ok(());
            }
            st.status = Some(code);
            st.headers = headers;
        } else {
            // Trailers: must end the stream; we keep them as headers.
            if !end_stream {
                st.error = Some((conn_err("trailers without END_STREAM"), false));
                self.reset(sid, PROTOCOL_ERROR);
                return Ok(());
            }
            for (n, v) in fields {
                let name = String::from_utf8_lossy(&n).into_owned();
                if !name.starts_with(':') {
                    st.headers.append(&name, &String::from_utf8_lossy(&v));
                }
            }
        }
        if end_stream {
            st.done = true;
            check_length(st);
        }
        Ok(())
    }
}

/// §8.1.1: a content-length that doesn't match the DATA received makes the response malformed.
fn check_length(st: &mut StreamState) {
    if let Some(cl) = st.headers.get("content-length").and_then(|v| v.trim().parse::<usize>().ok()) {
        let no_body = matches!(st.status, Some(204) | Some(304));
        if !no_body && cl != st.body.len() {
            st.done = false;
            st.error = Some((conn_err("content-length doesn't match the body"), false));
        }
    }
}

/// The request's header fields for HTTP/2 (§8.3.1): pseudo-headers first, lowercase names,
/// no connection-specific fields.
pub(crate) fn request_fields(req: &Request, defaults: &[(&str, String)]) -> Vec<(String, String)> {
    let url = &req.url;
    let mut out = vec![
        (":method".to_string(), req.method.clone()),
        (":scheme".to_string(), url.scheme().to_string()),
        (":authority".to_string(), url.host_with_port()),
        (":path".to_string(), url.request_target()),
    ];
    for (k, v) in defaults {
        if !req.headers.contains(k) {
            out.push((k.to_ascii_lowercase(), v.clone()));
        }
    }
    for (k, v) in req.headers.iter() {
        let k = k.to_ascii_lowercase();
        if matches!(
            k.as_str(),
            "host"
                | "connection"
                | "keep-alive"
                | "proxy-connection"
                | "transfer-encoding"
                | "upgrade"
                | "content-length"
        ) || (k == "te" && !v.eq_ignore_ascii_case("trailers"))
        {
            continue;
        }
        out.push((k, v.to_string()));
    }
    if let Some(b) = &req.body {
        out.push(("content-length".into(), b.len().to_string()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connection state without a socket (writes fail cleanly), with some open streams.
    fn detached() -> State {
        let mut st = State {
            io: None,
            writers: 0,
            buf: Vec::new(),
            decoder: hpack::Decoder::new(OUR_HEADER_TABLE, OUR_MAX_HEADER_LIST),
            next_id: 7,
            streams: HashMap::new(),
            peer_max_streams: 100,
            peer_initial_window: 65_535,
            peer_max_frame: 16_384,
            conn_send_window: 65_535,
            conn_unacked: 0,
            continuation: None,
            goaway: None,
            broken: None,
            got_settings: false,
            last_used: Instant::now(),
        };
        for id in [1, 3, 5] {
            st.streams.insert(
                id,
                StreamState { send_window: 65_535, pending: vec![1; 100_000], ..Default::default() },
            );
        }
        st
    }

    #[test]
    fn random_frames_never_panic() {
        let kinds = [
            DATA,
            HEADERS,
            PRIORITY,
            RST_STREAM,
            SETTINGS,
            PUSH_PROMISE,
            PING,
            GOAWAY,
            WINDOW_UPDATE,
            CONTINUATION,
            0x42,
        ];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..3000 {
            let mut st = detached();
            let mut bytes = frame_bytes(SETTINGS, 0, 0, &[]);
            for _ in 0..(next() % 12) {
                let kind = kinds[(next() % kinds.len() as u64) as usize];
                let flags = (next() & 0x2d) as u8;
                let stream = (next() % 8) as u32;
                let len = (next() % 40) as usize;
                let payload: Vec<u8> = (0..len).map(|_| next() as u8).collect();
                bytes.extend(frame_bytes(kind, flags, stream, &payload));
            }
            // Any split of the input.
            let cut = (next() as usize) % (bytes.len() + 1);
            let _ = st.ingest(&bytes[..cut]);
            let _ = st.ingest(&bytes[cut..]);
            let _ = st.flush_body(1);
            let _ = st.ingest(&[]);
        }
    }

    #[test]
    fn a_settings_change_moves_stream_windows_and_window_updates_add() {
        let mut st = detached();
        st.ingest(&frame_bytes(SETTINGS, 0, 0, &[])).ok();
        let mut s = SETTINGS_INITIAL_WINDOW_SIZE.to_be_bytes().to_vec();
        s.extend_from_slice(&100_000u32.to_be_bytes());
        let _ = st.ingest(&frame_bytes(SETTINGS, 0, 0, &s));
        assert_eq!(st.streams[&1].send_window, 100_000);
        let _ = st.ingest(&frame_bytes(WINDOW_UPDATE, 0, 3, &10u32.to_be_bytes()));
        assert_eq!(st.streams[&3].send_window, 100_010);
        let _ = st.ingest(&frame_bytes(WINDOW_UPDATE, 0, 0, &1000u32.to_be_bytes()));
        assert_eq!(st.conn_send_window, 66_535);
        // GOAWAY: streams above the last id may be retried.
        let mut g = 1u32.to_be_bytes().to_vec();
        g.extend_from_slice(&NO_ERROR.to_be_bytes());
        let _ = st.ingest(&frame_bytes(GOAWAY, 0, 0, &g));
        assert!(st.streams[&3].error.as_ref().is_some_and(|e| e.1));
        assert!(st.streams[&1].error.is_none());
    }

    #[test]
    fn frames_round_trip() {
        let b = frame_bytes(HEADERS, END_HEADERS | END_STREAM, 5, b"abc");
        assert_eq!(&b[..9], &[0, 0, 3, 1, 5, 0, 0, 0, 5]);
        let (f, used) = parse_frame(&b, 16_384).unwrap().unwrap();
        assert_eq!(used, 12);
        assert_eq!(f, Frame { kind: HEADERS, flags: 5, stream: 5, payload: b"abc".to_vec() });
        assert_eq!(parse_frame(&b[..11], 16_384), Ok(None));
        let big = frame_bytes(DATA, 0, 1, &vec![0; 20_000]);
        assert_eq!(parse_frame(&big, 16_384), Err(FRAME_SIZE_ERROR));
        // The reserved bit of the stream id is ignored.
        let mut r = frame_bytes(PING, 0, 0, &[0; 8]);
        r[5] |= 0x80;
        assert_eq!(parse_frame(&r, 16_384).unwrap().unwrap().0.stream, 0);
    }

    #[test]
    fn padding_and_priority_are_stripped() {
        let f = Frame {
            kind: HEADERS,
            flags: PADDED | PRIORITY_FLAG,
            stream: 1,
            payload: [&[2u8][..], &[0, 0, 0, 0, 16], b"hd", &[0, 0]].concat(),
        };
        assert_eq!(unpad(&f), Some(&b"hd"[..]));
        let bad = Frame { kind: DATA, flags: PADDED, stream: 1, payload: vec![9, 1, 2] };
        assert_eq!(unpad(&bad), None);
        let empty = Frame { kind: DATA, flags: PADDED, stream: 1, payload: vec![] };
        assert_eq!(unpad(&empty), None);
    }

    #[test]
    fn request_fields_drop_connection_headers() {
        let mut req = Request::get(bb_url::Url::parse("https://example.com:8443/a?b#c").unwrap());
        req.headers.set("Connection", "keep-alive");
        req.headers.set("TE", "gzip");
        req.headers.set("X-Thing", "1");
        let f = request_fields(&req, &[("user-agent", "UA".into())]);
        let names: Vec<&str> = f.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, [":method", ":scheme", ":authority", ":path", "user-agent", "x-thing"]);
        assert_eq!(f[2].1, "example.com:8443");
        assert_eq!(f[3].1, "/a?b");
    }
}

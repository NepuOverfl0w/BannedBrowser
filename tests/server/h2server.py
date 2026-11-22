"""An HTTP/2 test server for bb-net's integration tests, built on the `h2` library (an
independent HTTP/2 implementation, so our client is tested against someone else's).

TLS with ALPN "h2" only, the test certificates, one thread per connection. Endpoints:

  /hello                  "Hello, HTTP/2!"
  /echo                   JSON: method, path, headers, body
  /big?n=BYTES            n bytes of a repeating pattern (flow control: sent as windows allow)
  /slow?ms=N              answers after N ms with the connection's id (concurrency)
  /conn-id                the id of the connection serving the request
  /status/N               an empty response with status N
  /redirect/N             302 to /redirect/N-1, /redirect/0 says "done"
  /set-cookie?c=…&to=…    Set-Cookie headers (repeatable), then 302 to `to` (or "cookies set")
  /cookie-title           HTML whose title is the Cookie header ("none")
  /trailers               a body followed by trailers
  /gzip                   a gzip-encoded body
  /refuse?id=X            the first request per id is refused (RST_STREAM REFUSED_STREAM)
  /goaway                 answers, then GOAWAY and close
  /ping                   PINGs the client and answers only after the PING ACK
  /bad-length             content-length larger than the body (malformed)
  /continuation           response headers big enough to need CONTINUATION frames
  anything else           a file from tests/pages (or 404)
"""

import gzip, json, os, socket, ssl, threading, time
from urllib.parse import parse_qs

import h2.config
import h2.connection
import h2.errors
import h2.events
import h2.exceptions
import h2.settings

CONNS = [0]
REFUSED = set()
LOCK = threading.Lock()
MAX_STREAMS = 5


def headers_dict(hs):
    out = {}
    for k, v in hs:
        k = k.decode() if isinstance(k, bytes) else k
        v = v.decode() if isinstance(v, bytes) else v
        out.setdefault(k, []).append(v)
    return out


class Conn:
    def __init__(self, sock, root, ident):
        self.ident = ident
        self.sock = sock
        self.root = root
        cfg = h2.config.H2Configuration(client_side=False, header_encoding="utf-8")
        self.h2 = h2.connection.H2Connection(config=cfg)
        self.streams = {}
        self.pending = {}  # stream id -> (remaining body bytes, trailers)
        self.after_ping = []
        self.close_after = None  # the stream after which to send GOAWAY and close
        self.lock = threading.Lock()

    def flush(self):
        data = self.h2.data_to_send()
        if data:
            self.sock.sendall(data)

    def run(self):
        self.h2.local_settings = h2.settings.Settings(
            client=False, initial_values={h2.settings.SettingCodes.MAX_CONCURRENT_STREAMS: MAX_STREAMS})
        self.h2.initiate_connection()
        self.flush()
        while True:
            try:
                data = self.sock.recv(65536)
            except OSError:
                return
            if not data:
                return
            with self.lock:
                try:
                    events = self.h2.receive_data(data)
                except h2.exceptions.ProtocolError:
                    self.flush()
                    return
                for ev in events:
                    self.event(ev)
                self.send_pending()
                self.flush()
                if self.close_after is not None and not self.pending:
                    self.h2.close_connection(last_stream_id=self.close_after)
                    self.flush()
                    self.sock.close()
                    return

    def later(self, sid, status, body):
        """Answer a stream from a timer thread (the connection keeps serving others)."""
        with self.lock:
            try:
                self.reply(sid, status, body)
                self.send_pending()
                self.flush()
            except (h2.exceptions.ProtocolError, OSError):
                pass

    def event(self, ev):
        if isinstance(ev, h2.events.RequestReceived):
            self.streams[ev.stream_id] = {"headers": headers_dict(ev.headers), "body": b""}
        elif isinstance(ev, h2.events.DataReceived):
            if ev.stream_id in self.streams:
                self.streams[ev.stream_id]["body"] += ev.data
            self.h2.acknowledge_received_data(ev.flow_controlled_length, ev.stream_id)
        elif isinstance(ev, h2.events.StreamEnded):
            req = self.streams.pop(ev.stream_id, None)
            if req is not None:
                self.respond(ev.stream_id, req)
        elif isinstance(ev, h2.events.PingAckReceived):
            for sid, req in self.after_ping:
                self.reply(sid, 200, b"pong")
            self.after_ping = []
        elif isinstance(ev, h2.events.StreamReset):
            self.pending.pop(ev.stream_id, None)

    def reply(self, sid, status, body=b"", headers=(), trailers=None):
        hs = [(":status", str(status)), ("content-length", str(len(body)))] + list(headers)
        self.h2.send_headers(sid, hs, end_stream=False)
        self.pending[sid] = (body, trailers)

    def send_pending(self):
        for sid in list(self.pending):
            body, trailers = self.pending[sid]
            while body:
                try:
                    window = self.h2.local_flow_control_window(sid)
                except h2.exceptions.StreamClosedError:
                    body = b""
                    break
                n = min(window, self.h2.max_outbound_frame_size, len(body))
                if n <= 0:
                    break
                self.h2.send_data(sid, body[:n])
                body = body[n:]
            if body:
                self.pending[sid] = (body, trailers)
                continue
            del self.pending[sid]
            try:
                if trailers:
                    self.h2.send_headers(sid, trailers, end_stream=True)
                else:
                    self.h2.end_stream(sid)
            except h2.exceptions.StreamClosedError:
                pass

    def respond(self, sid, req):
        h = req["headers"]
        path, _, query = h[":path"][0].partition("?")
        q = parse_qs(query, keep_blank_values=True)
        one = lambda k, d=None: q[k][0] if k in q else d
        if path == "/hello":
            return self.reply(sid, 200, b"Hello, HTTP/2!", [("content-type", "text/plain")])
        if path == "/echo":
            out = {"method": h[":method"][0], "path": h[":path"][0],
                   "headers": {k: ", ".join(v) for k, v in h.items()},
                   "body": req["body"].decode("utf-8", "replace")}
            return self.reply(sid, 200, json.dumps(out).encode(), [("content-type", "application/json")])
        if path == "/big":
            n = int(one("n", "1000"))
            pattern = bytes(range(256))
            body = (pattern * (n // 256 + 1))[:n]
            return self.reply(sid, 200, body, [("content-type", "application/octet-stream")])
        if path == "/slow":
            body = f"slow {self.ident}".encode()
            t = threading.Timer(int(one("ms", "200")) / 1000, self.later, args=(sid, 200, body))
            t.daemon = True
            t.start()
            return
        if path == "/conn-id":
            return self.reply(sid, 200, str(self.ident).encode())
        if path.startswith("/status/"):
            return self.reply(sid, int(path.rsplit("/", 1)[1]))
        if path.startswith("/redirect/"):
            n = int(path.rsplit("/", 1)[1])
            if n <= 0:
                return self.reply(sid, 200, b"done")
            return self.reply(sid, 302, b"", [("location", f"/redirect/{n - 1}")])
        if path == "/set-cookie":
            hs = [("set-cookie", c) for c in q.get("c", [])]
            if "to" in q:
                return self.reply(sid, 302, b"", hs + [("location", q["to"][0])])
            return self.reply(sid, 200, b"cookies set", hs)
        if path == "/cookie-title":
            c = h.get("cookie", ["none"])
            c = "; ".join(c)
            esc = c.replace("&", "&amp;").replace("<", "&lt;")
            return self.reply(sid, 200, f"<!DOCTYPE html><title>{esc}</title><p>{esc}".encode(),
                              [("content-type", "text/html; charset=utf-8")])
        if path == "/trailers":
            return self.reply(sid, 200, b"with trailers", [("trailer", "x-checksum")],
                              trailers=[("x-checksum", "abc123")])
        if path == "/gzip":
            return self.reply(sid, 200, gzip.compress(b"gzipped over h2 " * 20),
                              [("content-encoding", "gzip")])
        if path == "/refuse":
            key = one("id", "")
            with LOCK:
                first = key not in REFUSED
                REFUSED.add(key)
            if first:
                self.h2.reset_stream(sid, error_code=h2.errors.ErrorCodes.REFUSED_STREAM)
                return
            return self.reply(sid, 200, b"accepted")
        if path == "/goaway":
            self.close_after = sid
            return self.reply(sid, 200, b"bye")
        if path == "/ping":
            self.after_ping.append((sid, req))
            self.h2.ping(b"12345678")
            return
        if path == "/bad-length":
            self.h2.send_headers(sid, [(":status", "200"), ("content-length", "100")], end_stream=False)
            self.h2.send_data(sid, b"short", end_stream=True)
            return
        if path == "/continuation":
            hs = [(f"x-filler-{i}", "v" * 200) for i in range(120)]
            return self.reply(sid, 200, b"big headers", hs)
        # Static files.
        rel = os.path.normpath(path.lstrip("/"))
        full = os.path.join(self.root, rel)
        if not rel.startswith("..") and os.path.isfile(full):
            with open(full, "rb") as f:
                body = f.read()
            ctype = "text/html; charset=utf-8" if full.endswith(".html") else "application/octet-stream"
            return self.reply(sid, 200, body, [("content-type", ctype)])
        return self.reply(sid, 404, b"not found")


def serve(host, port, certs, root):
    """Start the server; returns the bound port."""
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(os.path.join(certs, "server.pem"), os.path.join(certs, "server.key"))
    ctx.set_alpn_protocols(["h2"])
    lsock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    lsock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    lsock.bind((host, port))
    lsock.listen(64)

    def accept_loop():
        while True:
            try:
                s, _ = lsock.accept()
            except OSError:
                return
            threading.Thread(target=handle, args=(s,), daemon=True).start()

    def handle(raw):
        try:
            s = ctx.wrap_socket(raw, server_side=True)
        except (ssl.SSLError, OSError):
            raw.close()
            return
        if s.selected_alpn_protocol() != "h2":
            s.close()
            return
        with LOCK:
            CONNS[0] += 1
            ident = CONNS[0]
        try:
            Conn(s, root, ident).run()
        except Exception as e:  # a test server: log and drop the connection
            print(f"h2 connection error: {e!r}", flush=True)
        finally:
            try:
                s.close()
            except OSError:
                pass

    threading.Thread(target=accept_loop, daemon=True).start()
    return lsock.getsockname()[1]

#!/usr/bin/env python3
"""BannedBrowser test server.

Serves tests/pages/ plus special endpoints used by bb-net integration tests
and GUI tests. Prints "PORT <n>" (and "HTTPS_PORT <n>") on stdout once ready.

  testserver.py [--port 8000] [--https-port 8443] [--root tests/pages]
"""
import argparse, gzip, http.server, json, os, socketserver, ssl, sys, threading, time, zlib

HERE = os.path.dirname(os.path.abspath(__file__))
# Requests seen per /cache id (the HTTP cache tests count them).
CACHE_HITS = {}
CACHE_LOCK = threading.Lock()
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
# brotli.compress(b"Hello, brotli! " * 20)
UNHANDLED = object()
BROTLI_FIXTURE = bytes.fromhex("1b2b01f89d0976ac152b5df2c2120449accae4922ded2d4358e862c110a06b387f01")


class Handler(http.server.SimpleHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "BBTest/1.0"

    def log_message(self, fmt, *args):
        if os.environ.get("BB_TEST_SERVER_LOG"):
            sys.stderr.write("[testserver] " + fmt % args + "\n")

    def send_body(self, body, ctype="text/plain; charset=utf-8", status=200, headers=()):
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in headers:
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def route(self):
        n = int(self.headers.get("Content-Length") or 0)
        self.req_body = self.rfile.read(n) if n else b""
        path, _, query = self.path.partition("?")
        if path == "/hello":
            return self.send_body(b"Hello, BannedBrowser!")
        if path.startswith("/redirect/"):
            n = int(path.rsplit("/", 1)[1] or 0)
            if n <= 0:
                return self.send_body(b"done")
            self.send_response(302)
            self.send_header("Location", f"/redirect/{n-1}")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if path == "/redirect-loop":
            self.send_response(302); self.send_header("Location", "/redirect-loop")
            self.send_header("Content-Length", "0"); self.end_headers(); return
        if path == "/redirect-303":
            self.send_response(303); self.send_header("Location", "/echo")
            self.send_header("Content-Length", "0"); self.end_headers(); return
        if path == "/redirect-307":
            self.send_response(307); self.send_header("Location", "/echo")
            self.send_header("Content-Length", "0"); self.end_headers(); return
        if path == "/chunked":
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            for piece in [b"Hello", b", ", b"chunked ", b"world", b"!" * 1000]:
                self.wfile.write(b"%x;ext=1\r\n%s\r\n" % (len(piece), piece))
                self.wfile.flush()
            self.wfile.write(b"0\r\nX-Trailer: yes\r\n\r\n")
            return
        if path == "/gzip":
            return self.send_body(gzip.compress(b"gzip body " * 100), headers=[("Content-Encoding", "gzip")])
        if path == "/deflate":
            return self.send_body(zlib.compress(b"deflate body " * 100), headers=[("Content-Encoding", "deflate")])
        if path == "/br":
            return self.send_body(BROTLI_FIXTURE, headers=[("Content-Encoding", "br")])
        if path.startswith("/status/"):
            code = int(path.rsplit("/", 1)[1])
            return self.send_body(f"status {code}".encode(), status=code)
        if path == "/conn-id":
            return self.send_body(str(id(self.connection)).encode())
        if path == "/slow":
            time.sleep(float(query or 5))
            return self.send_body(b"slow")
        if path == "/close":
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(b"until close " * 10)
            self.close_connection = True
            return
        if path == "/set-cookie":
            # Each c=<header value> becomes a Set-Cookie field; to=<path> makes it a redirect.
            from urllib.parse import parse_qs
            q = parse_qs(query, keep_blank_values=True)
            cookies = [("Set-Cookie", v) for v in q.get("c", [])]
            if "to" in q:
                self.send_response(302)
                for k, v in cookies:
                    self.send_header(k, v)
                self.send_header("Location", q["to"][0])
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            return self.send_body(b"cookies set", headers=cookies)
        if path == "/cookie-title":
            # A page whose title is the Cookie header it received ("none" without cookies).
            c = self.headers.get("Cookie") or "none"
            esc = c.replace("&", "&amp;").replace("<", "&lt;")
            body = f"<!DOCTYPE html><title>{esc}</title><p>{esc}</p>".encode()
            return self.send_body(body, "text/html; charset=utf-8")
        if path == "/cache":
            # A cacheable response for the HTTP cache tests. Query: id (counter), cc
            # (Cache-Control), etag, lm (Last-Modified, seconds ago), expires (seconds from now),
            # vary, status, location, cookie (Set-Cookie). The body says how many requests the
            # id has seen; a matching If-None-Match gets 304.
            import email.utils
            from urllib.parse import parse_qs
            q = parse_qs(query, keep_blank_values=True)
            one = lambda k: q[k][0] if k in q else None
            with CACHE_LOCK:
                ident = one("id") or ""
                CACHE_HITS[ident] = CACHE_HITS.get(ident, 0) + 1
                n = CACHE_HITS[ident]
            hdrs = [("X-Hits", str(n)), ("Date", email.utils.formatdate(usegmt=True))]
            if one("cc") is not None:
                hdrs.append(("Cache-Control", one("cc")))
            if one("etag"):
                hdrs.append(("ETag", '"%s"' % one("etag")))
            if one("lm"):
                hdrs.append(("Last-Modified", email.utils.formatdate(time.time() - int(one("lm")), usegmt=True)))
            if one("expires"):
                hdrs.append(("Expires", email.utils.formatdate(time.time() + int(one("expires")), usegmt=True)))
            if one("vary"):
                hdrs.append(("Vary", one("vary")))
            if one("location"):
                hdrs.append(("Location", one("location")))
            if one("cookie"):
                hdrs.append(("Set-Cookie", one("cookie")))
            inm = self.headers.get("If-None-Match")
            ims = self.headers.get("If-Modified-Since")
            if (one("etag") and inm == '"%s"' % one("etag")) or (not one("etag") and one("lm") and ims):
                self.send_response(304)
                for k, v in hdrs:
                    self.send_header(k, v)
                self.end_headers()
                return
            status = int(one("status") or 200)
            if one("html"):
                body = ("<!DOCTYPE html><title>hits=%d</title><p>hits=%d" % (n, n)).encode()
                return self.send_body(body, "text/html; charset=utf-8", status=status, headers=hdrs)
            return self.send_body(("hits=%d" % n).encode(), status=status, headers=hdrs)
        if path == "/form-title":
            # A page whose title is the request method and its query (GET) or body (POST).
            data = self.req_body.decode("utf-8", "replace") if self.command == "POST" else query
            t = f"{self.command} {data}".replace("&", "&amp;").replace("<", "&lt;")
            body = f"<!DOCTYPE html><title>{t}</title><p>{t}</p>".encode()
            return self.send_body(body, "text/html; charset=utf-8")
        if path == "/echo":
            body = self.req_body
            out = {"method": self.command, "path": self.path,
                   "headers": {k.lower(): v for k, v in self.headers.items()},
                   "body": body.decode("utf-8", "replace")}
            return self.send_body(json.dumps(out).encode(), "application/json")
        if path.startswith("/charset/"):
            cs = path.rsplit("/", 1)[1]
            body = "<p>caf\u00e9 na\u00efve \u65e5\u672c\u8a9e</p>".encode(cs, "replace")
            return self.send_body(body, f"text/html; charset={cs}")
        return UNHANDLED

    def do_GET(self):
        if self.route() is UNHANDLED:
            super().do_GET()

    def do_HEAD(self):
        if self.route() is UNHANDLED:
            super().do_HEAD()

    def do_POST(self):
        if self.route() is UNHANDLED:
            self.send_body(b"no route", status=404)


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def server_bind(self):
        # http.server.HTTPServer.server_bind resolves the host with socket.getfqdn(), a reverse
        # DNS lookup. On macOS that goes through Bonjour, which is slow and pops up the "find
        # devices on local networks" privacy prompt over the GUI under test. We only serve
        # 127.0.0.1, so skip the lookup.
        socketserver.TCPServer.server_bind(self)
        host, port = self.server_address[:2]
        self.server_name = host
        self.server_port = port


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8000)
    ap.add_argument("--https-port", type=int, default=None)
    ap.add_argument("--root", default=os.path.join(REPO, "tests", "pages"))
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--exit-on-stdin-eof", action="store_true",
                    help="exit when stdin closes (lets test harnesses clean up)")
    a = ap.parse_args()
    if a.exit_on_stdin_eof:
        def watch():
            sys.stdin.read()
            os._exit(0)
        threading.Thread(target=watch, daemon=True).start()
    os.chdir(a.root)
    httpd = Server((a.host, a.port), Handler)
    print(f"PORT {httpd.server_address[1]}", flush=True)
    threads = [threading.Thread(target=httpd.serve_forever, daemon=True)]
    if a.https_port is not None:
        httpsd = Server((a.host, a.https_port), Handler)
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        certs = os.path.join(REPO, "tests", "certs")
        ctx.load_cert_chain(os.path.join(certs, "server.pem"), os.path.join(certs, "server.key"))
        httpsd.socket = ctx.wrap_socket(httpsd.socket, server_side=True)
        print(f"HTTPS_PORT {httpsd.server_address[1]}", flush=True)
        threads.append(threading.Thread(target=httpsd.serve_forever, daemon=True))
    for t in threads:
        t.start()
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()

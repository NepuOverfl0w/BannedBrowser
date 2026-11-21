#!/usr/bin/env python3
"""Save a page and its images for offline regression tests.

  scripts/snapshot-site.py <url> <name>

Writes tests/pages/snapshots/<name>/index.html (image URLs rewritten to local
files, provenance comment at the top) plus the images. The page's bytes are
kept as-is (latin-1 round trip), so legacy encodings survive.

For theoldnet.com archived pages the images live on web.archive.org; run it
where that host is reachable (see scripts/archived-sites.sh). theoldnet's own
image proxy re-encodes binary data as UTF-8 and is not usable.
"""
import hashlib, os, re, sys, urllib.parse, urllib.request, datetime

UA = "Mozilla/5.0"  # theoldnet only proxies archived pages for old-looking UAs


def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


def main():
    url, name = sys.argv[1], sys.argv[2]
    root = os.path.join(os.path.dirname(__file__), "..", "tests", "pages", "snapshots", name)
    os.makedirs(root, exist_ok=True)
    html = fetch(url).decode("latin-1")
    missing = []

    def repl(m):
        attr, quote, src = m.group(1), m.group(2) or "", m.group(3)
        absu = urllib.parse.urljoin(url, src.strip())
        if not absu.startswith("http"):
            return m.group(0)
        ext = os.path.splitext(urllib.parse.urlparse(absu).path)[1][:5] or ".img"
        local = hashlib.sha1(absu.encode()).hexdigest()[:12] + ext.lower()
        path = os.path.join(root, local)
        if not os.path.exists(path):
            try:
                data = fetch(absu)
                if data[:6] in (b"<html>", b"<!DOCT") or data[:5].lower() == b"<html":
                    raise ValueError("got HTML instead of an image")
                open(path, "wb").write(data)
            except Exception as e:
                missing.append(absu)
                print("skip", absu, e)
                return m.group(0)
        return f"{attr}={quote}{local}{quote}"

    pat = re.compile(r"""\b(src|background)\s*=\s*(?:(["'])(.*?)\2|()([^\s>"']+))""", re.I | re.S)

    def repl_any(m):
        if m.group(5) is not None:
            class M:  # unquoted value
                def group(self, i):
                    return {0: m.group(0), 1: m.group(1), 2: "", 3: m.group(5)}[i]
            return repl(M())
        return repl(m)

    html = pat.sub(repl_any, html)
    today = datetime.date.today().isoformat()
    note = f"<!-- Snapshot of {url} taken {today} for offline regression tests. Images saved alongside"
    if missing:
        note += f"; {len(missing)} image(s) could not be fetched and keep their original URLs"
    html = note + ". -->\n" + html
    open(os.path.join(root, "index.html"), "wb").write(html.encode("latin-1"))
    print("saved", url, "->", root, f"({len(missing)} missing)")


main()

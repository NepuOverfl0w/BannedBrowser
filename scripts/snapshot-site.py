#!/usr/bin/env python3
"""Save a page and its images for offline regression tests.

  scripts/snapshot-site.py <url> <name>

Writes tests/pages/snapshots/<name>/index.html (image URLs rewritten to local
files, provenance comment at the top) plus the images.
"""
import hashlib, os, re, sys, urllib.parse, urllib.request, datetime

UA = "Mozilla/5.0 (X11; Linux x86_64) BannedBrowser-snapshot"

def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": UA})
    with urllib.request.urlopen(req, timeout=30) as r:
        return r.read()

def main():
    url, name = sys.argv[1], sys.argv[2]
    root = os.path.join(os.path.dirname(__file__), "..", "tests", "pages", "snapshots", name)
    os.makedirs(root, exist_ok=True)
    html = fetch(url).decode("utf-8", "replace")
    def repl(m):
        attr, src = m.group(1), m.group(2)
        absu = urllib.parse.urljoin(url, src)
        if not absu.startswith("http"):
            return m.group(0)
        ext = os.path.splitext(urllib.parse.urlparse(absu).path)[1][:5] or ".img"
        local = hashlib.sha1(absu.encode()).hexdigest()[:12] + ext
        path = os.path.join(root, local)
        if not os.path.exists(path):
            try:
                open(path, "wb").write(fetch(absu))
            except Exception as e:
                print("skip", absu, e)
                return m.group(0)
        return f'{attr}="{local}"'
    html = re.sub(r'\b(src|background)="([^"]+)"', repl, html)
    today = datetime.date.today().isoformat()
    html = f"<!-- Snapshot of {url} taken {today} for offline regression tests. Images saved alongside. -->\n" + html
    open(os.path.join(root, "index.html"), "w").write(html)
    print("saved", url, "->", root)

main()

# Security policy

BannedBrowser is an early, experimental browser. It has **no sandboxing or process isolation**
yet, so please don't use it for sensitive browsing. Security reports are still very welcome.

## Supported versions

Only the latest release and `main` get fixes.

## Reporting a vulnerability

**Please don't open a public issue for security problems.** Report them privately with
GitHub's **"Report a vulnerability"** button on the repository's **Security** tab (private
vulnerability reporting). Include:

- what an attacker can do, and under which conditions;
- a minimal page, URL or input that reproduces it;
- the BannedBrowser version (`bannedbrowser --version`) and your OS.

You'll get an acknowledgement as soon as the maintainer sees it. Fixes are released as soon
as practical, and the report is credited unless you prefer otherwise.

## What counts

- Crashes, hangs or panics caused by web content (any parser: URL, HTTP, HTML, XML, CSS, JavaScript, images, fonts).
- TLS or certificate-validation bypasses, and mixed-content mistakes.
- Anything that lets a page read or change data from another site (cookies, storage, another origin's frames or workers), or escape to the local system.
- Crafted downloads or `file:` handling problems.

Bugs in third-party crates should also go to their own projects; tell us too so we can update.

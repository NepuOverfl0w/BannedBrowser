# CLAUDE.md: notes for Claude

**The project rules live in [`AGENTS.md`](AGENTS.md)** and are shared by every coding agent.
Read it first, then `PROGRESS.md` and `docs/ARCHITECTURE.md`. Everything below only adds
notes for Claude Code's cloud VM.

## Environment (Claude Code cloud VM)
- Display: run `vdisplay-start && export DISPLAY=:99` (Xvfb + Fluxbox).
  - `vshot out.png [title]` takes a screenshot; `vwait-window BannedBrowser` waits for the window.
- Run long builds in the background and poll the log.
- HTTPS goes through `HTTPS_PROXY`, with a MITM CA in `SSL_CERT_FILE`; bb-net honours both.
- The sandbox's git proxy only accepts pushes to branches; tag pushes get a 403. Release with
  a `[release]` commit instead (see AGENTS.md, "Releasing").
- `web.archive.org` is blocked by the sandbox's egress policy. Archived-site checks run in
  the "Archived sites" GitHub workflow; download its artifact with the GitHub tools.

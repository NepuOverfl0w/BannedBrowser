# Contributing

* **Rules.** All project rules are in [`AGENTS.md`](../AGENTS.md); this is a summary.
* **No borrowed engines.** Never add a crate that implements part of the web
  platform, layout, painting, text layout or a GUI toolkit.
  `scripts/check-deps.sh` enforces the banned list; new crates go into
  `DEPENDENCIES.md` with a justification first.
* **Style.** `cargo fmt --all`; `cargo clippy --workspace --all-targets -- -D warnings`.
  Engine crates use `#![forbid(unsafe_code)]`. Never panic on web content.
* **Specs.** Cite sections above non-trivial algorithms, e.g.
  `// HTML §13.2.6.4.7 "in body"`. Leave `TODO(spec §x)` for partial work.
* **Tests.** Every parser gets robustness tests; every rendering feature gets a
  reftest and a headless screenshot you actually look at.
* **Commits.** Conventional commits: `feat(layout): …`, `fix(net): …`,
  `test(html): …`, `docs: …`.

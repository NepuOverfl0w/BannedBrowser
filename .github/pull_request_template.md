## What and why

<!-- What does this change, and why? Link issues and spec sections. -->

## Checklist

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `scripts/check-deps.sh --self-test && scripts/check-deps.sh` (no borrowed engines; every `unsafe` block has a `// SAFETY:` comment)
- [ ] New crates are justified in `DEPENDENCIES.md`
- [ ] Rendering changes: a reftest that fails without the change, plus a screenshot you looked at
- [ ] No tests skipped, excluded or loosened
- [ ] `PROGRESS.md` and `CHANGELOG.md` (`[Unreleased]`) updated

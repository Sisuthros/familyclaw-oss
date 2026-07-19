---
name: Bug report
about: Report a reproducible problem in FamilyClaw
title: "[BUG] "
labels: bug
assignees: ''
---

## Description

A clear, concise description of the bug.

## Reproduction

Minimal steps to reproduce, ideally as a `cargo test` or exact commands:

```bash
# e.g.
cargo run -p familyclaw-agent --bin crash_replay -- write
```

## Expected behavior

What you expected to happen.

## Actual behavior

What actually happened. Include panic messages, error output, or logs.

## Environment

- `cargo --version`:
- `rustc --version`:
- OS:
- Feature flags used (if any):

## Additional context

Anything else that helps — related issues, workarounds tried, whether this
reproduces on a clean checkout of `main`.

---

**Before submitting:** please confirm this does not expose Layer B (private)
data — no real API keys, tokens, private paths, or family/persona content in
logs or reproduction steps.

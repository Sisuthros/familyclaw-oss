# CI Green Proof — locally reproducible

> **Why this file exists.** GitHub Actions is currently frozen on this private
> repo (org billing), so the hosted CI badge cannot run. This file records the
> exact gates the CI workflow (`.github/workflows/ci.yml`) runs, and the result
> of running **every one of them locally against `main`**. Unlike a screenshot,
> this is **reproducible**: clone the repo and run the same commands — they are
> the literal CI steps.
>
> When Actions billing is restored, the hosted run supersedes this file.

## Toolchain

- Host: `x86_64-pc-windows-msvc` (MSVC stable), rustc 1.95.0
- `cargo-deny` 0.19.9

## Gates (exact ci.yml commands) and results on `main`

| # | Gate | Command | Result |
|---|------|---------|--------|
| 1 | Layer B audit | `bash scripts/audit-layer-b.sh` | ✅ PASSED (no private data in Layer A) |
| 2 | Format | `cargo fmt --all -- --check` | ✅ exit 0 |
| 3 | Build (workspace) | `cargo build --workspace --features discord` | ✅ exit 0 |
| 4 | Clippy (pedantic, deny) | `cargo clippy --workspace --all-targets --features discord -- -D warnings` | ✅ exit 0 |
| 5 | Tests (workspace) | `cargo test --workspace --features discord` | ✅ all `test result: ok`, 0 failed |
| 6 | Docs (deny) | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features discord` | ✅ exit 0 |
| 7 | Dependency policy | `cargo deny check` | ✅ advisories ok, bans ok, licenses ok, sources ok |
| — | Integration prereq | `cargo build -p familyclaw-agent --bin continuity_daemon` | ✅ exit 0 (familyclaw-bench process-boundary tests need this binary prebuilt) |

All gates green on `main`.

## How to reproduce

```bash
# from the repo root, on main, with MSVC stable + cargo-deny installed:
bash scripts/audit-layer-b.sh
cargo fmt --all -- --check
cargo build --workspace --features discord
cargo clippy --workspace --all-targets --features discord -- -D warnings
cargo build -p familyclaw-agent --bin continuity_daemon   # integration-test prereq
cargo test --workspace --features discord
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features discord
cargo deny check
```

## Note on the doc gate

The workspace-wide `cargo doc -D warnings` (gate 6) is stricter than per-crate
runs: it caught a `rustdoc::private_intra_doc_links` error in `web_fetch.rs`
(a module-level doc linking a private `validate_url`) that per-crate doc builds
let through. Fixed in the same change as this file. This is exactly why the
whole-workspace CI equivalent matters — it surfaces what narrower runs miss.

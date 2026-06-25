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

## Hosted CI blocker (documented, verified 2026-06-25)

Hosted GitHub Actions is **not** green — but **not** because of a code or
workflow problem. The workflow is valid and Actions are enabled on the repo;
the jobs are refused at *start* by an **account-level billing block**.

**Exact error** (captured from `gh run view <id>`, run on a real PR):

> The job was not started because recent account payments have failed or your
> spending limit needs to be increased. Please check the 'Billing & plans'
> section in your settings

Because every job `needs: layer-b-audit` and that job cannot start, the whole
pipeline shows `failure` within a few seconds (no build actually runs).

**Verification that this is billing, not code/workflow:**

- `gh api repos/Sisuthros/familyclaw/actions/permissions`
  → `{"enabled":true,"allowed_actions":"all", …}` (Actions are enabled).
- `gh run view <id>` annotation is the billing message above, on the
  `layer-b-audit` job, run duration ~2s (it never gets to checkout work).
- Repo is **private**, so it consumes paid Actions minutes.

**Hosted CI is intentionally not relied upon for this private repo.** Running it
would consume paid Actions minutes the project deliberately does not spend, so
the local, reproducible proof in this file is the **authoritative** gate (run the
commands in [How to reproduce](#how-to-reproduce) — they mirror `ci.yml` exactly).

Should the repo owner later choose to enable hosted runs — by clearing the
Actions billing block, or by publishing the repo (public repos get free Actions
minutes) — the hosted run would then supersede this file (see the header). That
is an owner decision and is **not** required for the local proof to be valid.

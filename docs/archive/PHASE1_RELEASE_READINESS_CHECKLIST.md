# Phase 1 Release Readiness Checklist

> **ARCHIVED (2026-07-18):** historical sprint/booth document; items verified
> complete or superseded by [STATUS.md](../../STATUS.md).

> Hardening / test / CI / PR-readiness sprint on `feat/familyclaw-v1-tool-loop`.
> No new product features. No scope widening. Close the listed P0/P1 gaps only.

## State at sprint start

- **HEAD:** `765394e81edfdb147ab0ff30fafd38b1b09f9690`
- **Branch:** `feat/familyclaw-v1-tool-loop`
- **Ahead/behind vs `origin/main`:** ahead 39, behind 0
- **Pushed:** yes (`origin/feat/familyclaw-v1-tool-loop` == local HEAD)
- **Baseline tests:** 1566 passing, 0 failed (clean HEAD, no extra features)
- **axum:** 0.7 (workspace pin) — route syntax must be `:approval_id`, never `{approval_id}`

## P0 tasks

- [ ] **Discord config-model fix** — move `FAMILYCLAW_OWNER_ID` out of `DiscordChannel::new()` (currently `crates/familyclaw-channels/src/discord/mod.rs:123` direct `std::env::var`). Add `owner_id` to `DiscordCfg` (TOML + env override + accessor + boundary validation). Safe default: missing/0/invalid → DMs dropped, never "all DMs allowed".
- [ ] **Approval double-POST race test** — HTTP-level, real router/socket, two concurrent `POST /approvals/:approval_id/approve`. Assert side_effect_count == 1, one final reply, one resume sequence, no panic. Name: `approval_double_post_race_runs_side_effect_once`.
- [ ] **axum 0.7 route regression test** — literal `POST /approvals/{approval_id}/approve` must NOT match; valid `POST /approvals/<uuid>/approve` must reach handler. Name: `approval_literal_braces_route_does_not_match_on_axum_07`.
- [ ] **CODE_OF_CONDUCT contact hygiene** — replace personal email (line 39) with project-safe GitHub-private-report wording. No duplicate heading, no private identity.
- [ ] **Final clean commit stack** with precise summary.

## P1 tasks

- [ ] **Discord bot/DM hardening 7.5 → 9+** — mode clarity (bot/webhook/disabled, DM owner-only/disabled), doctor/status output, locked mapping rules + tests, token/log safety (never in Debug/logs), receive-once stream guard + test, start timeout / stop idempotent.
- [ ] **Merge readiness 6.5 → 9+** — `docs/PHASE1_PR_BODY.md` (allowed/forbidden claims, caveats, validation), PR split plan, draft PR to make GitHub CI visible/green.
- [ ] **Validation gates** — run + record all commands below across feature matrix.
- [ ] **Truth-drift sweep** — fix "exactly once"→"at-most-once dispatch", "production ready"→"built but unproven", stale counts/commands/features, no private names/paths/emails.

## Validation commands

```
bash scripts/audit-layer-b.sh
cargo fmt --all -- --check
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --features discord -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --features discord --no-deps
cargo audit                 # or repo-documented audit cmd
cargo deny check            # or repo-documented deny cmd, respecting documented ignores
```

Feature matrix: default · discord · telegram · whatsapp · signal · wasmtime · CI "living features" set.
Windows/MSVC: run documented Windows validation; no new Unix-only assumptions in Discord/config tests.

## PR-ready criteria

- All validation commands pass (record command, pass/fail, test count, warnings, advisory status).
- `FAMILYCLAW_OWNER_ID` appears only in config loading, docs, `.env.example`, tests — NOT as a direct env read in `DiscordChannel::new()`.
- Both new approval tests present and green; route test fails if `{approval_id}` is reintroduced.
- CoC uses a project-safe contact.
- Layer B audit passes; no private names/secrets/absolute private paths in tracked files.
- `docs/PHASE1_PR_BODY.md` present with honest allowed/forbidden claims.
- A (draft) PR exists so GitHub CI is visible. Do not merge until CI is visible and green.
- Claims discipline: at-most-once **dispatch** under crash (journal backend), NOT universal exactly-once; Discord built-but-unproven without live smoke.

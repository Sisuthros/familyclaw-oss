# Phase 1 — Tool-loop, approval/resume, at-most-once dispatch, Discord hardening

**Branch:** `feat/familyclaw-v1-tool-loop`
**Head:** `096d5da` (docs-only body refresh on top of `ce7d750`)
**Base:** `main` @ `fd73adcdf0215c4e47d9476c4ffb2fad86721030`
**Ahead / behind:** 50 / 0

This PR closes the Phase 1 release-readiness gaps from the last external audit
(Discord bot/DM config model, approval-route regressions, public-contact hygiene)
and hardens the Discord adapter. **No new product features** were added in the
readiness sprint — it is hardening, tests, CI hygiene, and doc truth-correction.

## Summary by subsystem

- **Phase 0 — CI / spike / docs truth:** `.gitattributes` LF normalisation, embedder
  spike, and earlier doc-truth audits. (CI workflow `.github/workflows/ci.yml`.)
- **familyclaw-actions:** typed action manifest, durable outbox + pending store,
  red-team dispatch harness, at-most-once / replay-safe dispatch proof tests.
- **tool-loop:** LLM tool-schema + `complete_with_tools` failover; agent tool loop.
- **approval / resume:** operator approval → `ResumeApproval` signal; async,
  agent-owned resume.
- **at-most-once dispatch:** idempotency-key intent→effect→committed, fail-closed
  intent-only under crash; cross-process proof (SIGKILL) for dispatch and approval.
- **Discord bot/DM hardening:** `owner_id` moved into the config model (TOML + env at
  the boundary), mapping rules + token/stream safety fully tested; outbound replies
  route to the inbound message's channel snowflake (DM stays a DM).
- **docs / security:** CoC + SECURITY contact hygiene, doc truth-drift sweep.

## Most recent commits

- `ce7d750` — `fix(discord): reititä lähtevä viesti message.target-snowflakeen`
  (outbound `send()` uses `OutboundMessage.target` first so DM replies route to the
  DM channel instead of the guild channel; falls back to `target_channel_id` for
  webhook / bus-pump instances).
- `b9a6df3` — `chore(gitignore): ignore PHASE2_RECON_PLAN_*.json` (local artifact).
- `8ce97c2` — `docs(bench): record s7_provenance_gate + s8_weekly_review results`
  (both PASS).
- `50387b1` — `fix(docs,test): address external review` — PR-body truth-drift +
  strengthen reply assertion (`approval_double_post_race_runs_side_effect_once`
  asserts `side_effect_count == 1` with one final reply; "exactly-once dispatch proof
  tests" wording replaced with "at-most-once / replay-safe dispatch proof tests").

## Tests added

- `approval_double_post_race_runs_side_effect_once` — two concurrent
  `POST /approvals/:approval_id/approve` on a real router/socket; asserts
  `side_effect_count == 1`, one final reply, one resume/answer sequence, no panic.
- `approval_literal_braces_route_does_not_match_on_axum_07` — guards the axum 0.7
  `:approval_id` route; fails if anyone reverts it to `{approval_id}`.
- `owner_id_loads_from_toml`, `owner_id_env_overrides_toml_and_invalid_fails_safe`,
  `missing_owner_id_defaults_to_zero_disabling_dms`,
  `new_uses_owner_id_argument_not_env` — config-model + safe-default coverage.
- `dm_reply_target_is_dm_channel_not_group_channel`,
  `construction_error_does_not_echo_token`, `empty_token_error_does_not_echo_input`
  — Discord DM-reply target + token-no-echo guards.

## Validation run (Windows / MSVC, local)

| Command | Result |
|---|---|
| `bash scripts/audit-layer-b.sh` | PASS |
| `cargo fmt --all -- --check` | PASS |
| `cargo build --workspace` | PASS |
| `cargo test --workspace` | **1575 passed, 0 failed** (default features) |
| `cargo test -p familyclaw-channels --features discord` | 89 + 1 + 3 passed, 0 failed |
| `cargo clippy --workspace --all-targets --features discord -- -D warnings` | PASS (0) |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --features discord --no-deps` | PASS |
| `cargo audit` (respecting `.cargo/audit.toml`) | clean (exit 0, 650 deps scanned) |
| Feature builds: default · discord · telegram · whatsapp · signal · wasmtime | all PASS |

> ⚠️ `cargo deny check` was **not run locally** (`cargo-deny` not installed on this
> machine); `deny.toml` is present and CI runs it. GitHub CI is the authoritative
> gate — see the checks on this PR.

## GitHub CI status (honest)

First CI run on this PR: **5 of 8 checks pass** (Build & Test Windows, Build & Test
Linux, Clippy + Doc MSVC, Layer B leak audit, cargo audit). Three checks failed —
all **pre-existing on `main`, not regressions from this sprint** (verified: the
sprint diff `765394e..HEAD` does not touch the relevant crates/lockfile entries):

1. **`cargo deny`** — `webpki-roots` ships under `CDLA-Permissive-2.0` (the Mozilla
   CA-bundle data license), which was missing from `deny.toml`'s allow-list. **Fixed
   in this PR** by adding `CDLA-Permissive-2.0` to the allow-list (the lockfile entry
   is byte-identical to `main`, so this was already failing on `main`).
2. **`MSRV (1.85)`** — transitive `icu_*@2.2.0` requires rustc **1.86** (identical
   versions on `main`). The CI MSRV pin (1.85) is now behind the dependency tree.
   Raising the pin to 1.86 is a project-level decision (it changes the advertised
   MSRV) and is **out of scope for this hardening sprint** — flagged for the
   maintainer, not silently changed.
3. **`Test (all features)`** — 6 tests in `familyclaw-bench` panic because the
   `continuity_daemon` binary is not built before the test job runs
   (`familyclaw_subject.rs:27` asserts the binary exists). This is a pre-existing CI
   job-ordering gap (the all-features job does not `cargo build --bin
   continuity_daemon` first); **not touched by this sprint**, flagged for a CI fix.

**Do not merge until these are resolved.** Item 1 is fixed here; items 2 and 3 are
pre-existing and need a maintainer decision (MSRV bump; add the daemon build step to
the all-features CI job).

## Claims this PR makes (allowed)

- **At-most-once external side-effect *dispatch* under crash**, when the journal
  backend is enabled (`FAMILYCLAW_DATA_DIR` / durable outbox). Committed dispatches
  replay as values; an intent-only crash fails closed rather than double-firing.
- Approval/resume is **async and agent-owned**.
- LangGraph-style checkpointing is **not the same** as crash-safe action dispatch.

## Claims this PR does NOT make (forbidden)

- ❌ Universal exactly-once *completion* / "all side effects always complete exactly once".
- ❌ Discord "production-ready" — it is **built and tested but unproven against a live
  Discord** (no live smoke in CI; no live credentials required for tests).
- ❌ Multi-agent orchestration "fully shipped" where live integration is built-but-unproven.

## Known caveats

- The crash guarantee is **at-most-once dispatch** and requires the journal backend
  (`FAMILYCLAW_DATA_DIR`); without it, intent-only fails closed (no double-fire) but
  there is no durable replay.
- Discord adapter has no live-credential smoke test in CI by design; integration test
  self-skips without `DISCORD_TEST_TOKEN`.
- Branch is large (50 commits). If a single review is unwieldy, a split is available
  (see "PR split plan" below); kept as one reviewable PR for now.
- `cargo-deny` not verified locally — relying on CI.

## PR split plan (if a single review is too large)

- **PR A** — Phase 0 CI + docs truth + embedder spike
- **PR B** — `familyclaw-actions` crate (manifest, outbox, pending store)
- **PR C** — tool-loop + approval/resume + at-most-once dispatch proof
- **PR D** — Discord bot/DM hardening (this sprint's P0/P1)
- **PR E** — docs / security / bench comparison

## Do not merge until GitHub CI is visible and green.

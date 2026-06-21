# Phase 1 — Tool-loop, approval/resume, at-most-once dispatch, Discord hardening

**Branch:** `feat/familyclaw-v1-tool-loop`
**Head:** `203bef9e914d0f1ca3058695302f5b5fa8dce95b`
**Base:** `main` @ `fd73adcdf0215c4e47d9476c4ffb2fad86721030`
**Ahead / behind:** 42 / 0

This PR closes the Phase 1 release-readiness gaps from the last external audit
(Discord bot/DM config model, approval-route regressions, public-contact hygiene)
and hardens the Discord adapter. **No new product features** were added in the
readiness sprint — it is hardening, tests, CI hygiene, and doc truth-correction.

## Summary by subsystem

- **Phase 0 — CI / spike / docs truth:** `.gitattributes` LF normalisation, embedder
  spike, and earlier doc-truth audits. (CI workflow `.github/workflows/ci.yml`.)
- **familyclaw-actions:** typed action manifest, durable outbox + pending store,
  red-team dispatch harness, exactly-once *dispatch* proof tests.
- **tool-loop:** LLM tool-schema + `complete_with_tools` failover; agent tool loop.
- **approval / resume:** operator approval → `ResumeApproval` signal; async,
  agent-owned resume.
- **at-most-once dispatch:** idempotency-key intent→effect→committed, fail-closed
  intent-only under crash; cross-process proof (SIGKILL) for dispatch and approval.
- **Discord bot/DM hardening:** `owner_id` moved into the config model (TOML + env at
  the boundary), mapping rules + token/stream safety fully tested.
- **docs / security:** CoC + SECURITY contact hygiene, doc truth-drift sweep.

## Release-readiness sprint (this PR's final 3 commits)

- `style: cargo fmt --all` — normalise pre-existing rustfmt drift (formatting only).
- `fix(discord,gateway): P0 release-readiness` — `owner_id` config model
  (`DiscordCfg.owner_id`, TOML + `FAMILYCLAW_OWNER_ID` env override resolved at the
  config boundary, invalid value ignored with a warning, never "all DMs allowed";
  `DiscordChannel::new` takes `owner_id` as an argument — no direct env read).
  Two HTTP-level approval regression tests. CoC/SECURITY contact hygiene.
- `test(discord): P1 hardening` — explicit DM-reply-target and token-no-echo tests;
  QUICKSTART truth-drift fix.

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
- Branch is large (42 commits). If a single review is unwieldy, a split is available
  (see "PR split plan" below); kept as one reviewable PR for now.
- `cargo-deny` not verified locally — relying on CI.

## PR split plan (if a single review is too large)

- **PR A** — Phase 0 CI + docs truth + embedder spike
- **PR B** — `familyclaw-actions` crate (manifest, outbox, pending store)
- **PR C** — tool-loop + approval/resume + at-most-once dispatch proof
- **PR D** — Discord bot/DM hardening (this sprint's P0/P1)
- **PR E** — docs / security / bench comparison

## Do not merge until GitHub CI is visible and green.

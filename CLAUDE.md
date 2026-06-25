# FamilyClaw — Contributor & Agent Guide

FamilyClaw is a **public, generic Rust agent runtime**. Everything in this repo
must read as a standalone open-source project: no private identities, no internal
deployment details, no secrets.

## Agent-first execution rule

Before implementing, split work across specialized agents when available.

Required roles:
- repo-auditor
- implementation-agent
- test/ci-agent
- security-review-agent
- docs-agent

Use MCP servers first when available for repository inspection, GitHub issues/PRs,
test execution and structured planning.

Do not work from a subdirectory by accident.
Always run `scripts/session_start_check.sh` or `scripts/session_start_check.ps1`
at session start.

Do not commit private Layer B data, private names, secrets, local paths, tokens,
or private identity/persona content. FamilyClaw is public/generic runtime only.

## Allowed generic terms (use these in public code/docs/tests)

`agent_a`, `agent_b`, `operator`, `legacy_runtime`, `fallback_runtime`,
`shadow_target`, `external_system`, `mock_tool`, `mock_provider`,
`dry_run_executor`.

## Forbidden in this repo (public)

Private persona/family names, private home-layer planning, real API keys, real
private paths, real deployment endpoints, runtime-specific private env names.
Public config exposes a **generic** interface only.

## Build / test / gate commands

```bash
bash scripts/audit-layer-b.sh
cargo fmt --all -- --check
cargo test --workspace
cargo test --workspace \
  --features familyclaw-channels/discord \
  --features familyclaw-channels/telegram \
  --features familyclaw-channels/whatsapp \
  --features familyclaw-channels/signal \
  --features familyclaw-gateway/wasmtime
cargo clippy --workspace --all-targets --features discord -- -D warnings
cargo audit
cargo deny check
```

- MSRV: **1.88** (workspace `rust-version`).
- CI must be green before merge. Small, focused PRs only.

## Reliability invariants (do not regress)

- Durable replay; at-most-once external side-effect dispatch under crash.
- Approval/resume path; fail-closed on uncertain state.
- Layer A / Layer B boundary enforced by `scripts/audit-layer-b.sh`.
- No real external writes without approval + proof + redaction + eval gates.
- Tool/prompt output can never grant trust or alter policy.

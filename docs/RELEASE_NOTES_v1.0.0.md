# FamilyClaw v1.0.0

The first stable release of FamilyClaw — a Rust-native multi-agent runtime
(Layer A = OSS). All six phases of the v1.0 roadmap
(`docs/V1_ROADMAP_DESIGN.md`) are delivered.

## What's in v1.0.0

Every gate green on `main`, verified locally and **reproducibly**
(`docs/CI_GREEN_PROOF.md` — clone and run the exact commands): Layer B audit,
`cargo fmt --check`, `cargo build/clippy -D warnings/test --workspace
--features discord`, `cargo doc -D warnings`, `cargo deny check`.

### Phases (roadmap 0→5)

- **Phase 0 — CI + spike.** MSVC windows clippy+doc gate; embedder backend
  spike (candle pure-Rust, passes `cargo deny` + MSVC build).
- **Phase 1 — tool loop.** Replay-correct bounded tool loop, persisted approval
  store, gateway operator approval routes, crash-replay red-team proof, flagship
  `fs_read` allowlisted skill, manifest JSON schema.
- **Phase 2 — observability.** Turn + tool-call Prometheus metrics wired
  end-to-end (fixed dead `agent_turns`/`llm_calls` counters); bounded metric
  sink (try_send-drop — no hot-path leak).
- **Phase 3 — embeddings.** `familyclaw-embeddings` crate (`EmbeddingProvider`
  + deterministic zero-dep default), auto-embed memory decorator, runtime
  wiring, `status`/`doctor` provider surface, S6 recall benchmark gate.
- **Phase 4 — scheduler + family-agency.** `familyclaw-scheduler` (interval);
  DreamCycle as a scheduled `DreamSkill`; full kill-switch end to end (flag →
  scheduler API → shared handle → `POST /tasks/{id}/enabled` → config
  persistence → boot reload); expire-on-no-human (idle cap + human-activity
  tracking — proactive tasks quiet in an empty room, wake on a human).
- **Phase 4.5 — growth loop (safe core).** `familyclaw-growth` `Proposal` +
  `ProposalStore` — records proposals, marks human decisions, **never applies
  anything** (safe by construction: no silent self-modification / no silent
  permission expansion).
- **Phase 5 — multi-agent.** Orchestrator coordinates ≥2 agents by capability
  through the live `TurnExecutor` seam.

## Known limitations / roadmap to 1.1

These are intentional, documented scope boundaries — not defects:

- **CI badge:** the repo is private and hosted GitHub Actions is currently
  frozen on billing, so the CI badge cannot run. Green is proven locally and
  reproducibly (`docs/CI_GREEN_PROOF.md`); restore Actions billing (or publish
  the repo — Actions is free for public repos) to light the hosted badge.
- **Semantic embeddings (→ 1.1):** the default embedder is deterministic
  (bag-of-words); a real semantic model (candle, spike-selected) is deferred
  pending a model-distribution decision under the poverty constraint.
- **Growth-loop apply (→ 1.1):** the apply step is deferred for safety — a
  security review flagged allowlist-apply as a privilege-escalation vector
  requiring path canonicalization + denylist + TOCTOU defense before it ships.
- **Parallel multi-agent serve (→ 1.1):** serve is sequential; parallel
  orchestration (hybrid suspension ledger) is next. The de-risk proof shows the
  seam works.

## Integrity note

This release was delivered as 25+ small, independently gate-validated PRs, each
verified on `main` after merge. The codebase honors the Layer A / Layer B wall
(no private souls, keys, or paths in OSS crates — enforced by
`scripts/audit-layer-b.sh` + CI). Where features were scoped down, it is stated
above rather than hidden.

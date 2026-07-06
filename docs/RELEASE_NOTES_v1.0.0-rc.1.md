# FamilyClaw v1.0.0-rc.1 — Release Candidate

> A **release candidate** for the v1.0 roadmap (`docs/archive/V1_ROADMAP_DESIGN.md`).
> All six roadmap phases are delivered at their v1.0 scope; this is tagged `-rc`
> (not final `v1.0.0`) for two honest reasons stated under "Why -rc" below.

## What's in this RC

All gates are green on `main`, verified locally and reproducible
(`docs/CI_GREEN_PROOF.md`): Layer B audit, `cargo fmt --check`,
`cargo build/clippy -D warnings/test --workspace --features discord`,
`cargo doc -D warnings`, `cargo deny check`.

### Phases (roadmap 0→5)

- **Phase 0 — CI + spike:** windows MSVC clippy+doc gate; embedder backend
  spike verified (candle pure-Rust, passes deny + MSVC build).
- **Phase 1 — tool loop:** replay-correct bounded tool loop, persisted approval
  store, gateway operator approval routes, crash-replay red-team, flagship
  `fs_read` skill, manifest JSON schema.
- **Phase 2 — observability:** turn + tool-call Prometheus metrics, wired
  end-to-end (fixed previously-dead `agent_turns`/`llm_calls` counters); bounded
  metric sink (try_send-drop, no hot-path leak).
- **Phase 3 — embeddings (infrastructure):** `familyclaw-embeddings` crate
  (`EmbeddingProvider` + deterministic zero-dep default), auto-embed memory
  decorator, runtime wiring, `status`/`doctor` surfaces the provider, S6 recall
  benchmark gate.
- **Phase 4 — scheduler + family-agency:** `familyclaw-scheduler` (interval),
  DreamCycle as a scheduled `DreamSkill`; **full kill-switch end to end**
  (flag → scheduler API → shared handle → HTTP `POST /tasks/{id}/enabled` →
  config persistence → boot reload); **expire-on-no-human** (idle cap +
  human-activity tracking).
- **Phase 4.5 — growth loop (safe core):** `familyclaw-growth` `Proposal` +
  `ProposalStore` — records proposals, marks human decisions, **never applies
  anything** (safe by construction; no silent self-modification / no silent
  permission expansion).
- **Phase 5 — multi-agent (de-risked, sequential):** Orchestrator coordinates
  ≥2 agents by capability through the live `TurnExecutor` seam.

## Why `-rc` and not final `v1.0.0`

1. **Hosted CI cannot run** — Actions is frozen on this private repo (org
   billing). Main is proven green locally + reproducibly (`CI_GREEN_PROOF.md`),
   but a final `v1.0.0` should carry a hosted, third-party-observable green run.
   Restore billing **or** publish the repo (Actions is free for public repos),
   then the hosted run is authoritative.
2. **Deliberately deferred to post-1.0 / 1.1** (documented, not hidden):
   - Phase 3 ships the *deterministic* embedder; a real semantic (candle) model
     is post-1.0 (needs a model-distribution decision under the poverty
     constraint).
   - Phase 4.5 ships only the *recording* core; an approval-gated *apply* step
     is deferred for safety (a security review flagged allowlist-apply as a
     privilege-escalation vector requiring path canonicalization + denylist +
     TOCTOU defense before it is safe).
   - Phase 5 serve is *sequential*; parallel multi-agent (hybrid suspension
     ledger) is 1.1 — the de-risk proof shows the seam works.

## Cutting the final v1.0.0

When the maintainer restores Actions billing (or publishes the repo) and the
hosted CI run on `main` is green, bump the workspace version `0.1.0 → 1.0.0`,
tag `v1.0.0`, and this RC's scope becomes the release.

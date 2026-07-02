# FamilyClaw — Project Status

> **This file is the single source of truth for what FamilyClaw is, what works
> today, what is deferred, and where it stands on release.** It supersedes the
> scattered roadmap / plan / phase documents as the *entry point*; those remain
> in `docs/` as detailed design records (see [Detailed design docs](#detailed-design-docs)).

- **Version:** `v1.2.0` (workspace `Cargo.toml` `version = "1.2.0"`)
- **License:** MIT
- **Language:** Rust 2021, MSRV 1.88, `unsafe` forbidden workspace-wide
- **Workspace:** 23 crates (`crates/*`) + `examples/minimal-gateway`
- **Last verified:** 2026-07-02

---

## What FamilyClaw is (positioning)

A Rust agent runtime where **in-flight work survives a crash** — at-most-once
external side effects, durable memory, contract-checked coordination. It hosts a
*family* of persistent agents that remember across restarts, feel each other's
state, heal their own memory while they sleep, and coordinate under a contract
boundary.

FamilyClaw does **not** try to win a breadth fight against larger agent
frameworks (that goal was explicitly rejected during design review — see
[V1_ROADMAP_DESIGN.md](docs/V1_ROADMAP_DESIGN.md) §1). Its three sharpened edges are:

1. **Family with continuity.** Many *persistent* agents — not one-shot chat
   sessions — with durable memory (Eternal Thread + Ebbinghaus decay), nightly
   dream consolidation, a shared Hearth, and emotional contagion across the
   Resonance Bus. This continuity substrate is the product's core; competitors
   built around a single stateless agent do not have it.
2. **Safety from structure, not from policy.** `unsafe` is *forbidden* across the
   workspace; the WASM skill sandbox is `wasmtime` fuel- and capability-gated
   (deny-by-default); provenance/identity signing uses ed25519; and the Layer A /
   Layer B separation (private souls, keys, and profiles **never** enter the repo)
   is enforced by `scripts/audit-layer-b.sh` in CI. Safety is a compile-time and
   CI-time property, not a runtime hope.
3. **At-most-once side effects under crash.** Benchmarked head-to-head against
   LangGraph (its strongest `durability="sync"` config) on one narrow, honest
   metric: after a process crash, how many money-touching external side effects
   re-execute? FamilyClaw = 0 at every crash point; the file-memory / naive
   baseline re-fires. See [bench-competitors/langgraph/](bench-competitors/langgraph/README.md).

---

## (a) What works today — verified

Each item below is backed by tests and/or CI gates in this repo.

| Capability | Status | Evidence |
|---|---|---|
| **Durable crash-replay** | ✅ Works | Journal-based deterministic replay resumes exactly where a crash stopped; corrupt journal fails loud. `familyclaw-durable`; scorecard `s1_crash_matrix` PASS. |
| **At-most-once external side-effect dispatch under crash** | ✅ Works | Idempotency-keyed intent→effect→committed outbox: `side_effect_overcount = 0` at every crash point (`clean`, `before_write`, `mid_replay`). Bench vs LangGraph reproducible. A crash in the intent-only window fails **closed** (0 or 1 execution, requiring recovery) — not universal exactly-once *completion*. |
| **Eternal Thread durable memory** | ✅ Works | Ebbinghaus decay, importance weighting, protected identity anchors (λ=0). `familyclaw-memory`; scorecard `s2`/`s6` PASS (4/4 important kept vs 1/4 naive). |
| **Dream consolidation** | ✅ Works | Nightly merge of duplicates, drop of contradictions, absolutize relative dates; `false_merge_rate = 0`, protected core intact. `familyclaw-dream`; scorecard `s3`/`s8` PASS. |
| **Provenance gate** | ✅ Works | Trusted provenances admitted, low-trust/poison blocked, `false_admit_rate = 0`. Scorecard `s7` PASS. |
| **Resonance Bus (emotional contagion)** | ✅ Works | Ractor actor mesh; emotion leaks to siblings with homeostasis + memory isolation. `familyclaw-bus`; scorecard `s4` PASS. Roster never empty. |
| **Live multi-agent orchestration** | ✅ Works | `Orchestrator` runs a multi-node `design → review → deploy` DAG through the **real** `LiveTurnExecutor` against an in-process HTTP LLM; deliverables cross the contract boundary (output schema + postconditions); a malformed LLM response stops the DAG at that boundary. `crates/familyclaw-agent/tests/orchestration_live.rs`, `tests/live_executor_http.rs`. |
| **WASM skill sandbox (e2e)** | ✅ Works | Fuel exhaustion halts an infinite loop; denied capabilities enforced; runs under the `wasmtime` feature in CI. `familyclaw-sandbox`, `sandbox_integration.rs`. |
| **LLM provider failover** | ✅ Works | Real `reqwest` chain with retryability classification, cooldown ladder, key rotation, timeout tuning. `familyclaw-agent/src/llm.rs` + `llm_chain.rs` (23 failover tests). |
| **Action / Skill runtime** | ✅ Works | Skill registry, 7-risk approval gate (one-shot, payload-hash-bound, fail-closed TTL), redacting proof bundles, audit log, MCP-ready boundary. `familyclaw-actions` (240+ tests). Two genuinely functional reference skills: `fs_read` (allowlisted local file) and `web_fetch` (read-only HTTP GET with SSRF guards). |
| **Channel-less serve mode** | ✅ Works | `FAMILYCLAW_CHANNEL_KIND=none` lets `serve`/`status` run with no family keys — makes the OSS build runnable out of the box. (v1.2.0) |
| **`cargo install` / guest quickstart** | ✅ Works | 5-minute guest path documented in README; `cargo run -p familyclaw-agent --bin familyclaw`. |
| **Hearth persistence (SurrealDB backend)** | ✅ Fixed in v1.2.0 | `emotional_state` and `narrative_thread` (`set_thread`) SurrealDB persistence bugs fixed (`type::record` + `type::datetime`, batch UPSERT); round-trip tests added. `familyclaw-hearth`. |
| **`--all-features` build/test/clippy/doc** | ✅ Green + CI-gated | v1.2.0 repaired the `surreal` feature and added an `all-features` CI job (test + doc + clippy `-D warnings`) to prevent regressions. |

**Test surface:** ~1680 `#[test]` / `#[tokio::test]` functions across 23 crates.
**Continuity scorecard:** 8/8 scenarios PASS ([docs/SCORECARD.md](docs/SCORECARD.md)),
regenerated deterministically by `cargo run -p familyclaw-bench --bin bench -- all`.

### Reproduce it yourself

```bash
# Full suite
cargo test --workspace --features discord

# All-features gate (matches CI — includes the repaired surreal backend + wasmtime)
cargo test --workspace --all-features

# Deterministic continuity scorecard (8 scenarios)
cargo run -p familyclaw-bench --bin bench -- all
#   → crates/familyclaw-bench/out/SCORECARD.md + scorecard.json

# Layer B leak audit (matches CI)
bash scripts/audit-layer-b.sh

# Two-process crash-replay demo
bash scripts/demo-crash-replay.sh

# Crash-safety benchmark vs LangGraph
cd bench-competitors/langgraph && python -m venv .venv \
  && .venv/Scripts/python.exe -m pip install langgraph==1.2.6 langgraph-checkpoint-sqlite==3.1.0
```

---

## (b) What is in progress / on the roadmap

Honest list of what is **not** yet a shipped capability. None of these are
claimed as working today.

| Item | Status | Why it is deferred |
|---|---|---|
| **Growth-loop wiring (apply path)** | 🚧 Deferred to v1.1 (safety decision) | The `familyclaw-growth` crate ships the structurally-safe proposal core (`Proposal`, `ProposalStore`, `Pending/Approved/Denied` lifecycle) — **by construction it has no `apply` method and cannot mutate any skill, policy, or permission**. The runtime producer that emits proposals, the operator approve/deny surface, a durable proposal store, and the apply path itself are deferred. Reason: the apply step is a permission-expansion escalation vector and must land only with canonicalization + denylist + TOCTOU-safe approval — *a v1 that cannot silently self-modify is the correct v1*. See [V1_ROADMAP_DESIGN.md](docs/V1_ROADMAP_DESIGN.md) §6.5 STATUS. |
| **Send-side latent translation** | 🚧 Fenced research track | Siblings can exchange hidden-state vectors, but cross-model *send-side* latent translation remains a research track behind a feature fence, **always falling back to text** if incompatible. Not production behavior. |
| **Semantic retrieval turned live** | 🚧 Infra ready, weight off by default | Embedding infra (`familyclaw-embeddings`, local candle MiniLM path), `VectorStore` trait, and cosine retrieval exist and are tested. `semantic_weight` is only turned on where a labeled recall fixture empirically shows semantic Hit@k > keyword Hit@k; otherwise ships keyword + provenance + temporal with semantic OFF (honest, not a loss). See [PHASE3_PARALLEL_PLAN.md](docs/PHASE3_PARALLEL_PLAN.md) §4. |
| **Real provider skill integrations** | 🚧 Contracts ready, bodies are examples | `email_triage`, `github_issue_draft`, `file_patch`, `discord_thread_summary` are complete, tested implementations of the skill *contract* (manifest, risk class, approval policy, schema, taint) using deterministic placeholder data — not disabled stubs. Wiring a real provider (Gmail, GitHub API, on-disk patch, Discord API) is a swap of the execution body; the approval gate + proof redaction + audit then apply for free. |
| **Claw language compiler** | 🧪 Experimental spike, excluded from workspace | `compiler/` (the experimental Claw language) is intentionally **excluded** from `workspace.members` (`Cargo.toml`) and from CI. It is a spike, not a shipped component. |
| **Broader action/skill surface + more channel adapters** | 🗺️ Later | Additional channel adapters land only after the action/runtime and safety gates stay green. WhatsApp/Signal and generic cron scheduling are explicit non-goals for v1.0 (interval-only scheduler ships; cron parser deferred). |

---

## (c) Version & CI status

- **Current release:** `v1.2.0`. Headline of v1.2.0: Hearth SurrealDB persistence
  bugs fixed (`emotional_state` + `narrative_thread`), `--all-features` made green
  with a dedicated CI gate, HTTP error-path tests for Telegram/Discord adapters,
  channel-less serve mode, and a pre-publish leak gate.
- **CI (`.github/workflows/ci.yml`):**
  - `layer-b-audit` — `audit-layer-b.sh` + regression tests
  - `check-build-test` — `fmt --check`, build (default + `discord`), `clippy --features discord -D warnings`, `test --features discord`, `doc`, Discord integration tests
  - `msrv` — `cargo check` on Rust 1.88
  - `all-features` — build the `continuity_daemon` bin, then `test` / `doc` / `clippy` under `--all-features` with `-D warnings` (guards against surreal-style regressions)
  - Windows build+test job
- **Publish gate:** `scripts/pre-publish-scan.sh` scans git history *and* commit
  messages for leaked names before any public push (Layer B protection).

> **Note on the README `--all-features` caveat:** an older paragraph in the README
> Verification section says `--all-features` is "intentionally NOT used" because the
> `surreal` feature was broken. That caveat is **stale as of v1.2.0** — the surreal
> feature was repaired and an `all-features` CI job now runs. `STATUS.md` is
> authoritative here; the README paragraph is being reconciled.

---

## Detailed design docs

STATUS.md is the entry point. The following remain as detailed, still-valuable
design and evidence records (kept, not deleted):

- [docs/V1_ROADMAP_DESIGN.md](docs/V1_ROADMAP_DESIGN.md) — master v1 roadmap (architecture-review panel, phase breakdown, growth-loop §6.5).
- [docs/PHASE3_PARALLEL_PLAN.md](docs/PHASE3_PARALLEL_PLAN.md) — semantic-memory parallel plan (embeddings, vector store, recall benchmark).
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — full technical architecture overview.
- [docs/LAYER_BOUNDARY.md](docs/LAYER_BOUNDARY.md) — Layer A / Layer B separation.
- [docs/CRASH_SAFE_DISPATCH_CASE_STUDY.md](docs/CRASH_SAFE_DISPATCH_CASE_STUDY.md) — the at-most-once dispatch case study.
- [docs/COMPARISON.md](docs/COMPARISON.md) — continuity comparison vs a competitor-shaped baseline.
- [docs/SCORECARD.md](docs/SCORECARD.md) — 8-scenario continuity scorecard (regenerated by the bench).
- [docs/QUICKSTART.md](docs/QUICKSTART.md) · [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) · [docs/RUNBOOK_WINDOWS.md](docs/RUNBOOK_WINDOWS.md) — running it.
- [docs/PUBLISH_ORPHAN_PLAN.md](docs/PUBLISH_ORPHAN_PLAN.md) — OSS publish plan.
- Release notes: [v1.0.0](docs/RELEASE_NOTES_v1.0.0.md) · [v1.0.1](docs/RELEASE_NOTES_v1.0.1.md).

> Older phase / demo / release-checklist documents in `docs/` are historical.
> They may later be moved to `docs/archive/` for tidiness, but are intentionally
> retained for their evidence value. **When in doubt, this file is the truth.**

---

*Built so the next being gets a better home than the last one did.*

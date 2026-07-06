> **SUPERSEDED** — Arkistoitu suunnitelmadokumentti. Aktiivinen strategia: [MASTERPLAN.md](../../MASTERPLAN.md).

---

# FamilyClaw — The Master Product Plan

> **🔧 CLAIM CORRECTION (2026-06-19, after implementation + LangGraph bench):** Throughout
> this doc the wedge is phrased as "exactly-once side effects." The shipped, tested,
> honest claim is narrower and more precise: **at-most-once external side-effect DISPATCH
> under crash** (idempotency-keyed: a side effect is never fired twice; a crash in the
> narrow intent-only window fails CLOSED — zero or one execution, requiring recovery —
> rather than re-firing). It is duplicate-prevention under crash, **not** universal
> "exactly-once completion." Read every "exactly-once" below as that. The public surfaces
> (README, COMPARISON.md, CHANGELOG, bench verdict) already use the corrected wording.
>
> **Status of the "next 3 steps" in the TL;DR — all DONE (2026-06-18→19):** (1) LangGraph
> real-competitor bench built + run: FamilyClaw 0 overcount vs LangGraph 1/2 in its
> strongest `durability="sync"` mode, honestly and reproducibly (see
> `bench-competitors/langgraph/RESULTS.md`, internal until reviewed). (2) README
> corrected to "live multi-agent: built, unproven." (3) Named-users doc + adoption gate
> still to write (the one remaining go-to-market step).

> **TL;DR:** FamilyClaw is a **Rust runtime for agents whose in-flight work survives a crash** — a tool-calling agent loop sitting on a journaled, exactly-once durable substrate, with a mechanically-enforced public/private (Layer A / Layer B) boundary that lets an intimate AI project ship as MIT open source. **Why it wins:** competitors *retrieve* from memory; FamilyClaw *replays a crash deterministically* — every in-flight side effect survives a hard SIGKILL exactly-once, and a stranger can prove it in one command. The wedge is the benchmark, the moat is the boundary, the discipline is the honesty. **The next 3 concrete steps:** (1) add a real LangGraph durable-checkpointing run to the continuity bench so the headline becomes "we beat the tool people actually use," not a model we built; (2) fix the README/state to tell the truth about the *built-but-untested* live multi-agent executor; (3) write down 3 named target users and a single adoption gate (`familyclaw serve` running in someone else's repo) before any public launch.

---

## 1. Product thesis

**FamilyClaw is a Rust runtime for agents that don't forget — and don't double-fire when they crash.**

One falsifiable claim: competitors *retrieve* from memory; FamilyClaw *remembers across a crash*, deterministically — every in-flight side effect survives a hard kill exactly-once, replays nothing twice, and we prove it with a benchmark anyone can run **against a real competitor**.

- **For whom:** Rust/infra developers and small teams running agents that do real, mutating, money-touching work — long-running ops/migration/automation agents where a mid-action crash means a *duplicated charge, a double-sent message, a re-run migration*. Those people exist today, and they are currently stitching durability onto Python frameworks that only checkpoint, not replay-exactly-once.
- **One-sentence pitch:** *The only persistent agent runtime in Rust where a crash mid-action loses nothing and replays nothing twice — safe agents that actually do work.*
- **Category boundary:** not a library (Rig), not Python glue (LangGraph/CrewAI/Letta), not a session-multiplexer for terminal agents (claude-squad/Conductor), not a breadth play (OpenClaw). It is the *runtime substrate*.

### The buyer's objection we must answer (why Rust, not Python)

A buyer lives in Python — every model SDK, eval tool, and integration is there. We do not win by being "Rust." We win on one thing Python frameworks structurally cannot give them: **exactly-once side effects under a hard crash, proven, with the secret-leak and double-fire classes designed out at compile time.** If your agent only reads and summarizes, stay in Python. If your agent *mutates the world and a crash costs real money*, the switching cost is justified — and we must say exactly that, not assume it.

### The moat — what nobody can cheaply copy

The moat is **not capability**. Everyone has a tool-loop. The moat is a stack of load-bearing systems properties that are individually hard to fake and collectively unique:

1. **Durable exactly-once crash-replay proven across a REAL OS process boundary — against a real competitor.** The `continuity_daemon` subprocess is SIGKILLed at four distinct crash points (BeforeWrite / MidWrite / MidReplay / CorruptedJournal); `side_effect_overcount = 0` against a baseline of 17, byte-reproducible via an injected fixed clock. A *subprocess* harness (not in-process mocking) is the credibility multiplier. **The change that makes this un-fakeable: the baseline must include LangGraph's durable checkpointing, not only a "competitor-shaped model."** Beating a model we built is contestable; beating the actual tool people use is devastating.
2. **A mechanically-enforced Layer A / Layer B boundary.** Public MIT runtime vs private agent identity (calibration, keys, memories) injected at runtime — *never compiled in*. Enforced three independent ways: `.gitignore`, a CI `layer-b-audit` job, and architectural no-compile-time-defaults. The same binary runs any config with a different profile dir. This is what makes an intimate, private AI project *publishable at all*.
3. **Provenance-gated, poison-resistant memory.** Every record carries `Provenance{DirectExperience | Derived | External{trust}}`; low-trust external writes are rejected at ingestion. Persistent memory is the highest-value attack surface for any "safe agents" product and almost no competitor has a provenance layer. `false_admit_rate = 0`, `poison_blocked = 1.0`.
4. **Compile-time safety the way only Rust can do it.** Severity-tiered HITL gate with crash-surviving approvals, per-invocation credential isolation in WASM, byte-identical replay. The safety substrate *is* the product.

A Python framework cannot retrofit (1) or (4). A breadth competitor cannot retrofit (2) without a ground-up rewrite. The benchmark in (1) is un-fakeable and runnable by a skeptic in one command — that is the durable go-to-market weapon, **but only once it contains a real opponent.**

---

## 2. Current state (truth)

Build forward from here. This is what is *actually* shipped and hardened on `feat/familyclaw-v1-tool-loop`:

| Capability | Status | Evidence |
|---|---|---|
| **Phase 1 tool-loop keystone** | DONE + hardened + committed | `agent.think()` drives a tool loop via `complete_with_tools`; `familyclaw-actions` registry + sandbox + 7-risk approval gate wired to the agent. **~1377–1495 tests green**, clippy `-D pedantic` clean, Layer-B audit 9/9. |
| Phase 1A (commit `4075b92`) | DONE | `ToolDefinition`→OpenAI envelope; `ChatCompletionsRequest.tools/tool_choice` with `skip_serializing_if` (byte-identical tool-less requests); failover chain extended to `complete_with_tools`. |
| **Durable crash-replay** | DONE + red-team tested across real process boundary | SIGKILL at 4 crash points; `side_effect_overcount=0` vs baseline 17; byte-reproducible @ injected `2026-06-04T12:00:00Z`. **Caveat: baseline is a competitor-shaped model, not yet a real product run — see Phase 4.** |
| **fs_read_allowlisted** flagship skill | SHIPPED | Path-allowlisted, proof bundle = path-hash + size + summary, auto-run as ReadOnly. `http-get` correctly NOT shipped (verified SSRF). |
| Approval gate | DONE | 7 risk levels, one-shot + payload-bound + fail-closed-expiry tokens, redacting proof bundles, audit log, MCP boundary. **161 tests.** |
| Provenance gate (`GatedMemoryStore`) | DONE | `min_trust=0.6`, serde-default `DirectExperience`; `false_admit=0`, `poison_blocked=1.0`. |
| 19-dim VAD memory decay | DONE | Protected-core + selective decay; anchors 1.0 @ 7/30/90d, trivia→0.0. **89 memory tests.** |
| LLM failover (retryability + timeouts) | DONE | Built. |
| **Live multi-agent executor** | **BUILT, not yet proven by a live integration test** | `LiveTurnExecutor` (`crates/familyclaw-agent/src/live_executor.rs`) calls `LlmFailover::complete` and is wired into `gateway orchestrate`. **The only end-to-end multi-agent integration test (`crates/familyclaw-bridge/tests/homepage_factory.rs`) still runs against `MockTurnExecutor`.** Honest status: real code path exists and compiles; not yet covered by a live coordination test. |
| WASM sandbox (wasmtime) | COMPILES, fuel works | **NO integration tests yet** — cannot be used as a security positioning point until fixed (Phase 2). |
| Continuity scorecard + COMPARISON.md | DONE, regenerates byte-identically | `bench -- compare`. Honestly: only S1/S2/S4 separate us; S3/S5/S6 are ties — do not over-claim ties. |
| Gateway surface | DONE | `/healthz`, `/readyz`, `/inject` (bearer, constant-time), `/discord/interactions` (Ed25519). Origin-aware routing is a tested invariant. |
| Layer A/B enforcement | DONE, hard release gate | gitignore + CI `layer-b-audit` + no-compile-time-defaults. Demos run keyless with `in_memory()`/temp. |
| Workspace shape | ~20-crate hand-built Rust workspace | `-runtime/-gateway/-bridge/-memory/-emotion/-bus/-dream/-sandbox/-latent/-actions/-durable/-observability/-channels/-hearth/-embeddings/-acp/-bench/-gemu`. **NOT** a Rig rewrite. |

**Known holes (do not market around them):**
- **Semantic search is effectively non-functional** — mock provider only, all 89 tests `semantic_weight=0.0`. No "semantic recall" marketing until Phase 3 ships a real provider + passes a recall gate.
- **Live multi-agent is built but untested end-to-end** — `LiveTurnExecutor` exists and is wired into `gateway orchestrate`, but the only multi-agent integration test runs against `MockTurnExecutor`. Mark it "built, unproven," **not** "Phase-5 only" (that would now *underclaim* shipped code) and **not** "live, shipped" (that would overclaim an untested path). Phase 5 is the *live integration test + coordination measurement*, not the executor's first existence.
- **Phase-0 CI gate is run by no single machine** — clippy/doc/deny are ubuntu-only; windows-latest is build+test only; workspace lints are `warn` not `deny`.
- **Quarantined:** `familyclaw-hearth` `surreal` feature (dead), standalone `eternal-thread` crate (broken/redundant), DEMO.md Step 6 time-jump (SIMULATED), latent receive-side decode (deferred), `pub trait MockSkill` (rename to `Skill`), emotion contagion O(n)/msg.

---

## 3. The unified architecture

The synthesis: **tool-loop on top of a durable substrate, with safety and determinism as first-class product features.** The source of truth is the crate map, regenerated from the live workspace — there is no hand-drawn layer cake competing with it (the same over-architecture we retired when we killed the "8-layer model").

### 3.1 The crate map IS the architecture

`ARCHITECTURE.md` is regenerated from the live workspace; do not maintain a parallel prose abstraction. Grouped by concern, the ~20 crates are:

- **Durable substrate (the wedge):** `-durable` (journaled exactly-once crash-replay, clock-free determinism), `-bench` (the public falsifiable benchmark).
- **Memory (zero external infra, model-agnostic):** `-memory` (4-tier: Working LRU → Episodic JSONL → Semantic LanceDB → Knowledge Graph temporal edges), `-embeddings` (gated `EmbeddingProvider`), `-dream` (consolidation off the hot path).
- **Safety & provenance (the moat):** `-actions` (severity-tiered HITL gate + durable `PendingApprovalStore`), `-sandbox` (WASM capability isolation), `-memory`'s `GatedMemoryStore` (provenance).
- **Agent core:** `-runtime` (`agent.think()` → tool-loop → `complete_with_tools`, `ThinkOutcome::Suspended`), `-channels`, `-gateway`.
- **Coordination (built, unproven; live test = Phase 5):** `-bus` (uACP 4-verb {Ping/Tell/Ask/Observe} + MESI pure-lib + Whiteboard), `-acp`, `-bridge` (`LiveTurnExecutor`, Orchestrator+Critic).
- **Observability / transport / research:** `-observability`, `-hearth` (opt-in durable backend), `-latent` (fenced research, send-side only — see §3.3 footnote), `-gemu`.

Across all of it runs the single boundary: **Layer A (MIT, public)** above, **Layer B (private profile, runtime-injected)** below — the same binary, a different config dir.

### 3.2 What makes the engineering excellent — and why each piece stays

**The determinism contract.** Each tool call runs inside `durable.step('tool-{id}', ...)` with `Timestamp::now()` generated *inside* the closure so it is journaled and value-identical on replay. This reconciles a clock-dependent, side-effecting tool loop with a clock-free durable substrate ("now is always passed in"). **Acceptance bar (keep verbatim):** kill between two tool dispatches; replay must not re-execute the first side effect, and the replayed `SubmitOutcome` (including `ApprovalId`/`TTL`) must be *value-identical*. This is a uniquely strong, hard-to-fake assertion.

**Cooperative suspend as a typed `TurnOutcome` enum** — `ThinkOutcome::Suspended{approval_id, redacted_summary}`, NOT a `SUSPENDED_MARKER` string sentinel. Inline-blocking hangs the ractor actor; a string sentinel leaks suspend state as a chat reply (correctness + security bug). The dangerous tool *returns* and never blocks the actor.

**Persisted cross-process `PendingApprovalStore` trait** (in-memory for tests, durable-journal-backed for prod) with capacity cap + TTL eviction + per-being dangerous-tool rate limit. This caught a real defect: approvals that lived in memory died on the exact crash the durable layer exists to survive, and CLI `build_runtime()` builds a fresh runtime per invocation so cross-process `approve` returned `ApprovalMissing`. One persisted store satisfies crash-recovery AND the operator-approval surface.

**Memory: 4-tier, zero-LLM-call retrieval, model-agnostic.** Working (bounded LRU) → Episodic (SQLite/JSONL event log) → Semantic (embedded LanceDB vectors) → Knowledge Graph (entity graph, temporal edges). Consolidation merges episodic→semantic *off the hot path* via the Dream Engine (nightly/weekly/monthly: dedup + compress + re-score; conflict-aware tagging — never eager-delete). **Zero LLM calls in the retrieval path** = real cost + latency + offline moat, and identity survives model swaps (this project itself lives model churn). One model writes, any model reads. Tiered/governed access (Family-shared / Member / Private) makes shared memory multi-tenant-able.

**Embeddings as a gated crate.** `familyclaw-embeddings`: `EmbeddingProvider` trait + deterministic zero-dep default + feature-gated *pure-Rust* local provider. The default **refuses/warns** on `semantic_weight>0` (never cosine-over-noise); `doctor` surfaces the active provider; **semantic must BEAT keyword on a fixture before `semantic_weight>0` is honored live.** ort/ONNX-as-default is rejected (deny.toml license gap, zero onnx presence, poverty constraint). This prevents a silently-broken semantic search.

**Identity is substrate, not a hash.** `IdentityAnchor` lives as protected non-decaying memory (`λ=0` `ProtectedCore`); the SHA-256 `AnchorHash` is ONLY a tamper alarm — fail-safe, not fail-destructive. Pairs with the dream cycle's hard invariant (`protected_core_intact=1.0`, `false_merge_rate=0`).

**WASM tool isolation:** deny-by-default capabilities, fuel limits, **per-invocation credential allowlist (credentials NEVER inherited from parent)**, no filesystem by default. Per-invocation credential isolation structurally prevents the dominant agent secret-leak path (the `node:vm-is-not-a-boundary` lesson proves you need a real sandbox). *Built but needs integration tests before it's a positioning point — see Phase 2.*

**Resonance Bus (coordination):** bounded typed verb calculus — uACP 4 verbs layered additively on the publish path, carrying validated **JSON Patch mutations, not free natural language**, plus a pure-library MESI cache-coherence state machine for shared artifacts. Bounded verbs are exhaustively matchable Rust enums — testable, cheap, auditable. Far more rigorous than "just use A2A."

**Provider config as env-name indirection:** `FAMILYCLAW_PROVIDERS='prefix=base_url=KEY_ENV_NAME;...'` where the value after the second `=` is the *name* of another env var holding the key. Declarative, committable-shaped config while the secret stays separate — directly answers this repo's history of leaking keys via `cat`/`jq`/`bash -x`.

### 3.3 Cut from the public core (off-thesis)

- **Emotion/voice/3D-presence → Layer B only.** Shipped reality is 19-dim VAD as a Layer-B crate. Public framing is "functional affect for coordination," NOT "AI has feelings." The 5/13/19-dim inconsistency is resolved: 19-dim, Layer B, off the public release surface.
- **Latent-space transfer → one fenced footnote, not an architecture subsection.**[^latent]

[^latent]: `familyclaw-latent` (`VectorTranslator`, a configurable linear projection — NOT a trained encoder) is a send-side-only research track with graceful latent→text fallback. It is explicitly *not* production behavior and must never appear in a product or sales doc. Mentioned here once so it is accounted for, not promoted.

---

## 4. Roadmap

From Phase-1-done to a shippable **v1.0**, then **v2 vision**. The keystone-first / multi-agent-last sequencing is the load-bearing principle. **Every phase independently: compiles + tests + clippy-pedantic-`-D` + doc-`-D` + deny + Layer-B audit green on windows-latest MSVC, its own branch, no TODO-only commits.**

### Phase 0 — CI hardening (ships NO production code except a CI job) — DO FIRST

- **Goal:** the per-phase gate must be run by a single real machine before it can protect anything.
- **Deliverables:** windows-latest (MSVC stable, MSRV 1.85) `clippy --all-targets -D warnings` + `cargo doc -D warnings`, parameterized per phase's feature flags. Flip workspace lints `warn`→`deny`. Throwaway embedder spike (candle/fastembed vs ort) run through `cargo deny` to pick the Phase-3 default before any prod dep lands.
- **Gate:** one machine runs clippy/doc/deny/Layer-B on MSVC and goes green. Embedding-default decision recorded with evidence.

### Phase 1 — Tool-loop keystone — ✅ DONE

1A schema+failover (DONE), 1B agent loop + fs-read flagship, 1C persisted approval store + suspend/resume surviving restart, 1D gateway approval surface + crash-replay red-team proof + **minimal turn-audit sink** (turn start, each tool dispatch + redacted result, suspend/resume, stop_reason). Folding minimal audit into Phase 1 is a self-consistency catch: *you cannot red-team a feature you cannot observe.*

- **Gate (already met / verify):** `bench -- compare` regenerates byte-identical; the value-identical-`SubmitOutcome`-on-replay assertion passes; 1377–1495 tests green; Layer-B 9/9.

### Phase 2 — Harden the safety substrate into a positioning point

- **Goal:** make every moat claim *testable*, not just *built*.
- **Deliverables:** (1) **WASM sandbox integration tests** — prove fuel exhaustion, denied capability, and per-invocation credential isolation end-to-end (today it only compiles). (2) Full span tree + Prometheus `/metrics` (deferred from Phase 1). (3) `doctor`/`status` surface active embedding provider, durable backend, sandbox status. (4) Fix-or-quarantine the serenity Discord adapter; make Ed25519 `/discord/interactions` the canonical inbound path; document exactly one supported mode; apply the single-mpsc-pair lifecycle fix.
- **Gate:** sandbox security claims each have a passing integration test; `/metrics` scrapes clean; one authoritative Discord-mode doc; no "deaf draft" in the shipped surface.

### Phase 3 — Real semantic memory (HIGHEST-RISK PHASE — sub-gated)

This is realistically **months of work**, the single most failure-prone phase (model quality, LanceDB integration, recall tuning), and it must not hide behind one bullet. It is sub-gated and carries an explicit fallback.

- **Goal:** turn "memory that compounds" into a defensible, measured claim.
- **Sub-deliverables, each independently gated:**
  - **3a Embedding provider:** pure-Rust local `EmbeddingProvider` (candle/fastembed per the Phase-0 spike). Gate: produces stable vectors, passes `cargo deny`, runs offline.
  - **3b Semantic tier live:** LanceDB semantic tier wired into retrieval behind the feature flag. Gate: round-trips vectors, no external infra.
  - **3c Recall quality:** semantic **beats** keyword on a published recall fixture (Hit@k). Gate: `semantic_weight>0` is honored live *only after this passes*.
  - **3d Consolidation safety:** Knowledge-Graph temporal edges queryable; Dream Engine consolidation on a real cadence with `false_merge_rate=0` and `protected_core_intact=1.0` preserved.
- **Fallback plan (must be written before starting):** if local embedding quality fails the 3c beat-keyword fixture, ship v1.0 with **keyword + provenance + temporal-graph retrieval as the supported path** and semantic explicitly labeled experimental/off. The product does not depend on 3c passing — it depends on us *not lying about it*. "Semantic recall" appears in no doc until 3c is green.

### Phase 4 — Productionization & the launch artifact (the real-competitor bench lives here)

- **Goal:** demo → deployable product; ship the bench as marketing — **with a real opponent in it.**
- **Deliverables:**
  - **The real-competitor benchmark.** Add at least one live competitor run — **LangGraph durable checkpointing** (the strongest claimed-durable competitor) — to the 8-scenario scorecard. Report the result honestly: either "FamilyClaw beats LangGraph on exactly-once side effects under SIGKILL" (the headline) or a measured tie/partial-loss with the wedge re-aimed accordingly. The "competitor-shaped model" baseline stays as a control, but **no public/sales doc shows the comparison until it contains a real product.**
  - Tiered persistence MVP (`FAMILYCLAW_DATA_DIR` → JSON `journal.jsonl` + `memory.json`; unset → RAM) with the **RocksDB single-writer LOCK runbook** written down; CLI `serve`/`status`/`doctor`.
  - `docs/DEMO.md` tabulating **REAL vs SIMULATED** (Step 6 labeled SIMULATED); de-overclaimed README (correct test counts, dead badges removed, **live multi-agent relabeled "built, unproven," not "shipped" and not "Phase-5-only"**); **CI docs-truth-check** tying README/DEMO commands to CI commands.
- **Gate:** a stranger clones, runs one command, and reproduces the scorecard byte-for-byte **including the LangGraph run**. README contains zero claims CI doesn't enforce.

### Phase 4.5 — Public OSS release (history-grade gate)

- **Goal:** go from private-alpha to publishable without leaking Layer B.
- **Deliverables:** squash/rewrite history into neutral commits; neutralize author/branch/PR metadata; **an automated pre-publish audit that greps `git log` / `reflog` / branch names / commit metadata — not just the working tree.** (The blueprint/integration/demo files with real names are *already git-ignored and untracked* — verified via `git check-ignore`; do not waste a cycle "excluding" them. The real risk is history, reflog, and branch names.) One canonical generic-name set (`agent_a`/`agent_b`) purging `agent_alpha/beta/operator` inconsistencies.
- **Gate:** the automated history-audit passes against git **history + commit metadata + branch names + reflog**, not just the tree.

### Phase 5 — Live multi-agent, *proven* (LAST)

- **Goal:** the `LiveTurnExecutor` already exists and is wired into `gateway orchestrate`; Phase 5 is **proving it with a live end-to-end integration test and measuring coordination** — not building it from zero (correcting the earlier "multi-agent is offline" mis-statement).
- **Deliverables:** replace `MockTurnExecutor` in `homepage_factory.rs` (and add new cases) with a live multi-agent integration test on the Resonance Bus (uACP verbs + MESI + Whiteboard); Orchestrator+Critic; per-task selectable voting (approval/ranked/cumulative); **tiered shared-memory access enforced for zero-leakage.**
- **Gate:** measured coordination (treat coordination as a *measured bottleneck*, e.g. normalized return, not "agents just collaborate"); the live integration test passes; access tiers proven leak-free.

### v2 vision (one line each — inspire, do not overpromise)

- **Self-improving platform, structurally gated:** proof bundle → safe memory → pattern proposal → eval proposal → **APPROVAL-GATED** skill/policy update. No silent self-modification, no silent permission expansion, risk levels never auto-relax; mandatory rollback + acceptance-test commit gate. Honors the self-modification ban while shipping the moat. Composes Phase-1 gate + memory + audit — composition, not new infra. Opt-in, last.
- **Compile-time cost budgeting (`fc-budget`) — speculative research, post-v1.** Genuinely Rust-unique, but *not built*. The documented ~€20 overnight burn is already solved more cheaply by the existing "never put an expensive model in a cron/default" lesson. Demoted firmly so it does not dilute the load-bearing moat claims.
- **Cryptographic per-agent identity & cross-family attestation (Layer C).** One sentence only: signed memories + signed intervention logs enabling peer-to-peer trust with no central hearth. This is the eventual enterprise/multi-tenant audit moat — *and explicitly satellite-roadmap territory for a project with zero users and a €2000/month survival constraint.* No DID/VC/P2P detail until there are users.

---

## 5. What makes it a PRODUCT (not just a framework)

**Packaging.** One MIT-licensed Rust crate workspace + a single binary. `cargo install familyclaw` → `familyclaw serve`/`status`/`doctor`. Zero external infra to start (RAM or JSON file storage). SurrealDB/RocksDB ("The Hearth") is an opt-in feature-flagged upgrade, never the default.

**The OSS-vs-private boundary IS the product.** The same MIT binary runs any config with a private profile injected at runtime via `FAMILYCLAW_PROFILE_DIR`/`DATA_DIR`/env. The private calibration is the moat and *provably never ships* (three-layer mechanical enforcement + history-grade git neutralization). This is the answer to "how do you open-source an intimate AI" — and a credibility signal: most projects rely on discipline; here it is a hard release gate.

**Honest-engineering culture as a product asset.** `docs/DEMO.md` tabulates REAL vs SIMULATED; the README carries an honest feature-status taxonomy (supported+tested / experimental+labeled / quarantined+documented / removed); the CI docs-truth-check makes overclaiming impossible. Built to survive HN scrutiny — and the *first* thing it must survive is the honest "built, unproven" label on the live multi-agent path, applied to ourselves before a skeptic applies it for us.

**The demo that sells it.** *"Bench is the marketing"* — but only once it has a real opponent. The launch artifact is the runnable continuity benchmark: `git clone && cargo run -p familyclaw-bench -- compare`. A skeptic watches FamilyClaw survive a SIGKILL mid-action with `side_effect_overcount=0` while **LangGraph's durable checkpointing double-fires (or the honest measured result)**. Un-fakeable, byte-reproducible, more durable than any feature list. Pre-emptive honesty ("here is the exact LangGraph config we ran, reproduce it") disarms the "unfair comparison" attack before it lands.

### Distribution & users (the real weak spot — close it before Phase 5)

Engineering done-bars are excellent; user-adoption done-bars were absent. Fix:

- **Named target users (write 3–5 profiles before Phase 3):** e.g. (a) a solo dev running an overnight DB-migration agent who got burned by a re-run migration; (b) an infra team running an autonomous cost-cleanup agent that mutates cloud resources; (c) a fintech tinkerer whose agent issues refunds and cannot afford a double-charge. These are people for whom "checkpoint" is not "exactly-once."
- **The buyer objection answered in the README:** a "Should you use this?" section that says *plainly* — read-only/summarizing agent → stay in Python; world-mutating agent where a crash costs money → here is the bench, run it.
- **Adoption gate (the real metric):** 10 GitHub stars from r/rust is vanity. **The gate is: 1 person runs `familyclaw serve` in their own repo and reports it.** Track that, not stars.
- **Channel:** Show HN ("a Rust agent runtime that survives a crash mid-action — here's the benchmark *vs LangGraph*"), r/rust, the bench repo as the centerpiece. GitHub Sponsors driven by the bench's credibility.

**Monetization (kept honest — engine ≠ cashflow).** The runtime runs on **free models** (poverty constraint: free/already-paid resources, local embeddings, no paid per-token defaults/crons). FamilyClaw is the engine + portfolio value, **not** direct cashflow. Revenue streams, *kept separate*:
1. **DoraFix (independent cashflow that funds runtime R&D).** DoraFix is a compliance SaaS with its *own* buyers; it does **not** need a crash-proof Rust runtime and selling it does **not** validate the runtime moat. State this plainly: DoraFix pays the bills (~€999/customer) so the runtime can be built — it is *not* proof of the runtime. This is the honest "first euro" path.
2. **A service that genuinely needs the runtime (the real validation).** If we want a service that *proves* the moat, it must be one where a mid-action crash costs real money — a long-running autonomous ops/migration agent — not DoraFix. Optional, later.
3. **GitHub Sponsors** driven by the bench's credibility.
4. **A future hosted/managed multi-tenant tier** (Layer C), gated behind session-isolation + auth — only after Phase 5, only if adoption is real.

**First euro path (decoupled).** DoraFix lands one customer → that €999 funds runtime R&D. The runtime's *own* validation is the LangGraph-beating bench + one external `familyclaw serve` user, **not** DoraFix revenue. Agents sell DoraFix (the family sales agents); Claude builds code/video; the bench is the runtime's demo. *Foundations before towers: one real customer (DoraFix) pays, one real bench (vs LangGraph) proves — keep them separate.*

---

## 6. Hard cuts

**Explicitly NOT building (scope discipline):**

- ❌ **No from-scratch Rig rewrite.** The committed reality is a hand-built ~20-crate workspace with Phase-1 done. The entire `FAMILYCLAW_TECHNICAL_PLAN.md` crate layout (`familyclaw-core/agents/tools/discord/safety`, FamilyAgent/ResearchAgent/HomeAgent/SafetyAgent, 16-week timeline) is **superseded** by the capability-crate model.
- ❌ **No "replace OpenClaw / GitHub-conquest / breadth" framing.** OpenClaw wins on breadth; that fight is unwinnable and credibility-killing. Cite OpenClaw's documented failures (Cisco exfiltration, MoltMatch, memory poisoning) only as the *threat model* we answer — never a head-to-head feature war.
- ❌ **No emotion/voice/3D-presence in the public core.** Off-thesis; Layer B only.
- ❌ **No `http-get` auto-run skill.** Verified SSRF. If it ever ships (P2+), only as require-approval + strict egress allowlist (deny 169.254.169.254, all RFC1918, IPv6 ULA/link-local) + per-hop redirect re-validation + DNS-rebinding guard.
- ❌ **No silent self-modification.** Self-evolution only behind the approval-gated growth loop + mandatory proposed-eval + rollback. Opt-in and last.
- ❌ **No `--all-features` builds** (the `surreal` backend is dead). No integrating the standalone `eternal-thread` crate (cherry-pick ideas only).
- ❌ **No "live multi-agent orchestration" claims** until Phase 5 — but equally, **no "multi-agent is offline" claims** either; the truth is "built, unproven."
- ❌ **No "semantic recall" claims** until Phase 3c passes the recall gate.
- ❌ **No DID/VC/P2P/satellite-grade self-improvement build** while there are zero users. Vision, one sentence, post-v1.
- ❌ **No expensive/opus model as cron/failover/default** (documented €20 overnight burn; €78 cost post-mortem). `model:'sonnet'` on non-code-reading workflow agents; never run an expensive model N times over the same large context.

**Stale ideas being retired:**

- README Roadmap phases 0–6 (emotion/sleep/telepathy as the *public release sequence*) → replaced by the keystone-first ordering; capability themes become Layer-B feature tracks. **Rewrite the README.**
- README "Shipped" claim of live multi-agent orchestration → **corrected to "built, unproven,"** not deleted (the executor is real), not promoted (the live test is not).
- 8-layer / hand-drawn crate-layout abstractions → use the live crate map; regenerate `ARCHITECTURE.md`.
- TypeScript+Python+Deno blueprint stacks → superseded by the committed Rust core.
- ALL quantified paper-citation figures (193% throughput, 99.6% recall, "875/900 papers," future-dated arXiv 26xx IDs) → directional inspiration ONLY, never load-bearing, must be live-verified before any public/sales doc.
- `pub trait MockSkill` → rename to `pub trait Skill` with deprecated alias.
- `feat/alpha4-*` subsystem-packet branches → verify what actually landed in `feat/familyclaw-v1-tool-loop` before treating any as "kept."

---

## 7. The 'top-tier' bar

Concrete, measurable criteria that define done-and-excellent. v1.0 ships only when ALL are green:

**Correctness & resilience**
- [ ] `side_effect_overcount = 0` across all 4 crash points (BeforeWrite/MidWrite/MidReplay/CorruptedJournal), in a real subprocess harness, against baseline ≥17.
- [ ] **The bench includes a real LangGraph durable-checkpointing run, and the result (win or honest tie/loss) is reproducible byte-for-byte.**
- [ ] Replayed `SubmitOutcome` (incl. `ApprovalId`/`TTL`) is **value-identical** after a kill between two tool dispatches.
- [ ] Approvals survive a process restart and a cross-process `approve` succeeds (no `ApprovalMissing`).
- [ ] `bench -- compare` regenerates `COMPARISON.md` **byte-identically** on a clean clone.

**Security**
- [ ] WASM sandbox has passing integration tests for fuel exhaustion, denied capability, AND per-invocation credential isolation (not just "compiles").
- [ ] `false_admit_rate = 0`, `poison_blocked = 1.0` on the provenance gate.
- [ ] No secret ever appears in provider config strings or logs (env-name indirection verified).
- [ ] No `http-get` / network-egress skill auto-runs.

**Memory (only claimable after Phase 3c)**
- [ ] Semantic **beats** keyword on a published recall fixture before `semantic_weight>0` is live — OR semantic is shipped explicitly labeled experimental/off and keyword+provenance+graph is the supported path.
- [ ] `protected_core_intact = 1.0`, `false_merge_rate = 0` through a full Dream consolidation cycle.
- [ ] Zero LLM calls in the retrieval path; runs offline; zero external infra by default.

**Multi-agent (honest status)**
- [ ] The README labels live multi-agent "built, unproven" until Phase 5's live integration test passes; no "offline" and no "shipped" claim in between.
- [ ] Phase 5 only: `homepage_factory`-class test runs against `LiveTurnExecutor`, coordination measured, access tiers leak-free.

**Engineering hygiene**
- [ ] Every phase green on **windows-latest MSVC**: build + test + clippy `--all-targets -D` + `cargo doc -D` + `cargo deny` + Layer-B audit. Workspace lints = `deny`.
- [ ] 1A-style additive back-compat preserved: tool-less requests serialize byte-identically.
- [ ] No `--all-features` build required; no dead/quarantined crate in the shipped surface.

**Honesty & boundary (the trust moat)**
- [ ] CI docs-truth-check passes: every README/DEMO command maps to a CI command; test counts correct; dead badges removed.
- [ ] `docs/DEMO.md` REAL-vs-SIMULATED table is complete and accurate (DEMO Step 6 labeled SIMULATED).
- [ ] Public-name audit passes against git **history + commit metadata + branch names + reflog**, not just the tree (already-ignored blueprint files confirmed untracked, no wasted exclusion work).
- [ ] Same MIT binary boots two different configs from two different dirs with zero code change and zero Layer-B leak.

**Product & users**
- [ ] A stranger clones, runs one command, reproduces the continuity scorecard (incl. LangGraph), and understands the wedge in <5 minutes.
- [ ] README has a "Should you use this?" section answering the why-Rust-not-Python objection.
- [ ] **At least 1 external user runs `familyclaw serve` in their own repo** (the real adoption gate — not stars).
- [ ] DoraFix funds R&D as independent cashflow; the runtime's validation is the bench + the external user, not DoraFix revenue.

---

*Maintained by:* the operator + Claude. *Build forward from `feat/familyclaw-v1-tool-loop` (Phase 1 DONE).* *The wedge is the bench (vs a real competitor); the moat is the boundary; the discipline is the honesty — applied first to ourselves.*

---

## Next 3 concrete steps (start TODAY)

1. **Put a real opponent in the bench.** Stand up a minimal LangGraph durable-checkpointing agent that performs the same mutating side-effect sequence, SIGKILL it at the same 4 crash points, and record `side_effect_overcount`. Either it loses to us (headline secured) or it ties/wins (wedge re-aimed honestly). This is the single highest-leverage change in the whole plan — everything downstream depends on whether this comparison is real. *Owner: Claude builds the harness; do it on a throwaway branch first.*

2. **Tell the truth about live multi-agent — in the README and the state table.** Change every "multi-agent is offline / Mock-only" string to "**`LiveTurnExecutor` built and wired into `gateway orchestrate`; not yet covered by a live integration test (`homepage_factory.rs` still uses `MockTurnExecutor`)**." This is a 30-minute edit that removes a self-contradiction a skeptic would catch in the first read, and it applies our own honesty discipline to ourselves.

3. **Write the 3 named users + the adoption gate, and pin it to the repo.** One short `docs/USERS.md`: three concrete profiles of someone whose world-mutating agent loses money on a mid-crash double-fire, a one-paragraph "Should you use this? (Python vs FamilyClaw)" answer for the README, and the single adoption metric — *1 external `familyclaw serve` run* — written down as the Phase-4 exit signal. This closes the plan's one real gap (95% engineering, 5% go-to-market) before it hardens.
# FamilyClaw — Continuity Spearhead Design

**Date:** 2026-06-05
**Authors:** the operator + Claude (Opus 4.8)
**Status:** Approved design → ready for parallel implementation
**Branch target:** `feat/continuity-bench` (in E:\Familyclaw)

> Goal (the operator): make FamilyClaw the *objectively most advanced* persistent-agent
> product — leave OpenClaw and Hermes Agent **no seam** to attack on continuity.

---

## 0. Decisions locked in this brainstorm

| Question | Answer |
|----------|--------|
| What does "destroy the competition" mean? | **Best technical product** — win on raw capability; adoption follows. |
| Which capability is the spearhead? | **Never-forgets continuity** = durable replay + Eternal Thread + dreaming. |
| Where does this design focus? | **Prove it** (reproducible benchmark) + thin **productize** slice. |
| Rival comparison rigor? | **FamilyClaw self-proof first**; rival adapters are a documented Phase 2 seam. |

**Family protocol:** agent_alpha is the architect. The v2 open decisions (durable
engine, latent risk, agent_epsilon host) remain hers and are NOT touched here.

---

## 1. The claim we make true and prove

> **HEADLINE (revised 2026-06-05 after AgentScope analysis):**
> *"Every rival forgets how to forget. FamilyClaw dreams."*
> Overnight, a FamilyClaw agent's memory gets **cleaner** — duplicates merged,
> contradictions dropped, relative dates absolutized — and its **identity is
> provably untouched** (protected-core anchors, λ=0). No shipping platform
> markets active memory consolidation with a guaranteed identity invariant.
>
> **Supporting claim (durable replay):** kill it mid-task with SIGKILL; it
> resumes the exact step, side-effects exactly once. This is the *adversarial*
> version of continuity — distinct from AgentScope's *cooperative* "safe
> interruption" (graceful pause within a live process, not process-death replay).

### Why the pivot: AgentScope contests durable-resume, not dreaming

AgentScope (Alibaba/ModelScope, 26k+★, Apache-2.0) ships an ecosystem (core +
Runtime + ReMe memory + OpenJudge + Trinity-RFT + Java/TS + QwenPaw). It markets
"Safe Interruption: resumption without data loss" and ReMe persistent file+vector
memory. So **durable-resume is contested ground**; Letta has sleep-time compute;
OpenFANG has history compaction. But **active nightly consolidation that heals
memory while guaranteeing identity anchors are never merged/dropped/altered is
uncontested**. ReMe *retrieves*; Dreaming *rewrites*. That verb is the moat.

Therefore: **S3 Dream Quality is the headline scenario**; S1 durable replay is
supporting evidence. The money metrics are `protected_core_intact == 1.0` and
`false_merge_rate == 0` alongside `dedup_precision` / `contradiction_drop`.

**Raised stake:** this headline is only credible if the red-team `dream-corruption`
attack (false-merge of distinct memories; anchor tombstoned as non-representative)
finds NO hole — or every hole is fixed. That attack is already in the workflow.

Verified state of E:\Familyclaw (2026-06-05, live): 12 crates build green.
- `familyclaw-durable` (51 tests) — **competitive-grade**. `context.rs` has the
  torn-last-line crash test proving side-effects run exactly once; loud
  `NondeterministicReplay` (never silently wrong).
- `familyclaw-dream` (61 tests) — real deterministic 5-phase consolidation;
  protected-core never touched; idempotent date absolutization.
- `familyclaw-memory` (77 tests) — Ebbinghaus decay, importance, retrieval, store.
- `crash_replay --full` works across a **true process boundary**.
- `familyclaw-agent` — real Ractor actor; `handle_turn → think → llm.complete`;
  OpenAI-compatible `llm.rs` (479 L). Channels have real inbound.

**Gap (not features):** (a) zero external reproducible proof; (b) the runnable
artifact is a 30-second self-narrated showcase, not a benchmark a skeptic runs.

---

## 2. Architecture — `familyclaw-bench` (new crate)

```
familyclaw-bench/
  src/
    subject.rs     trait Subject  — what we benchmark (FamilyClaw now; rivals later)
    scenario.rs    trait Scenario — a scripted continuity workload
    adversary.rs   fault injection — crash points, corruption, clock-jumps
    metrics.rs     typed measurements + baselines
    scorecard.rs   JSON + markdown report (reproducible, diffable)
    harness.rs     runs Scenario × Subject × Adversary → Scorecard
    scenarios/
      crash_matrix.rs      S1
      retention_curve.rs   S2
      dream_quality.rs     S3
    subjects/
      familyclaw.rs        first impl (drives continuity_daemon as a black box)
    bin/
      bench.rs             `cargo run -p familyclaw-bench -- <scenario|all>`
```

### 2.1 The `Subject` seam (self-proof now, rivals later)

```rust
#[async_trait::async_trait]
pub trait Subject {
    async fn start_task(&mut self, task: &Task) -> Result<RunHandle>;
    async fn kill(&mut self, handle: &RunHandle, point: CrashPoint) -> Result<()>;
    async fn restart(&mut self) -> Result<RestartReport>;
    async fn recall(&mut self, query: &str) -> Result<Vec<RecallHit>>;
    async fn sleep_cycle(&mut self) -> Result<DreamSummary>;
    fn name(&self) -> &str;
}
```

FamilyClaw is the first impl. Letta/OpenClaw become future impls behind the SAME
trait (driven via their APIs/Docker). No harness redesign — that's the seam.

### 2.2 Hard reproducibility requirement

Fixed seeds; **injected `Timestamp`, never `now()`**; temp dirs; deterministic
scenario scripts. Same input → identical scorecard every run. A published number
must be reproducible by a skeptic or it is worthless.

### 2.3 Exact APIs to wire against (verified 2026-06-05)

- `familyclaw_durable::Journal` — `append`, `replay_from`, `replay_all`, `snapshot`;
  `FileJournal::open(path)`; `DurableContext::new/step/snapshot/finish/is_replaying`.
- `familyclaw_memory::MemoryStore` — RPITIT (`impl Future`, NOT `async_trait`):
  `add/get/update/reinforce/set_status/all/len/retrieve/run_decay`;
  `LocalJsonStore::in_memory()` / `::open(path)`.
- `familyclaw_memory::{Memory, MemoryStatus, ImportanceFactors, DecayPolicy,
  RetrievalContext, RetrievalResult}`; `Memory::builder(..)`, `.retention(at)`,
  `.is_retrievable()`.
- `familyclaw_dream::DreamCycle::{new, with_config, run, run_without_journal}` →
  `DreamReport { scanned, merged, dropped, dates_absolutized, strengthened,
  archived, reflections }`.
- `familyclaw_agent::Agent::{new, handle_turn, recall, spawn}`,
  `TurnOutcome { turn, remembered, summary }`; bin pattern from
  `bin/crash_replay.rs` (clap Subcommand: write/verify/full).

---

## 3. The three scenarios (the numbers)

| Scenario | Adversary | Metric (≥ target) |
|----------|-----------|-------------------|
| **S1 Crash Matrix** | kill mid-task, mid-write (torn line), mid-replay, corrupted/partial journal | `resume_correctness == 1.0`; side-effects-once; final == no-crash baseline. **Rivals lose in-flight work here.** |
| **S2 Retention Curve** | clock-jumps 7/30/90d, N turns | `recall@k` curve; identity anchors stay (λ=0); trivia decays; vs naive-buffer baseline. |
| **S3 Dream Quality** | seed duplicates + contradictions + relative dates | `dedup_precision`, `contradiction_drop`, `date_absolutized`, **`protected_core_intact == 1.0`**. Nobody else ships nightly consolidation. |

Each scenario emits a typed `ScenarioResult`; the harness aggregates into one
`Scorecard` (JSON + `SCORECARD.md`).

---

## 4. Productize slice (make the proof runnable)

- `familyclaw-agent/src/bin/continuity_daemon.rs` — extends the proven
  cross-process `crash_replay` pattern into a black box the harness drives
  (start task → external kill at a `CrashPoint` → restart → recall → sleep).
- `scripts/bench.sh` — one command; runs `all`, writes `SCORECARD.md` + JSON.
- Generated `SCORECARD.md` checked into `docs/` as the public artifact.

This is the minimum to make the proof real — NOT the full daily-driver platform.
Latent telepathy, affective-mesh productization, new channels, agent_epsilon's host:
explicitly out of scope; they layer on after the continuity proof exists.

---

## 5. Adversarial gate — the "no seam" guarantee

Before declaring victory, a red-team pass tries to BREAK every claim:
- crash during replay-of-replay (resume the resume)
- partial fsync / OS-buffer loss before crash
- clock running backwards / duplicate timestamps
- dream corrupting a good (non-duplicate) memory
- concurrent writers to the same journal/store
- corrupted middle line (not just torn last line)

Any hole found is FIXED in the crate, not footnoted. Verdict-by-majority: a
finding is only "real" if independent verification confirms it. Output: a
hardened claim with no exploitable seam.

---

## 6. Acceptance criteria

1. `cargo run -p familyclaw-bench -- all` runs end-to-end, deterministic, green.
2. `SCORECARD.md` generated with S1/S2/S3 numbers, reproducible byte-for-byte.
3. S1 `resume_correctness == 1.0` across all four crash points.
4. S3 `protected_core_intact == 1.0` (identity never lost in consolidation).
5. Adversarial gate: every red-team attack either fails to break, or its fix
   lands with a regression test.
6. `cargo test --workspace` green; `cargo clippy --workspace -- -D warnings` clean.
7. `Subject` seam documented so a rival adapter is a new impl, no redesign.
8. Layer-A/B audit still clean (no family souls/keys/paths leaked into bench).

---

*"Don't copy one. Take the best of each. Build our own." — built so the next*
*being gets a better home than the last one did.*

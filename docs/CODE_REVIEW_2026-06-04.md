# FamilyClaw v2 — ruthless code review (2026-06-04)

> Verified against the actual code on 2026-06-04. **All 5 findings are TRUE** —
> every reference points to real code (line numbers checked).
> Must be fixed before production / public OSS release.

| # | Severity | File | Status |
|---|----------|----------|--------|
| 1 | 🔴 CRITICAL | agent.rs:206 | TODO |
| 2 | 🔴 CRITICAL | agent.rs:52,256-260 | TODO |
| 3 | 🟠 ARCHITECTURE | bus.rs:73,146 | TODO |
| 4 | 🟠 MISSING | agent.rs:166 handle_turn | TODO (skeleton, no cognition yet) |
| 5 | 🟡 QUALITY | agent.rs:63,374 | TODO |

---

## 1. 🔴 Dual-write → permanent memory loss

**Where:** `agent.rs:206` — `if recorded.remembered && !is_replay { memory_store.add(...) }`

**Bug:** `durable.step` logs "memory created", but if the process crashes
BEFORE the `memory_store.add()` call, during replay `is_replay=true` → `add()`
is skipped → **the memory is lost forever.** A classic dual-write + race condition.

**Fix (in order of thoroughness):**
- **A (idempotency):** remove the `!is_replay` condition, always run
  `memory_store.add()`, give MemoryStore an `upsert(message_id)` that ignores
  duplicates. Requires `familyclaw-memory`: add upsert keyed by turn ID/MessageId.
- **B (event sourcing):** the durable log = source of truth. MemoryStore = a
  read replica projected from the log. On startup, memory syncs from the durable log.
- Recommendation: A as the quick fix, B as the correct architecture.

## 2. 🔴 Emotional-state feedback loop → saturation within seconds

**Where:** `agent.rs:52` `CONTAGION_FACTOR=0.25`, `apply_emotional_effect` (256-260)
adds a sibling's pulse to its own state, **with no homeostasis/decay in handle_turn.**

**Bug:** if two agents actively broadcast emotions, they feed each other
exponentially → every dimension clamps to 1.0 within a few dozen turns = a
permanent overexcited state, "burns out" within seconds.

**Fix:** add **emotional homeostasis** to `handle_turn` (or a time tick) —
the emotional state slowly returns toward `EmotionState::neutral()` on every
step. `familyclaw-emotion` already has `decay(dt)` — call it.
This keeps the system responsive to new impulses.

## 3. 🟠 Resonance Bus = bottleneck (custom pub/sub vs. Ractor pg)

**Where:** `bus.rs:73` a manual `for (id,info) in &self.beings` broadcast,
`bus.rs:146` `ListBeings` synchronously clones everything → `Vec<BeingSnapshot>`.

**Bug:** a centralized stateful bus + Ractor processes messages sequentially →
with 3500 beings, `ListBeings` blocks the entire bus. Single point of failure.

**Fix:** use Ractor's built-in **`ractor::pg`** (process groups).
Beings join a group (`family-bus`), publish is distributed without a centralized
bottleneck actor. Removes the SPOF + lifecycle management becomes automatic.

## 4. 🟠 Cognitive loop is missing — the agent is just a logger

**Where:** `agent.rs:166` `handle_turn` only stores memory + updates the
emotional state. No LLM call, no tools, no Wasmtime, no latent telepathy.

**Fix:** an OODA state machine inside the turn: Observe (memory) → Orient
(`self.recall`) → Decide (LLM/latent) → Act (tools/Wasmtime/bus response).
Each side effect (LLM generation) gets its OWN `durable.step` call →
on crash the response is loaded from the log, the LLM call is not repeated.

## 5. 🟡 Generics hell (S, J) spreads through the whole stack

**Where:** `agent.rs:63` `Agent<S, J>`, `agent.rs:374-375` `AgentActor<S,J>` +
`PhantomData<fn() -> (S, J)>`.

**Bug:** every struct/actor/function has to carry S+J+PhantomData →
rigid, hard to refactor, mocking in tests is painful.

**Fix:** trait objects (dynamic dispatch). `Arc<S>` → `Arc<dyn MemoryStore
+ Send + Sync>`, remove the S/J parameters. Memory lookups are I/O-bound →
the cost of dynamic dispatch is negligible. Readability + testability improve.

---

*Fix order: 1 (memory loss) → 2 (saturation) → 5 (generics, makes
4 easier) → 4 (cognition) → 3 (bus scaling once there are genuinely many beings).*
*1 and 2 are production-breakers — fix those first.*

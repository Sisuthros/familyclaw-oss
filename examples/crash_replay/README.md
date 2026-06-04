# Crash Replay Demo
**Proving Durable Execution Survives Process Death**

This demo demonstrates FamilyClaw's killer feature: **deterministic replay** — kill the process mid-work, restart, and work resumes exactly where it stopped with side effects NOT re-run, and memory SURVIVES.

## What It Proves

| Property | What Happens |
|----------|--------------|
| **Work resumes** | In-flight operations resume at exact stopping point |
| **No double side effects** | Journal replay skips already-executed steps |
| **Memory survives** | Eternal Thread memories persist across restarts |
| **Bus state recovers** | Agents re-register, message order preserved |

## The Concept (not a separate binary — uses the same demo)

The `familyclaw-durable` crate provides a `DurableContext` with a `Journal` trait. Two implementations:
- `InMemoryJournal` — for demo/testing (what the living seed uses)
- `FileJournal` — for production (survives process death)

When an agent does work wrapped in `DurableContext::execute()`:
1. Step intent is written to journal BEFORE execution
2. On success, step marked COMPLETE
3. On crash/restart, journal replay reads steps
4. COMPLETE steps are SKIPPED (no double side effects)
5. PENDING/FAILED steps ARE re-run

## Memory + Durable = Crash-Proof Sibling Memory

The demo binary (`cargo run -p familyclaw-agent --bin familyclaw`) already proves the memory part:
- Agents store memories via `familyclaw-memory` (Eternal Thread)
- Memory store is independent of `DurableContext`
- In demo: `LocalJsonStore::in_memory()` — survives as long as process lives
- In production: `LocalJsonStore::file()` or `SurrealDB` — survives process death

**The full crash-replay proof requires `FileJournal` + persistent memory store**, which is Phase 1+ work. The living seed demo (Phase 0) uses in-memory for simplicity.

## Future: True Crash Replay Script

```bash
# This would be the full crash-replay proof (Phase 1+)
# ./scripts/crash-replay-proof.sh
#
# 1. Start agents with FileJournal + file-based memory
# 2. agent_a sends "deploy key is blue-raven"
# 3. agent_b receives and stores in memory
# 4. KILL process (SIGKILL)
# 5. Restart
# 6. Query agent_b: "What did agent_a tell you?"
# 7. agent_b answers: "blue-raven"
# 8. Show journal log proving no double-execution
```

## Current Status

- ✅ `familyclaw-durable` crate: Journal trait, InMemoryJournal, execution wrapper
- ✅ `familyclaw-memory` crate: Eternal Thread, Ebbinghaus decay, identity anchors
- ✅ `familyclaw-agent`: Composes both, demo proves memory + emotion + dream
- 🔄 **Next**: FileJournal implementation + integration test for crash-replay

---

**Layer A Only** — Generic agents, no private content.
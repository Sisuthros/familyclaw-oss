# FamilyClaw Demo Documentation

This document describes exactly what the current demos prove, what is experimental, and how to run them.

## Available Demos

### 1. Living Seed Demo (Default)

**Command:**
```bash
cargo run -p familyclaw-agent
```

**Duration:** ~30 seconds

**What This Proves (Executable Code):**

| Step | Component | Proven By |
|------|-----------|-----------|
| 1 | Resonance Bus startup | Bus spawns, accepts registrations |
| 2 | Agent spawning | 2 agents register on bus, listed via `bus.beings()` |
| 3 | MockChannel transport | Messages injected, envelope published to bus |
| 4 | Message exchange + memory storage | 3 messages exchanged, `memory.len()` = expected count |
| 5 | Emotion contagion | `agent_a` broadcasts pulse → bus delivers to siblings → `AffectiveBeing` absorbs it into its own `EmotionState` via `EmotionTransition::blend` (receive-side wired in alpha.5; verified by `pulse_over_real_bus_shifts_receiver_toward_sender`) |
| 6 | Time jump / memory aging | **SIMULATED** — logs show retention percentages but no actual clock advance |
| 7 | Dream cycle | **REAL EXECUTION** — `DreamCycle::run_without_journal()` merges 1 duplicate, logs `merged=1` |
| 8 | Memory retrieval | `retrieve()` finds stored messages |

**What Is SIMULATED (Not Real):**
- Memory decay curves (Step 6) — demo logs ~70%/~95% but doesn't actually advance time
- Identity-anchor preservation — logged but not verified against ProtectedCore memory

**Expected Output:** See `QUICKSTART.md` "What You'll See" section.

---

### 2. Crash Replay Demo

**Command:**
```bash
cargo run -p familyclaw-agent --bin crash_replay
```

**Duration:** ~5 seconds

**What This Proves (Executable Code):**

| Phase | Action | Verification |
|-------|--------|--------------|
| 1 | Create FileJournal + LocalJsonStore(file) | Files created at `/tmp/familyclaw-crash-demo/` |
| 1 | Agent handles turn, memory stored | `recall()` returns 1 hit immediately |
| 1 | Journal entry written | Raw JSONL shown: `step_completed` with `turn-0` |
| 2 | Reopen same journal/store | `FileJournal::open()` + `LocalJsonStore::open()` succeed |
| 2 | Journal replay | `replay_all()` returns 1 step (`turn-0`) |
| 2 | DurableContext replay mode | `is_replaying()` = true |
| 2 | Memory recall after restart | **SUCCESS** — same content recalled, retention=1.00 |

**Files Created:**
- `/tmp/familyclaw-crash-demo/agent.journal.jsonl` — append-only JSONL with fsync
- `/tmp/familyclaw-crash-demo/agent.memory.json` — atomic JSON with tmp+rename

**What This Demonstrates:**
- Crash-proof workflow steps (deterministic replay)
- Memory survives process boundary
- Journal tolerates truncated last line (see `FileJournal` tests)

---

### 3. Discord Webhook Send (Feature-Gated)

**Command:**
```bash
DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/... cargo run -p familyclaw-agent --features familyclaw-channels/discord
```

**Status:** Webhook send + **inbound gateway live** — Ed25519 signature verification, slash command parsing, route at `/discord/interactions` (default listen `127.0.0.1:8787`; see `handle_discord_interaction` in `gateway/src/main.rs`, `channels/src/discord_interactions.rs`). Success-path signature verification is regression-tested (alpha.7).

---

### 4. Multi-Agent Orchestration (Live, alpha.5)

**Command:**
```bash
# Built-in smoke plan (no LLM resolver needed to construct; LLM call needs FAMILYCLAW_PROVIDERS)
cargo run -p familyclaw-gateway -- orchestrate

# Custom DAG plan as JSON:
FAMILYCLAW_PLAN='{"id":"p","nodes":[{"id":"n1","title":"...","description":"..."}]}' \
  cargo run -p familyclaw-gateway -- orchestrate
```

**What This Proves:** The `orchestrate` subcommand assembles a `FamilyBridge`,
registers an online `Executor` worker, builds a `LiveTurnExecutor` (real LLM
chain via the same `build_resolver()` as `serve`), and runs
`Orchestrator::run_with` over a plan — printing the `RunReport`. This is the
**live entrypoint** for multi-agent DAG execution.

**Honest boundary:** runs on the bridge's own substrate (`EventBus` +
`AgentRegistry` + `TaskBoard`), **not** the `FamilyRuntime`'s ractor agents /
`ResonanceBus`. It makes DAG orchestration runnable with real LLM turns; fusing
it with the living runtime agents is separate, larger work.

---

### 5. Provenance-Gated Memory (alpha.4/.5)

`familyclaw-memory` carries a `Provenance` on every memory
(`DirectExperience` / `Derived` / `External{source, trust}`) and a
`GatedMemoryStore` decorator that rejects low-trust `External` writes at
ingestion (`ProvenanceGate::admit`) — a Sleeper-Memory-Poisoning defense
(arXiv 2605.15338). `DirectExperience`/`Derived` always admit, so wrapping an
existing store is backward-compatible. Proven by bench scenario `provenance_gate`.

---

## What Is Still Experimental

| Feature | Crate | Status |
|---------|-------|--------|
| Latent messaging (hidden-state channel) | `familyclaw-latent` / `familyclaw-bus` | Send-side `VectorTranslator` (scale/offset/matrix projection) wired into `BusLatentChannel` (alpha.6); **receive-side decode into agent cognition is intentionally deferred** (family-boundary: it would let a sibling's latent state flow into another being's cognition) |
| WASM sandbox execution | `familyclaw-sandbox` | Compiles, fuel metering works, no integration tests |
| Dream cycle with contradictions | `familyclaw-dream` | Works with journal, contradiction markers tested |
| Emotion contagion scaling | `familyclaw-bus` | Manual HashMap iteration — O(n) per message (see architecture debt) |

---

## Verification Commands

Run locally to verify everything works:

```bash
# Full test suite (1209 tests)
cargo test --workspace

# Feature matrix (matches CI exactly — living features only)
cargo test --workspace \
  --features familyclaw-channels/discord \
  --features familyclaw-channels/telegram \
  --features familyclaw-channels/whatsapp \
  --features familyclaw-channels/signal \
  --features familyclaw-sandbox/wasmtime

# NB: `--all-features` is intentionally NOT used — the `surreal` feature in
# familyclaw-hearth is a deferred dead backend (8 compile errors against the
# current SurrealDB API), excluded from CI until repaired or removed.

# Code quality
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --features discord -- -D warnings

# Demos
cargo run -p familyclaw-agent                # Living Seed
cargo run -p familyclaw-agent --bin crash_replay  # Crash Replay

# Layer B audit (matches CI)
./scripts/audit-layer-b.sh
```

---

## No Private Profiles Needed

All demos run with **generic, in-memory or temp-file storage**. No SOUL.md, no calibration files, no API keys, no real family names. This is KERROS A (public infrastructure) only.
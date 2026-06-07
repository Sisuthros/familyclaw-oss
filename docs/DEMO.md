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
| 5 | Emotion contagion | `agent_a` broadcasts pulse, `agent_b` state updated (verified via exact f32 comparison in tests) |
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

**Status:** Webhook **send only**. Inbound gateway (receiving messages) is **future work**.

---

## What Is Still Experimental

| Feature | Crate | Status |
|---------|-------|--------|
| Latent messaging (hidden-state channel) | `familyclaw-latent` | Prototype — simple pad/truncate/resize, no semantic projection |
| WASM sandbox execution | `familyclaw-sandbox` | Compiles, fuel metering works, no integration tests |
| Dream cycle with contradictions | `familyclaw-dream` | Works with journal, contradiction markers tested |
| Emotion contagion scaling | `familyclaw-bus` | Manual HashMap iteration — O(n) per message (see architecture debt) |

---

## Verification Commands

Run locally to verify everything works:

```bash
# Full test suite (535+ tests)
cargo test --workspace

# Feature matrix (matches CI)
cargo test --workspace --all-features
cargo check -p familyclaw-channels --features discord
cargo test -p familyclaw-sandbox --features wasmtime

# Code quality
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings -A clippy::doc_markdown -A clippy::too_many_lines

# Demos
cargo run -p familyclaw-agent                # Living Seed
cargo run -p familyclaw-agent --bin crash_replay  # Crash Replay

# Layer B audit (matches CI)
./scripts/audit-layer-b.sh
```

---

## No Private Profiles Needed

All demos run with **generic, in-memory or temp-file storage**. No SOUL.md, no calibration files, no API keys, no real family names. This is KERROS A (public infrastructure) only.
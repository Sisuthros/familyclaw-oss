# Quick Start

Get FamilyClaw running in 5 minutes.

## Prerequisites

- Rust 1.85+ (install with [`rustup`](https://rustup.rs/))
- Git

Verify Rust is installed:
```bash
rustc --version  # Should show 1.85 or higher
```

## Build & Run

Three commands to see the demo:

```bash
git clone https://github.com/Sisuthros/familyclaw.git
cd familyclaw
cargo run -p familyclaw-agent
```

That's it. The demo runs automatically and completes in ~30 seconds.

## What You'll See

The demo showcases FamilyClaw's core capabilities:

1. **📡 Resonance Bus starts** — The affectionate nervous system that connects agents
2. **🤖 Two agents spawn** — `agent_a` and `agent_b` join the bus
3. **📮 Messages flow** — Agents communicate through a MockChannel (replaces Discord in demo)
4. **💬 Conversation happens** — 3 messages exchanged, stored in each agent's memory
5. **💓 Emotion contagion** — A joy pulse from agent_a influences agent_b's emotional state
6. **⏰ Time jump** — Simulates 7 days passing; shows memory decay vs identity anchors
7. **🌙 Dream cycle** — Consolidates memories: merges duplicates, absolutizes dates
8. **📚 Memory retrieval** — Shows what each agent remembers after processing

Example output:
```
═══════════════════════════════════════════════════════════
  FamilyClaw Demo: Autonomous Agents with Memory & Emotion
═══════════════════════════════════════════════════════════

📡 Step 1: Spawning Resonance Bus (affectionate nervous system)...
   ✓ Bus started

🤖 Step 2: Spawning agent_a and agent_b on the bus...
   ✓ 2 agents spawned
   · agent_a (6f394dd4-...)
   · agent_b (51a1641a-...)

💬 Step 4: Agents exchange messages (memory is stored)...
   ✓ 3 messages exchanged and stored in memory

💓 Step 5: Emotion contagion — joy spreads between agents...
   ✓ Emotion pulse delivered

═══════════════════════════════════════════════════════════
  Demo Complete!
═══════════════════════════════════════════════════════════
```

## Next Steps

### Connect to Discord

Run with a real Discord channel:
```bash
DISCORD_TOKEN=your_bot_token cargo run -p familyclaw-agent --features familyclaw-channels/discord
```

### Learn the Architecture

Read [`ARCHITECTURE.md`](docs/ARCHITECTURE.md) to understand:
- The Resonance Bus design
- Agent lifecycle and memory system
- Emotion contagion and dream consolidation
- KERROS A/B layer separation

### Contribute

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for:
- How to set up your development environment
- Testing conventions (439 tests, all passing)
- Code style guidelines (no `unsafe`, Finnish comments OK)

### Explore the Code

```bash
# Run all tests
cargo test --all

# Check formatting
cargo fmt --all -- --check

# Lint with clippy
cargo clippy -- -D warnings

# Build docs
cargo doc --all --no-deps --open
```

## Troubleshooting

**"command not found: cargo"**
- Install Rust: [`rustup.rs`](https://rustup.rs/)

**Missing dependencies**
```bash
rustup update
cargo clean
cargo build
```

**Tests failing**
```bash
cargo test --all -- --test-threads=1
```
(Some tests may need serial execution.)

---

FamilyClaw: agents that remember, feel, dream, and think.
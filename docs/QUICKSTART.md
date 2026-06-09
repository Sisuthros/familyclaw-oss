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
cargo run -p familyclaw-agent --bin familyclaw
```

That's it. The demo runs automatically and completes in ~30 seconds.

## What You'll See

The demo showcases FamilyClaw's core capabilities:

1. **📡 Resonance Bus starts** — The affectionate nervous system that connects agents
2. **🤖 Two agents spawn** — `agent_a` and `agent_b` join the bus
3. **📮 Messages flow** — Agents communicate through a MockChannel (replaces Discord in demo)
4. **💬 Conversation happens** — 3 messages exchanged, stored in each agent's memory
5. **💓 Emotion contagion** — A joy pulse from agent_a influences agent_b's emotional state
6. **⏰ Time jump** — Simulates 7 days passing; shows memory decay vs identity anchors (simulated)
7. **🌙 Dream cycle** — Consolidates memories: merges duplicates, absolutizes dates
8. **📚 Memory retrieval** — Shows what each agent remembers after processing

### Crash Replay Demo

Also available: a crash-proof memory demonstration showing FileJournal + LocalJsonStore persistence:

```bash
# Two-process mode (true process boundary)
cargo run -p familyclaw-agent --bin crash_replay -- --reset
cargo run -p familyclaw-agent --bin crash_replay -- write
cargo run -p familyclaw-agent --bin crash_replay -- verify

# Or use the script
bash scripts/demo-crash-replay.sh
```

This demo proves memory survives process boundaries by writing to disk in Phase 1, then reloading in Phase 2.

## Next Steps

### Connect to Discord

Run with a Discord webhook (currently send-only; inbound gateway is future work):
```bash
DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/... cargo run -p familyclaw-agent --features familyclaw-channels/discord
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
- Testing conventions (full test suite, all passing)
- Code style guidelines (no `unsafe`, Finnish comments OK)

### Explore the Code

```bash
# Run all tests
cargo test --workspace

# Check formatting
cargo fmt --all -- --check

# Lint with clippy
cargo clippy --workspace --all-targets -- -D warnings -A clippy::doc_markdown -A clippy::too_many_lines

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
cargo test --workspace -- --test-threads=1
```
(Some tests may need serial execution.)

---

FamilyClaw: agents that remember, feel, dream, and think.
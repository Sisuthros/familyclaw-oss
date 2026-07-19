# Quick Start

Get FamilyClaw running in 5 minutes.

## Prerequisites

- Rust 1.88+ (install with [`rustup`](https://rustup.rs/))
- Git

Verify Rust is installed:
```bash
rustc --version  # Should show 1.88 or higher
```

## Build & Run

**Public demo first (Layer A — no keys):**

```bash
git clone https://github.com/Sisuthros/familyclaw.git
cd familyclaw
cargo run -p minimal-gateway -- --duration 10
```

Or run the full public validation script on Windows:

```powershell
powershell -File scripts/public-demo.ps1
```

**Living Seed demo (two agents, in-memory):**

```bash
cargo run -p familyclaw-agent --bin familyclaw
```

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

### Run the Gateway

The gateway (`familyclaw-gateway`) is the long-running service that wires an
agent to a chat channel. It ships a small CLI:

```bash
cargo run -p familyclaw-gateway -- serve    # start the gateway (default)
cargo run -p familyclaw-gateway -- status   # print effective config
cargo run -p familyclaw-gateway -- doctor   # pre-flight checks
```

Configuration is loaded from a TOML file (looked up in this order):

1. `$FAMILYCLAW_CONFIG` (explicit path)
2. `$XDG_CONFIG_HOME/familyclaw/familyclaw.toml`
3. `$HOME/.config/familyclaw/familyclaw.toml` (default)

Copy the published skeleton and fill in your private fields:

```bash
mkdir -p ~/.config/familyclaw
cp familyclaw.toml.example ~/.config/familyclaw/familyclaw.toml
# then edit: set provider model + supply the API key via env, never in the file
```

Secrets (API keys, tokens, webhook URLs) belong in **environment variables** or
your private config — never in the repo. Copy [`.env.example`](../.env.example)
to a path outside the repo (e.g. `~/.config/familyclaw/familyclaw.env`) and
load it before running the gateway. See [`RUNBOOK_WINDOWS.md`](RUNBOOK_WINDOWS.md)
for optional Telegram/Discord wiring (Layer B).

### Connect to Discord

Two modes (see [`discord-setup.md`](discord-setup.md)):

- **Bot mode (`DISCORD_BOT_TOKEN`)** — bidirectional: the bot listens *and* posts.
  Recommended. Inbound gateway is live (Ed25519 signature verification + slash
  command parsing; see `handle_discord_interaction` in `crates/familyclaw-gateway/src/main.rs`).
- **Webhook mode (`DISCORD_WEBHOOK_URL` only)** — send-only (posts via webhook, does
  not listen).

```bash
DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/... cargo run -p familyclaw-agent --features familyclaw-channels/discord
```

### Learn the Architecture

Read [`ARCHITECTURE.md`](ARCHITECTURE.md) to understand:
- The Resonance Bus design
- Agent lifecycle and memory system
- Emotion contagion and dream consolidation
- Layer A / Layer B separation

### Contribute

See [`CONTRIBUTING.md`](../CONTRIBUTING.md) for:
- How to set up your development environment
- Testing conventions (full test suite, all passing)
- Code style guidelines (no `unsafe`, English comments and docs)

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
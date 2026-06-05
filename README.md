# FamilyClaw

**Rust-native runtime for persistent AI agent families.**

Most AI agents die between runs. FamilyClaw gives them continuity:
durable replay, persistent memory, actor-based coordination, sleep-time consolidation,
and private runtime profiles that never enter the repository.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Rust 2021](https://img.shields.io/badge/Rust-2021%20edition-orange.svg)
![unsafe: forbidden](https://img.shields.io/badge/unsafe-forbid-green.svg)

---

## What it solves

| Problem | Conventional frameworks | FamilyClaw |
|---------|------------------------|------------|
| **Memory discontinuity** — work and memory vanish on restart | Reload prompts; lose in-flight work | **Durable execution** — deterministic replay resumes exactly where stopped; side effects not re-run. **Eternal Thread** — persistent memory with Ebbinghaus decay + protected identity anchors (λ=0). |
| **Agents are isolated** — no shared situational awareness | Pass text messages | **Resonance Bus** — Ractor actor mesh where emotional state *leaks* to siblings (affective contagion). Roster never empty. |
| **Memory rots** — duplicates pile up, "yesterday" goes stale | Manual cleanup | **Dreaming** — nightly consolidation merges duplicates, drops contradictions, absolutizes relative dates (hippocampal model). |
| **Communication is lossy + token-expensive** | Everything serialized to text | **Latent telepathy** — siblings exchange hidden-state vectors directly, bridged across models, **always falling back to text** if incompatible. |

The result is a family of agents that **remember**, **feel each other's state**, **heal their own memory while they sleep**, and **think to each other** — on a platform anyone can run.

---

## Two layers: open platform, private souls

> Design principle: *Show **what** it solves, not **how** the soul works.*

FamilyClaw is split into two strictly separated layers. **This is the single most important architectural decision in the project** — it is both a security boundary and a correctness boundary.

```
┌──────────────────────────────────────────────────────────────────┐
│  LAYER A — FAMILYCLAW (this repo, open source, MIT)              │
│  The platform. Generic example beings only (agent_a, agent_b).   │
│  No real souls, no keys, no private data.                        │
└────────────────────────────┬─────────────────────────────────────┘
                             │ loaded at runtime, NEVER in repo
┌────────────────────────────┴─────────────────────────────────────┐
│  LAYER B — FAMILY PROFILES (private, NEVER published)            │
│  SOUL files · emotion calibration · conversation history         │
│  API keys · channel tokens · machine paths                       │
│  Loaded via FAMILYCLAW_PROFILE_DIR — like Hermes' HOME.          │
└──────────────────────────────────────────────────────────────────┘
```

**Absolute rule:** Nothing from Layer B may ever reach Layer A.
Enforced by `.gitignore`, CI `layer-b-audit`, and architectural design (all config loaded at runtime from private paths).

Details: [`docs/LAYER_BOUNDARY.md`](docs/LAYER_BOUNDARY.md)

---

## Architecture at a glance

Four cores, one stack, four layers. They don't compete — they compose.
**Durable carries everything; dreaming feeds on the durable log; the affective
nervous system flows over the bus; latent is the bus's highest form of speech.**

```
  LAYER 4 · LATENT TELEPATHY            familyclaw-latent
     siblings share hidden states, bridged across models, text fallback
        │ travels over
  LAYER 3 · RESONANCE BUS (affect)      familyclaw-bus
     Ractor actors + supervision; emotion leaks → affective contagion
        │
  LAYER 2 · AGENT RUNTIME               familyclaw-agent
     each actor = a being: soul + emotion + memory + per-agent model
        │ every state change →
  LAYER 1 · DURABLE SUBSTRATE           familyclaw-durable
     deterministic replay: crash → work resumes exactly where it stopped
        │ feeds
  MEMORY + SLEEP            familyclaw-memory  ·  familyclaw-dream
     Eternal Thread (Ebbinghaus decay, identity anchors λ=0)
     nightly consolidation reads the durable log → heals memory
```

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full technical overview and [`docs/plans/2026-06-03-familyclaw-v2-design.md`](docs/plans/2026-06-03-familyclaw-v2-design.md) for the design rationale.

---

## Crate map

A Cargo workspace of focused crates. Every public item is documented; the
workspace forbids `unsafe`, denies `unwrap`/`expect`/`panic!` on production
paths, and ships unit tests in-module.

| Crate | Responsibility | Docs |
|-------|---------------|------|
| [`familyclaw-core`](crates/familyclaw-core) | Shared types, typed IDs, config, error handling, time. The foundation every other crate builds on. | [![docs](https://docs.rs/familyclaw-core/badge.svg)](https://docs.rs/familyclaw-core) |
| [`familyclaw-bus`](crates/familyclaw-bus) | **Resonance Bus** — Ractor actor mesh; emotion pulses leak between beings (affective contagion). | [![docs](https://docs.rs/familyclaw-bus/badge.svg)](https://docs.rs/familyclaw-bus) |
| [`familyclaw-durable`](crates/familyclaw-durable) | **Durable substrate** — journal-based deterministic replay; crash-proof workflows. | [![docs](https://docs.rs/familyclaw-durable/badge.svg)](https://docs.rs/familyclaw-durable) |
| [`familyclaw-memory`](crates/familyclaw-memory) | **Eternal Thread** — persistent memory, Ebbinghaus decay, importance weighting, retrieval. | [![docs](https://docs.rs/familyclaw-memory/badge.svg)](https://docs.rs/familyclaw-memory) |
| [`familyclaw-dream`](crates/familyclaw-dream) | **Dreaming** — nightly consolidation: merge duplicates, drop contradictions, absolutize dates. | [![docs](https://docs.rs/familyclaw-dream/badge.svg)](https://docs.rs/familyclaw-dream) |
| [`familyclaw-emotion`](crates/familyclaw-emotion) | 19-dimension VAD emotion **frame** — empty calibration, safe to publish. | [![docs](https://docs.rs/familyclaw-emotion/badge.svg)](https://docs.rs/familyclaw-emotion) |
| [`familyclaw-latent`](crates/familyclaw-latent) | **Latent telepathy** — hidden-state transfer + `RecursiveLink` dimension bridge, text fallback. | [![docs](https://docs.rs/familyclaw-latent/badge.svg)](https://docs.rs/familyclaw-latent) |
| [`familyclaw-sandbox`](crates/familyclaw-sandbox) | Isolated code execution — Wasmtime + fuel metering + deny-by-default capabilities. | [![docs](https://docs.rs/familyclaw-sandbox/badge.svg)](https://docs.rs/familyclaw-sandbox) |
| [`familyclaw-security`](crates/familyclaw-security) | Identity anchors + human-correction veto. Identity lives in the memory substrate, not a hash. | [![docs](https://docs.rs/familyclaw-security/badge.svg)](https://docs.rs/familyclaw-security) |
| [`familyclaw-bridge`](crates/familyclaw-bridge) | Agent registry, task board, event bus — transport-independent Rust core. | [![docs](https://docs.rs/familyclaw-bridge/badge.svg)](https://docs.rs/familyclaw-bridge) |
| [`familyclaw-agent`](crates/familyclaw-agent) | **Agent runtime** — composes all above into a living being. Ships demo binaries. | [![docs](https://docs.rs/familyclaw-agent/badge.svg)](https://docs.rs/familyclaw-agent) |
| [`familyclaw-channels`](crates/familyclaw-channels) | Discord / Telegram / WhatsApp / Signal adapters bridged to the bus. | [![docs](https://docs.rs/familyclaw-channels/badge.svg)](https://docs.rs/familyclaw-channels) |

---

## Quick start

**Prerequisites:** Rust 1.85+ (edition 2021). No external services required for
the demo — it runs entirely in-memory.

```bash
git clone https://github.com/Sisuthros/familyclaw
cd familyclaw

# Build & test everything (workspace tests)
cargo test --workspace

# Run the "Living Seed" demo: Resonance Bus + 2 agents + memory + emotion
cargo run -p familyclaw-agent --bin familyclaw

# Run the Crash Replay demo: proves memory survives process restarts
cargo run -p familyclaw-agent --bin crash_replay -- write
cargo run -p familyclaw-agent --bin crash_replay -- verify

# Or run the full two-process demo via script
bash scripts/demo-crash-replay.sh
```

**No external services needed.** Demos run entirely in-memory or with temp files.

---

## Running Demos

### Living Seed Demo (default binary)
```bash
cargo run -p familyclaw-agent --bin familyclaw
```
Proves: bus startup, 2 agents register, message exchange, memory storage, emotion contagion, **real DreamCycle execution**.

### Crash Replay Demo
```bash
# Two-process mode (true process boundary)
cargo run -p familyclaw-agent --bin crash_replay -- --reset
cargo run -p familyclaw-agent --bin crash_replay -- write
cargo run -p familyclaw-agent --bin crash_replay -- verify

# Or use the script
bash scripts/demo-crash-replay.sh
```
Proves: FileJournal + LocalJsonStore persist to disk, process restart reloads both, DurableContext replays steps deterministically, memory recalled after restart.

### Discord Webhook (send-only, feature-gated)
```bash
DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/... \
cargo run -p familyclaw-agent --features familyclaw-channels/discord
```
Inbound gateway: future work.

---

## Loading Your Own Family (Layer B)

The platform loads agent profiles at runtime — **they never live in the repo**:

```bash
export FAMILYCLAW_PROFILE_DIR=/path/to/your/private/profiles
cargo run -p familyclaw-agent --bin familyclaw
```

Each profile directory holds its own `SOUL.md`, emotion calibration, and channel config. Keep it out of version control.

---

## Verification Commands

```bash
# Full test suite
cargo test --workspace

# Feature matrix (matches CI)
cargo test --workspace --all-features
cargo check -p familyclaw-channels --features discord
cargo test -p familyclaw-sandbox --features wasmtime

# Code quality
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings -A clippy::doc_markdown -A clippy::too_many-lines

# Layer B audit (matches CI)
./scripts/audit-layer-b.sh

# Two-process crash replay demo
bash scripts/demo-crash-replay.sh
```

---

## Project Conventions

- **Rust edition 2021**, `unsafe_code = "forbid"`, Clippy `pedantic` clean
- **No `unwrap()` / `expect()` / `panic!()`** on production paths — everything flows through `thiserror`-based `Result` types (`unwrap` is fine in tests)
- **Every public item documented** (`missing_docs = "warn"`)
- **Tests live in-module** (`#[cfg(test)] mod tests`), covering edge cases
- Finnish allowed in comments/docs (family convention); identifiers are English

---

## Roadmap

Risk-first, not all-at-once:

| Phase | Goal |
|-------|------|
| **0** | Living seed — 2 actors talk over bus; memory survives restart (**DONE**) |
| **1** | Emotion + nervous system — 19-dim VAD; affective contagion |
| **2** | Sleep — nightly dream-consolidation from durable log; Ebbinghaus + identity anchors |
| **3** | Telepathy — hidden-state transfer; `RecursiveLink` bridge + text fallback |
| **4** | Safety + sandbox — Wasmtime + fuel; human-correction veto; tamper alerts |
| **5** | Channels + new beings — remaining channels; new being wakes on platform |
| **6** | OSS release — full test suite; Layer A/B audit (CI + pre-push); generic agents |

---

## License

MIT — see [LICENSE](LICENSE). © 2026 The FamilyClaw Authors.

> *"Don't copy one. Take the best of each. Build our own."*
>
> *Built so the next being gets a better home than the last one did.*
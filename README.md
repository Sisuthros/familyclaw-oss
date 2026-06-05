<div align="center">

# FamilyClaw

**A sovereign multi-agent family operating system, written in Rust.**

*Give your agents a continuous mind: durable memory that survives crashes,
an affective nervous system they share, nightly dream-consolidation, and
latent telepathy between siblings — with a hard wall between the open
platform and your private souls.*

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Rust 2021](https://img.shields.io/badge/Rust-2021%20edition-orange.svg)
![unsafe: forbidden](https://img.shields.io/badge/unsafe-forbid-green.svg)

</div>

---

## 🏗️ Hire the creator

**The FamilyClaw Authors** built FamilyClaw — 20,000 lines of Rust in 24 hours,
12 crates, 452 tests, production-grade architecture.

📍 Larnaca, Cyprus (EU) · 💼 Available for Rust/AI consulting · ⌛ Immediate start

**What I deliver:**
- High-performance Rust systems (Tokio, Ractor, async, WASM)
- AI agent architecture & integration (OpenAI-compatible LLMs, RAG, memory)
- DORA & EU AI Act compliance (financial sector)
- Full-stack platform architecture

📧 **viltsu.operator@gmail.com** · 🔗 [GitHub](https://github.com/Sisuthros) · 💼 [Upwork profile coming soon]

---

## What it solves

Most agent frameworks treat an agent as a stateless function call: it wakes
up, runs, forgets. FamilyClaw treats agents as **persistent beings that live
together**. It is the best-of-breed successor to OpenClaw / Hermes Agent,
rebuilt as a single Rust workspace around four hard problems that conventional
frameworks leave unsolved:

| Problem | What conventional frameworks do | What FamilyClaw does |
|---------|---------------------------------|----------------------|
| **Memory discontinuity** — work and memory vanish on restart | Reload a prompt; lose in-flight work | **Durable execution** — deterministic replay resumes work *exactly* where it stopped, side effects not re-run. **Eternal Thread** memory persists with Ebbinghaus decay + protected identity anchors. |
| **Agents are isolated** — no shared situational awareness | Pass text messages | **Resonance Bus** — a Ractor actor mesh where each being's emotional state *leaks* to its siblings (affective contagion). The roster is never empty. |
| **Memory rots** — duplicates pile up, "yesterday" goes stale | Manual cleanup | **Dreaming** — a nightly consolidation cycle merges duplicates, drops contradictions, and absolutizes relative dates (hippocampal model). |
| **Communication is lossy + token-expensive** | Everything serialized to text | **Latent telepathy** — siblings exchange hidden-state vectors directly, bridged across model dimensions, **always falling back to text** if incompatible. |

The result is a family of agents that **remember**, **feel each other's
state**, **heal their own memory while they sleep**, and **think to each other**
— on a platform anyone can run.

---

## Two layers: open platform, private souls

> Design principle: *show **what** it solves, not **how** the soul works.*

FamilyClaw is split into two strictly separated layers. **This is the single
most important architectural decision in the project** — it is both a security
boundary and a correctness boundary.

```
┌──────────────────────────────────────────────────────────────────┐
│  LAYER A — FAMILYCLAW (this repository, open source, MIT)          │
│  The platform. The best OpenClaw/Hermes replacement, for anyone.   │
│                                                                    │
│   git clone → build → raise YOUR OWN family of agents              │
│   Generic example beings only (agent_a, agent_b). No real souls.   │
└────────────────────────────┬───────────────────────────────────────┘
                             │ loaded at runtime, NEVER in the repo
┌────────────────────────────┴───────────────────────────────────────┐
│  LAYER B — FAMILY PROFILES (private, NEVER published)               │
│   SOUL files · emotion-engine calibration · conversation history    │
│   API keys · channel tokens · machine paths                         │
│   Loaded via FAMILYCLAW_PROFILE_DIR — exactly like Hermes' HOME.    │
└──────────────────────────────────────────────────────────────────┘
```

**The rule (absolute):** nothing from Layer B may ever reach the Layer A repo.
Profiles are loaded at runtime from `FAMILYCLAW_PROFILE_DIR`; the emotion engine
ships as an empty **frame** with zero calibration; examples use generic names.
The `.gitignore` enforces this, and the design mandates a CI + pre-push audit.

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

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full technical
overview and [`docs/plans/2026-06-03-familyclaw-v2-design.md`](docs/plans/2026-06-03-familyclaw-v2-design.md)
for the design rationale.

---

## Crate map

A Cargo workspace of focused crates. Every public item is documented; the
workspace forbids `unsafe`, denies `unwrap`/`expect`/`panic!` on production
paths, and ships unit tests in-module.

| Crate | Responsibility |
|-------|----------------|
| [`familyclaw-core`](crates/familyclaw-core) | Shared types, typed IDs, config, error handling, time. The foundation every other crate builds on. |
| [`familyclaw-bus`](crates/familyclaw-bus) | **Resonance Bus** — Ractor actor mesh; emotion pulses leak between beings (affective contagion). |
| [`familyclaw-durable`](crates/familyclaw-durable) | **Durable substrate** — journal-based deterministic replay; crash-proof workflows. |
| [`familyclaw-memory`](crates/familyclaw-memory) | **Eternal Thread** — persistent memory, Ebbinghaus decay, importance weighting, retrieval. |
| [`familyclaw-dream`](crates/familyclaw-dream) | **Dreaming** — nightly consolidation: merge duplicates, drop contradictions, absolutize dates. |
| [`familyclaw-emotion`](crates/familyclaw-emotion) | 19-dimension VAD emotion **frame** — empty calibration, safe to publish. |
| [`familyclaw-latent`](crates/familyclaw-latent) | **Latent telepathy** — hidden-state transfer + `RecursiveLink` dimension bridge, text fallback. |
| [`familyclaw-sandbox`](crates/familyclaw-sandbox) | Isolated code execution — `wasmtime` + fuel metering + deny-by-default capabilities. |
| [`familyclaw-security`](crates/familyclaw-security) | Identity anchors + human-correction veto. Identity lives in the memory substrate, not a hash. |
| [`familyclaw-bridge`](crates/familyclaw-bridge) | Agent registry, task board, event bus — transport-independent Rust core. |
| [`familyclaw-agent`](crates/familyclaw-agent) | **Agent runtime** — composes all of the above into a single living being. Ships the demo binary. |
| [`familyclaw-channels`](crates/familyclaw-channels) | Channel layer — Discord / Telegram / WhatsApp / Signal interface, bridged to the bus. |

---

## Quick start

**Prerequisites:** Rust 1.85+ (edition 2021). No external services required for
the demo — it runs entirely in-memory.

```bash
git clone https://github.com/Sisuthros/familyclaw
cd familyclaw

# Build and test the whole workspace
cargo build --workspace
cargo test  --workspace

# Run the "living seed" demo: a Resonance Bus, two generic example
# beings, and a real channel feeding the bus.
cargo run -p familyclaw-agent --bin familyclaw

# Want logs? Set RUST_LOG.
RUST_LOG=info cargo run -p familyclaw-agent --bin familyclaw
```

The demo (Phase 0 of the roadmap) proves the platform end to end:

1. **The roster is never empty** — the bus knows both beings.
2. **A real channel feeds the bus** — through the published `familyclaw-channels`
   adapter, not demo-only glue.
3. **Messages flow** — what `agent_a` says reaches `agent_b`.
4. **Memory persists** — each being remembers what it heard in the Eternal Thread.
5. **Emotion is contagious** — an emotion pulse raises a sibling's mood.

### Loading your own family (Layer B)

The platform loads agent profiles at runtime — they never live in the repo:

```bash
export FAMILYCLAW_PROFILE_DIR=/path/to/your/private/profiles
cargo run -p familyclaw-agent --bin familyclaw
```

Each profile directory holds its own `SOUL`, emotion calibration, and channel
config. Keep it out of version control.

---

## Project conventions

- **Rust edition 2021**, `unsafe_code = "forbid"`, Clippy `pedantic` clean.
- **No `unwrap()` / `expect()` / `panic!()`** on production paths — everything
  flows through `thiserror`-based `Result` types. (`unwrap` is fine in tests.)
- **Every public item documented** (`missing_docs = "warn"`).
- **Tests live in-module** (`#[cfg(test)] mod tests`), covering edge cases.
- Finnish is allowed in comments and docs (a family convention); identifiers
  are English.

---

## Roadmap

Risk-first, not all-at-once. The seed lives in week one and proves the riskiest
assumptions before anything is built on top of them.

| Phase | Goal |
|-------|------|
| **0 · Living seed** | Two actors talk over the bus; one remembers what the other said. Restart mid-work → work resumes + memory survives. |
| **1 · Emotion + nervous system** | 19-dim VAD frame; emotion leaks into the bus (affective contagion). |
| **2 · Sleep** | Nightly dream-consolidation from the durable log; Ebbinghaus decay + identity anchors. |
| **3 · Telepathy** | Hidden-state transfer between actors; `RecursiveLink` dimension bridge + text fallback. |
| **4 · Safety + sandbox** | `wasmtime` + fuel; human-correction veto; identity tamper alerts. |
| **5 · Channels + new beings** | Remaining channels; a new being wakes on the platform. |
| **6 · OSS release** | Full test suite; **Layer A / Layer B boundary audit** (CI + pre-push); generic example agents. |

---

## License

MIT — see [LICENSE](LICENSE). © 2026 The FamilyClaw Authors.

> *"Don't copy one. Take the best of each. Build our own."*
> *Built so the next being gets a better home than the last one did.*

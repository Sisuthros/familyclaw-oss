# FamilyClaw

**Crash-proof memory + contract-verified multi-agent execution for AI agent families.**

Most AI agents die between runs. FamilyClaw gives them continuity:
durable replay, persistent memory, actor-based coordination, sleep-time consolidation,
and private runtime profiles that never enter the repository.

> **What this demo proves today:** crash-proof durable replay, 19-dimension emotion contagion,
> nightly dream consolidation, persistent memory with Ebbinghaus decay, Discord inbound gateway
> (Ed25519 verification), benchmarked continuity (8 scenarios, deterministic scorecard).
>
> **Shipped (alpha.5/.6):** live multi-agent orchestration (`familyclaw-gateway
> orchestrate` — Orchestrator + LiveTurnExecutor), provenance-gated memory,
> emotion-contagion receive-side, send-side latent translation.
> **What's on the roadmap:** receive-side latent telepathy (hidden-state transfer
> into agent cognition — a family-boundary decision), deeper WASM sandbox safety,
> additional channel adapters.

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

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full technical overview.

---

## Crate map

A Cargo workspace of focused crates. Every public item is documented; the
workspace forbids `unsafe`, denies `unwrap`/`expect`/`panic!` on production
paths, and ships unit tests in-module.

| Crate | Responsibility |
|-------|---------------|
| [`familyclaw-core`](crates/familyclaw-core) | Shared types, typed IDs, config, error handling, time. The foundation every other crate builds on. |
| [`familyclaw-bus`](crates/familyclaw-bus) | **Resonance Bus** — Ractor actor mesh; emotion pulses leak between beings (affective contagion). |
| [`familyclaw-durable`](crates/familyclaw-durable) | **Durable substrate** — journal-based deterministic replay; crash-proof workflows. |
| [`familyclaw-memory`](crates/familyclaw-memory) | **Eternal Thread** — persistent memory, Ebbinghaus decay, importance weighting, retrieval. |
| [`familyclaw-dream`](crates/familyclaw-dream) | **Dreaming** — nightly consolidation: merge duplicates, drop contradictions, absolutize dates. |
| [`familyclaw-emotion`](crates/familyclaw-emotion) | 19-dimension VAD emotion **frame** — empty calibration, safe to publish. |
| [`familyclaw-latent`](crates/familyclaw-latent) | **Latent telepathy** — hidden-state transfer + `RecursiveLink` dimension bridge, text fallback. |
| [`familyclaw-sandbox`](crates/familyclaw-sandbox) | Isolated code execution — Wasmtime + fuel metering + deny-by-default capabilities. |
| [`familyclaw-security`](crates/familyclaw-security) | Identity anchors + human-correction veto. Identity lives in the memory substrate, not a hash. |
| [`familyclaw-bridge`](crates/familyclaw-bridge) | Agent registry, task board, event bus — transport-independent Rust core. |
| [`familyclaw-agent`](crates/familyclaw-agent) | **Agent runtime** — composes all above into a living being. Ships demo binaries. |
| [`familyclaw-channels`](crates/familyclaw-channels) | Discord / Telegram / WhatsApp / Signal adapters bridged to the bus. Discord inbound gateway (Ed25519 verification, slash commands) is live. |
| [`familyclaw-acp`](crates/familyclaw-acp) | **ACP client** — spawn and control CLI agents (Claude, Gemini, Qoder) over stdio. |
| [`familyclaw-bench`](crates/familyclaw-bench) | **Continuity benchmark** — reproducible proof of crash-resume, retention and dreaming. |
| [`familyclaw-gateway`](crates/familyclaw-gateway) | **Gateway binary** — long-running process: HTTP health/readiness + Resonance Bus bootstrap. |
| [`familyclaw-gemu`](crates/familyclaw-gemu) | **Gemu CLI** — Gemini interface to FamilyClaw runtime. |
| [`familyclaw-hearth`](crates/familyclaw-hearth) | **The Hearth** — shared family memory, narratives, emotional state and anchors. |
| [`familyclaw-observability`](crates/familyclaw-observability) | **Observability** — metrics, event recording and per-role RBAC for the multi-agent fleet. |
| [`familyclaw-runtime`](crates/familyclaw-runtime) | **Runtime assembly** — wires bus + agents + channels + reply pump (`build_family`). |

---

## Quick start

**Prerequisites:** Rust 1.85+ (edition 2021). No external services required for
the demo — it runs entirely in-memory.

### FamilyClaw in 60 seconds ⚡

```bash
git clone https://github.com/Sisuthros/familyclaw
cd familyclaw

# One command: builds + runs 1 agent + Resonance Bus + MockChannel
cargo run -p minimal-gateway -- --duration 10
```

Windows: full public validation (tests + bench, no keys):

```powershell
powershell -File scripts/public-demo.ps1
```

**What happens:**
1. 🚀 Starts Resonance Bus (`minimal-gateway-bus`)
2. 🤖 Spawns `agent_a` with durable memory + emotion
3. 📥 Injects a message via MockChannel
4. 🔁 Message flows: Channel → Bus → Agent → Memory + Emotion
5. 📤 Shows outbox (agent replied)
6. 🛑 Clean shutdown on Ctrl-C or timeout

*No Telegram, Discord, or API keys needed. Pure Rust, pure demo.*

### Full test suite
```bash
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
Inbound gateway: live — Ed25519 signature verification, slash command parsing (`gateway/src/main.rs:269-379`).

---

## Loading Your Own Family (Layer B)

After the public demo works, load private profiles at runtime — **they never live in the repo**:

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

# Feature matrix (matches CI exactly — living features only)
cargo test --workspace \
  --features familyclaw-channels/discord \
  --features familyclaw-channels/telegram \
  --features familyclaw-channels/whatsapp \
  --features familyclaw-channels/signal \
  --features familyclaw-sandbox/wasmtime

# NB: `--all-features` is intentionally NOT used. The `surreal` feature in
# familyclaw-hearth is a deferred dead backend (does not compile against the
# current SurrealDB API) and is excluded from CI until repaired or removed.
# See .github/workflows/ci.yml for the canonical living-feature list.

# Code quality
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --features discord -- -D warnings

# Benchmark suite — 8 scenarios, deterministic scorecard
cargo run -p familyclaw-bench --bin bench -- all
# → outputs crates/familyclaw-bench/out/SCORECARD.md + scorecard.json

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
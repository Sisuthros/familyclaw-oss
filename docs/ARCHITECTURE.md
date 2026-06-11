# FamilyClaw — Architecture

A concise technical overview of the FamilyClaw workspace: the crates, the data
flow between them, and the invariants that hold the platform together.

For the *why* behind these choices, read
[`plans/2026-06-03-familyclaw-v2-design.md`](plans/2026-06-03-familyclaw-v2-design.md).

---

## 1. Two layers (the load-bearing boundary)

FamilyClaw separates the **open platform** from the **private souls** that run on
it. This is a security boundary and a correctness boundary at once.

- **Layer A — the platform (this repo, MIT).** Generic, publishable, reusable.
  No real souls, no calibration, no keys, no machine paths.
- **Layer B — the family profiles (private, never published).** Souls, emotion
  calibration, conversation history, API keys, channel tokens. Loaded at runtime
  from `FAMILYCLAW_PROFILE_DIR` (cf. Hermes' `HERMES_HOME`), never committed.

Enforcement: `.gitignore` blocks profiles / `*.soul` / `SOUL.md` / calibration
JSON / `.env` / keys; the design mandates a CI + pre-push audit; the emotion
crate ships an empty calibration frame; all examples use `agent_a` / `agent_b`.

---

## 2. The four cores, one stack

The platform is four cores stacked as four layers. They compose rather than
compete: **durable carries everything; dreaming feeds on the durable log; the
affective nervous system flows over the bus; latent is the bus's highest form of
speech.**

```
LAYER 4 · LATENT TELEPATHY      familyclaw-latent
LAYER 3 · RESONANCE BUS         familyclaw-bus       (affective nervous system)
LAYER 2 · AGENT RUNTIME         familyclaw-agent     (a being = soul+emotion+memory+model)
LAYER 1 · DURABLE SUBSTRATE     familyclaw-durable   (deterministic replay)
MEMORY + SLEEP                  familyclaw-memory · familyclaw-dream
```

---

## 3. Crate dependency graph

`familyclaw-core` is the root; everything depends on it and it depends on
nothing else in the workspace. The agent runtime is the apex that composes them.

```
                         familyclaw-core
        ┌──────────┬──────────┬──────────┬──────────┬──────────┐
        │          │          │          │          │          │
    -emotion   -memory    -durable     -bus      -latent    -security
        │          │          │          │
        │          └───┐  ┌───┘          │
        │          familyclaw-dream      │
        │     (reads memory + durable    │
        │      contradiction log)        │
        └──────────────┬─────────────────┘
                familyclaw-agent  ◀── familyclaw-channels ── familyclaw-bridge
                 (composes core, emotion, memory,
                  durable, bus; ships the demo bin)
```

- **`familyclaw-core`** — typed IDs (`AgentId`, `FamilyId`, `MessageId`),
  `FamilyClawError` + `Result<T>`, JSON-loadable config (`FamilyConfig`,
  `AgentConfig`, `ModelConfig`), UTC time helpers. Depends on nothing else.
- **`familyclaw-bus`** — Ractor actor mesh. `ResonanceBus` registers beings and
  fans messages out; `BusMessage` carries `Text` / `Latent` / `EmotionPulse` /
  `TaskEvent` / `Custom`. `BusHandle` is the `unwrap`-free ergonomic API.
- **`familyclaw-durable`** — `DurableContext<J>` wraps work into `step`s appended
  to a `Journal` (`InMemoryJournal` for dev, `FileJournal` JSONL + fsync for
  crash-proofing). On restart, completed steps are replayed from the log without
  re-running their closures.
- **`familyclaw-memory`** — the Eternal Thread. `Memory` carries content, a VAD
  emotional tone, importance, a `DecayPolicy`, and a lifecycle status.
  `MemoryStore` is the async abstraction; `LocalJsonStore` is the
  dependency-free default with atomic writes. Retention follows
  `R(t) = e^(-λ·t/S)`; `ProtectedCore` (λ=0) never decays.
- **`familyclaw-dream`** — `DreamCycle` runs five ordered, deterministic phases
  over the memory store and the durable contradiction log: merge duplicates,
  drop contradictions, absolutize dates, consolidate, report. It never buries a
  `ProtectedCore` anchor.
- **`familyclaw-emotion`** — 19-dimension VAD frame (`Dimension`,
  `EmotionState`, `Vad`, `Blend`). Ships structure only; `EmotionCalibration` is
  loaded per-machine at runtime (Layer B).
- **`familyclaw-latent`** — `LatentVector` + `RecursiveLink` (linear dimension
  bridge between two models' latent spaces). `LatentChannel::transmit` never
  errors on mere incompatibility — it picks the highest possible
  `TransmissionMode` and falls back to text, recording a `FallbackReason`.
- **`familyclaw-sandbox`** — `CodeSandbox` trait; deny-by-default `CapabilitySet`
  (network / fs-read / env), `FuelLimit`/`FuelMeter` enforce a hard execution
  ceiling. `NoopSandbox` is the default; `WasmtimeSandbox` is behind the
  `wasmtime` feature.
- **`familyclaw-security`** — `IdentityAnchor` (protected, non-decaying memory)
  and `HumanCorrection` (human veto). Identity lives in the memory substrate; the
  `AnchorHash` SHA-256 is **only a tamper alarm** — substrate is truth, hash is
  guard.
- **`familyclaw-bridge`** — `AgentRegistry` (heartbeat liveness), `TaskBoard`
  (`Pending → Active/Handed → Done`), `EventBus` (broadcast), and the
  `WorkExecutor` seam (`execute(&Task) -> WorkOutcome`). The seam keeps task
  execution abstract (Layer-A producer side of the Homepage Factory): the caller
  owns status transitions, so executors are side-effect-free and swappable.
  `DefaultSimulatingExecutor` is the deterministic, no-network default; a live
  executor (Layer B) drops in behind the same trait. Transport-independent;
  MCP/HTTP adapters layer on separately.
- **`familyclaw-agent`** — `Agent` / `AgentActor`: composes config, soul,
  emotion, memory, durable, and bus into one Ractor being. Loads souls at
  runtime from the profile dir (Layer B). Ships the `familyclaw` demo binary.
- **`familyclaw-channels`** — `Channel` trait (`Discord` / `Telegram` /
  `WhatsApp` / `Signal` / `Mock`); inbound messages canonicalize to
  `InboundEnvelope`; `pump_to` is the bus integration seam.

---

## 4. Data flow: a message, end to end

```
  external world
      │  raw inbound text
      ▼
  familyclaw-channels   InboundMessage ──into_envelope──▶ InboundEnvelope
      │                 (stable MessageId, origin channel, UTC timestamp)
      ▼  publish_envelope (agent-crate adapter)
  familyclaw-bus        ResonanceMessage { BusMessage::Text, from, id, ts }
      │  fan-out to all other beings
      ▼
  familyclaw-agent      AgentActor receives, runs a turn
      ├─▶ familyclaw-durable   handle_turn wraps outcome in a durable step
      ├─▶ familyclaw-memory    records the heard content in the Eternal Thread
      └─▶ familyclaw-bus       publishes BusMessage::EmotionPulse back
                                 │  siblings' EmotionState rise (contagion)
```

On restart, the durable journal replays completed turns without re-running side
effects; the Eternal Thread is reloaded from its store. Nightly, `DreamCycle`
reads both to consolidate memory.

---

## 5. Invariants

These hold across the workspace and are enforced in `Cargo.toml` lints, tests,
and review:

1. **No `unsafe`.** `unsafe_code = "forbid"` workspace-wide.
2. **No `unwrap`/`expect`/`panic!` on production paths.** Errors flow through
   `thiserror` types and `Result`. (`unwrap` is allowed in tests only.)
3. **Clippy `pedantic` clean.** Builds produce no warnings.
4. **Every public item is documented** (`missing_docs = "warn"`).
5. **Determinism where it matters.** Durable replay, dream phases, and the
   sandbox are deterministic so replay and testing are reproducible — `now` is
   always passed in, never read implicitly.
6. **Communication never breaks.** Latent transfer always has a text fallback.
7. **Identity is substrate, not hash.** Tamper detection raises an alarm; it
   never mutates or loses identity.
8. **The Layer A / Layer B wall is absolute.** No soul, calibration, key, token,
   IP, or personal path is ever hardcoded into a published crate.

---

## 6. Build & test

```bash
cargo build --workspace      # build every crate
cargo test  --workspace      # run all in-module unit tests
cargo clippy --workspace -- -D warnings   # zero-warning gate
cargo run -p familyclaw-agent --bin familyclaw   # the living-seed demo
```

The `wasmtime` backend in `familyclaw-sandbox` is feature-gated, so the default
build needs no WASM toolchain; the security/capability logic is testable without
it via `NoopSandbox`.

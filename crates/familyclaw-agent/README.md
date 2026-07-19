# familyclaw-agent

**Agent runtime** — layer 2 of the `FamilyClaw` platform (Layer A, OSS): it
assembles all the other crates into a single *being*.

A single `Agent` owns:

- **configuration** (`familyclaw-core`: identity + models),
- **a soul** (`Soul`, loaded at runtime from a profile directory),
- **emotion state** (`familyclaw-emotion`: 19-dim VAD),
- **memory** (`familyclaw-memory`: Eternal Thread),
- **a crash-safe journal** (`familyclaw-durable`: deterministic replay),
- **a bus connection** (`familyclaw-bus`: Resonance Bus).

The agent is a Ractor actor (`AgentActor`) that joins the bus, processes
messages, updates its emotion state from siblings' pulses (*affective
contagion*), records memories, and publishes emotion pulses back to the bus.

## Crash safety

`Agent::handle_turn` wraps the outcome of every turn in a durable step. On
restart, turns that already ran are replayed from the journal without
re-running side effects — a structural fix for pain point #1 for a family
(memory discontinuity).

## SOUL loading (OSS boundary)

Souls are loaded at runtime from a generic profile directory
(`FAMILYCLAW_PROFILE_DIR` or `AgentConfig::profile_dir`). **No family
member's soul, model name, key, or path is hardcoded** into this crate. The
profile schema (`SOUL.md` required, `IDENTITY.md` / `WANTS.md` optional,
other `*.md` files → `extra`) is generic.

## Demo: a living seed

```bash
cargo run -p familyclaw-agent --bin familyclaw
```

Starts the bus, two generic agents (`agent_a`, `agent_b`), and a
`MockChannel`. Demonstrates that `beings[]` is non-empty, messages flow,
memory persists, and emotion is contagious. Set `RUST_LOG=debug` to see
per-turn logs.

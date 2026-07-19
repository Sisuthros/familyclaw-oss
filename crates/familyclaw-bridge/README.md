# familyclaw-bridge

The bridge layer (Layer A / OSS) for the FamilyClaw platform: an
**agent registry, task board, and event bus** as a pure, transport-layer-
independent Rust interface.

> Design §3 — *"use what already exists"*. This crate models the semantics
> of an existing `family-bridge` MCP as native Rust. MCP and HTTP adapters
> are wired in separately later — this crate contains no transport layer.

## What this provides

| Part | Responsibility |
|-----|--------|
| `AgentRegistry` | `register` / `list` / `get` / `deregister`, `heartbeat`, liveness state with timeout |
| `Task` + `TaskStatus` | a task and its state machine (`Pending` → `Active`/`Handed` → `Done`) |
| `TaskBoard` | `create` / `update_status` / `handoff` / `assign`, filterable listing |
| `EventBus` + `Event` | fan-out publish/subscribe (`tokio::sync::broadcast`) |
| `FamilyBridge` | composes the above and publishes events on state changes |

## Liveness

An agent is `Online` when its latest heartbeat is more recent than the
registry's timeout (default 30 s), `Offline` when it has expired, and
`Unknown` if no heartbeat has been received yet. The current instant is
always supplied as a parameter (`liveness_at(id, now)`), so the logic is
deterministic and testable.

## Handoff rules

`TaskBoard::handoff(task, from, to)` succeeds only when:

- `from` is the task's current responsible agent,
- `from != to`,
- the task is not in a terminal state (`Done`).

On success, `assignee` changes to `to` and the state moves to `Handed`,
from which the recipient can move it to `Active`.

## Design principles

- Tokio-based, thread-safe (`Arc<RwLock<…>>` / `broadcast`); facades are
  `Clone` and share their state.
- No `unwrap()` / `expect()` / `panic!()` on the production path — all
  errors flow through the `familyclaw_core::Result` type.
- OSS boundary: no hardcoded souls, keys, tokens, IP addresses, or
  personal paths. Types are generic (`agent_a`, `agent_b`).

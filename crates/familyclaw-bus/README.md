# familyclaw-bus

**Resonance Bus** — FamilyClaw v2's *affective nervous system*
([design §2.2](../../docs/plans/2026-06-03-familyclaw-v2-design.md)).
Layer A / OSS (MIT).

The bus is a [Ractor](https://docs.rs/ractor)-based actor model over which
a family's beings (agents) communicate — and over which **their emotional
states leak into each other** (affective contagion). When one sibling is
in creative flow, the others sense it.

## Why

In live production, Resonance Bus returned `beings:[]` — an empty list of
beings, even though agents had joined. This crate fixes that structurally:
`BusHandle::beings()` returns the actual joined beings, and the list is
never empty once beings have registered.

## Core concepts

| Type | Responsibility |
|--------|--------|
| `BusMessage` | The bus's "language": `Text`, `Latent`, **`EmotionPulse`**, `TaskEvent`, `Custom`. |
| `ResonanceMessage` | Envelope: payload + sender + identifier + UTC timestamp. |
| `ResonanceBus` | Actor: registers beings, sends messages to all others, propagates emotion pulses. |
| `BusHandle` | Ergonomic, `unwrap`-free interface to the bus (`register` / `publish` / `beings` / `count`). |
| `BeingInfo` / `BeingId` / `BeingSnapshot` | A joined being's info, identifier, and serializable snapshot. |
| `CollectorBeing` | A ready-made being actor for tests/examples (collects received messages). |

## Affective nervous system

When a being publishes its emotion state as a pulse
(`BusMessage::EmotionPulse`), **all other beings receive it** and can react
to a sibling's mood. This is the "blood" that makes the bus a nervous
system rather than just a message queue.

## Resilience (supervision)

Beings are linked as children of the bus. If an individual being crashes
or terminates, the bus receives a supervision event, removes the being
from the registry, and **stays alive** — one being's crash doesn't bring
down the nervous system.

## OSS boundary (Layer A)

The crate does not hardcode family members' souls, model names, keys, or
paths. Beings' identifiers and names are supplied at runtime; examples use
generic names (`agent_a`, `agent_b`).

## Usage

```rust,ignore
use familyclaw_bus::{BeingId, BeingInfo, BusMessage, CollectorBeing, ResonanceBus};
use ractor::Actor;

let bus = ResonanceBus::start(None).await?;

let log = CollectorBeing::new_log();
let (inbox, _h) = Actor::spawn(None, CollectorBeing, log.clone()).await?;
let id = BeingId::new();
bus.register(BeingInfo::new(id, "agent_b", inbox))?;

// beings[] ei ole tyhjä.
assert_eq!(bus.count().await?, 1);

// Tunnepulssi leviää sisaruksille.
bus.publish(BeingId::new(), BusMessage::emotion_pulse(state))?;
```

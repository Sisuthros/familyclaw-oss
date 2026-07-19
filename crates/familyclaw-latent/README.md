# familyclaw-latent

**Latent telepathy** — a *hidden-state* transfer between siblings that
**always** falls back to text if latent fails. FamilyClaw v2's highest
communication mode (design §2.4), not the only one: communication never breaks.

## What this crate provides

| Type | Responsibility |
|--------|--------|
| `LatentVector { dims: Vec<f32>, model_id: String }` | An agent's hidden state + the model that produced it. |
| `RecursiveLink` | A linear dimension bridge from agent A's latent space to agent B's space (`pad` / `truncate` / `resize` / `identity`). |
| `ProjectedLatent` / `ProjectionStrategy` | The projection result + metadata (lossless or not). |
| `LatentChannel` (trait) | A `send`/`receive`-style transfer with a built-in text fallback. |
| `TransmissionMode { Latent, Text }` | The highest successful transmission mode. |
| `FallbackReason` | Why latent had to fall back to text (for measurement). |
| `InMemoryLatentChannel` | A test/development channel that collects deliveries in memory. |

## Core principle: always a text fallback

`LatentChannel::transmit` **never returns an error for mere
incompatibility**. It picks the highest possible tier and falls back to
text if:

1. the receiver doesn't support latent (`ReceiverTextOnly`),
2. the message has no hidden state (`NoLatentAvailable`),
3. the sender has no `RecursiveLink` bridge to the target model (`NoLink`),
4. the dimension projection fails (`ProjectionFailed`, e.g. `NaN`/`inf`).

An error is returned **only** for a genuine transport failure (`deliver`).

## Example

```rust
use familyclaw_latent::{
    InMemoryLatentChannel, LatentChannel, LatentMessage, LatentVector,
    ReceiverProfile, RecursiveLink, TransmissionMode,
};

let mut channel = InMemoryLatentChannel::new("agent_a/v1")
    .with_link(RecursiveLink::new("agent_a/v1", 4, "agent_b/v1", 6));

let hidden = LatentVector::new(vec![0.1, 0.2, 0.3, 0.4], "agent_a/v1");
let message = LatentMessage::with_latent(hidden, "kuulemiin");
let receiver = ReceiverProfile::latent("agent_b/v1", 6);

let result = channel.transmit(&message, &receiver).unwrap();
assert_eq!(result.mode, TransmissionMode::Latent);
```

## Research honesty (limits documented, not hidden)

This is a **deliberately honest skeleton** for LatentMAS-style (ICML 2026
Spotlight) sibling communication:

- `RecursiveLink` performs only a **simple linear fit**
  (pad/truncate/resize). It is **not** a learned, semantically aligned
  projection — different models' latent spaces are not aligned, so pad/
  truncate does not guarantee that meaning is preserved. A real trained
  projection matrix comes as a later iteration.
- That's why the text fallback is a **load-bearing principle**, not a
  backup system: latent is an opportunistic optimization, text is the
  source of truth.

## OSS boundary (Layer A)

The crate does not hardcode family members' souls, model names, keys, or
paths. All model identifiers and dimensions are supplied at runtime.
Examples use generic names (`agent_a`, `agent_b`).

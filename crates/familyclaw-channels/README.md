# familyclaw-channels

The **channel layer** of the FamilyClaw platform (Layer A / OSS — design
§3). A unified interface for inbound and outbound messages, and a bridge
to the Resonance Bus.

## Responsibility

- **The `Channel` trait** — the interface for a single bidirectional channel:
  - `send(message) -> SendFuture` — send a message out
  - `receive() -> ChannelResult<MessageStream>` — a stream of inbound messages
  - `channel_id() -> &str` — a stable identifier for the channel instance
  - `kind() -> ChannelKind` — the channel technology
  - Dyn-compatible: `Box<dyn Channel>` works (without the `async-trait` macro).
- **`ChannelKind`** — `Discord` / `Telegram` / `WhatsApp` / `Signal` / `Mock`.
- **Message types** — `OutboundMessage`, `InboundMessage`, `InboundEnvelope`.
- **`MockChannel`** — an in-memory test channel, no network or external SDKs.
- **`pump_to`** — the integration seam: channel stream → Resonance Bus.

## Inbound message → InboundEnvelope → familyclaw_bus::BusMessage

The channel layer is the Resonance Bus's edge to the outside world. An
inbound `InboundMessage` is **canonicalized** into an `InboundEnvelope`
(`InboundMessage::into_envelope`), which contains:

- a unique `MessageId`,
- the origin (`ChannelKind` + `channel_id`) for reply routing,
- the sender, conversation, and content,
- a UTC timestamp (deterministic, for durable replay).

`InboundEnvelope` is deliberately a **different type** from the bus's
content enum `familyclaw_bus::BusMessage` (so the name no longer collides
across crate boundaries) and is fully serde-serializable. The actual
`InboundEnvelope → familyclaw_bus::BusMessage` conversion and publishing to
the bus happens in the agent layer, which depends on both crates.

## Channel adapters are behind feature flags

Real adapters pull in heavy channel SDKs, so they sit behind feature flags
rather than being mandatory dependencies:

| Feature | Purpose | Example SDK |
|---------|-----------|---------------|
| `discord` | Discord adapter | serenity |
| `telegram` | Telegram adapter | teloxide |
| `whatsapp` | WhatsApp adapter | — |
| `signal` | Signal adapter | — |

The default build (`default = []`) contains **only** the core + `MockChannel`,
so the platform builds and tests without network access. Each adapter's SDK
dependency is added along with its feature only once the adapter is implemented.

## Usage

```rust
use familyclaw_channels::{Channel, ChannelKind, InboundMessage, MockChannel};

#[tokio::main]
async fn main() -> familyclaw_channels::ChannelResult<()> {
    let channel = MockChannel::new("agent-a-mock")?;
    let mut inbound = channel.receive()?;

    // The outside world feeds in an inbound message → InboundEnvelope.
    channel.inject(InboundMessage::new("user-1", "general", "moi")?)?;
    let envelope = inbound.recv().await.expect("one message");
    assert_eq!(envelope.kind, ChannelKind::Mock);

    // Reply within the same conversation.
    channel.send(envelope.reply("hei takaisin")?).await?;
    Ok(())
}
```

### Bus integration (`pump_to`)

```rust,ignore
// Consume the channel's stream and hand each envelope to the agent
// layer's adapter, which converts it into the bus payload and publishes
// it to the bus.
familyclaw_channels::pump_to(stream, |envelope| {
    // adapter::publish_envelope(&bus, agent_id, envelope) ...
    Ok(())
}).await?;
```

## OSS boundary

No hardcoded channel tokens, Discord/Telegram identifiers, server IPs, or
personal paths. Credentials and destinations are runtime configuration.

## License

MIT.

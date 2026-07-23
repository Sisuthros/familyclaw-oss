# Architecture: Channels (Channel Abstraction)

FamilyClaw uses a unified channel abstraction to connect the core system to various communication platforms. This keeps the system's internal behavior independent of external communication services.

**Implemented adapters:** Discord and Telegram (behind the `discord` and `telegram` feature flags). **`whatsapp` and `signal` are reserved empty feature flags** — no adapter source yet; they are explicit non-goals for v1.0 (see [STATUS.md](../STATUS.md)).

## `Channel` trait and interface

All adapters implement a common abstraction that defines four basic operations (the contract):
- `start().await`: Connects to the platform's servers (e.g. a WebSocket gateway) and returns a ready state or an error (`ready`/`error`).
- `stop().await`: Closes the connection cleanly.
- `send(OutboundMessage).await`: Sends a message to the external platform.
- `receive()`: Returns a `MessageStream` for listening to incoming messages (callable only once).

## Discord adapter structure

The Discord adapter is implemented using the `serenity` library (version 0.12).
- **Gateway task:** A background async task that maintains the Discord Gateway connection.
- **Receiving (MPSC):** The gateway task reads events and forwards incoming messages to the core via an `mpsc` (multi-producer, single-consumer) channel, which is returned as a `MessageStream` from `receive()`.
- **Sending:** An `Arc<Http>` instance is used for async API calls, allowing messages to be sent concurrently without blocking the gateway task.

## LAYER A principle

The channels abstraction and its adapters are designed to strictly follow the **Layer A** principle:
All configuration (such as bot tokens and channel IDs) is supplied at runtime. The code must not contain any hardcoded values, secrets, or project-specific identifiers. This ensures nothing sensitive ends up in the repo.

## Feature gating

The Discord and Telegram adapters and their dependencies are isolated behind the `discord` and `telegram` features.
**Why?** This isolation reduces build time and binary size for users who don't need those channels, and allows other adapters to be developed and compiled independently in parallel. Reserved `whatsapp` / `signal` flags stay empty until an adapter is implemented.

## Message flow (sequence diagram)

```mermaid
sequenceDiagram
    participant FC as FamilyClaw Core
    participant CH as DiscordChannel
    participant GW as Gateway Task
    participant API as Discord API / Gateway

    %% Connecting
    FC->>CH: new(token, target_channel_id)
    FC->>CH: start()
    CH->>API: Connect to the WebSocket gateway
    API-->>CH: Ready
    CH-->>FC: Ok(())

    FC->>CH: receive()
    CH-->>FC: MessageStream

    %% Receiving
    API->>GW: MessageCreate Event
    GW->>GW: Filter (correct channel)
    GW->>FC: via mpsc (MessageStream -> InboundMessage)

    %% Sending
    FC->>CH: send(OutboundMessage)
    CH->>API: HTTP POST /channels/{id}/messages (Arc<Http>)
    API-->>CH: 200 OK
    CH-->>FC: Ok(())

    %% Stopping
    FC->>CH: stop()
    CH->>API: Close the Gateway connection
    CH-->>FC: Ok(())
```

//! # familyclaw-channels
//!
//! The **channel layer** of the `FamilyClaw` platform (Layer A / OSS, design
//! §3): a unified [`Channel`] interface for inbound and outbound messages,
//! and a bridge to the Resonance Bus ([`InboundEnvelope`]).
//!
//! ## What this crate provides
//! - [`Channel`] — the interface for a bidirectional channel:
//!   [`Channel::send`], [`Channel::receive`] ([`MessageStream`]),
//!   [`Channel::channel_id`], [`Channel::kind`]. Dyn-compatible
//!   (`Box<dyn Channel>`).
//! - [`InboundEnvelope`] — a canonicalized, origin-aware envelope.
//! - [`ChannelKind`] — Discord / Telegram / `WhatsApp` / Signal / Mock.
//!   **Implemented adapters:** Discord and Telegram (behind feature flags).
//!   **`whatsapp` / `signal` features are reserved empty flags** — no adapter
//!   source yet; enabling them does not add an implementation.
//! - [`OutboundMessage`] / [`InboundMessage`] / [`InboundEnvelope`] —
//!   message types and canonicalization (`inbound message → InboundEnvelope`).
//! - [`MockChannel`] — an in-memory test channel with no external SDK.
//! - [`pump_to`] — the integration seam: channel stream → Resonance Bus.
//!
//! ## Channel adapters are behind feature flags
//! Real adapters (e.g. **serenity** for Discord, HTTP Bot API for Telegram)
//! pull in heavy channel SDKs. That's why they sit behind the crate's
//! feature flags (`discord`, `telegram`, `whatsapp`, `signal`) rather than
//! being mandatory dependencies. The default build contains **only** the
//! core + [`MockChannel`], so the platform builds and tests without
//! network access or heavy SDKs. `whatsapp` and `signal` are **reserved /
//! not implemented** (empty feature flags). Each adapter's concrete SDK
//! dependencies are added to that feature's `[dependencies]` section only
//! once the adapter is implemented.
//!
//! ## OSS boundary (Layer A)
//! This crate does not hardcode channel tokens, Discord/Telegram
//! identifiers, server IPs, or personal paths. Credentials and
//! destinations are runtime configuration; the types carry only the
//! generic structure.
//!
//! ## Example
//! ```
//! # use familyclaw_channels::{Channel, ChannelKind, InboundMessage, MockChannel, OutboundMessage};
//! # #[tokio::main]
//! # async fn main() -> familyclaw_channels::ChannelResult<()> {
//! let channel = MockChannel::new("agent-a-mock")?;
//! let mut inbound = channel.receive()?;
//!
//! // The outside world feeds in an inbound message → it is canonicalized into an InboundEnvelope.
//! channel.inject(InboundMessage::new("user-1", "general", "moi")?)?;
//! let bus_msg = inbound.recv().await.expect("one message");
//! assert_eq!(bus_msg.kind, ChannelKind::Mock);
//! assert_eq!(bus_msg.body, "moi");
//!
//! // Reply to the same conversation.
//! channel.send(bus_msg.reply("hei takaisin")?).await?;
//! assert_eq!(channel.sent()[0].body, "hei takaisin");
//! # Ok(())
//! # }
//! ```

mod channel;
mod error;
mod message;
mod mock;

#[cfg(feature = "discord")]
pub mod discord;

#[cfg(feature = "discord")]
mod discord_interactions;

#[cfg(feature = "telegram")]
mod telegram;

pub use channel::{Channel, MessageStream, SendFuture};
pub use error::{ChannelError, ChannelResult};
pub use message::{ChannelKind, InboundEnvelope, InboundMessage, OutboundKind, OutboundMessage};
pub use mock::{pump_to, MockChannel};

#[cfg(feature = "discord")]
pub use discord::DiscordChannel;

#[cfg(feature = "discord")]
pub use discord_interactions::{
    verify_signature, DiscordInteraction, RESPONSE_CHANNEL_MESSAGE,
    RESPONSE_DEFERRED_CHANNEL_MESSAGE, RESPONSE_PONG,
};

#[cfg(feature = "telegram")]
pub use telegram::TelegramChannel;

/// The crate's version at build time (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn public_api_is_reexported() {
        // If any re-export is removed, this test will fail to compile.
        let kind = ChannelKind::Mock;
        assert_eq!(kind, ChannelKind::Mock);
        let out = OutboundMessage::new("c", "b").expect("outbound");
        assert_eq!(out.body, "b");
        let inbound = InboundMessage::new("s", "c", "b").expect("inbound");
        assert_eq!(inbound.sender, "s");
        let ch = MockChannel::new("m").expect("mock channel");
        assert_eq!(ch.channel_id(), "m");
        let err = ChannelError::closed("c");
        assert!(matches!(err, ChannelError::Closed(_)));
        let ok: ChannelResult<()> = Ok(());
        assert!(ok.is_ok());
    }

    #[tokio::test]
    async fn end_to_end_inbound_to_bus_and_reply() {
        let ch = MockChannel::with_kind("e2e", ChannelKind::Telegram).expect("channel");
        let mut stream = ch.receive().expect("stream");

        ch.inject(InboundMessage::new("u", "chat-9", "hello").expect("inbound"))
            .expect("inject");

        let env: InboundEnvelope = stream.recv().await.expect("message");
        assert_eq!(env.kind, ChannelKind::Telegram);
        assert_eq!(env.channel_id, "e2e");

        ch.send(env.reply("hi").expect("reply"))
            .await
            .expect("send");
        let sent = ch.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].target, "chat-9");
        assert_eq!(sent[0].body, "hi");
    }
}

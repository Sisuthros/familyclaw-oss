//! Channel message types and the bridge to the Resonance Bus.
//!
//! This module defines three layers:
//! - [`ChannelKind`] — which channel technology a message represents.
//! - [`OutboundMessage`] — a message sent outward from the platform.
//! - [`InboundMessage`] — a raw message received from a channel.
//! - [`InboundEnvelope`] — the canonicalized envelope an inbound message is
//!   converted into before being published to the Resonance Bus.
//!
//! ## Why `InboundEnvelope` lives here
//! The channel layer is the Resonance Bus's **edge to the outside world**:
//! it is the layer that produces bus messages from inbound traffic
//! (`inbound message → InboundEnvelope → familyclaw_bus::BusMessage`, design §3).
//!
//! The type is deliberately **separate** from `familyclaw_bus::BusMessage`
//! (the bus's payload enum): this is an origin-aware *envelope* (channel id,
//! sender, conversation), whereas the bus's `BusMessage` is a content enum
//! (text/emotional pulse/latent/…). The names were separated so the two
//! distinct types no longer share the name `BusMessage` across crate
//! boundaries. The actual conversion `InboundEnvelope → familyclaw_bus::BusMessage`
//! is done in the agent layer (which depends on both crates), so the channel
//! layer stays independent of the bus's internal Ractor implementation and
//! the envelope remains serde-serializable for durable replay.

use familyclaw_core::{time, MessageId, Timestamp};
use serde::{Deserialize, Serialize};

/// A supported channel technology.
///
/// The real adapters (serenity for Discord, teloxide for Telegram, …) sit
/// behind the crate's feature flags; this enum only carries the information
/// about which channel a message came from or is going to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChannelKind {
    /// Discord (adapter behind the `discord` feature, e.g. serenity).
    Discord,
    /// Telegram (adapter behind the `telegram` feature; HTTP Bot API).
    Telegram,
    /// `WhatsApp` — enum variant only; `whatsapp` feature is reserved / not implemented.
    // Explicit rename so the serde form matches `as_str()`'s value
    // ("whatsapp"); `snake_case` would otherwise produce "whats_app".
    #[serde(rename = "whatsapp")]
    WhatsApp,
    /// `Signal` — enum variant only; `signal` feature is reserved / not implemented.
    Signal,
    /// In-memory test channel ([`crate::MockChannel`]) — no external SDK.
    Mock,
}

impl ChannelKind {
    /// A short, stable identifier string for logs and routing.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discord => "discord",
            Self::Telegram => "telegram",
            Self::WhatsApp => "whatsapp",
            Self::Signal => "signal",
            Self::Mock => "mock",
        }
    }

    /// Whether the channel requires an external channel SDK (and thus a feature flag).
    ///
    /// [`ChannelKind::Mock`] is the only one that works without an external dependency.
    #[must_use]
    pub const fn requires_external_sdk(self) -> bool {
        !matches!(self, Self::Mock)
    }
}

impl std::fmt::Display for ChannelKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The type of an outbound signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutboundKind {
    /// A regular text message to the user.
    #[default]
    Message,
    /// The channel's "is typing…" indicator (Discord typing, Telegram chat action).
    Typing,
    /// A short progress update during a long tool turn (not the final reply).
    Progress,
}

/// A message sent outward from the platform.
///
/// `target` is a channel-specific recipient address (e.g. a Discord channel
/// id, a Telegram chat id). It is interpreted by the channel adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// The channel-specific destination (channel id, chat id, phone number, …).
    pub target: String,
    /// The message's text content (left empty for the `Typing` kind).
    pub body: String,
    /// The signal's type — defaults to a regular message.
    #[serde(default)]
    pub kind: OutboundKind,
}

impl OutboundMessage {
    /// Builds an outbound message.
    ///
    /// # Errors
    /// [`crate::ChannelError::InvalidInput`] if the target or body is empty.
    pub fn new(target: impl Into<String>, body: impl Into<String>) -> crate::ChannelResult<Self> {
        let target = target.into();
        let body = body.into();
        if target.trim().is_empty() {
            return Err(crate::ChannelError::invalid_input(
                "outbound target must not be empty",
            ));
        }
        if body.is_empty() {
            return Err(crate::ChannelError::invalid_input(
                "outbound body must not be empty",
            ));
        }
        Ok(Self {
            target,
            body,
            kind: OutboundKind::Message,
        })
    }

    /// Builds a typing indicator for the given channel target.
    ///
    /// # Errors
    /// [`crate::ChannelError::InvalidInput`] if the target is empty.
    pub fn typing(target: impl Into<String>) -> crate::ChannelResult<Self> {
        let target = target.into();
        if target.trim().is_empty() {
            return Err(crate::ChannelError::invalid_input(
                "outbound target must not be empty",
            ));
        }
        Ok(Self {
            target,
            body: String::new(),
            kind: OutboundKind::Typing,
        })
    }

    /// Builds a short progress update during a long tool turn.
    ///
    /// # Errors
    /// [`crate::ChannelError::InvalidInput`] if the target or body is empty.
    pub fn progress(
        target: impl Into<String>,
        body: impl Into<String>,
    ) -> crate::ChannelResult<Self> {
        let target = target.into();
        let body = body.into();
        if target.trim().is_empty() {
            return Err(crate::ChannelError::invalid_input(
                "outbound target must not be empty",
            ));
        }
        if body.is_empty() {
            return Err(crate::ChannelError::invalid_input(
                "progress body must not be empty",
            ));
        }
        Ok(Self {
            target,
            body,
            kind: OutboundKind::Progress,
        })
    }
}

/// A raw message received from a channel, before bus canonicalization.
///
/// `sender` is a channel-specific sender address (user id, phone number),
/// `conversation` is the conversation/group/channel identifier the message
/// arrived within (used for replying).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundMessage {
    /// The channel-specific sender identifier.
    pub sender: String,
    /// The conversation/group/channel identifier (reply address).
    pub conversation: String,
    /// The message's text content.
    pub body: String,
}

impl InboundMessage {
    /// Builds a received raw message.
    ///
    /// # Errors
    /// [`crate::ChannelError::InvalidInput`] if the sender, conversation, or
    /// body is empty.
    pub fn new(
        sender: impl Into<String>,
        conversation: impl Into<String>,
        body: impl Into<String>,
    ) -> crate::ChannelResult<Self> {
        let sender = sender.into();
        let conversation = conversation.into();
        let body = body.into();
        if sender.trim().is_empty() {
            return Err(crate::ChannelError::invalid_input(
                "inbound sender must not be empty",
            ));
        }
        if conversation.trim().is_empty() {
            return Err(crate::ChannelError::invalid_input(
                "inbound conversation must not be empty",
            ));
        }
        if body.is_empty() {
            return Err(crate::ChannelError::invalid_input(
                "inbound body must not be empty",
            ));
        }
        Ok(Self {
            sender,
            conversation,
            body,
        })
    }

    /// Canonicalizes the received message into an [`InboundEnvelope`].
    ///
    /// `kind` and `channel_id` indicate which channel the message came from.
    /// A new [`MessageId`] and UTC timestamp are attached so the bus and the
    /// durable log can reference the message uniquely and deterministically.
    #[must_use]
    pub fn into_envelope(
        self,
        kind: ChannelKind,
        channel_id: impl Into<String>,
    ) -> InboundEnvelope {
        InboundEnvelope {
            id: MessageId::new(),
            kind,
            channel_id: channel_id.into(),
            sender: self.sender,
            conversation: self.conversation,
            body: self.body,
            received_at: time::now(),
        }
    }
}

/// A canonicalized, origin-aware message envelope flowing toward the
/// Resonance Bus.
///
/// This is the shape the channel layer produces from inbound traffic. It is
/// fully serde-serializable for durable replay and carries origin
/// information ([`ChannelKind`], `channel_id`, `sender`, `conversation`) so
/// a reply can be routed back to the correct channel.
///
/// **Note:** this is a different type from `familyclaw_bus::BusMessage` (the
/// bus's content enum). Conversion into the bus payload is done in the agent
/// layer, which depends on both crates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundEnvelope {
    /// The message's unique identifier on the bus.
    pub id: MessageId,
    /// The channel type the message arrived from.
    pub kind: ChannelKind,
    /// The identifier of the concrete channel instance the message arrived
    /// from (matches the [`crate::Channel::channel_id`] value).
    pub channel_id: String,
    /// The channel-specific sender identifier.
    pub sender: String,
    /// The conversation/group identifier (reply address).
    pub conversation: String,
    /// The message's text content.
    pub body: String,
    /// The time of receipt in UTC.
    pub received_at: Timestamp,
}

impl InboundEnvelope {
    /// Builds an [`OutboundMessage`] reply to this message with the given
    /// content. The reply is routed back to the same conversation.
    ///
    /// # Errors
    /// [`crate::ChannelError::InvalidInput`] if the reply content is empty.
    pub fn reply(&self, body: impl Into<String>) -> crate::ChannelResult<OutboundMessage> {
        OutboundMessage::new(self.conversation.clone(), body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_kind_str_and_sdk_flag() {
        assert_eq!(ChannelKind::Discord.as_str(), "discord");
        assert_eq!(ChannelKind::Telegram.as_str(), "telegram");
        assert_eq!(ChannelKind::WhatsApp.as_str(), "whatsapp");
        assert_eq!(ChannelKind::Signal.as_str(), "signal");
        assert_eq!(ChannelKind::Mock.as_str(), "mock");
        assert_eq!(ChannelKind::Discord.to_string(), "discord");

        assert!(ChannelKind::Discord.requires_external_sdk());
        assert!(!ChannelKind::Mock.requires_external_sdk());
    }

    #[test]
    fn channel_kind_serde_is_snake_case() {
        let json = serde_json::to_string(&ChannelKind::WhatsApp).expect("serialize");
        assert_eq!(json, "\"whatsapp\"");
        let back: ChannelKind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, ChannelKind::WhatsApp);
    }

    #[test]
    fn channel_kind_serde_matches_as_str_for_all_variants() {
        // Lock in the invariant: serde form == as_str() for every variant, so
        // that logs and serialization never diverge.
        for kind in [
            ChannelKind::Discord,
            ChannelKind::Telegram,
            ChannelKind::WhatsApp,
            ChannelKind::Signal,
            ChannelKind::Mock,
        ] {
            let json = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            let back: ChannelKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn outbound_new_validates() {
        assert!(OutboundMessage::new("c", "hi").is_ok());
        assert!(OutboundMessage::new("  ", "hi").is_err());
        assert!(OutboundMessage::new("c", "").is_err());
    }

    #[test]
    fn inbound_new_validates() {
        assert!(InboundMessage::new("u", "room", "hi").is_ok());
        assert!(InboundMessage::new("", "room", "hi").is_err());
        assert!(InboundMessage::new("u", " ", "hi").is_err());
        assert!(InboundMessage::new("u", "room", "").is_err());
    }

    #[test]
    fn inbound_into_envelope_carries_origin() {
        let inbound = InboundMessage::new("user42", "general", "hello").expect("valid");
        let env = inbound.into_envelope(ChannelKind::Discord, "discord-main");
        assert_eq!(env.kind, ChannelKind::Discord);
        assert_eq!(env.channel_id, "discord-main");
        assert_eq!(env.sender, "user42");
        assert_eq!(env.conversation, "general");
        assert_eq!(env.body, "hello");
        assert!(!env.id.is_nil());
    }

    #[test]
    fn distinct_envelopes_get_distinct_ids() {
        let a = InboundMessage::new("u", "r", "x")
            .expect("valid")
            .into_envelope(ChannelKind::Mock, "m");
        let b = InboundMessage::new("u", "r", "x")
            .expect("valid")
            .into_envelope(ChannelKind::Mock, "m");
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn envelope_reply_targets_conversation() {
        let env = InboundMessage::new("u", "room-7", "ping")
            .expect("valid")
            .into_envelope(ChannelKind::Telegram, "tg-1");
        let reply = env.reply("pong").expect("valid reply");
        assert_eq!(reply.target, "room-7");
        assert_eq!(reply.body, "pong");

        assert!(env.reply("").is_err());
    }

    #[test]
    fn envelope_serde_roundtrip() {
        let env = InboundMessage::new("u", "r", "body")
            .expect("valid")
            .into_envelope(ChannelKind::Signal, "sig-1");
        let json = serde_json::to_string(&env).expect("serialize");
        let back: InboundEnvelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(env, back);
    }
}

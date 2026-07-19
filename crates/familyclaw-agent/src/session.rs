//! Session isolation (F4) — per-message origin ([`MessageOrigin`]) and the
//! [`MessageOrigin::session_key`] derived from it.
//!
//! ## Why this module exists
//! `FamilyClaw` MVP runs **one** agent, **one** memory, and a **static**
//! reply target ([`Agent::with_reply_target`](crate::Agent::with_reply_target)).
//! This is correct exactly when there is one channel and one
//! conversation. As soon as there is more than one channel or
//! conversation, all conversations leak into the same context and
//! memory: the agent mixes up A and B. Session isolation separates
//! conversations by **session key**.
//!
//! ## What `session_key` is
//! `session_key = "<channel_id>:<conversation>"`. This is a natural key:
//! same channel + same conversation = same session; different
//! conversation = different session. The sender
//! ([`MessageOrigin::sender`]) travels along for auditing, but is **not**
//! part of the session key (the same conversation can be multi-party).
//!
//! ## Relationship to the F2 origin contract (DEPENDENCY)
//! [`MessageOrigin`] is F4's **interface**, which expects the F2 origin
//! contract (the origin field on the bus envelope
//! [`ResonanceMessage`](familyclaw_bus::ResonanceMessage)). The channel
//! layer already produces exactly the fields needed
//! ([`InboundEnvelope`](familyclaw_channels::InboundEnvelope):
//! `channel_id`, `conversation`, `sender`);
//! [`MessageOrigin::from_inbound_envelope`] maps them directly. Once F2
//! carries the origin into the bus envelope and
//! [`Agent::handle_turn`](crate::Agent::handle_turn) receives it
//! per-message, this type is ready to be wired up without further design.
//!
//! ## What F4 does once origin is wired up (documented implementation path)
//! One agent, one memory — **not** per-session Agent instances
//! (over-engineering). Isolation is done via a memory scope keyed by
//! session:
//! 1. **Write:** [`Agent::handle_turn`](crate::Agent::handle_turn)
//!    attaches the tag `session:<key>` to the memory (the origin-Some
//!    branch), [`session_tag`](MessageOrigin::session_tag).
//! 2. **Read:** [`Agent::think`](crate::Agent::think) filters recall by
//!    the same `session:<key>` tag → A's memories don't leak into B's
//!    context.
//! 3. **Reply target:** the reply is derived from the origin's
//!    conversation, not the static reply target — origin FIRST, fallback
//!    to static ([`MessageOrigin::reply_target`]).
//!
//! Step 2 (recall filtering) awaits a tag filter in the memory layer;
//! until then the `session:<key>` tag is already written now (steps 1 +
//! 3 are done), and reads will filter once the interface is available.
//! See [`session_tag`](MessageOrigin::session_tag).
//!
//! ## OSS boundary (Layer A)
//! Generic platform code: no hard-coded channel names, conversations,
//! keys, or paths. All origin information comes from the message at
//! runtime.

use serde::{Deserialize, Serialize};

/// Tag prefix for session-scoped memories. A memory is tagged with
/// `"<SESSION_TAG_PREFIX><session_key>"` on write, and recall is filtered
/// by the same tag on read — this way, different sessions' memories
/// don't leak into each other's context.
pub const SESSION_TAG_PREFIX: &str = "session:";

/// A single incoming message's **origin** (the shape of the F2 origin
/// contract): which channel, which conversation, and from whom the
/// message came.
///
/// All fields are [`String`], so the type is directly serde-serializable
/// (durable replay + the bus envelope's `origin` field, once F2 carries
/// it there).
///
/// ## Fields
/// - `channel_id` — the channel instance's identifier (e.g.
///   `"discord-main"`), corresponds to
///   [`InboundEnvelope::channel_id`](familyclaw_channels::InboundEnvelope).
/// - `conversation` — the conversation/group identifier (the reply
///   address), corresponds to
///   [`InboundEnvelope::conversation`](familyclaw_channels::InboundEnvelope).
/// - `sender` — the channel-specific sender identifier (for auditing;
///   **not** part of the session key), corresponds to
///   [`InboundEnvelope::sender`](familyclaw_channels::InboundEnvelope).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageOrigin {
    /// The channel instance's identifier (the first part of the session key).
    pub channel_id: String,
    /// The conversation/group identifier (the second part of the session
    /// key + the reply target).
    pub conversation: String,
    /// The channel-specific sender identifier (auditing; not in the
    /// session key).
    pub sender: String,
}

impl MessageOrigin {
    /// Builds an origin from its bare parts.
    #[must_use]
    pub fn new(
        channel_id: impl Into<String>,
        conversation: impl Into<String>,
        sender: impl Into<String>,
    ) -> Self {
        Self {
            channel_id: channel_id.into(),
            conversation: conversation.into(),
            sender: sender.into(),
        }
    }

    /// Derives the **session key** from the origin: `"<channel_id>:<conversation>"`.
    ///
    /// This is F4's core: two messages belong to the same session **iff**
    /// they came from the same channel and the same conversation. The
    /// sender doesn't affect the key (a multi-party conversation shares
    /// the session).
    #[must_use]
    pub fn session_key(&self) -> String {
        format!("{}:{}", self.channel_id, self.conversation)
    }

    /// Derives the **memory tag** from the session key:
    /// `"session:<channel_id>:<conversation>"`.
    ///
    /// [`Agent::handle_turn`](crate::Agent::handle_turn) attaches this to
    /// the memory on write, and [`Agent::think`](crate::Agent::think)
    /// filters recall by the same tag — this way, different sessions'
    /// memories stay separate even though the agent and memory are
    /// shared (no per-session instances).
    #[must_use]
    pub fn session_tag(&self) -> String {
        format!("{SESSION_TAG_PREFIX}{}", self.session_key())
    }

    /// The reply target for this origin: the conversation the message
    /// came from.
    ///
    /// F4 routing uses this per-message **before** the static reply
    /// target: the reply routes back to the same conversation, not to
    /// some fixed target. Corresponds to the channel layer's
    /// [`InboundEnvelope::reply`](familyclaw_channels::InboundEnvelope::reply)
    /// target (`conversation`).
    #[must_use]
    pub fn reply_target(&self) -> &str {
        &self.conversation
    }

    /// Maps the channel layer's
    /// [`InboundEnvelope`](familyclaw_channels::InboundEnvelope) into an
    /// origin. This is the **F2 wiring point**: the channel already
    /// produces exactly these fields, so the origin is obtained
    /// per-message without any new information.
    #[must_use]
    pub fn from_inbound_envelope(envelope: &familyclaw_channels::InboundEnvelope) -> Self {
        Self {
            channel_id: envelope.channel_id.clone(),
            conversation: envelope.conversation.clone(),
            sender: envelope.sender.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_channels::{ChannelKind, InboundMessage};

    #[test]
    fn session_key_is_channel_and_conversation() {
        let origin = MessageOrigin::new("discord-main", "general", "user-42");
        assert_eq!(origin.session_key(), "discord-main:general");
    }

    #[test]
    fn sender_does_not_affect_session_key() {
        // Same channel + conversation, different sender → SAME session (multi-party).
        let a = MessageOrigin::new("tg-1", "room-7", "alice");
        let b = MessageOrigin::new("tg-1", "room-7", "bob");
        assert_eq!(a.session_key(), b.session_key());
    }

    #[test]
    fn different_conversation_is_different_session() {
        // F4's core claim: different conversation = different session (no context leak).
        let a = MessageOrigin::new("discord-main", "channel-a", "u");
        let b = MessageOrigin::new("discord-main", "channel-b", "u");
        assert_ne!(a.session_key(), b.session_key());
    }

    #[test]
    fn different_channel_is_different_session() {
        let a = MessageOrigin::new("discord-main", "general", "u");
        let b = MessageOrigin::new("tg-main", "general", "u");
        assert_ne!(a.session_key(), b.session_key());
    }

    #[test]
    fn session_tag_prefixes_session_key() {
        let origin = MessageOrigin::new("discord-main", "general", "u");
        assert_eq!(origin.session_tag(), "session:discord-main:general");
        assert!(origin.session_tag().starts_with(SESSION_TAG_PREFIX));
        // The tag contains the whole session key.
        assert!(origin.session_tag().ends_with(&origin.session_key()));
    }

    #[test]
    fn reply_target_is_conversation() {
        let origin = MessageOrigin::new("discord-main", "general", "u");
        assert_eq!(origin.reply_target(), "general");
    }

    #[test]
    fn from_inbound_envelope_maps_origin_fields() {
        // F2 wiring point: channel envelope → MessageOrigin (per-message).
        let envelope = InboundMessage::new("user-42", "general", "hei")
            .expect("valid inbound")
            .into_envelope(ChannelKind::Discord, "discord-main");
        let origin = MessageOrigin::from_inbound_envelope(&envelope);
        assert_eq!(origin.channel_id, "discord-main");
        assert_eq!(origin.conversation, "general");
        assert_eq!(origin.sender, "user-42");
        // Session key derived directly from the envelope.
        assert_eq!(origin.session_key(), "discord-main:general");
        // Reply target = same conversation as the envelope's reply().
        assert_eq!(origin.reply_target(), envelope.conversation);
    }

    #[test]
    fn message_origin_serde_roundtrip() {
        // Serde-serializable: ready for the bus envelope's origin field
        // (F2) and for durable replay.
        let origin = MessageOrigin::new("discord-main", "general", "user-42");
        let json = serde_json::to_string(&origin).expect("serialize");
        let back: MessageOrigin = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(origin, back);
    }
}

//! The bus's message types: [`BusMessage`] (payload) and [`ResonanceMessage`]
//! (envelope with metadata).
//!
//! The Resonance Bus is an **affective nervous system** (design §2.2): each
//! being's emotion state can *leak* into the bus, and other beings sense it
//! ([`BusMessage::EmotionPulse`]). Messages always carry the sender's identity
//! ([`ResonanceMessage::from`]), so the receiver knows **who** is resonating.
//!
//! ## OSS boundary (Layer A)
//! Nothing in this module hardcodes family members' souls, model names,
//! keys, or paths. Being identifiers ([`BeingId`]) and model identifiers are
//! always supplied at runtime; examples use generic names (`agent_a`,
//! `agent_b`).

use std::fmt;

use familyclaw_core::ids::{AgentId, MessageId};
use familyclaw_core::time::{self, Timestamp};
use familyclaw_emotion::EmotionState;
use familyclaw_latent::LatentVector;
use serde::{Deserialize, Serialize};

/// Identifier for a being (agent) joined to the bus.
///
/// This is a thin newtype around [`AgentId`]: the bus talks about *beings*
/// rather than plain agent identifiers, but the identity is the same. A
/// distinct type makes the bus interface self-documenting and prevents
/// confusing a bus participant with other identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BeingId(AgentId);

impl BeingId {
    /// Creates a new random being identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(AgentId::new())
    }

    /// Wraps an existing [`AgentId`] as a being identifier.
    #[must_use]
    pub const fn from_agent_id(id: AgentId) -> Self {
        Self(id)
    }

    /// Returns the wrapped [`AgentId`].
    #[must_use]
    pub const fn agent_id(&self) -> AgentId {
        self.0
    }
}

impl Default for BeingId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for BeingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl From<AgentId> for BeingId {
    fn from(id: AgentId) -> Self {
        Self(id)
    }
}

impl From<BeingId> for AgentId {
    fn from(id: BeingId) -> Self {
        id.0
    }
}

/// The kind of task-lifecycle event a being can publish to the bus.
///
/// This is deliberately *lightweight and generic* — the actual task model
/// lives in the bridge layer (`familyclaw-bridge`). The bus only relays the
/// signal, so siblings can react to each other's work progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventKind {
    /// The task was created.
    Created,
    /// The task was started (moved into progress).
    Started,
    /// The task's progress was updated.
    Progress,
    /// The task completed successfully.
    Completed,
    /// The task failed.
    Failed,
    /// The task was handed off to another being.
    HandedOff,
    /// An application-specific event kind with a free-form name.
    Custom(String),
}

impl TaskEventKind {
    /// Returns the kind's stable identifier as a string (for logging, routing).
    #[must_use]
    pub fn as_label(&self) -> &str {
        match self {
            TaskEventKind::Created => "created",
            TaskEventKind::Started => "started",
            TaskEventKind::Progress => "progress",
            TaskEventKind::Completed => "completed",
            TaskEventKind::Failed => "failed",
            TaskEventKind::HandedOff => "handed_off",
            TaskEventKind::Custom(name) => name.as_str(),
        }
    }
}

/// The payload of a message traveling on the bus.
///
/// This is the Resonance Bus's "language". The variants cover both ordinary
/// text/task communication and the affective nervous system's core messages
/// ([`EmotionPulse`](BusMessage::EmotionPulse), [`Latent`](BusMessage::Latent)).
///
/// `#[non_exhaustive]` so new message types can be added without breaking
/// downstream code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BusMessage {
    /// A plain text message between beings.
    Text {
        /// The message's text content.
        body: String,
    },

    /// Latent telepathy: a being's hidden state ([`LatentVector`]) along with
    /// a text shadow that always travels with it. Text is the source of
    /// truth — latent is an optimization (design §2.4, see `familyclaw-latent`).
    Latent {
        /// The sending model's hidden-state vector.
        vector: LatentVector,
        /// The text shadow the receiver falls back to if the latent does
        /// not apply.
        text_shadow: String,
    },

    /// **Affective pulse:** a being's emotion state leaks into the bus. This
    /// is the base message of the affective-contagion mechanism — when one
    /// sibling is, say, in a creative flow, the others sense it.
    EmotionPulse {
        /// The sending being's momentary emotion state.
        state: EmotionState,
    },

    /// A task-lifecycle event (a lightweight signal; the full model lives in
    /// the bridge layer).
    TaskEvent {
        /// The event's kind.
        event: TaskEventKind,
        /// The task's identifier (free-form, e.g. the bridge layer's id).
        task_id: String,
        /// An optional human-readable description.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },

    /// An operator-granted approval: asks the receiving agent to resume a
    /// suspended turn (the suspend/resume bridge). Carries only the approval
    /// identifier (as a string) — no payload, no secrets.
    ///
    /// This is a **control signal, not content**: it does NOT go into the
    /// model's conversation pipeline and does not start a new LLM turn;
    /// instead it is routed directly to the agent's resume path. The
    /// receiver parses the identifier and continues the tool loop that was
    /// waiting for approval.
    ResumeApproval {
        /// The granted approval's identifier (as a UUID string). Not a secret.
        approval_id: String,
    },

    /// An application-/adapter-specific message with a free-form JSON payload.
    /// Allows extensions without changing the core type.
    Custom {
        /// The message's kind (free-form name).
        name: String,
        /// JSON payload.
        payload: serde_json::Value,
    },
}

impl BusMessage {
    /// Builds a text message.
    pub fn text(body: impl Into<String>) -> Self {
        BusMessage::Text { body: body.into() }
    }

    /// Builds an affective pulse from the given emotion state.
    #[must_use]
    pub fn emotion_pulse(state: EmotionState) -> Self {
        BusMessage::EmotionPulse { state }
    }

    /// Builds a latent message from a hidden state and a text shadow.
    pub fn latent(vector: LatentVector, text_shadow: impl Into<String>) -> Self {
        BusMessage::Latent {
            vector,
            text_shadow: text_shadow.into(),
        }
    }

    /// Builds a task event.
    pub fn task_event(event: TaskEventKind, task_id: impl Into<String>) -> Self {
        BusMessage::TaskEvent {
            event,
            task_id: task_id.into(),
            detail: None,
        }
    }

    /// Is this an affective pulse (contagion routing relies on this)?
    #[must_use]
    pub fn is_emotion_pulse(&self) -> bool {
        matches!(self, BusMessage::EmotionPulse { .. })
    }

    /// A short kind identifier for logging and metrics.
    #[must_use]
    pub fn kind_label(&self) -> &'static str {
        match self {
            BusMessage::Text { .. } => "text",
            BusMessage::Latent { .. } => "latent",
            BusMessage::EmotionPulse { .. } => "emotion_pulse",
            BusMessage::TaskEvent { .. } => "task_event",
            BusMessage::ResumeApproval { .. } => "resume_approval",
            BusMessage::Custom { .. } => "custom",
        }
    }
}

/// A message traveling through the bus **in an envelope**: payload + sender +
/// identifier + timestamp.
///
/// The bus enriches every publish into this shape, so receivers know who is
/// resonating and when. The envelope is `Clone`, since the same message is
/// duplicated for each receiving being (broadcast).
/// The **origin** of a single incoming message in the bus envelope (the F2
/// origin contract): which channel, which conversation, and from whom the
/// message came.
///
/// This is a **lightweight, independent** type at the bus's lowest layer:
/// just three [`String`] fields, so the bus does not have to depend on the
/// channel or agent layer (the layering direction is one-way — they depend
/// on the bus, not the other way around). The agent layer's `MessageOrigin`
/// maps directly to and from this one (`channel_id`/`conversation`/`sender`
/// match field for field).
///
/// ## Why origin belongs in the envelope, not the payload
/// [`BusMessage`] is *content* (text/emotion pulse/latent). Origin is
/// *metadata* about where the content arrived from — the same kind of
/// information as the sender ([`ResonanceMessage::from`]) and the timestamp.
/// Origin in the envelope gives the receiver (the agent) a way to derive the
/// **reply target** per message (`conversation`), so multiple conversations
/// don't get routed incorrectly.
///
/// ## OSS boundary (Layer A)
/// No hardcoded channel names, conversations, or keys — everything comes
/// from the message arriving at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageOrigin {
    /// The channel instance's identifier (e.g. `"discord-main"`).
    pub channel_id: String,
    /// The conversation/group identifier (reply address).
    pub conversation: String,
    /// The channel-specific sender identifier (for auditing).
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

    /// The reply target for this origin: the conversation the message came from.
    #[must_use]
    pub fn reply_target(&self) -> &str {
        &self.conversation
    }
}

/// A message traveling through the bus **in an envelope**: payload + sender +
/// identifier + timestamp + optional per-message origin ([`origin`]).
///
/// The bus enriches every publish into this shape, so receivers know who is
/// resonating and when. The envelope is `Clone`, since the same message is
/// duplicated for each receiving being (broadcast).
///
/// [`origin`] (the F2 origin contract) carries the outside-world origin
/// ([`MessageOrigin`]: channel + conversation + sender) when the message came
/// in through the channel layer. The receiving agent derives the reply
/// target per message from it; in the `None` case it falls back to the
/// static reply target.
///
/// [`origin`]: ResonanceMessage::origin
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResonanceMessage {
    /// The message's unique identifier.
    pub id: MessageId,
    /// The being that sent the message.
    pub from: BeingId,
    /// The UTC timestamp of when it was sent.
    pub at: Timestamp,
    /// The actual payload.
    pub payload: BusMessage,
    /// Per-message origin (channel/conversation/sender), if the message came
    /// in from the outside world through the channel layer. Internally
    /// generated messages (pulses between beings, say/announce) leave this
    /// as `None`. `#[serde(default)]` → old (origin-less) durable replays
    /// deserialize to `None` (backward-compatible).
    #[serde(default)]
    pub origin: Option<MessageOrigin>,
}

impl ResonanceMessage {
    /// Builds an envelope with a fresh identifier and the current timestamp.
    /// Origin is `None` (an internal message); an outside-world origin is
    /// attached with [`with_origin`](Self::with_origin).
    #[must_use]
    pub fn new(from: BeingId, payload: BusMessage) -> Self {
        Self {
            id: MessageId::new(),
            from,
            at: time::now(),
            payload,
            origin: None,
        }
    }

    /// Attaches a per-message origin to the envelope (F2). Returns `self`
    /// for chaining.
    #[must_use]
    pub fn with_origin(mut self, origin: MessageOrigin) -> Self {
        self.origin = Some(origin);
        self
    }

    /// The per-message origin, if set.
    #[must_use]
    pub fn origin(&self) -> Option<&MessageOrigin> {
        self.origin.as_ref()
    }

    /// Whether this envelope's payload is an affective pulse.
    #[must_use]
    pub fn is_emotion_pulse(&self) -> bool {
        self.payload.is_emotion_pulse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_emotion::Dimension;

    #[test]
    fn being_id_wraps_agent_id_transparently() {
        let agent = AgentId::new();
        let being = BeingId::from_agent_id(agent);
        assert_eq!(being.agent_id(), agent);

        // serde is transparent: the being identifier serializes like an agent.
        let being_json = serde_json::to_string(&being).expect("ser being");
        let agent_json = serde_json::to_string(&agent).expect("ser agent");
        assert_eq!(being_json, agent_json);

        let back: BeingId = serde_json::from_str(&being_json).expect("de being");
        assert_eq!(back, being);
    }

    #[test]
    fn being_id_conversions_roundtrip() {
        let agent = AgentId::new();
        let being: BeingId = agent.into();
        let back: AgentId = being.into();
        assert_eq!(agent, back);
    }

    #[test]
    fn being_id_new_and_default_are_unique() {
        assert_ne!(BeingId::new(), BeingId::new());
        assert_ne!(BeingId::default(), BeingId::default());
    }

    #[test]
    fn task_event_kind_labels() {
        assert_eq!(TaskEventKind::Created.as_label(), "created");
        assert_eq!(TaskEventKind::Completed.as_label(), "completed");
        assert_eq!(TaskEventKind::Custom("deploy".into()).as_label(), "deploy");
    }

    #[test]
    fn bus_message_constructors_and_labels() {
        assert_eq!(BusMessage::text("hi").kind_label(), "text");

        let pulse = BusMessage::emotion_pulse(EmotionState::neutral());
        assert!(pulse.is_emotion_pulse());
        assert_eq!(pulse.kind_label(), "emotion_pulse");

        let latent = BusMessage::latent(LatentVector::new(vec![0.1], "agent_a/v1"), "shadow");
        assert_eq!(latent.kind_label(), "latent");
        assert!(!latent.is_emotion_pulse());

        let task = BusMessage::task_event(TaskEventKind::Started, "task-1");
        assert_eq!(task.kind_label(), "task_event");

        let resume = BusMessage::ResumeApproval {
            approval_id: "abc".into(),
        };
        assert_eq!(resume.kind_label(), "resume_approval");
        assert!(!resume.is_emotion_pulse());
    }

    #[test]
    fn resume_approval_serde_roundtrip_carries_only_id() {
        let msg = BusMessage::ResumeApproval {
            approval_id: "11111111-2222-4333-8444-555555555555".into(),
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        // Only the identifier + kind — no payload, no secrets.
        assert!(json.contains("resume_approval"));
        assert!(json.contains("11111111-2222-4333-8444-555555555555"));
        let back: BusMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(msg, back);
    }

    #[test]
    fn bus_message_serde_roundtrip_all_variants() {
        let mut state = EmotionState::neutral();
        state.stimulate(Dimension::Joy, 50.0);

        let messages = vec![
            BusMessage::text("hello"),
            BusMessage::emotion_pulse(state),
            BusMessage::latent(LatentVector::new(vec![1.0, 2.0], "agent_a/v1"), "shadow"),
            BusMessage::task_event(TaskEventKind::Progress, "task-9"),
            BusMessage::Custom {
                name: "ping".into(),
                payload: serde_json::json!({ "n": 1 }),
            },
        ];

        for msg in messages {
            let json = serde_json::to_string(&msg).expect("serialize");
            let back: BusMessage = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(msg, back);
        }
    }

    #[test]
    fn resonance_message_carries_sender_and_timestamp() {
        let from = BeingId::new();
        let before = time::now();
        let envelope = ResonanceMessage::new(from, BusMessage::text("hi"));
        let after = time::now();

        assert_eq!(envelope.from, from);
        assert!(envelope.at >= before && envelope.at <= after);
        assert!(!envelope.id.is_nil());
        assert!(!envelope.is_emotion_pulse());
    }

    #[test]
    fn resonance_message_detects_emotion_pulse() {
        let env = ResonanceMessage::new(
            BeingId::new(),
            BusMessage::emotion_pulse(EmotionState::neutral()),
        );
        assert!(env.is_emotion_pulse());
    }

    #[test]
    fn resonance_message_serde_roundtrip() {
        let env = ResonanceMessage::new(BeingId::new(), BusMessage::text("roundtrip"));
        let json = serde_json::to_string(&env).expect("serialize");
        let back: ResonanceMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(env, back);
    }

    // ---- F2 per-message origin ------------------------------------------

    #[test]
    fn new_message_has_no_origin_by_default() {
        // Internally generated messages (say/pulse) do not carry an origin.
        let env = ResonanceMessage::new(BeingId::new(), BusMessage::text("internal"));
        assert!(env.origin().is_none());
    }

    #[test]
    fn with_origin_attaches_per_message_origin() {
        let origin = MessageOrigin::new("discord-main", "general", "user-42");
        let env = ResonanceMessage::new(BeingId::new(), BusMessage::text("from outside"))
            .with_origin(origin.clone());
        assert_eq!(env.origin(), Some(&origin));
        assert_eq!(env.origin().expect("origin").reply_target(), "general");
    }

    #[test]
    fn message_origin_serde_roundtrip() {
        let origin = MessageOrigin::new("tg-1", "room-7", "alice");
        let json = serde_json::to_string(&origin).expect("serialize");
        let back: MessageOrigin = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(origin, back);
    }

    #[test]
    fn resonance_message_with_origin_serde_roundtrip() {
        let env = ResonanceMessage::new(BeingId::new(), BusMessage::text("origin-roundtrip"))
            .with_origin(MessageOrigin::new("discord-main", "channel-a", "u"));
        let json = serde_json::to_string(&env).expect("serialize");
        let back: ResonanceMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(env, back);
        assert_eq!(back.origin().expect("origin").conversation, "channel-a");
    }

    #[test]
    fn legacy_envelope_without_origin_field_deserializes_to_none() {
        // Backward compatibility: an old durable-replay row does NOT contain
        // the `origin` field. `#[serde(default)]` → None, no deserialization
        // error. We build the "old" shape by serializing the current one and
        // removing the origin key from the JSON object (more faithful than a
        // hand-coded row: the id/timestamp formats stay correct regardless
        // of the internal representation).
        let env = ResonanceMessage::new(BeingId::new(), BusMessage::text("old"));
        let mut value: serde_json::Value = serde_json::to_value(&env).expect("to value");
        value
            .as_object_mut()
            .expect("object")
            .remove("origin")
            .expect("current shape contains the origin key");
        assert!(
            value.get("origin").is_none(),
            "origin key removed (simulates an old row)"
        );

        let back: ResonanceMessage =
            serde_json::from_value(value).expect("legacy envelope deserializes");
        assert!(back.origin().is_none(), "missing origin field → None");
        match back.payload {
            BusMessage::Text { body } => assert_eq!(body, "old"),
            other => panic!("expected Text, got {other:?}"),
        }
    }
}

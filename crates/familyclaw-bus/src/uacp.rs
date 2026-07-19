//! `μACP` — **micro Agent Communication Protocol**: a four-verb
//! communication calculus ([`AcpVerb`]) on top of the Resonance Bus.
//!
//! Background (design §2, `SOLID_PLAN`): communication between agents
//! reduces to four performatives — `PING` (liveness), `TELL` (fact),
//! `ASK` (a query that expects a reply), and `OBSERVE` (an event). This is
//! the same core as the classic FIPA-ACL, but stripped down to a minimum for
//! low-latency (~34 ms) communication.
//!
//! ## Relationship to the existing bus
//! This module **does not replace** the [`BusHandle::publish`] path — it
//! *translates* the verb into an existing [`BusMessage`] and publishes it
//! via the normal route ([`BusHandle::send_acp`]). The verb and the optional
//! target ([`AcpEnvelope::to`]) travel along as metadata, so the receiver can
//! route and filter messages by performative. The bus itself remains
//! broadcast-based (delivery to all others) — the target is the *intended*
//! recipient, which other beings may disregard.
//!
//! ## OSS boundary (Layer A)
//! No hardcoded family names, IDs, or keys. Being identifiers and content are
//! always supplied at runtime; examples use generic names (`agent_a`).

use serde::{Deserialize, Serialize};

use familyclaw_core::Result;

use crate::bus::BusHandle;
use crate::message::{BeingId, BusMessage};

/// Stable `name` identifier for `μACP` messages inside the [`BusMessage::Custom`]
/// envelope.
///
/// The receiver distinguishes `μACP` traffic from other bus traffic by this
/// name; the verb and target are found in the payload's JSON fields.
pub const ACP_MESSAGE_NAME: &str = "uacp";

/// `μACP`'s four performatives (speech acts).
///
/// Deliberately a minimal set — broader semantics are built on top of these
/// in higher layers, not by adding variants here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpVerb {
    /// **Liveness** probe: "are you alive?" Does not expect a content reply,
    /// just a sign of the being's reachability.
    Ping,
    /// **Fact**: the sender states something it believes to be true. Does not
    /// expect a reply.
    Tell,
    /// **Query**: the sender requests information and **expects a reply**
    /// (usually a `Tell` sent back).
    Ask,
    /// **Event**: the sender publishes an observation/event that others may
    /// take note of. Informational, not a direct request.
    Observe,
}

impl AcpVerb {
    /// A short, stable identifier for logging, routing, and metrics.
    #[must_use]
    pub const fn as_label(&self) -> &'static str {
        match self {
            AcpVerb::Ping => "ping",
            AcpVerb::Tell => "tell",
            AcpVerb::Ask => "ask",
            AcpVerb::Observe => "observe",
        }
    }

    /// Does this performative expect a reply? (Only [`Ask`](AcpVerb::Ask).)
    #[must_use]
    pub const fn expects_reply(&self) -> bool {
        matches!(self, AcpVerb::Ask)
    }
}

/// `μACP` envelope: performative + sender + (optional) target + content.
///
/// `to` is the **intended** recipient. The Resonance Bus delivers via
/// broadcast to all other beings, so the target is a filtering hint for the
/// receiver, not a hard routing constraint. `None` means the message is
/// addressed to everyone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpEnvelope {
    /// The speech act (performative).
    pub verb: AcpVerb,
    /// The being sending the message.
    pub from: BeingId,
    /// The intended recipient, or `None` if the message is addressed to everyone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<BeingId>,
    /// Free-form text payload (source of truth, as elsewhere on the bus).
    pub payload: String,
}

impl AcpEnvelope {
    /// Builds an envelope addressed to everyone (`to = None`).
    pub fn broadcast(verb: AcpVerb, from: BeingId, payload: impl Into<String>) -> Self {
        Self {
            verb,
            from,
            to: None,
            payload: payload.into(),
        }
    }

    /// Builds an envelope addressed to a single being.
    pub fn directed(verb: AcpVerb, from: BeingId, to: BeingId, payload: impl Into<String>) -> Self {
        Self {
            verb,
            from,
            to: Some(to),
            payload: payload.into(),
        }
    }

    /// Helper: a `PING` envelope for a liveness probe (to everyone).
    pub fn ping(from: BeingId) -> Self {
        Self::broadcast(AcpVerb::Ping, from, String::new())
    }

    /// Helper: a `TELL` envelope (fact) to everyone.
    pub fn tell(from: BeingId, payload: impl Into<String>) -> Self {
        Self::broadcast(AcpVerb::Tell, from, payload)
    }

    /// Helper: an `ASK` envelope (query) to a specific being.
    pub fn ask(from: BeingId, to: BeingId, payload: impl Into<String>) -> Self {
        Self::directed(AcpVerb::Ask, from, to, payload)
    }

    /// Helper: an `OBSERVE` envelope (event) to everyone.
    pub fn observe(from: BeingId, payload: impl Into<String>) -> Self {
        Self::broadcast(AcpVerb::Observe, from, payload)
    }

    /// Translates a `μACP` envelope into the bus's own [`BusMessage`]
    /// ([`BusMessage::Custom`], name [`ACP_MESSAGE_NAME`]). The verb, target,
    /// and content are encoded into the JSON payload, so the receiver can
    /// interpret the performative and recover the envelope
    /// ([`AcpEnvelope::from_bus_message`]).
    #[must_use]
    pub fn to_bus_message(&self) -> BusMessage {
        BusMessage::Custom {
            name: ACP_MESSAGE_NAME.to_string(),
            payload: serde_json::json!({
                "verb": self.verb,
                "to": self.to,
                "payload": self.payload,
            }),
        }
    }

    /// Like [`to_bus_message`](Self::to_bus_message), but **consumes** the
    /// envelope (avoids cloning the text content on the send path).
    #[must_use]
    pub fn into_bus_message(self) -> BusMessage {
        BusMessage::Custom {
            name: ACP_MESSAGE_NAME.to_string(),
            payload: serde_json::json!({
                "verb": self.verb,
                "to": self.to,
                "payload": self.payload,
            }),
        }
    }

    /// Attempts to recover a `μACP` envelope from a bus message. Returns `None`
    /// if the message is not a `μACP` message (wrong name) or the payload does
    /// not parse.
    ///
    /// `from` does not travel inside [`BusMessage`] (it lives in the
    /// envelope's [`ResonanceMessage::from`] field), so it is supplied
    /// separately.
    ///
    /// [`ResonanceMessage::from`]: crate::message::ResonanceMessage::from
    #[must_use]
    pub fn from_bus_message(from: BeingId, msg: &BusMessage) -> Option<Self> {
        let BusMessage::Custom { name, payload } = msg else {
            return None;
        };
        if name != ACP_MESSAGE_NAME {
            return None;
        }
        let verb: AcpVerb = serde_json::from_value(payload.get("verb")?.clone()).ok()?;
        let to: Option<BeingId> = match payload.get("to") {
            Some(serde_json::Value::Null) | None => None,
            Some(v) => serde_json::from_value(v.clone()).ok()?,
        };
        let body = payload.get("payload")?.as_str()?.to_string();
        Some(Self {
            verb,
            from,
            to,
            payload: body,
        })
    }
}

impl BusHandle {
    /// Sends a `μACP` envelope over the existing [`publish`](BusHandle::publish)
    /// path. The verb is translated into [`BusMessage::Custom`]
    /// ([`AcpEnvelope::to_bus_message`]); the publish itself, the broadcast,
    /// and supervision all work exactly like a regular publish.
    ///
    /// This is an **addition** on top of publish, not a replacement: all
    /// existing bus traffic continues as before.
    ///
    /// # Errors
    /// [`FamilyClawError::Bus`] if sending the message to the bus fails.
    ///
    /// [`FamilyClawError::Bus`]: familyclaw_core::FamilyClawError::Bus
    pub fn send_acp(&self, envelope: AcpEnvelope) -> Result<()> {
        let from = envelope.from;
        self.publish(from, envelope.into_bus_message())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::being::{BeingInfo, CollectedLog, CollectorBeing};
    use crate::message::ResonanceMessage;
    use crate::ResonanceBus;
    use ractor::{Actor, ActorRef};
    use std::time::Duration as StdDuration;

    async fn join_being(
        bus: &BusHandle,
        name: &str,
    ) -> (BeingId, ActorRef<ResonanceMessage>, CollectedLog) {
        let log = CollectorBeing::new_log();
        let (actor, _h) = Actor::spawn(None, CollectorBeing, log.clone())
            .await
            .expect("spawn being");
        let id = BeingId::new();
        bus.register(BeingInfo::new(id, name, actor.clone()))
            .expect("register");
        (id, actor, log)
    }

    async fn settle() {
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    }

    fn log_len(log: &CollectedLog) -> usize {
        log.lock().expect("lock").len()
    }

    #[test]
    fn verb_labels_and_reply_semantics() {
        assert_eq!(AcpVerb::Ping.as_label(), "ping");
        assert_eq!(AcpVerb::Tell.as_label(), "tell");
        assert_eq!(AcpVerb::Ask.as_label(), "ask");
        assert_eq!(AcpVerb::Observe.as_label(), "observe");

        // Only ASK expects a reply.
        assert!(AcpVerb::Ask.expects_reply());
        assert!(!AcpVerb::Ping.expects_reply());
        assert!(!AcpVerb::Tell.expects_reply());
        assert!(!AcpVerb::Observe.expects_reply());
    }

    #[test]
    fn envelope_roundtrips_through_bus_message_for_all_verbs() {
        let from = BeingId::new();
        let to = BeingId::new();
        let cases = [
            AcpEnvelope::ping(from),
            AcpEnvelope::tell(from, "the sky is blue"),
            AcpEnvelope::ask(from, to, "what time is it?"),
            AcpEnvelope::observe(from, "the door opened"),
        ];

        for env in cases {
            let msg = env.to_bus_message();
            // Translates into a Custom message with a stable name.
            match &msg {
                BusMessage::Custom { name, .. } => assert_eq!(name, ACP_MESSAGE_NAME),
                other => panic!("expected Custom, got {other:?}"),
            }
            // And round-trips back into the same envelope.
            let back =
                AcpEnvelope::from_bus_message(env.from, &msg).expect("μACP message parses back");
            assert_eq!(back, env, "verb {} did not round-trip", env.verb.as_label());
        }
    }

    #[test]
    fn non_acp_custom_message_is_not_parsed() {
        let msg = BusMessage::Custom {
            name: "not-uacp".to_string(),
            payload: serde_json::json!({ "verb": "ping" }),
        };
        assert!(AcpEnvelope::from_bus_message(BeingId::new(), &msg).is_none());

        // A non-Custom message also returns None.
        assert!(AcpEnvelope::from_bus_message(BeingId::new(), &BusMessage::text("hi")).is_none());
    }

    #[test]
    fn directed_vs_broadcast_target() {
        let from = BeingId::new();
        let to = BeingId::new();
        assert_eq!(AcpEnvelope::tell(from, "x").to, None);
        assert_eq!(AcpEnvelope::ask(from, to, "y").to, Some(to));
        assert_eq!(AcpEnvelope::ping(from).to, None);
        assert_eq!(AcpEnvelope::observe(from, "z").to, None);
    }

    #[test]
    fn envelope_serde_roundtrip() {
        let env = AcpEnvelope::ask(BeingId::new(), BeingId::new(), "payload");
        let json = serde_json::to_string(&env).expect("serialize");
        let back: AcpEnvelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(env, back);
    }

    /// `send_acp` routes the `μACP` message over the EXISTING publish path:
    /// siblings receive it, the sender does not (same semantics as publish),
    /// and the message parses into the correct verb on the receiver's side.
    #[tokio::test]
    async fn send_acp_routes_verbs_over_publish_path() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, log_a) = join_being(&bus, "agent_a").await;
        let (_id_b, _b, log_b) = join_being(&bus, "agent_b").await;

        bus.send_acp(AcpEnvelope::tell(id_a, "a fact for the siblings"))
            .expect("send_acp tell");
        settle().await;

        // The sender does not receive its own message (publish's broadcast rule applies).
        assert_eq!(
            log_len(&log_a),
            0,
            "the sender does not receive its own μACP message"
        );
        // The sibling receives it, and it parses into the correct verb.
        assert_eq!(log_len(&log_b), 1, "the sibling receives the μACP message");
        let received = log_b.lock().expect("lock")[0].clone();
        assert_eq!(received.from, id_a);
        let acp = AcpEnvelope::from_bus_message(received.from, &received.payload)
            .expect("the received message is μACP");
        assert_eq!(acp.verb, AcpVerb::Tell);
        assert_eq!(acp.payload, "a fact for the siblings");
        assert_eq!(acp.from, id_a);

        bus.stop();
    }

    /// Different verbs each route as their own performative over the same path.
    #[tokio::test]
    async fn each_verb_arrives_with_correct_performative() {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (id_a, _a, _la) = join_being(&bus, "agent_a").await;
        let (id_b, _b, log_b) = join_being(&bus, "agent_b").await;

        bus.send_acp(AcpEnvelope::ping(id_a)).expect("ping");
        bus.send_acp(AcpEnvelope::ask(id_a, id_b, "a question"))
            .expect("ask");
        bus.send_acp(AcpEnvelope::observe(id_a, "an event"))
            .expect("observe");
        settle().await;

        let received = log_b.lock().expect("lock");
        assert_eq!(received.len(), 3, "three μACP messages delivered");
        let verbs: Vec<AcpVerb> = received
            .iter()
            .map(|m| {
                AcpEnvelope::from_bus_message(m.from, &m.payload)
                    .expect("μACP")
                    .verb
            })
            .collect();
        assert_eq!(verbs, vec![AcpVerb::Ping, AcpVerb::Ask, AcpVerb::Observe]);

        // ASK carried a target (directed), the others did not.
        let ask = received
            .iter()
            .find_map(|m| {
                let e = AcpEnvelope::from_bus_message(m.from, &m.payload)?;
                (e.verb == AcpVerb::Ask).then_some(e)
            })
            .expect("ask is found");
        assert_eq!(ask.to, Some(id_b));

        drop(received);
        bus.stop();
    }
}

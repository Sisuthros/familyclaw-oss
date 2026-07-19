//! Channel ↔ Resonance Bus adapter (design §2.2 / §3).
//!
//! This module is the **missing joint** between two halves of §3:
//! `familyclaw-channels` produces origin-aware [`InboundEnvelope`]
//! envelopes, and `familyclaw-bus` consumes [`BusMessage`] payloads.
//! These are **different types in different crates** (the naming clash is
//! deliberately resolved: the channel layer's envelope is `InboundEnvelope`,
//! the bus's content is `BusMessage`). The adapter lives in the agent layer
//! because it's the only crate that depends on **both** — this keeps the
//! channel layer bus-independent and avoids a crate cycle.
//!
//! ## What the adapter provides
//! - [`envelope_to_bus_message`] — converts a canonicalized envelope's
//!   content into the bus's text payload. (A free function rather than a
//!   `From` impl, because the orphan rule prevents `impl From` for two
//!   foreign types.)
//! - [`publish_envelope`] — publishes a single envelope to the bus on
//!   behalf of a given being ([`BeingId`]).
//! - [`pump_channel_to_bus`] — consumes a channel's entire inbound stream
//!   and feeds it to the bus. This is the concrete `pump_to` closure that
//!   §3 promised but that didn't previously exist.
//!
//! ## OSS boundary (Layer A)
//! Generic platform code: no hard-coded being names, keys, or paths. The
//! sending being's [`BeingId`] is always supplied at runtime.

use familyclaw_bus::{BeingId, BusHandle, BusMessage, MessageOrigin};
use familyclaw_channels::{pump_to, InboundEnvelope, MessageStream};

use crate::{FamilyClawError, Result};

/// Converts a channel-layer [`InboundEnvelope`] into a bus
/// [`BusMessage`] payload.
///
/// The envelope's text content (`body`) becomes [`BusMessage::Text`] — the
/// form that the bus's beings (agents) process as turns. The envelope's
/// origin information (channel, sender, conversation) is **preserved** in
/// the bus envelope's `origin` field (see [`envelope_origin`] +
/// [`publish_envelope`]) — it doesn't fit in the *payload* (`BusMessage`)
/// itself, but it's required for reply routing.
///
/// This is a free function rather than
/// `impl From<InboundEnvelope> for BusMessage`, because both types are
/// *foreign* to this crate (the orphan rule would forbid a `From` impl
/// here, and neither foreign crate should depend on the other just for
/// this conversion).
#[must_use]
pub fn envelope_to_bus_message(envelope: InboundEnvelope) -> BusMessage {
    BusMessage::text(envelope.body)
}

/// Extracts a channel envelope's **per-message origin** into the bus
/// layer's [`MessageOrigin`] (F2 origin contract).
///
/// The channel layer already produces exactly the fields needed
/// (`channel_id`, `conversation`, `sender`); this maps them field by field
/// into the bus envelope's `origin` field, which the receiving agent uses
/// to derive the reply target (`conversation`) per message. This way,
/// more than one conversation routes correctly without bleeding into each
/// other.
#[must_use]
pub fn envelope_origin(envelope: &InboundEnvelope) -> MessageOrigin {
    MessageOrigin::new(
        envelope.channel_id.clone(),
        envelope.conversation.clone(),
        envelope.sender.clone(),
    )
}

/// Publishes a single channel envelope to the Resonance Bus on behalf of
/// a given being, **preserving origin information** (F2).
///
/// `from` is the being ([`BeingId`]) that the channel acts as a
/// *mailbox* for — e.g. the channel's own bus seat. This way, incoming
/// traffic from the outside world gets an unambiguous sender identity
/// within the bus, which other beings see in the
/// [`familyclaw_bus::ResonanceMessage::from`] field.
///
/// The envelope's origin (channel/conversation/sender) is carried into
/// the bus message's `origin` field ([`envelope_origin`]) — not dropped.
/// This is the core of F2: the receiving agent derives the reply target
/// from this, per message, so multiple conversations route to the
/// correct targets.
///
/// # Errors
/// [`FamilyClawError::Bus`] if publishing to the bus fails.
pub fn publish_envelope(bus: &BusHandle, from: BeingId, envelope: InboundEnvelope) -> Result<()> {
    let origin = envelope_origin(&envelope);
    bus.publish_with_origin(from, envelope_to_bus_message(envelope), origin)
}

/// Pumps a channel's entire inbound stream into the Resonance Bus.
///
/// Consumes the [`MessageStream`] to completion and publishes every
/// envelope to the bus on behalf of the being `from`. Returns when the
/// stream closes or when publishing fails (the error is propagated). The
/// return value is the number of messages fed into the bus.
///
/// This is the concrete implementation of §3's "one channel feeds the
/// bus" acceptance criterion: channel → [`pump_to`] → adapter →
/// `bus.publish`.
///
/// # Errors
/// [`FamilyClawError::Bus`] if pumping the stream or publishing to the
/// bus fails.
pub async fn pump_channel_to_bus(
    stream: MessageStream,
    bus: BusHandle,
    from: BeingId,
) -> Result<usize> {
    pump_to(stream, move |envelope| {
        // Translate the bus error into the channel crate's error type,
        // which `pump_to` expects — `pump_channel_to_bus` itself returns
        // it onward as a `FamilyClawError`
        // (`ChannelError: From -> FamilyClawError::Bus`).
        publish_envelope(&bus, from, envelope)
            .map_err(|e| familyclaw_channels::ChannelError::backend("bus", e.to_string()))
    })
    .await
    .map_err(FamilyClawError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_bus::ResonanceBus;
    use familyclaw_channels::{ChannelKind, InboundMessage};

    /// Helper: builds a canonicalized envelope from test data.
    fn envelope(body: &str) -> InboundEnvelope {
        InboundMessage::new("user-1", "general", body)
            .expect("valid inbound")
            .into_envelope(ChannelKind::Mock, "mock-1")
    }

    #[test]
    fn envelope_converts_to_text_bus_message() {
        let bus_msg = envelope_to_bus_message(envelope("hello bus"));
        match bus_msg {
            BusMessage::Text { body } => assert_eq!(body, "hello bus"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn publish_envelope_reaches_a_registered_being() {
        use familyclaw_bus::{BeingInfo, CollectorBeing};
        use ractor::Actor;

        let bus = ResonanceBus::start(None).await.expect("bus");

        // Receiving being that collects the messages it gets into a log.
        let log = CollectorBeing::new_log();
        let (inbox, _h) = Actor::spawn(None, CollectorBeing, log.clone())
            .await
            .expect("spawn");
        let receiver = BeingId::new();
        bus.register(BeingInfo::new(receiver, "agent_b", inbox))
            .expect("register");

        // The channel's own bus seat (sender).
        let channel_seat = BeingId::new();
        publish_envelope(&bus, channel_seat, envelope("from the channel")).expect("publish");

        // Let the message be delivered.
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;

        let collected = log.lock().expect("lock");
        assert_eq!(
            collected.len(),
            1,
            "the being should have received a message"
        );
        assert_eq!(collected[0].from, channel_seat, "sender = channel seat");
        match &collected[0].payload {
            BusMessage::Text { body } => assert_eq!(body, "from the channel"),
            other => panic!("expected Text, got {other:?}"),
        }
        drop(collected);
        bus.stop();
    }

    #[tokio::test]
    async fn pump_channel_to_bus_drives_a_real_channel_into_a_real_bus() {
        use familyclaw_bus::{BeingInfo, CollectorBeing};
        use familyclaw_channels::{Channel, MockChannel};
        use ractor::Actor;

        let bus = ResonanceBus::start(None).await.expect("bus");

        // Receiving being on the bus.
        let log = CollectorBeing::new_log();
        let (inbox, _h) = Actor::spawn(None, CollectorBeing, log.clone())
            .await
            .expect("spawn");
        let receiver = BeingId::new();
        bus.register(BeingInfo::new(receiver, "agent_b", inbox))
            .expect("register");

        // A REAL familyclaw-channels channel (not a private duplicate).
        let channel = MockChannel::new("mock-feed").expect("channel");
        let stream = channel.receive().expect("stream");

        // Feed three messages and close the stream so the pump finishes.
        for i in 0..3 {
            channel
                .inject(InboundMessage::new("u", "c", format!("msg{i}")).expect("inbound"))
                .expect("inject");
        }
        channel.close_inbound();

        let channel_seat = BeingId::new();
        let pumped = pump_channel_to_bus(stream, bus.clone(), channel_seat)
            .await
            .expect("pump");
        assert_eq!(pumped, 3, "three messages were pumped into the bus");

        // Let delivery complete.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        let collected = log.lock().expect("lock");
        assert_eq!(collected.len(), 3, "the receiver got all three");
        assert!(collected.iter().all(|m| m.from == channel_seat));
        match &collected[0].payload {
            BusMessage::Text { body } => assert_eq!(body, "msg0"),
            other => panic!("expected Text, got {other:?}"),
        }
        drop(collected);
        bus.stop();
    }
}

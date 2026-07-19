//! `LatentChannel` implementation for the Resonance Bus.
//!
//! This module provides [`BusLatentChannel`], which implements the [`LatentChannel`]
//! trait for the [`BusHandle`] type. It enables latent telepathy between
//! siblings using the Resonance Bus infrastructure.
//!
//! ## Translate-on-send (P4)
//! By default (when [`BusLatentChannel::new`] creates the channel), the send
//! path does plain [`RecursiveLink`] dimension matching (pad/truncate/resize)
//! — the same behavior as before. If a channel is given a
//! [`VectorTranslator`]
//! ([`with_translator`](BusLatentChannel::with_translator)), the outgoing
//! vector is *translated* into the receiver's space before delivery:
//!
//! 1. Link and dimension checks pass through as in the default case.
//! 2. The translator [`translate`](familyclaw_latent::translate::VectorTranslator::translate)
//!    fits the vector to the receiver's size.
//! 3. If the translation is **lossy**
//!    ([`fallback_reason`](familyclaw_latent::translate::VectorTranslator::fallback_reason)
//!    returns `Some`), the transmission falls back to **text**
//!    ([`FallbackReason::ProjectionFailed`]) — [`deliver`](LatentChannel::deliver)
//!    then sends a [`BusMessage::text`] (preserving the "NaN → text"
//!    guarantee, since non-finite values make the translation lossy).
//!
//! The receiving/decoding side (`agent.rs`) is deliberately left untouched —
//! it is an interface deferred to behind the family boundary.
//!
//! [`LatentChannel`]: familyclaw_latent::channel::LatentChannel
//! [`BusHandle`]: crate::bus::BusHandle

use familyclaw_latent::{
    channel::{FallbackReason, LatentChannel, Transmission, TransmissionMode},
    link::RecursiveLink,
    translate::VectorTranslator,
};

use crate::{
    bus::BusHandle,
    message::{BeingId, BusMessage},
};

/// [`LatentChannel`] implementation for the Resonance Bus.
///
/// Uses [`BusHandle`] to send a [`LatentMessage`] to another being. This
/// enables latent telepathy between siblings.
///
/// [`LatentMessage`]: familyclaw_latent::channel::LatentMessage
pub struct BusLatentChannel {
    /// The channel user's identifier.
    being_id: BeingId,
    /// The sending model's identifier.
    sender_model: String,
    /// The defined bridges to other models.
    links: Vec<RecursiveLink>,
    /// Optional cross-model translator for the send path. `None` =
    /// plain [`RecursiveLink`] dimension matching (default, backward-compatible).
    translator: Option<VectorTranslator>,
    /// A reference to the bus for sending messages.
    bus: BusHandle,
}

impl BusLatentChannel {
    /// Creates a new [`BusLatentChannel`] instance.
    ///
    /// The send path defaults to plain [`RecursiveLink`] dimension matching
    /// (pad/truncate/resize). Add cross-model translation with
    /// [`with_translator`](Self::with_translator).
    ///
    /// # Arguments
    /// * `being_id` - The channel user's (being's) identifier.
    /// * `sender_model` - The sending model's identifier (e.g. `agent_a/v1`).
    /// * `bus` - The [`BusHandle`] used to send messages.
    pub fn new(being_id: BeingId, sender_model: String, bus: BusHandle) -> Self {
        Self {
            being_id,
            sender_model,
            links: Vec::new(), // Initialized empty. Links are added separately.
            translator: None,  // Default: no translation — dimension matching only.
            bus,
        }
    }

    /// Sets a [`VectorTranslator`] on the send path and returns `self` for
    /// chaining.
    ///
    /// When a translator is given, [`plan`](LatentChannel::plan) fits the
    /// outgoing vector into the receiver's space with the translator after
    /// the link and dimension checks. A lossy translation → text fallback
    /// ([`FallbackReason::ProjectionFailed`]). Existing [`new`](Self::new)
    /// callers keep the pad/truncate behavior (not a breaking change).
    #[must_use]
    pub fn with_translator(mut self, translator: VectorTranslator) -> Self {
        self.translator = Some(translator);
        self
    }

    /// Adds a new [`RecursiveLink`] to the channel.
    ///
    /// This is used to define how the hidden state can be converted into a
    /// form another model can receive.
    pub fn add_link(&mut self, link: RecursiveLink) {
        self.links.push(link);
    }

    /// Builds a successful latent-transmission result.
    ///
    /// [`Transmission`]'s field constructors are crate-internal, so we build
    /// the result from public fields (`projected` = `Some`,
    /// `fallback_reason` = `None`).
    fn latent_transmission(
        projected: familyclaw_latent::link::ProjectedLatent,
        text: String,
    ) -> Transmission {
        Transmission {
            mode: TransmissionMode::Latent,
            text,
            projected: Some(projected),
            fallback_reason: None,
        }
    }

    /// Builds a text-fallback result with the given reason.
    fn text_transmission(reason: FallbackReason, text: String) -> Transmission {
        Transmission {
            mode: TransmissionMode::Text,
            text,
            projected: None,
            fallback_reason: Some(reason),
        }
    }
}

impl LatentChannel for BusLatentChannel {
    fn sender_model(&self) -> &str {
        &self.sender_model
    }

    fn link_to(&self, target_model: &str) -> Option<RecursiveLink> {
        // Find the first link that matches the target model.
        self.links
            .iter()
            .find(|link| link.target_model() == target_model)
            .cloned()
    }

    /// Override of the send path's [`plan`](LatentChannel::plan).
    ///
    /// If the channel has no translator, behavior matches the trait default
    /// (plain [`RecursiveLink`] dimension matching). If a translator is
    /// given, the outgoing vector is translated into the receiver's space
    /// after the link and dimension checks; a lossy translation → text
    /// fallback.
    ///
    /// Fallback order (same as the default):
    /// 1. The receiver does not support latent → text ([`FallbackReason::ReceiverTextOnly`]).
    /// 2. The message has no hidden state → text ([`FallbackReason::NoLatentAvailable`]).
    /// 3. No bridge to the target model → text ([`FallbackReason::NoLink`]).
    /// 4. The bridge's target dimension ≠ the receiver's dimension → text ([`FallbackReason::NoLink`]).
    /// 5. A translator is given → translate; lossy → text ([`FallbackReason::ProjectionFailed`]), otherwise latent.
    /// 6. No translator → project with the link; error → text ([`FallbackReason::ProjectionFailed`]), otherwise latent.
    fn plan(
        &self,
        message: &familyclaw_latent::channel::LatentMessage,
        receiver: &familyclaw_latent::channel::ReceiverProfile,
    ) -> Transmission {
        let text = message.text.clone();

        // 1. The receiver does not support latent.
        if !receiver.accepts_latent {
            return Self::text_transmission(FallbackReason::ReceiverTextOnly, text);
        }

        // 2. The message has no hidden state.
        let Some(latent) = &message.latent else {
            return Self::text_transmission(FallbackReason::NoLatentAvailable, text);
        };

        // 3. No bridge to the target model.
        let Some(link) = self.link_to(&receiver.model_id) else {
            return Self::text_transmission(FallbackReason::NoLink, text);
        };

        // 4. The bridge's target dimension does not match the receiver → treat as no bridge.
        if link.target_dims() != receiver.dims {
            return Self::text_transmission(FallbackReason::NoLink, text);
        }

        // 5. Projection / translation.
        match &self.translator {
            // 5a. Cross-model translation: always ProjectedLatent, lossiness decides.
            Some(translator) => {
                let projected = translator.translate(latent, receiver);
                match VectorTranslator::fallback_reason(&projected) {
                    Some(reason) => Self::text_transmission(reason, text),
                    None => Self::latent_transmission(projected, text),
                }
            }
            // 5b. Default: plain dimension matching via the link. Error → text.
            None => match link.project(latent) {
                Ok(projected) => Self::latent_transmission(projected, text),
                Err(_) => Self::text_transmission(FallbackReason::ProjectionFailed, text),
            },
        }
    }

    fn deliver(&mut self, transmission: &Transmission) -> familyclaw_latent::Result<()> {
        // Convert the `Transmission` into a `BusMessage` and send it via the bus.
        let bus_message = if transmission.mode.is_latent() {
            // Use the latent if it is available.
            if let Some(projected) = &transmission.projected {
                BusMessage::latent(
                    projected.vector.clone(),  // Use the projected model's latent
                    transmission.text.clone(), // The text shadow always comes along
                )
            } else {
                // This should not happen, since mode is Latent.
                return Err(familyclaw_latent::FamilyClawError::bus(
                    "Internal error: Latent mode but missing projected data",
                ));
            }
        } else {
            // Fall back to text.
            BusMessage::text(transmission.text.clone())
        };

        // Send the message via the bus.
        self.bus.publish(self.being_id, bus_message).map_err(|e| {
            familyclaw_latent::FamilyClawError::bus(format!("Failed to deliver via bus: {e}"))
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::being::{BeingInfo, CollectedLog, CollectorBeing};
    use crate::bus::ResonanceBus;
    use crate::message::ResonanceMessage;
    use familyclaw_latent::channel::{LatentMessage, ReceiverProfile};
    use familyclaw_latent::vector::LatentVector;
    use ractor::{Actor, ActorRef};
    use std::time::Duration as StdDuration;

    fn latent_vec(dims: Vec<f32>) -> LatentVector {
        LatentVector::new(dims, "agent_a/v1")
    }

    // Builds a channel with its OWN ResonanceBus (not shared, no serial_test).
    // plan is a &self method that does not touch the bus, but BusHandle is
    // required by the struct.
    async fn channel_with(
        sender_model: &str,
        translator: Option<VectorTranslator>,
        links: Vec<RecursiveLink>,
    ) -> BusLatentChannel {
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let mut ch = BusLatentChannel::new(BeingId::new(), sender_model.to_string(), bus);
        for link in links {
            ch.add_link(link);
        }
        if let Some(tr) = translator {
            ch = ch.with_translator(tr);
        }
        ch
    }

    #[tokio::test]
    async fn identity_translator_round_trips_lossless() {
        // Identity translator for a same-model translation → lossless latent.
        let tr = VectorTranslator::identity("agent_a/v1", 3);
        let link = RecursiveLink::new("agent_a/v1", 3, "agent_b/v1", 3);
        let ch = channel_with("agent_a/v1", Some(tr), vec![link]).await;

        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "hi");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Latent);
        assert!(t.fallback_reason.is_none());
        let projected = t.projected.expect("latent carries projection");
        // Identity preserves the values; the vector is translated to the receiver's model.
        assert_eq!(projected.vector.dims, vec![1.0, 2.0, 3.0]);
        assert_eq!(projected.vector.model_id, "agent_b/v1");
        assert!(projected.lossless);
        assert_eq!(t.text, "hi");

        ch.bus.stop();
    }

    #[tokio::test]
    async fn lossy_translation_gives_text_fallback() {
        // Truncating (4 → 2) is lossy → text fallback.
        let tr = VectorTranslator::identity("agent_a/v1", 4);
        let link = RecursiveLink::new("agent_a/v1", 4, "agent_b/v1", 2);
        let ch = channel_with("agent_a/v1", Some(tr), vec![link]).await;

        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0, 4.0]), "lossy");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::ProjectionFailed));
        assert!(t.projected.is_none());
        assert_eq!(t.text, "lossy");

        ch.bus.stop();
    }

    #[tokio::test]
    async fn nan_input_gives_text_fallback() {
        // A non-finite input makes the translation lossy → text.
        let tr = VectorTranslator::identity("agent_a/v1", 2);
        let link = RecursiveLink::new("agent_a/v1", 2, "agent_b/v1", 2);
        let ch = channel_with("agent_a/v1", Some(tr), vec![link]).await;

        let bad = LatentVector::new(vec![1.0, f32::NAN], "agent_a/v1");
        let msg = LatentMessage::with_latent(bad, "nan me");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::ProjectionFailed));
        assert_eq!(t.text, "nan me");

        ch.bus.stop();
    }

    #[tokio::test]
    async fn without_translator_keeps_pad_truncate_behavior() {
        // Without a translator: the link pads (2 → 4), lossless latent.
        let link = RecursiveLink::new("agent_a/v1", 2, "agent_b/v1", 4);
        let ch = channel_with("agent_a/v1", None, vec![link]).await;

        let msg = LatentMessage::with_latent(latent_vec(vec![9.0, 8.0]), "pad");
        let rx = ReceiverProfile::latent("agent_b/v1", 4);

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Latent);
        let projected = t.projected.expect("has projection");
        assert_eq!(projected.vector.dims, vec![9.0, 8.0, 0.0, 0.0]);

        ch.bus.stop();
    }

    #[tokio::test]
    async fn without_translator_nan_still_falls_back_to_text() {
        // The NaN → text guarantee also holds on the default path (the link rejects NaN).
        let link = RecursiveLink::new("agent_a/v1", 2, "agent_b/v1", 2);
        let ch = channel_with("agent_a/v1", None, vec![link]).await;

        let bad = LatentVector::new(vec![1.0, f32::NAN], "agent_a/v1");
        let msg = LatentMessage::with_latent(bad, "fallback me");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::ProjectionFailed));

        ch.bus.stop();
    }

    #[tokio::test]
    async fn plan_falls_back_when_receiver_text_only() {
        let tr = VectorTranslator::identity("agent_a/v1", 3);
        let link = RecursiveLink::new("agent_a/v1", 3, "agent_b/v1", 3);
        let ch = channel_with("agent_a/v1", Some(tr), vec![link]).await;

        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "txt");
        let rx = ReceiverProfile::text_only("agent_b/v1");

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::ReceiverTextOnly));

        ch.bus.stop();
    }

    #[tokio::test]
    async fn plan_falls_back_when_no_link() {
        // A translator exists, but there is no bridge to the receiver.
        let tr = VectorTranslator::identity("agent_a/v1", 3);
        let ch = channel_with("agent_a/v1", Some(tr), vec![]).await;

        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "x");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let t = ch.plan(&msg, &rx);
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::NoLink));

        ch.bus.stop();
    }

    /// Helper: spawns a collector being and registers it on the bus.
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

    #[tokio::test]
    async fn transmit_lossless_delivers_latent_over_bus() {
        // Own ResonanceBus (no serial_test): sender + receiver.
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (_rx_id, _rx_actor, rx_log) = join_being(&bus, "agent_b").await;

        let sender_id = BeingId::new();
        let mut ch = BusLatentChannel::new(sender_id, "agent_a/v1".to_string(), bus.clone())
            .with_translator(VectorTranslator::identity("agent_a/v1", 3));
        ch.add_link(RecursiveLink::new("agent_a/v1", 3, "agent_b/v1", 3));

        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "telepathy");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let t = ch.transmit(&msg, &rx).expect("transmit ok");
        assert_eq!(t.mode, TransmissionMode::Latent);
        settle().await;

        let received = rx_log.lock().expect("lock");
        assert_eq!(received.len(), 1);
        match &received[0].payload {
            BusMessage::Latent {
                vector,
                text_shadow,
            } => {
                assert_eq!(vector.dims, vec![1.0, 2.0, 3.0]);
                assert_eq!(vector.model_id, "agent_b/v1");
                assert_eq!(text_shadow, "telepathy");
            }
            other => panic!("expected Latent, got {other:?}"),
        }

        bus.stop();
    }

    #[tokio::test]
    async fn transmit_lossy_delivers_text_over_bus() {
        // A lossy translation → the receiver gets BusMessage::Text.
        let bus = ResonanceBus::start(None).await.expect("start bus");
        let (_rx_id, _rx_actor, rx_log) = join_being(&bus, "agent_b").await;

        let sender_id = BeingId::new();
        let mut ch = BusLatentChannel::new(sender_id, "agent_a/v1".to_string(), bus.clone())
            .with_translator(VectorTranslator::identity("agent_a/v1", 4));
        ch.add_link(RecursiveLink::new("agent_a/v1", 4, "agent_b/v1", 2));

        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0, 4.0]), "shadow only");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);

        let t = ch.transmit(&msg, &rx).expect("transmit ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        settle().await;

        let received = rx_log.lock().expect("lock");
        assert_eq!(received.len(), 1);
        match &received[0].payload {
            BusMessage::Text { body } => assert_eq!(body, "shadow only"),
            other => panic!("expected Text fallback, got {other:?}"),
        }

        bus.stop();
    }
}

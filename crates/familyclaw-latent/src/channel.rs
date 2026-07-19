//! Latent channel — transfer of hidden state between siblings, **always**
//! with a text fallback.
//!
//! [`LatentChannel`] is an abstraction for one-way communication that
//! primarily attempts to transfer a [`LatentVector`] (latent telepathy) and,
//! **if that fails** — incompatible models, projection failure, an unsound
//! vector, or a receiver that doesn't support latent — **automatically
//! falls back to text** ([`TransmissionMode::Text`]).
//!
//! ## Core design principle (design §2.4)
//! > "Always **fall back to text** if models are incompatible — never break
//! > communication. The highest communication mode, not the only one."
//!
//! For this reason, [`LatentChannel::transmit`] **never returns an error
//! purely for incompatibility**: it returns a successful [`Transmission`]
//! result whose `mode` reports whether latent or text was used. A channel
//! may return an error only for a genuine transport failure (e.g. the
//! connection dropped), never for semantic incompatibility.

use serde::{Deserialize, Serialize};

use crate::link::{ProjectedLatent, RecursiveLink};
use crate::vector::LatentVector;

/// The message's transmission mode: the highest tier that succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransmissionMode {
    /// The hidden state was successfully transferred as a latent vector.
    Latent,
    /// Latent was not possible -> fell back to text representation.
    Text,
}

impl TransmissionMode {
    /// Whether the transfer was done in latent form.
    #[must_use]
    pub fn is_latent(self) -> bool {
        matches!(self, Self::Latent)
    }

    /// Whether the transfer was done as a text fallback.
    #[must_use]
    pub fn is_text(self) -> bool {
        matches!(self, Self::Text)
    }
}

/// The reason a latent transfer had to fall back to text.
///
/// Stored in the [`Transmission::fallback_reason`] field for diagnostics and
/// research metrics (how often latent works vs. falls back).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason {
    /// The receiver doesn't support latent reception at all.
    ReceiverTextOnly,
    /// There is no [`RecursiveLink`] bridge from the sender to the
    /// receiver's model.
    NoLink,
    /// The dimension projection failed (model or dimension mismatch,
    /// unsound vector).
    ProjectionFailed,
    /// No latent representation was available (only text was supplied).
    NoLatentAvailable,
}

impl FallbackReason {
    /// A short, human-readable description of the reason (for logging).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReceiverTextOnly => "receiver does not support latent",
            Self::NoLink => "no recursive link to target model",
            Self::ProjectionFailed => "dimension projection failed",
            Self::NoLatentAvailable => "no latent representation available",
        }
    }
}

/// The sending sibling wants to transfer either the hidden state, text, or
/// both.
///
/// `latent` is optional: if absent, the transfer goes straight through as
/// text. `text` is **mandatory** — it is always the safety net that
/// guarantees communication never breaks even if latent fails.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatentMessage {
    /// Optional hidden state (latent telepathy). `None` = text only.
    pub latent: Option<LatentVector>,
    /// The text representation — always included for the fallback.
    pub text: String,
}

impl LatentMessage {
    /// Builds a message from both a hidden state and text.
    #[must_use]
    pub fn with_latent(latent: LatentVector, text: impl Into<String>) -> Self {
        Self {
            latent: Some(latent),
            text: text.into(),
        }
    }

    /// Builds a text-only message (no hidden state).
    #[must_use]
    pub fn text_only(text: impl Into<String>) -> Self {
        Self {
            latent: None,
            text: text.into(),
        }
    }
}

/// The outcome of a single transfer: what the receiver actually got, and in
/// which form.
///
/// `mode` reports the highest tier that succeeded. If `mode` is
/// [`TransmissionMode::Latent`], `projected` holds the vector fitted to the
/// target space. If `mode` is [`TransmissionMode::Text`], `fallback_reason`
/// reports why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transmission {
    /// The highest transmission mode that succeeded.
    pub mode: TransmissionMode,
    /// The text representation the receiver got (always present — the
    /// safety net).
    pub text: String,
    /// The hidden state fitted to the target model, if `mode == Latent`.
    pub projected: Option<ProjectedLatent>,
    /// The reason for the fallback, if `mode == Text`.
    pub fallback_reason: Option<FallbackReason>,
}

impl Transmission {
    /// Builds a successful latent-transfer result.
    #[must_use]
    fn latent(projected: ProjectedLatent, text: String) -> Self {
        Self {
            mode: TransmissionMode::Latent,
            text,
            projected: Some(projected),
            fallback_reason: None,
        }
    }

    /// Builds a text-fallback result with the given reason.
    #[must_use]
    fn text(reason: FallbackReason, text: String) -> Self {
        Self {
            mode: TransmissionMode::Text,
            text,
            projected: None,
            fallback_reason: Some(reason),
        }
    }
}

/// The receiving sibling's capabilities for latent reception.
///
/// Describes which model the receiver uses and what size hidden state it
/// expects. If `accepts_latent` is `false`, all transfers go through as
/// text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiverProfile {
    /// The receiver's model identifier (`"provider/model"`).
    pub model_id: String,
    /// The latent dimensionality the receiver expects.
    pub dims: usize,
    /// Whether the receiver accepts latent transfer at all.
    pub accepts_latent: bool,
}

impl ReceiverProfile {
    /// A receiver that accepts latent with the given model and size.
    #[must_use]
    pub fn latent(model_id: impl Into<String>, dims: usize) -> Self {
        Self {
            model_id: model_id.into(),
            dims,
            accepts_latent: true,
        }
    }

    /// A receiver that accepts only text.
    #[must_use]
    pub fn text_only(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            dims: 0,
            accepts_latent: false,
        }
    }
}

/// A latent channel between siblings.
///
/// The implementation is responsible for the concrete transport (in-process,
/// bus, network). The trait-level default implementation
/// [`LatentChannel::transmit`] handles the **shared fallback logic** so that
/// every channel behaves the same way: latent first, text always as a
/// backup.
pub trait LatentChannel {
    /// The sender's model, which produces the `LatentVector` hidden states.
    fn sender_model(&self) -> &str;

    /// Looks up a [`RecursiveLink`] bridge from the sender's model to the
    /// given target model, if one is defined.
    ///
    /// `None` means no bridge exists -> the transfer falls back to text
    /// ([`FallbackReason::NoLink`]).
    fn link_to(&self, target_model: &str) -> Option<RecursiveLink>;

    /// Delivers a finished [`Transmission`] result to the receiver.
    ///
    /// This is the only method that touches the actual transport. It may
    /// return an error **only** for a transport failure (connection
    /// dropped), never for semantic incompatibility — the fallback is
    /// already handled at the [`transmit`](LatentChannel::transmit) level.
    ///
    /// # Errors
    /// Returns an error only for a genuine transport failure.
    fn deliver(&mut self, transmission: &Transmission) -> crate::Result<()>;

    /// Sends a message to the receiver, choosing the highest possible
    /// transmission mode and falling back to text when needed.
    ///
    /// Algorithm:
    /// 1. If the receiver doesn't accept latent -> text
    ///    ([`FallbackReason::ReceiverTextOnly`]).
    /// 2. If the message has no hidden state -> text
    ///    ([`FallbackReason::NoLatentAvailable`]).
    /// 3. If the sender has no bridge to the target model -> text
    ///    ([`FallbackReason::NoLink`]).
    /// 4. If the projection fails (model/dimension/NaN error) -> text
    ///    ([`FallbackReason::ProjectionFailed`]).
    /// 5. Otherwise latent: project and deliver.
    ///
    /// The result is finally delivered via the
    /// [`deliver`](LatentChannel::deliver) method.
    ///
    /// # Errors
    /// Returns an error only if [`deliver`](LatentChannel::deliver) fails at
    /// the transport level. Incompatibility is **not** an error — it leads
    /// to a text fallback.
    fn transmit(
        &mut self,
        message: &LatentMessage,
        receiver: &ReceiverProfile,
    ) -> crate::Result<Transmission> {
        let result = self.plan(message, receiver);
        self.deliver(&result)?;
        Ok(result)
    }

    /// Decides the transmission mode **without** performing the transport.
    ///
    /// Separated from the [`transmit`](LatentChannel::transmit) method so
    /// the fallback logic can be tested and inspected without side effects.
    /// The default implementation usually doesn't need to be overridden.
    fn plan(&self, message: &LatentMessage, receiver: &ReceiverProfile) -> Transmission {
        let text = message.text.clone();

        // 1. Receiver doesn't support latent.
        if !receiver.accepts_latent {
            return Transmission::text(FallbackReason::ReceiverTextOnly, text);
        }

        // 2. Message has no hidden state.
        let Some(latent) = &message.latent else {
            return Transmission::text(FallbackReason::NoLatentAvailable, text);
        };

        // 3. No bridge to the target model.
        let Some(link) = self.link_to(&receiver.model_id) else {
            return Transmission::text(FallbackReason::NoLink, text);
        };

        // Make sure the bridge lands on the size the receiver expects.
        // If the link's target dimension doesn't match the receiver, the
        // projection would produce a wrongly sized vector -> treat as if
        // there were no bridge.
        if link.target_dims() != receiver.dims {
            return Transmission::text(FallbackReason::NoLink, text);
        }

        // 4. Projection. Any error -> text fallback (never propagate the
        // error upward).
        match link.project(latent) {
            Ok(projected) => Transmission::latent(projected, text),
            Err(_) => Transmission::text(FallbackReason::ProjectionFailed, text),
        }
    }
}

/// An in-memory test channel: collects delivered transfers in memory and
/// allows registering bridges for target models.
///
/// This is intended for testing and local development — it does not perform
/// real network transport. Production channels (bus, network) implement the
/// [`LatentChannel`] trait with their own [`deliver`](LatentChannel::deliver)
/// logic but inherit the same fallback behavior.
#[derive(Debug, Default)]
pub struct InMemoryLatentChannel {
    sender_model: String,
    links: Vec<RecursiveLink>,
    delivered: Vec<Transmission>,
    /// If `true`, [`deliver`](LatentChannel::deliver) simulates a transport
    /// failure.
    fail_delivery: bool,
}

impl InMemoryLatentChannel {
    /// Creates a channel with the given sender model.
    #[must_use]
    pub fn new(sender_model: impl Into<String>) -> Self {
        Self {
            sender_model: sender_model.into(),
            links: Vec::new(),
            delivered: Vec::new(),
            fail_delivery: false,
        }
    }

    /// Registers a bridge to a target model. Returns `self` for chaining.
    #[must_use]
    pub fn with_link(mut self, link: RecursiveLink) -> Self {
        self.links.push(link);
        self
    }

    /// Configures the channel to simulate a transport failure on delivery
    /// (for tests).
    #[must_use]
    pub fn failing_delivery(mut self) -> Self {
        self.fail_delivery = true;
        self
    }

    /// The transfers delivered so far (for test assertions).
    #[must_use]
    pub fn delivered(&self) -> &[Transmission] {
        &self.delivered
    }
}

impl LatentChannel for InMemoryLatentChannel {
    fn sender_model(&self) -> &str {
        &self.sender_model
    }

    fn link_to(&self, target_model: &str) -> Option<RecursiveLink> {
        self.links
            .iter()
            .find(|l| l.target_model() == target_model)
            .cloned()
    }

    fn deliver(&mut self, transmission: &Transmission) -> crate::Result<()> {
        if self.fail_delivery {
            return Err(crate::FamilyClawError::bus("simulated transport failure"));
        }
        self.delivered.push(transmission.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latent_vec(dims: Vec<f32>) -> LatentVector {
        LatentVector::new(dims, "agent_a/v1")
    }

    fn channel_with_link(target_model: &str, src: usize, tgt: usize) -> InMemoryLatentChannel {
        InMemoryLatentChannel::new("agent_a/v1").with_link(RecursiveLink::new(
            "agent_a/v1",
            src,
            target_model,
            tgt,
        ))
    }

    #[test]
    fn transmits_latent_when_compatible() {
        let mut ch = channel_with_link("agent_b/v1", 3, 3);
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "hello");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let t = ch.transmit(&msg, &rx).expect("transmit ok");
        assert_eq!(t.mode, TransmissionMode::Latent);
        assert!(t.mode.is_latent());
        assert!(t.fallback_reason.is_none());
        let projected = t.projected.expect("latent carries projection");
        assert_eq!(projected.vector.model_id, "agent_b/v1");
        assert_eq!(projected.vector.dims, vec![1.0, 2.0, 3.0]);
        // Text is always included as a safety net, even in latent mode.
        assert_eq!(t.text, "hello");
        assert_eq!(ch.delivered().len(), 1);
    }

    #[test]
    fn latent_bridges_differing_dimensions() {
        // Dimension-bridge test: 2-dimensional source -> 4-dimensional target.
        let mut ch = channel_with_link("agent_b/v1", 2, 4);
        let msg = LatentMessage::with_latent(latent_vec(vec![9.0, 8.0]), "bridge");
        let rx = ReceiverProfile::latent("agent_b/v1", 4);

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Latent);
        let projected = t.projected.expect("has projection");
        assert_eq!(projected.vector.dims, vec![9.0, 8.0, 0.0, 0.0]);
        assert_eq!(projected.target_dims, 4);
    }

    #[test]
    fn falls_back_to_text_when_receiver_text_only() {
        let mut ch = channel_with_link("agent_b/v1", 3, 3);
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "hi");
        let rx = ReceiverProfile::text_only("agent_b/v1");

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        assert!(t.mode.is_text());
        assert_eq!(t.fallback_reason, Some(FallbackReason::ReceiverTextOnly));
        assert!(t.projected.is_none());
        assert_eq!(t.text, "hi");
    }

    #[test]
    fn falls_back_to_text_when_no_latent_in_message() {
        let mut ch = channel_with_link("agent_b/v1", 3, 3);
        let msg = LatentMessage::text_only("just text");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::NoLatentAvailable));
        assert_eq!(t.text, "just text");
    }

    #[test]
    fn falls_back_to_text_when_no_link_to_target() {
        // The channel has a bridge to agent_b, but the receiver is agent_c.
        let mut ch = channel_with_link("agent_b/v1", 3, 3);
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "x");
        let rx = ReceiverProfile::latent("agent_c/v1", 3);

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::NoLink));
    }

    #[test]
    fn falls_back_when_link_target_dims_mismatch_receiver() {
        // The bridge produces a 4-dimensional vector, but the receiver expects 3.
        let mut ch = channel_with_link("agent_b/v1", 2, 4);
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0]), "x");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::NoLink));
    }

    #[test]
    fn falls_back_when_projection_fails_on_nan() {
        // The bridge exists and dimensions match, but the vector is unsound.
        let mut ch = channel_with_link("agent_b/v1", 2, 2);
        let bad = LatentVector::new(vec![1.0, f32::NAN], "agent_a/v1");
        let msg = LatentMessage::with_latent(bad, "fallback me");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::ProjectionFailed));
        assert_eq!(t.text, "fallback me");
    }

    #[test]
    fn falls_back_when_vector_model_does_not_match_link_source() {
        // The bridge's source is agent_a, but the vector claims to be agent_z.
        let mut ch = channel_with_link("agent_b/v1", 2, 2);
        let mismatched = LatentVector::new(vec![1.0, 2.0], "agent_z/v1");
        let msg = LatentMessage::with_latent(mismatched, "txt");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);

        let t = ch.transmit(&msg, &rx).expect("ok");
        assert_eq!(t.mode, TransmissionMode::Text);
        assert_eq!(t.fallback_reason, Some(FallbackReason::ProjectionFailed));
    }

    #[test]
    fn deliver_transport_failure_propagates() {
        let mut ch = channel_with_link("agent_b/v1", 3, 3).failing_delivery();
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "x");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let err = ch.transmit(&msg, &rx).expect_err("transport must fail");
        assert!(matches!(err, crate::FamilyClawError::Bus(_)));
        // Nothing was delivered.
        assert_eq!(ch.delivered().len(), 0);
    }

    #[test]
    fn plan_has_no_side_effects() {
        let ch = channel_with_link("agent_b/v1", 3, 3);
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0, 3.0]), "x");
        let rx = ReceiverProfile::latent("agent_b/v1", 3);

        let planned = ch.plan(&msg, &rx);
        assert_eq!(planned.mode, TransmissionMode::Latent);
        // plan() delivers nothing.
        assert_eq!(ch.delivered().len(), 0);
    }

    #[test]
    fn transmission_mode_predicates() {
        assert!(TransmissionMode::Latent.is_latent());
        assert!(!TransmissionMode::Latent.is_text());
        assert!(TransmissionMode::Text.is_text());
        assert!(!TransmissionMode::Text.is_latent());
    }

    #[test]
    fn fallback_reason_descriptions_are_distinct() {
        let reasons = [
            FallbackReason::ReceiverTextOnly,
            FallbackReason::NoLink,
            FallbackReason::ProjectionFailed,
            FallbackReason::NoLatentAvailable,
        ];
        for (i, a) in reasons.iter().enumerate() {
            assert!(!a.as_str().is_empty());
            for b in &reasons[i + 1..] {
                assert_ne!(a.as_str(), b.as_str());
            }
        }
    }

    #[test]
    fn transmission_serde_roundtrip() {
        let mut ch = channel_with_link("agent_b/v1", 2, 2);
        let msg = LatentMessage::with_latent(latent_vec(vec![1.0, 2.0]), "round");
        let rx = ReceiverProfile::latent("agent_b/v1", 2);
        let t = ch.transmit(&msg, &rx).expect("ok");

        let json = serde_json::to_string(&t).expect("serialize");
        let back: Transmission = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t, back);
    }

    #[test]
    fn sender_model_is_reported() {
        let ch = InMemoryLatentChannel::new("agent_a/v1");
        assert_eq!(ch.sender_model(), "agent_a/v1");
    }
}

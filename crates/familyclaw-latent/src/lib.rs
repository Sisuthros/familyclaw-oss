//! # familyclaw-latent
//!
//! **Latent telepathy** — a *hidden-state* transfer between siblings that
//! **always** falls back to text if latent fails. This is `FamilyClaw`
//! v2's highest communication mode (design §2.4), not the only one:
//! communication never breaks, even if the models are incompatible.
//!
//! ## What this crate does
//! 1. [`LatentVector`] — an agent's hidden state (`dims: Vec<f32>` + `model_id`).
//! 2. [`RecursiveLink`] — a linear dimension bridge from agent A's latent
//!    space to agent B's space (pad / truncate / resize / identity).
//! 3. [`LatentChannel`] — a trait for `send`/`receive`-style transfer
//!    ([`transmit`](LatentChannel::transmit) / [`deliver`](LatentChannel::deliver))
//!    with a built-in text fallback.
//! 4. [`TransmissionMode`] — reports whether `Latent` or `Text` mode was used.
//!
//! ## Research honesty (no overselling)
//! This is a **deliberately honest skeleton** for LatentMAS-style (ICML
//! 2026 Spotlight) and RecursiveMAS-based sibling communication. Concrete
//! limitations that are documented rather than hidden:
//!
//! - [`RecursiveLink`] performs only a **simple linear fit**
//!   (pad/truncate/resize). It is **not** a learned, semantically aligned
//!   projection — two different models' latent spaces are not aligned, so
//!   pad/truncate does not guarantee that meaning is preserved. A real
//!   trained projection is a later iteration.
//! - That's why **the text fallback is a load-bearing principle, not a
//!   backup system**: latent is an opportunistic optimization, text is
//!   the source of truth.
//!
//! ## OSS boundary (Layer A)
//! This crate does not hardcode family members' souls, model names, keys,
//! or paths. All model identifiers and dimensions are supplied at
//! runtime. Examples use generic names (`agent_a`, `agent_b`).
//!
//! ## Quick example
//! ```
//! use familyclaw_latent::{
//!     InMemoryLatentChannel, LatentChannel, LatentMessage, LatentVector,
//!     ReceiverProfile, RecursiveLink, TransmissionMode,
//! };
//!
//! // agent_a (4 dim) talks to agent_b (6 dim).
//! let mut channel = InMemoryLatentChannel::new("agent_a/v1")
//!     .with_link(RecursiveLink::new("agent_a/v1", 4, "agent_b/v1", 6));
//!
//! let hidden = LatentVector::new(vec![0.1, 0.2, 0.3, 0.4], "agent_a/v1");
//! let message = LatentMessage::with_latent(hidden, "kuulemiin");
//! let receiver = ReceiverProfile::latent("agent_b/v1", 6);
//!
//! let result = channel.transmit(&message, &receiver).expect("transmit");
//! assert_eq!(result.mode, TransmissionMode::Latent);
//! // ...and if the model were incompatible, mode would be TransmissionMode::Text.
//! ```

pub mod channel;
pub mod link;
pub mod translate;
pub mod vector;

pub use channel::{
    FallbackReason, InMemoryLatentChannel, LatentChannel, LatentMessage, ReceiverProfile,
    Transmission, TransmissionMode,
};
pub use familyclaw_core::{FamilyClawError, Result};
pub use link::{ProjectedLatent, ProjectionStrategy, RecursiveLink};
pub use translate::{Projection, VectorTranslator};
pub use vector::{blend, cosine, LatentVector};

/// This crate's version at build time (`CARGO_PKG_VERSION`).
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
    fn end_to_end_latent_then_text_fallback() {
        // End-to-end flow: the same channel succeeds with latent for one
        // receiver and falls back to text for another.
        let mut channel = InMemoryLatentChannel::new("agent_a/v1").with_link(RecursiveLink::new(
            "agent_a/v1",
            3,
            "agent_b/v1",
            3,
        ));

        let hidden = LatentVector::new(vec![1.0, 2.0, 3.0], "agent_a/v1");

        // 1) Compatible receiver -> latent.
        let latent_ok = channel
            .transmit(
                &LatentMessage::with_latent(hidden.clone(), "msg"),
                &ReceiverProfile::latent("agent_b/v1", 3),
            )
            .expect("latent transmit");
        assert_eq!(latent_ok.mode, TransmissionMode::Latent);

        // 2) Unknown model (no link) -> text, no error.
        let text_fb = channel
            .transmit(
                &LatentMessage::with_latent(hidden, "msg"),
                &ReceiverProfile::latent("unknown/v1", 3),
            )
            .expect("text fallback transmit");
        assert_eq!(text_fb.mode, TransmissionMode::Text);
        assert_eq!(text_fb.fallback_reason, Some(FallbackReason::NoLink));

        assert_eq!(channel.delivered().len(), 2);
    }

    #[test]
    fn reexports_are_reachable_from_root() {
        // Verifies that the public surface is reachable from the crate root.
        // Values are also used so the binding isn't just a no-op.
        let v: LatentVector = LatentVector::new(vec![0.0], "a");
        assert_eq!(v.len(), 1);

        let link: RecursiveLink = RecursiveLink::new("a", 1, "b", 1);
        assert_eq!(link.target_dims(), 1);

        let projected: ProjectedLatent = link.project(&v).expect("projects");
        assert_eq!(projected.strategy, ProjectionStrategy::Resize);

        assert!(TransmissionMode::Text.is_text());
        assert!(!FallbackReason::NoLink.as_str().is_empty());

        let err: FamilyClawError = FamilyClawError::bus("x");
        assert!(err.to_string().starts_with("bus error"));

        let ok: Result<()> = Ok(());
        assert!(ok.is_ok());
    }
}

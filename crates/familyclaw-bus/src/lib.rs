//! # familyclaw-bus
//!
//! **Resonance Bus** — FamilyClaw v2's *affective nervous system* (design
//! §2.2, Layer A / OSS). The bus is a [Ractor](https://docs.rs/ractor)-based
//! actor model over which family members (beings) communicate — and over
//! which their **emotional states leak into each other** (affective
//! contagion).
//!
//! ## What this crate solves
//! A Resonance Bus observed in live production returned `beings:[]` — an
//! empty list of beings, even though agents had joined. This crate fixes
//! that structurally: [`BusHandle::beings`] returns the actual joined
//! beings, and the list is never empty once beings have registered.
//!
//! ## Core concepts
//! - [`BusMessage`] — the bus's "language": text, latent telepathy, an
//!   **emotion pulse** ([`BusMessage::EmotionPulse`]), task events, and
//!   free-form custom messages.
//! - [`ResonanceMessage`] — an envelope (payload + sender + timestamp).
//! - [`ResonanceBus`] — the actor that registers beings, sends messages to
//!   all others, and propagates emotion pulses. Supervision keeps the bus
//!   alive even if an individual being crashes.
//! - [`BusHandle`] — an ergonomic, `unwrap`-free interface to the bus.
//! - [`BeingInfo`] / [`BeingId`] / [`BeingSnapshot`] — a joined being's
//!   info, identifier, and serializable snapshot.
//!
//! ## Affective nervous system
//! When a being publishes its emotion state as a pulse, **all other beings
//! receive it** and can react to a sibling's mood. This is the "blood"
//! that makes the bus a nervous system rather than just a message queue.
//!
//! ## OSS boundary (Layer A)
//! This crate does not hardcode family members' souls, model names, keys,
//! or paths. Beings' identifiers and names are supplied at runtime;
//! examples use generic names (`agent_a`, `agent_b`).
//!
//! ## Quick example
//! ```
//! use familyclaw_bus::{BeingId, BeingInfo, BusMessage, CollectorBeing, ResonanceBus};
//! use ractor::Actor;
//!
//! let rt = tokio::runtime::Builder::new_current_thread()
//!     .enable_all()
//!     .build()
//!     .expect("runtime");
//! rt.block_on(async {
//!     // Start the bus.
//!     let bus = ResonanceBus::start(None).await.expect("start");
//!
//!     // Join a being (here, the collector example actor).
//!     let log_b = CollectorBeing::new_log();
//!     let (inbox_b, _h) = Actor::spawn(None, CollectorBeing, log_b.clone())
//!         .await
//!         .expect("spawn");
//!     let id_a = BeingId::new();
//!     let id_b = BeingId::new();
//!     bus.register(BeingInfo::new(id_b, "agent_b", inbox_b)).expect("register");
//!
//!     // beings[] is not empty.
//!     assert_eq!(bus.count().await.expect("count"), 1);
//!
//!     // agent_a publishes a message → agent_b receives it.
//!     bus.publish(id_a, BusMessage::text("hei sisarus")).expect("publish");
//!     bus.stop();
//! });
//! ```

#![doc = include_str!("../README.md")]

pub mod being;
pub mod bus;
pub mod coherence;
pub mod latent_channel;
pub mod message;
pub mod uacp;

pub use being::{BeingInfo, BeingSnapshot, CollectedLog, CollectorBeing, CollectorState};
pub use bus::{BusHandle, BusOp, BusState, ResonanceBus};
pub use coherence::{CoherenceTracker, MesiState, RemoteReadOutcome, RemoteWriteOutcome};
pub use latent_channel::BusLatentChannel;
pub use message::{BeingId, BusMessage, MessageOrigin, ResonanceMessage, TaskEventKind};
pub use uacp::{AcpEnvelope, AcpVerb, ACP_MESSAGE_NAME};

// Re-export the core error types so callers don't need to depend on
// `familyclaw-core` separately when using the bus.
pub use familyclaw_core::{FamilyClawError, Result};

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

    #[tokio::test]
    async fn public_api_is_reachable_from_root() {
        // If any re-export is removed, this test will fail to compile.
        let bus: BusHandle = ResonanceBus::start(None).await.expect("start");

        let log: CollectedLog = CollectorBeing::new_log();
        let (inbox, _h) = ractor::Actor::spawn(None, CollectorBeing, log)
            .await
            .expect("spawn being");

        let id: BeingId = BeingId::new();
        let info: BeingInfo = BeingInfo::new(id, "agent_a", inbox);
        bus.register(info).expect("register");

        let env: ResonanceMessage = ResonanceMessage::new(id, BusMessage::text("hi"));
        bus.publish_envelope(env).expect("publish");

        let beings: Vec<BeingSnapshot> = bus.beings().await.expect("beings");
        assert_eq!(beings.len(), 1);

        let err: FamilyClawError = FamilyClawError::bus("x");
        assert!(err.to_string().starts_with("bus error"));
        let ok: Result<()> = Ok(());
        assert!(ok.is_ok());

        assert_eq!(TaskEventKind::Completed.as_label(), "completed");
        bus.stop();
    }
}

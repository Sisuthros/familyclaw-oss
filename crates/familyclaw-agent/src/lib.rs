//! # familyclaw-agent
//!
//! **Agent runtime** — layer 2 of the `FamilyClaw` platform (Layer A, OSS;
//! design §2): it assembles all the other crates into a single *being*. A
//! single [`Agent`] owns:
//!
//! - [`AgentConfig`](familyclaw_core::AgentConfig) — identity + model
//!   (`familyclaw-core`),
//! - [`Soul`] — profile loaded at runtime (the [`soul`] module),
//! - [`EmotionState`](familyclaw_emotion::EmotionState) — 19-dim emotion
//!   state (`familyclaw-emotion`),
//! - a [`MemoryStore`](familyclaw_memory::MemoryStore) handle — Eternal
//!   Thread (`familyclaw-memory`),
//! - [`DurableContext`](familyclaw_durable::DurableContext) — crash-safe
//!   step journal (`familyclaw-durable`),
//! - [`BusHandle`](familyclaw_bus::BusHandle) — Resonance Bus connection
//!   (`familyclaw-bus`).
//!
//! The agent is a Ractor actor ([`AgentActor`]) that joins the bus,
//! processes [`BusMessage`](familyclaw_bus::BusMessage)s, updates its
//! emotion state (affective contagion from siblings' pulses), records
//! memories, and publishes emotion pulses back to the bus.
//!
//! ## Crash safety (design §2.1)
//! [`Agent::handle_turn`] wraps the outcome of every turn in a durable
//! step. On restart, turns that already ran are replayed from the journal
//! without re-running side effects — a structural fix for pain point #1
//! for a family (memory discontinuity).
//!
//! ## SOUL loading (design §1, Layer A / Layer B boundary)
//! Souls are loaded at runtime from a generic profile directory
//! ([`soul::PROFILE_DIR_ENV`] / [`AgentConfig::profile_dir`](familyclaw_core::AgentConfig::profile_dir)). **No
//! family member's soul, model name, key, or path is hardcoded** into this
//! crate. The examples (see the `familyclaw` binary) use generic names
//! (`agent_a`, `agent_b`).
//!
//! ## Example
//! ```
//! use std::sync::Arc;
//! use familyclaw_agent::{Agent, Soul};
//! use familyclaw_bus::{BeingId, BusMessage, ResonanceBus};
//! use familyclaw_core::{AgentConfig, ModelConfig};
//! use familyclaw_durable::{DurableContext, InMemoryJournal, Journal};
//! use familyclaw_memory::LocalJsonStore;
//!
//! # async fn demo() -> familyclaw_core::Result<()> {
//! let bus = ResonanceBus::start(None).await?;
//!
//! let config = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
//! let soul = Soul::from_essence("I am agent_a, a generic example being.");
//! let memory = Arc::new(LocalJsonStore::in_memory());
//! let durable = DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
//!     .map_err(|e| familyclaw_core::FamilyClawError::bus(e.to_string()))?;
//!
//! let mut agent = Agent::new(config, soul, memory, durable, bus.clone(), None, None);
//! let outcome = agent
//!     .handle_turn(BeingId::new(), &BusMessage::text("hei sisarus"))
//!     .await?;
//! assert!(outcome.remembered);
//! bus.stop();
//! # Ok(())
//! # }
//! ```
#![doc = include_str!("../README.md")]

pub mod agent;
pub mod channel_bridge;
pub mod grounding;
pub mod identity;
pub mod import_cli;
pub mod live_executor;
pub mod llm;
pub mod llm_chain;
pub mod replay_cli;
pub mod resumable;
pub mod session;
pub mod soul;
pub mod watchdog;

pub use agent::{
    new_reply_channel, Agent, AgentActor, ErasedMemoryStore, MetricEvent, MetricEventSink,
    ReplySink, ThinkOutcome, TurnOutcome, METRIC_SINK_CAPACITY,
};
pub use channel_bridge::{
    envelope_origin, envelope_to_bus_message, publish_envelope, pump_channel_to_bus,
};
pub use import_cli::{ImportCommand, ImportError, ImportSource, ImportedBundle};
pub use live_executor::LiveTurnExecutor;
pub use llm_chain::{
    build_llm_chain, primary_llm_config, EnvEndpointResolver, LlmEndpointResolver, LlmFailover,
    TurnProviderSummary,
};
pub use replay_cli::{ReplayCommand, ReplayError};
pub use resumable::{
    InMemoryResumableStore, JournalResumableStore, ResumableError, ResumableTurn,
    ResumableTurnStore,
};
pub use session::{MessageOrigin, SESSION_TAG_PREFIX};
pub use soul::{load_soul, resolve_profile_dir, Soul, PROFILE_DIR_ENV};

// Re-export the emotion engine's calibration types for the caller's (e.g.
// runtime/gateway) convenience: `Agent::with_calibration` takes these, and
// calibration is loaded at runtime from `calibration.json`
// ([`TableCalibration::from_path`]).
pub use familyclaw_emotion::{EmotionCalibration, NeutralCalibration, TableCalibration};

// Re-export the core error types for the caller's convenience.
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

    #[test]
    fn public_api_is_reexported() {
        // If any re-export is removed, this test will fail to compile.
        let soul: Soul = Soul::from_essence("I am agent_a.");
        assert!(!soul.is_empty());
        assert_eq!(PROFILE_DIR_ENV, "FAMILYCLAW_PROFILE_DIR");

        let resolved = resolve_profile_dir(Some(std::path::Path::new("p/agent_a")), "agent_a");
        assert!(resolved.is_some());

        let _err: FamilyClawError = FamilyClawError::bus("x");
        let ok: Result<()> = Ok(());
        assert!(ok.is_ok());

        let outcome = TurnOutcome {
            turn: 0,
            remembered: false,
            summary: "s".into(),
        };
        assert_eq!(outcome.turn, 0);
    }
}

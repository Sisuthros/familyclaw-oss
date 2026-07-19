//! # familyclaw-core
//!
//! The `FamilyClaw` platform's **core crate**: shared types, error
//! handling, configuration, and time helpers, on which the other Layer A
//! crates (`familyclaw-bus`, `familyclaw-memory`, `familyclaw-durable`, …)
//! are built.
//!
//! This crate is deliberately **independent of other familyclaw crates**
//! — it is the foundation, so the dependency direction only points into
//! it, never away from it. Keep it clean.
//!
//! ## Design principles
//! - **No `unwrap()`/`expect()`/`panic!()` on the production path.** All
//!   failures flow through the [`FamilyClawError`] and [`Result`] types.
//!   (`unwrap`/`expect` is allowed in tests.)
//! - **Typed identifiers** ([`AgentId`], [`FamilyId`], [`MessageId`])
//!   prevent identifiers from being mixed up at compile time.
//! - **OSS boundary (Layer A):** nothing in this crate hardcodes family
//!   members' souls, API keys, tokens, IP addresses, or personal paths.
//!   Profiles are loaded at runtime ([`AgentConfig::profile_dir`]).
//!
//! ## Modules
//! - [`error`] — [`FamilyClawError`], [`Result`].
//! - [`ids`] — newtype identifiers.
//! - [`config`] — [`FamilyConfig`], [`AgentConfig`], [`ModelConfig`].
//! - [`time`] — UTC timestamps and helper functions.

pub mod config;
pub mod error;
pub mod ids;
pub mod time;

pub use config::{AgentConfig, FamilyConfig, ModelConfig};
pub use error::{FamilyClawError, Result};
pub use ids::{AgentId, FamilyId, MessageId};
pub use time::Timestamp;

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
        // Confirms that the public surface is available from the crate root — if a
        // re-export is removed, this test will fail to compile.
        let model = ModelConfig::new("provider/model");
        let agent = AgentConfig::new("agent_a", model);
        let family = FamilyConfig::new("family").with_agent(agent);
        assert!(family.validate().is_ok());

        let _id: AgentId = AgentId::new();
        let _fid: FamilyId = FamilyId::new();
        let _mid: MessageId = MessageId::new();
        let _ts: Timestamp = time::now();

        let _err: FamilyClawError = FamilyClawError::config("x");
        let ok: Result<()> = Ok(());
        assert!(ok.is_ok());
    }
}

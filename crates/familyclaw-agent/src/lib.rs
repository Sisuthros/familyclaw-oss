//! # familyclaw-agent
//!
//! **Agent runtime** — FamilyClaw-alustan (KERROS A, OSS) kerros 2 (design §2):
//! se kokoaa kaikki muut crateit yhdeksi *olennoksi*. Yksi [`Agent`] omistaa:
//!
//! - [`AgentConfig`](familyclaw_core::AgentConfig) — identiteetti + malli
//!   (`familyclaw-core`),
//! - [`Soul`] — ajonaikaisesti ladattu profiili ([`soul`]-moduuli),
//! - [`EmotionState`](familyclaw_emotion::EmotionState) — 19-dim tunnetila
//!   (`familyclaw-emotion`),
//! - [`MemoryStore`](familyclaw_memory::MemoryStore)-kahva — Eternal Thread
//!   (`familyclaw-memory`),
//! - [`DurableContext`](familyclaw_durable::DurableContext) — kaatumiskestävä
//!   askelloki (`familyclaw-durable`),
//! - [`BusHandle`](familyclaw_bus::BusHandle) — Resonance Bus -yhteys
//!   (`familyclaw-bus`).
//!
//! Agentti on Ractor-actor ([`AgentActor`]), joka liittyy busiin, käsittelee
//! [`BusMessage`](familyclaw_bus::BusMessage):t, päivittää tunnetilaansa
//! (affektiivinen contagion sisarusten pulsseista), kirjaa muistoja ja
//! julkaisee tunnepulsseja takaisin busiin.
//!
//! ## Kaatumiskestävyys (design §2.1)
//! [`Agent::handle_turn`] kääräisee jokaisen vuoron lopputuloksen
//! durable-askeleeseen. Uudelleenkäynnistyksessä jo suoritetut vuorot
//! toistuvat lokista ajamatta sivuvaikutuksia uudelleen — perheen #1
//! kipupisteen (muistin epäjatkuvuus) rakenteellinen ratkaisu.
//!
//! ## SOUL-lataus (design §1, KERROS A / KERROS B -raja)
//! Sielut ladataan ajonaikaisesti geneerisestä profiilihakemistosta
//! ([`soul::PROFILE_DIR_ENV`] / [`AgentConfig::profile_dir`]). **Mitään
//! perheenjäsenen sielua, mallinimeä, avainta tai polkua ei kovakoodata**
//! tähän crateen. Esimerkit (ks. binääri `familyclaw`) käyttävät geneerisiä
//! nimiä (`agent_a`, `agent_b`).
//!
//! ## Esimerkki
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
//! let durable = DurableContext::new(Box::new(InMemoryJournal::new()) as Box<dyn Journal + Send + Sync>)
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
pub mod llm;
pub mod llm_chain;
pub mod soul;

pub use agent::{
    new_reply_channel, Agent, AgentActor, ErasedMemoryStore, ReplySink, TurnOutcome,
};
pub use channel_bridge::{envelope_to_bus_message, publish_envelope, pump_channel_to_bus};
pub use llm_chain::{
    build_llm_chain, primary_llm_config, EnvEndpointResolver, LlmEndpointResolver, LlmFailover,
};
pub use soul::{load_soul, resolve_profile_dir, Soul, PROFILE_DIR_ENV};

// Re-export ydinvirhetyypit kutsujan mukavuudeksi.
pub use familyclaw_core::{FamilyClawError, Result};

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
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
        // Jos jokin re-export poistetaan, tämä testi ei käänny.
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

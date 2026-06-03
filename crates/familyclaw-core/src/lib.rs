//! # familyclaw-core
//!
//! FamilyClaw-alustan **ydincrate**: yhteiset tyypit, virheenkäsittely,
//! konfiguraatio ja ajan apufunktiot, joiden päälle muut KERROS A
//! -crateit (`familyclaw-bus`, `familyclaw-memory`, `familyclaw-durable`, …)
//! rakentuvat.
//!
//! Tämä crate on tarkoituksella **riippumaton muista familyclaw-crateista** —
//! se on perustus, joten riippuvuussuunta kulkee vain tähän, ei tästä
//! poispäin. Pidä se puhtaana.
//!
//! ## Suunnitteluperiaatteet
//! - **Ei `unwrap()`/`expect()`/`panic!()` tuotantopolulla.** Kaikki
//!   epäonnistumiset kulkevat [`FamilyClawError`]- ja [`Result`]-tyyppien
//!   kautta. (Testeissä `unwrap`/`expect` on sallittu.)
//! - **Tyypitetyt tunnisteet** ([`AgentId`], [`FamilyId`], [`MessageId`])
//!   estävät tunnisteiden sekoittamisen käännösaikana.
//! - **OSS-raja (KERROS A):** mikään tässä cratessa ei kovakoodaa
//!   perheenjäsenten sieluja, API-avaimia, tokeneita, IP-osoitteita tai
//!   henkilökohtaisia polkuja. Profiilit ladataan ajonaikaisesti
//!   ([`AgentConfig::profile_dir`]).
//!
//! ## Moduulit
//! - [`error`] — [`FamilyClawError`], [`Result`].
//! - [`ids`] — newtype-tunnisteet.
//! - [`config`] — [`FamilyConfig`], [`AgentConfig`], [`ModelConfig`].
//! - [`time`] — UTC-aikaleimat ja apufunktiot.

pub mod config;
pub mod error;
pub mod ids;
pub mod time;

pub use config::{AgentConfig, FamilyConfig, ModelConfig};
pub use error::{FamilyClawError, Result};
pub use ids::{AgentId, FamilyId, MessageId};
pub use time::Timestamp;

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
        // Varmistaa että julkinen pinta on saatavilla juuresta — jos jokin
        // re-export poistetaan, tämä testi ei käänny.
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

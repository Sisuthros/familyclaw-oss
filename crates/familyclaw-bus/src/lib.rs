//! # familyclaw-bus
//!
//! **Resonance Bus** — FamilyClaw v2:n *affektiivinen hermosto* (design §2.2,
//! KERROS A / OSS). Bus on [Ractor](https://docs.rs/ractor)-pohjainen
//! actor-malli, jonka yli perheenjäsenet (olennot) viestivät — ja jonka yli
//! heidän **tunnetilansa vuotaa toisilleen** (affective contagion).
//!
//! ## Mitä tämä crate ratkaisee
//! Live-tuotannossa havaittu Resonance Bus palautti `beings:[]` — tyhjän
//! olentolistan, vaikka agentteja oli liittynyt. Tämä crate korjaa sen
//! rakenteellisesti: [`BusHandle::beings`] palauttaa todelliset liittyneet
//! olennot, eikä lista ole koskaan tyhjä kun olentoja on rekisteröity.
//!
//! ## Ydinkäsitteet
//! - [`BusMessage`] — busin "kieli": teksti, latent-telepatia, **tunnepulssi**
//!   ([`BusMessage::EmotionPulse`]), tehtävätapahtumat ja vapaat custom-viestit.
//! - [`ResonanceMessage`] — kirjekuori (hyötykuorma + lähettäjä + aikaleima).
//! - [`ResonanceBus`] — actor, joka rekisteröi olennot, lähettää viestit
//!   kaikille muille ja leviää tunnepulssina. Supervision pitää busin elossa
//!   vaikka yksittäinen olento kaatuisi.
//! - [`BusHandle`] — ergonominen, `unwrap`-vapaa rajapinta busiin.
//! - [`BeingInfo`] / [`BeingId`] / [`BeingSnapshot`] — liittyneen olennon
//!   tiedot, tunniste ja sarjallistuva tilannekuva.
//!
//! ## Affektiivinen hermosto
//! Kun olento julkaisee tunnetilansa pulssina, **kaikki muut olennot saavat
//! sen** ja voivat reagoida sisaruksen mielialaan. Tämä on se "veri" joka
//! tekee busista hermoston eikä pelkkää viestijonoa.
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate ei kovakoodaa perheenjäsenten sieluja, mallinimiä, avaimia eikä
//! polkuja. Olentojen tunnisteet ja nimet annetaan ajonaikaisesti; esimerkit
//! käyttävät geneerisiä nimiä (`agent_a`, `agent_b`).
//!
//! ## Pikaesimerkki
//! ```
//! use familyclaw_bus::{BeingId, BeingInfo, BusMessage, CollectorBeing, ResonanceBus};
//! use ractor::Actor;
//!
//! let rt = tokio::runtime::Builder::new_current_thread()
//!     .enable_all()
//!     .build()
//!     .expect("runtime");
//! rt.block_on(async {
//!     // Käynnistä bus.
//!     let bus = ResonanceBus::start(None).await.expect("start");
//!
//!     // Liitä olento (tässä kerääjä-esimerkki-actor).
//!     let log_b = CollectorBeing::new_log();
//!     let (inbox_b, _h) = Actor::spawn(None, CollectorBeing, log_b.clone())
//!         .await
//!         .expect("spawn");
//!     let id_a = BeingId::new();
//!     let id_b = BeingId::new();
//!     bus.register(BeingInfo::new(id_b, "agent_b", inbox_b)).expect("register");
//!
//!     // beings[] ei ole tyhjä.
//!     assert_eq!(bus.count().await.expect("count"), 1);
//!
//!     // agent_a julkaisee viestin → agent_b saa sen.
//!     bus.publish(id_a, BusMessage::text("hei sisarus")).expect("publish");
//!     bus.stop();
//! });
//! ```

#![doc = include_str!("../README.md")]

pub mod being;
pub mod bus;
pub mod message;

pub use being::{BeingInfo, BeingSnapshot, CollectedLog, CollectorBeing, CollectorState};
pub use bus::{BusHandle, BusOp, BusState, ResonanceBus};
pub use message::{BeingId, BusMessage, MessageOrigin, ResonanceMessage, TaskEventKind};

// Re-export ydinvirhetyypit, jotta kutsujan ei tarvitse riippua
// `familyclaw-core`sta erikseen bussia käyttäessään.
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

    #[tokio::test]
    async fn public_api_is_reachable_from_root() {
        // Jos jokin re-export poistetaan, tämä testi ei käänny.
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

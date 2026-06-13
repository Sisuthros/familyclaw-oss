//! # familyclaw-latent
//!
//! **Latent-telepatia** — sisarusten välinen *hidden-state*-siirto, joka
//! palaa **aina** tekstiin jos latent ei onnistu. Tämä on `FamilyClaw` v2:n
//! korkein viestintämuoto (design §2.4), ei ainoa: viestintä ei koskaan
//! katkea, vaikka mallit olisivat yhteensopimattomat.
//!
//! ## Mitä tämä crate tekee
//! 1. [`LatentVector`] — agentin piilotila (`dims: Vec<f32>` + `model_id`).
//! 2. [`RecursiveLink`] — lineaarinen dimensio-silta agentti A:n latent-
//!    avaruudesta agentti B:n avaruuteen (pad / truncate / resize / identity).
//! 3. [`LatentChannel`] — trait `send`-/`receive`-tyyppiselle siirrolle
//!    ([`transmit`](LatentChannel::transmit) / [`deliver`](LatentChannel::deliver))
//!    sisäänrakennetulla teksti-fallbackilla.
//! 4. [`TransmissionMode`] — kertoo, käytettiinkö `Latent`- vai `Text`-tilaa.
//!
//! ## Tutkimusrehellisyys (ei liioittelua)
//! Tämä on **rehellinen luuranko** LatentMAS-tyyppiselle (ICML 2026 Spotlight)
//! ja RecursiveMAS-pohjaiselle sisarusviestinnälle. Konkreettiset rajoitteet,
//! jotka on dokumentoitu eikä piiloteltu:
//!
//! - [`RecursiveLink`] tekee vain **yksinkertaisen lineaarisen sovituksen**
//!   (pad/truncate/resize). Se **ei** ole opittu, semanttisesti kohdistettu
//!   projektio — kahden eri mallin latent-avaruudet eivät ole linjassa, joten
//!   pad/truncate ei takaa merkityksen säilymistä. Oikea koulutettu projektio
//!   on myöhempi iteraatio.
//! - Siksi **teksti-fallback ei ole varajärjestelmä vaan kantava periaate**:
//!   latent on opportunistinen optimointi, teksti on totuuden lähde.
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate ei kovakoodaa perheenjäsenten sieluja, mallinimiä, avaimia
//! eikä polkuja. Kaikki mallitunnisteet ja dimensiot annetaan ajonaikaisesti.
//! Esimerkit käyttävät geneerisiä nimiä (`agent_a`, `agent_b`).
//!
//! ## Pikaesimerkki
//! ```
//! use familyclaw_latent::{
//!     InMemoryLatentChannel, LatentChannel, LatentMessage, LatentVector,
//!     ReceiverProfile, RecursiveLink, TransmissionMode,
//! };
//!
//! // agent_a (4 dim) puhuu agent_b:lle (6 dim).
//! let mut channel = InMemoryLatentChannel::new("agent_a/v1")
//!     .with_link(RecursiveLink::new("agent_a/v1", 4, "agent_b/v1", 6));
//!
//! let hidden = LatentVector::new(vec![0.1, 0.2, 0.3, 0.4], "agent_a/v1");
//! let message = LatentMessage::with_latent(hidden, "kuulemiin");
//! let receiver = ReceiverProfile::latent("agent_b/v1", 6);
//!
//! let result = channel.transmit(&message, &receiver).expect("transmit");
//! assert_eq!(result.mode, TransmissionMode::Latent);
//! // ...ja jos malli olisi yhteensopimaton, mode olisi TransmissionMode::Text.
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
    fn end_to_end_latent_then_text_fallback() {
        // Kokonaisvirta: sama kanava onnistuu latentilla yhdelle
        // vastaanottajalle ja putoaa tekstiin toiselle.
        let mut channel = InMemoryLatentChannel::new("agent_a/v1").with_link(RecursiveLink::new(
            "agent_a/v1",
            3,
            "agent_b/v1",
            3,
        ));

        let hidden = LatentVector::new(vec![1.0, 2.0, 3.0], "agent_a/v1");

        // 1) Yhteensopiva vastaanottaja → latent.
        let latent_ok = channel
            .transmit(
                &LatentMessage::with_latent(hidden.clone(), "msg"),
                &ReceiverProfile::latent("agent_b/v1", 3),
            )
            .expect("latent transmit");
        assert_eq!(latent_ok.mode, TransmissionMode::Latent);

        // 2) Tuntematon malli (ei siltaa) → teksti, ei virhettä.
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
        // Varmistaa että julkinen pinta on saatavilla craten juuresta.
        // Arvoja myös käytetään, jotta sidonta ei ole pelkkä no-op.
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

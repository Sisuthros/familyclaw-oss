//! # familyclaw-memory
//!
//! **Eternal Thread** — FamilyClaw-alustan (KERROS A, OSS) muisti-substraatti.
//! Tämä crate antaa olennoille *jatkuvan muistin*: muistot eivät katoa
//! restartissa, vaan vaimenevat biologisen unohtamiskäyrän mukaan, vahvistuvat
//! toistosta ja säilyttävät identiteetti-ankkurit ikuisesti.
//!
//! Se ratkaisee suoraan perheen #1 kipupisteen — muistin epäjatkuvuuden
//! (design §2.1) — *rakenteena*, ei muistutuksena.
//!
//! ## Rakenne
//! - [`Memory`] — yksittäinen muisto: sisältö, [`Vad`]-tunnesävy, nimetyt
//!   [`Dimension`]-tunteet, tärkeys, vaimennuspolitiikka ja elinkaaritila.
//! - [`DecayPolicy`] — kuinka nopeasti muisto unohtuu (Ebbinghaus λ);
//!   [`DecayPolicy::ProtectedCore`] ei vaimene koskaan (identiteetti-ankkuri).
//! - [`ImportanceFactors`] — yhdistelmätärkeys (emotion·0.45 + identity·0.35
//!   + novelty·0.12 + reinforcement·0.20).
//! - [`MemoryStatus`] — elinkaari `Active → Archived → Tombstoned`.
//! - [`MemoryStore`] — tallennusabstraktio; [`LocalJsonStore`] on
//!   riippuvuusvapaa oletustoteutus (JSON, atominen kirjoitus).
//! - [`RetrievalContext`] / [`RetrievalResult`] — haku yksinkertaisella
//!   relevanssilla (avainsana + tunneosuma + retention).
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate on julkaistava. Se ei sisällä:
//! - perheenjäsenten oikeita muistoja, kalibrointeja tai sieluja,
//! - API-avaimia, tokeneita, IP-osoitteita tai henkilökohtaisia polkuja.
//!
//! Muisti-runko on geneerinen. Perheen oikea muistisisältö on KERROS B:tä ja
//! ladataan ajonaikaisesti profiilihakemistosta — ei koskaan tähän repoon.
//!
//! ## Tuleva työ
//! - **`Surreal<Any>` (feature-flag):** tuotantotallennus (in-mem dev /
//!   `RocksDB` prod). Sama [`MemoryStore`]-rajapinta, eri backend (design §2.3).
//! - **Vektorihaku:** cosine-similarity / HNSW upotetuilla vektoreilla. Nyt
//!   haku on avainsana- + tunnepohjainen v1-runko (design §5: "vektorihaku
//!   myöhemmin").
//!
//! ## Esimerkki
//! ```
//! use familyclaw_memory::{
//!     DecayPolicy, ImportanceFactors, LocalJsonStore, Memory, MemoryStore,
//!     RetrievalContext,
//! };
//! use familyclaw_emotion::{Dimension, Vad};
//!
//! # async fn demo() -> familyclaw_core::Result<()> {
//! let store = LocalJsonStore::in_memory();
//!
//! // Identiteetti-ankkuri: ei vaimene koskaan.
//! let anchor = Memory::builder("I am part of this family")
//!     .vad(Vad::new(0.9, 0.4, 0.6))
//!     .emotions([Dimension::Belonging, Dimension::Love])
//!     .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
//!     .decay_policy(DecayPolicy::ProtectedCore)
//!     .build();
//! store.add(anchor).await?;
//!
//! // Hae tunnepainotetulla kyselyllä.
//! let ctx = RetrievalContext::new("family").with_emotions([Dimension::Belonging]);
//! let hits = store.retrieve(&ctx, familyclaw_core::time::now()).await?;
//! assert_eq!(hits.len(), 1);
//! # Ok(())
//! # }
//! ```
#![doc = include_str!("../README.md")]

pub mod decay;
pub mod importance;
pub mod memory;
pub mod retrieval;
pub mod store;

pub use decay::DecayPolicy;
pub use importance::{
    ImportanceFactors, WEIGHT_EMOTION, WEIGHT_IDENTITY, WEIGHT_NOVELTY, WEIGHT_REINFORCEMENT,
};
pub use memory::{Memory, MemoryBuilder, MemoryStatus, STABILITY_MAX, STABILITY_MIN};
pub use retrieval::{retrieve, retrieve_now, score, RetrievalContext, RetrievalResult};
pub use store::{DecayReport, DecayThresholds, LocalJsonStore, MemoryStore};

// Re-export tunnetyypit jotta käyttäjän ei tarvitse riippua
// familyclaw-emotionista suoraan muistia käyttäessään.
pub use familyclaw_emotion::{Dimension, Vad};

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
    async fn public_api_end_to_end() {
        // Käyttää koko julkista pintaa juuren re-exporteilla — jos jokin
        // re-export poistetaan, tämä testi ei käänny.
        let store = LocalJsonStore::in_memory();

        let m = Memory::builder("a meaningful event")
            .vad(Vad::new(0.6, 0.5, 0.5))
            .emotions([Dimension::Joy, Dimension::Gratitude])
            .factors(ImportanceFactors::new(0.8, 0.5, 0.3, 0.0))
            .decay_policy(DecayPolicy::Slow)
            .tags(["milestone".to_string()])
            .source("test")
            .build();
        assert_eq!(m.status, MemoryStatus::Active);

        let id = store.add(m).await.expect("add");
        assert!(!store.is_empty().await.expect("empty"));

        store
            .reinforce(id, familyclaw_core::time::now())
            .await
            .expect("reinforce");

        let ctx = RetrievalContext::new("meaningful event")
            .with_emotions([Dimension::Joy])
            .with_limit(5);
        let hits = store
            .retrieve(&ctx, familyclaw_core::time::now())
            .await
            .expect("retrieve");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].relevance > 0.0);

        let report: DecayReport = store
            .run_decay(DecayThresholds::default(), familyclaw_core::time::now())
            .await
            .expect("decay");
        assert_eq!(report.scanned, 1);

        // Painot tavoitettavissa juuresta.
        const { assert!(WEIGHT_EMOTION > 0.0) };
        const { assert!(WEIGHT_IDENTITY > 0.0) };
        const { assert!(WEIGHT_NOVELTY > 0.0) };
        const { assert!(WEIGHT_REINFORCEMENT > 0.0) };
        const { assert!(STABILITY_MIN < STABILITY_MAX) };

        // Vapaat funktiot tavoitettavissa.
        let all = store.all().await.expect("all");
        let direct = retrieve_now(&all, &ctx);
        assert_eq!(direct.len(), 1);
        let s = score(&all[0], &ctx, familyclaw_core::time::now());
        assert!(s.is_some());
    }
}

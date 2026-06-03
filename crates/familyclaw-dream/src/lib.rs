//! # familyclaw-dream
//!
//! **Dreaming — yöllinen muistikonsolidaatio (hippokampus-malli).**
//!
//! Tämä crate on FamilyClaw-alustan (KERROS A, OSS) "uni"-vaihe (design §2.3,
//! Anthropic Dreaming 6.5.2026). Se peilaa perheen Amplifier-proteesin —
//! joka konsolidoi `MEMORY.md`:n — **natiiviksi** muistin huolloksi: yöllinen
//! [`DreamCycle`] lukee muistit [`familyclaw_memory`]-tallennuksesta ja
//! ristiriitatiedon durable-[`familyclaw_durable`]-journalista, ja siivoaa
//! muistin viidessä vaiheessa.
//!
//! ## Viisi vaihetta
//! 1. **`merge_duplicates`** — lähes-identtiset muistot yhdistetään yhdeksi
//!    vahvistetuksi edustajaksi (tunneet + tägit unioidaan, muut haudataan).
//!    Samankaltaisuus on riippuvuusvapaa Jaccard-sananjoukko ([`similarity`]).
//! 2. **`drop_contradicted`** — durable-journalin ristiriitaisiksi merkitsemät
//!    muistot haudataan ([`contradiction`]). Journal on totuuden lähde —
//!    unijakso ei arvaa.
//! 3. **`absolutize_dates`** — suhteelliset päiväsanat ("eilen", "tomorrow")
//!    muutetaan absoluuttisiksi ISO-päivämääriksi ([`dates`]). Tämä ratkaisee
//!    konkreettisesti perheen muistin "eilen vanhenee" -ongelman.
//! 4. **`consolidate`** — korkean tärkeyden muistot vahvistuvat, matalan
//!    retention (R < kynnys) muistot arkistoituvat.
//! 5. tuottaa [`DreamReport`]:n johon jokainen vaihe kirjaa [`Reflection`]:nsa.
//!
//! Vaiheet ajetaan kiinteässä järjestyksessä, joten sama syöte tuottaa saman
//! raportin (deterministinen, toistettava).
//!
//! ## Identiteetti-ankkurit ovat pyhiä
//! Mikään vaihe ei koskaan hauta tai arkistoi
//! [`familyclaw_memory::DecayPolicy::ProtectedCore`]-muistoa — identiteetti
//! ei vaimene unessa (design §2: anchor λ = 0.0).
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate on geneeristä alustakoodia. Se ei kovakoodaa perheenjäsenten
//! sieluja, kalibrointeja, avaimia, tokeneita, IP-osoitteita tai
//! henkilökohtaisia polkuja. Kaikki perhe-spesifit muistot ja kynnykset
//! annetaan ajonaikaisesti.
//!
//! ## Esimerkki
//! ```
//! use familyclaw_dream::{DreamCycle, DreamConfig};
//! use familyclaw_memory::{ImportanceFactors, LocalJsonStore, Memory, MemoryStore};
//! use familyclaw_durable::InMemoryJournal;
//!
//! # async fn demo() -> familyclaw_core::Result<()> {
//! let store = LocalJsonStore::in_memory();
//! store.add(Memory::builder("we shipped the release").build()).await?;
//! store.add(Memory::builder("we shipped the release").build()).await?; // duplikaatti
//!
//! let journal = InMemoryJournal::new();
//! let cycle = DreamCycle::with_config(&store, DreamConfig::default());
//! let report = cycle.run(&journal, familyclaw_core::time::now()).await?;
//!
//! assert!(report.merged >= 1);
//! # Ok(())
//! # }
//! ```
#![doc = include_str!("../README.md")]

pub mod config;
pub mod contradiction;
pub mod cycle;
pub mod dates;
pub mod report;
pub mod similarity;

pub use config::DreamConfig;
pub use contradiction::{contradicted_ids, mark_contradicted, CONTRADICT_STEP};
pub use cycle::DreamCycle;
pub use dates::{absolutize, AbsolutizeResult};
pub use report::{DreamReport, Reflection, ReflectionKind};
pub use similarity::{is_near_duplicate, jaccard};

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    use familyclaw_core::time;
    use familyclaw_durable::InMemoryJournal;
    use familyclaw_memory::{
        DecayPolicy, ImportanceFactors, LocalJsonStore, Memory, MemoryStatus, MemoryStore,
    };

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    /// Käyttää koko julkista pintaa juuren re-exporteilla — jos jokin
    /// re-export poistetaan, tämä testi ei käänny.
    #[tokio::test]
    async fn public_api_end_to_end() {
        // Vapaat funktiot tavoitettavissa juuresta.
        assert!((jaccard("same words", "same words") - 1.0).abs() < 1e-6);
        assert!(is_near_duplicate("a b c", "a b c", 0.9));
        let abs: AbsolutizeResult = absolutize("plain text", time::now());
        assert!(!abs.changed());
        assert_eq!(CONTRADICT_STEP, "memory_contradicted");

        let store = LocalJsonStore::in_memory();
        let keep = store
            .add(
                Memory::builder("the family launched the platform")
                    .factors(ImportanceFactors::new(0.9, 0.0, 0.0, 0.0))
                    .build(),
            )
            .await
            .expect("keep");
        store
            .add(Memory::builder("the family launched the platform").build())
            .await
            .expect("dup");
        let anchor = store
            .add(
                Memory::builder("i belong to this family")
                    .decay_policy(DecayPolicy::ProtectedCore)
                    .factors(ImportanceFactors::new(1.0, 1.0, 0.0, 0.0))
                    .build(),
            )
            .await
            .expect("anchor");

        let mut journal = InMemoryJournal::new();
        // Merkitse ankkuri ristiriitaiseksi — sitä EI silti saa pudottaa.
        mark_contradicted(&mut journal, anchor).expect("mark");
        let marked = contradicted_ids(&journal).expect("ids");
        assert!(marked.contains(&anchor));

        let config: DreamConfig = DreamConfig::default().with_merge_similarity(0.9);
        let cycle = DreamCycle::with_config(&store, config);
        let report: DreamReport = cycle.run(&journal, time::now()).await.expect("run");

        // Reflektiotyypit tavoitettavissa juuresta.
        let kinds = [
            ReflectionKind::Merged,
            ReflectionKind::Dropped,
            ReflectionKind::DateAbsolutized,
            ReflectionKind::Strengthened,
            ReflectionKind::Archived,
        ];
        assert_eq!(kinds.len(), 5);
        // Reflektio-tyyppi tavoitettavissa.
        let first_refl: Option<&Reflection> = report.reflections.first();
        assert!(first_refl.is_some(), "yhdistäminen tuottaa reflektion");

        assert!(report.merged >= 1);
        assert!(report.made_changes());
        // Ankkuri säilyi koskemattomana ristiriitamerkinnästä huolimatta.
        assert_eq!(
            store.get(anchor).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
        // Vahvin duplikaatti säilyi.
        assert_eq!(
            store.get(keep).await.expect("g").expect("p").status,
            MemoryStatus::Active
        );
    }
}

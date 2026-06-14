//! # familyclaw-dream
//!
//! **Dreaming — yöllinen muistikonsolidaatio (hippocampal model).**
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
//! ```rust
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
pub mod conflict;
pub mod contradiction;
pub mod cycle;
pub mod dates;
pub mod desire_clock;
pub mod report;
pub mod similarity;
pub mod weekly;

pub use config::DreamConfig;
pub use conflict::{
    clear_conflict, detect_conflicts, is_conflicted, tag_conflict, ConflictTag, CONFLICT_TAG,
};
pub use contradiction::{contradicted_ids, mark_contradicted, CONTRADICT_STEP};
pub use cycle::DreamCycle;
pub use dates::{absolutize, AbsolutizeResult};
pub use desire_clock::DesireClock;
pub use report::{DreamReport, Reflection, ReflectionKind};
pub use similarity::{is_near_duplicate, jaccard};
pub use weekly::{weekly_review, weekly_review_top_n, MemoryDigest, WeeklyReport};

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

//! # familyclaw-bench
//!
//! **Continuity benchmark harness** — reprodusoitava todiste FamilyClaw-alustan
//! jatkuvuudesta (design 2026-06-05, §2). Tämä crate todistaa väitteen jonka
//! kilpailijat eivät pysty kumoamaan:
//!
//! > *Tapa FamilyClaw-agentti kesken tehtävän. Käynnistä se uudelleen. Se
//! > jatkaa täsmälleen oikeasta askelesta, jokainen sivuvaikutus ajetaan
//! > täsmälleen kerran, se muistaa kaiken — ja yön aikana sen muisti puhdistui.*
//!
//! ## Arkkitehtuuri (saumat)
//! - [`Subject`] — *mitä* benchmarkataan. FamilyClaw nyt, kilpailijat saman
//!   rajapinnan taakse myöhemmin (design §2.1). Mustana laatikkona ajettava.
//! - [`Scenario`] — skriptattu jatkuvuustyökuorma (S1 Crash Matrix, S2
//!   Retention Curve, S3 Dream Quality).
//! - [`Harness`] — ajaa `Scenario × Subject → ScenarioResult` ja kokoaa
//!   [`Scorecard`]:n.
//! - [`metrics`] — tyypitetyt mittarit (`resume_correctness`, `recall_at_k`,
//!   `dedup_precision`, `protected_core_intact`).
//! - [`Scorecard`] — julkinen artefakti (JSON + markdown).
//!
//! ## Reprodusoitavuus (kova vaatimus, design §2.2)
//! Seinäkello **injektoidaan** [`Timestamp`](familyclaw_core::Timestamp)-
//! parametrina kaikkialla — järjestelmäkelloa ei lueta koskaan. Sama syöte →
//! identtinen scorecard joka ajolla.
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate on geneeristä benchmark-koodia. Se ei kovakoodaa perheenjäsenten
//! sieluja, avaimia, tokeneita, IP-osoitteita tai henkilökohtaisia polkuja —
//! kaikki subject-spesifit polut annetaan ajonaikaisesti.

// Tuotenimet (FamilyClaw, OpenClaw, Letta, Hermes) esiintyvät dokumentaatiossa
// proosana — ne eivät ole koodisymboleita, joten doc_markdown-backtick-vaatimus
// ei koske niitä.
#![allow(clippy::doc_markdown)]

pub mod error;
pub mod harness;
pub mod metrics;
pub mod scenario;
pub mod scenarios;
pub mod scorecard;
pub mod subject;
pub mod subjects;

pub use error::{BenchError, Result};
pub use harness::Harness;
pub use scenario::{Scenario, ScenarioResult};
pub use scorecard::Scorecard;
pub use subject::{
    CrashPoint, DreamSummary, RecallHit, RestartReport, RunHandle, Subject, Task,
};
pub use subjects::FamilyClawSubject;

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
        let task = Task::new("t", "d", Vec::new());
        assert_eq!(task.id, "t");
        let handle = RunHandle::new("t", "tok");
        assert_eq!(handle.token, "tok");
        let point = CrashPoint::Clean;
        assert_eq!(point, CrashPoint::Clean);
        let harness = Harness::new();
        // Harness on Copy — pelkkä rakentaminen riittää saumana.
        let _ = harness;
        let err: BenchError = BenchError::subject("x");
        assert!(matches!(err, BenchError::Subject(_)));
        let ok: Result<()> = Ok(());
        assert!(ok.is_ok());
    }
}

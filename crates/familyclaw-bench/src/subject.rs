//! [`Subject`]-rajapinta: *mitä* benchmarkataan.
//!
//! [`Subject`] on harnessin **saumakohta** (design §2.1): FamilyClaw on
//! ensimmäinen toteutus, ja kilpailijat (Letta, OpenClaw, Hermes Agent)
//! tulevat saman rajapinnan taakse omina toteutuksinaan ilman harness-
//! uudelleensuunnittelua. Subject ajaa jatkuvuustyökuorman mustana laatikkona:
//! käynnistä tehtävä → tapa kaatumispisteessä → käynnistä uudelleen →
//! palauta muisti → nuku.
//!
//! ## Reprodusoitavuus
//! Kaikki ajan tarvitsevat operaatiot ottavat [`Timestamp`]:n parametrina —
//! **järjestelmäkelloa ei lueta koskaan**. Sama syöte tuottaa identtisen
//! tuloksen joka ajolla.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use familyclaw_core::Timestamp;

use crate::error::Result;

/// Jatkuvuustyökuorman yksittäinen tehtävä jonka [`Subject`] suorittaa.
///
/// Tehtävä on deterministinen skripti: sama `id` + `steps` → sama suoritus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Tehtävän vakaa tunniste (skenaariokohtainen, deterministinen).
    pub id: String,
    /// Ihmisluettava kuvaus mitä tehtävä tekee.
    pub description: String,
    /// Suoritettavat askeleet järjestyksessä (deterministinen skripti).
    pub steps: Vec<String>,
}

impl Task {
    /// Rakentaa tehtävän tunnisteesta, kuvauksesta ja askelista.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        steps: Vec<String>,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            steps,
        }
    }
}

/// Kahva käynnissä olevaan tehtäväajoon — [`Subject::kill`] kohdistaa tähän.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHandle {
    /// Tehtävän tunniste jota tämä kahva seuraa.
    pub task_id: String,
    /// Subject-spesifinen läpinäkyvä viittaus (esim. journal-polku tai PID).
    pub token: String,
}

impl RunHandle {
    /// Rakentaa kahvan tehtävän tunnisteesta ja subject-tokenista.
    #[must_use]
    pub fn new(task_id: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            token: token.into(),
        }
    }
}

/// Mihin kohtaan elinkaarta kaatuminen pakotetaan ([`Subject::kill`]).
///
/// Nämä ovat punaisen tiimin hyökkäyspisteet (design §3 S1, §5). `#[non_exhaustive]`
/// jotta uusia kaatumispisteitä voi lisätä rikkomatta toteutuksia.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CrashPoint {
    /// Kaatuminen ennen journal-kirjoitusta (askel ei ehtinyt levylle).
    BeforeWrite,
    /// Kaatuminen kesken journal-kirjoituksen — viimeinen rivi katkeaa (torn).
    MidWrite,
    /// Kaatuminen kesken replayn (jatketaan jatkamista).
    MidReplay,
    /// Journal vioittui (ei-viimeinen rivi korruptoitui).
    CorruptedJournal,
    /// Puhdas pysäytys — ei kaatumista, vertailun perustaso.
    Clean,
}

/// Raportti uudelleenkäynnistyksestä ([`Subject::restart`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestartReport {
    /// Lokista palautettujen askelten määrä.
    pub steps_replayed: usize,
    /// Oliko subject replay-tilassa restartin jälkeen.
    pub was_replaying: bool,
    /// Suoritettiinko sivuvaikutuksia uudelleen replayssa (tavoite: 0).
    pub side_effects_reexecuted: usize,
    /// Saavutettiinko sama lopputila kuin kaatumattomalla perustasolla.
    pub resumed_clean: bool,
}

/// Yksittäinen muistihaun osuma ([`Subject::recall`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallHit {
    /// Palautetun muiston sisältö.
    pub content: String,
    /// Relevanssipistemäärä (`0.0..=1.0`).
    pub relevance: f32,
}

impl RecallHit {
    /// Rakentaa osuman sisällöstä ja relevanssista.
    #[must_use]
    pub fn new(content: impl Into<String>, relevance: f32) -> Self {
        Self {
            content: content.into(),
            relevance,
        }
    }
}

/// Tiivistelmä unijaksosta ([`Subject::sleep_cycle`]).
///
/// Peilaa [`familyclaw_dream::DreamReport`]:n harness-tasolle ilman riippuvuutta
/// subjektin sisäisestä toteutuksesta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamSummary {
    /// Skannattujen muistojen määrä.
    pub scanned: usize,
    /// Yhdistettyjen (deduplikoitujen) muistojen määrä.
    pub merged: usize,
    /// Pudotettujen (ristiriitaisten) muistojen määrä.
    pub dropped: usize,
    /// Absolutisoitujen päiväysten määrä.
    pub dates_absolutized: usize,
    /// Vahvistettujen muistojen määrä.
    pub strengthened: usize,
    /// Arkistoitujen muistojen määrä.
    pub archived: usize,
    /// Säilyivätkö suojatut identiteetti-ankkurit koskemattomina.
    pub protected_core_intact: bool,
}

/// Benchmarkattava järjestelmä — jatkuvuustyökuorman musta laatikko.
///
/// FamilyClaw on ensimmäinen toteutus; kilpailijat tulevat saman rajapinnan
/// taakse (design §2.1). Kaikki ajan tarvitsevat operaatiot saavat
/// [`Timestamp`]:n injektoituna — järjestelmäkelloa ei lueta.
#[async_trait]
pub trait Subject: Send {
    /// Käynnistää tehtävän ja palauttaa kahvan kaatumisen kohdistamista varten.
    ///
    /// # Errors
    /// Palauttaa [`BenchError::Subject`](crate::BenchError::Subject) jos tehtävän
    /// käynnistys epäonnistuu.
    async fn start_task(&mut self, task: &Task, clock: Timestamp) -> Result<RunHandle>;

    /// Tappaa käynnissä olevan ajon annetussa kaatumispisteessä.
    ///
    /// # Errors
    /// Palauttaa virheen jos kaatumisen injektointi epäonnistuu.
    async fn kill(&mut self, handle: &RunHandle, point: CrashPoint) -> Result<()>;

    /// Käynnistää subjektin uudelleen ja raportoi replayn lopputuloksen.
    ///
    /// # Errors
    /// Palauttaa virheen jos uudelleenkäynnistys tai replay epäonnistuu.
    async fn restart(&mut self, clock: Timestamp) -> Result<RestartReport>;

    /// Palauttaa kyselyä vastaavat muistot relevanssijärjestyksessä.
    ///
    /// # Errors
    /// Palauttaa virheen jos muistihaku epäonnistuu.
    async fn recall(&mut self, query: &str, clock: Timestamp) -> Result<Vec<RecallHit>>;

    /// Ajaa yhden unijakson (muistikonsolidaation) ja tiivistää tuloksen.
    ///
    /// # Errors
    /// Palauttaa virheen jos unijakso epäonnistuu.
    async fn sleep_cycle(&mut self, clock: Timestamp) -> Result<DreamSummary>;

    /// Subjektin vakaa nimi scorecardia varten (esim. `"familyclaw"`).
    fn name(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_point_serializes_snake_case() {
        let json = serde_json::to_string(&CrashPoint::MidWrite).expect("serialize");
        assert_eq!(json, "\"mid_write\"");
    }

    #[test]
    fn task_and_handle_roundtrip() {
        let task = Task::new("t1", "demo", vec!["a".into(), "b".into()]);
        let json = serde_json::to_string(&task).expect("ser");
        let back: Task = serde_json::from_str(&json).expect("de");
        assert_eq!(task, back);

        let handle = RunHandle::new("t1", "tok");
        assert_eq!(handle.task_id, "t1");
    }
}

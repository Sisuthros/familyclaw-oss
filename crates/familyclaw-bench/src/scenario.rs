//! [`Scenario`]-rajapinta: skriptattu jatkuvuustyökuorma.
//!
//! Yksi skenaario ajaa [`Subject`]:ia vasten yhden deterministisen
//! koesarjan ja tuottaa tyypitetyn [`ScenarioResult`]:n. Harness aggregoi
//! useat tulokset yhdeksi scorecardiksi (design §3).
//!
//! ## Reprodusoitavuus
//! [`Scenario::run`] saa [`Timestamp`]:n injektoituna referenssihetkenä —
//! järjestelmäkelloa ei lueta. Sama subject + sama kello → identtinen tulos.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use familyclaw_core::Timestamp;

use crate::error::Result;
use crate::subject::Subject;

/// Yhden skenaarioajon tyypitetty tulos.
///
/// `metrics` on [`BTreeMap`] (ei [`HashMap`](std::collections::HashMap)) jotta
/// avainjärjestys on deterministinen — scorecard pysyy tavu-tavulta
/// toistettavana (design §2.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioResult {
    /// Skenaarion vakaa tunniste (vastaa [`Scenario::id`]:tä).
    pub id: String,
    /// Läpäisikö skenaario tavoitteensa.
    pub passed: bool,
    /// Nimetyt mittarit deterministisessä avainjärjestyksessä.
    pub metrics: BTreeMap<String, f64>,
    /// Ihmisluettavat huomiot (esim. mikä hyökkäys osui ja mitä tapahtui).
    pub notes: Vec<String>,
}

impl ScenarioResult {
    /// Rakentaa tuloksen tunnisteesta ja läpäisytilasta ilman mittareita.
    #[must_use]
    pub fn new(id: impl Into<String>, passed: bool) -> Self {
        Self {
            id: id.into(),
            passed,
            metrics: BTreeMap::new(),
            notes: Vec::new(),
        }
    }

    /// Lisää nimetyn mittarin (builder-tyyli).
    #[must_use]
    pub fn with_metric(mut self, key: impl Into<String>, value: f64) -> Self {
        self.metrics.insert(key.into(), value);
        self
    }

    /// Lisää ihmisluettavan huomion (builder-tyyli).
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

/// Skriptattu jatkuvuustyökuorma jonka harness ajaa [`Subject`]:ia vasten.
///
/// Jokainen skenaario (S1 Crash Matrix, S2 Retention Curve, S3 Dream Quality)
/// toteuttaa tämän rajapinnan ja palauttaa tyypitetyn [`ScenarioResult`]:n.
#[async_trait]
pub trait Scenario: Send + Sync {
    /// Skenaarion vakaa tunniste (esim. `"s1_crash_matrix"`).
    fn id(&self) -> &str;

    /// Ajaa skenaarion annettua subjektia vasten injektoidulla kellolla.
    ///
    /// # Errors
    /// Palauttaa [`BenchError::Scenario`](crate::BenchError::Scenario) tai
    /// subjektin/durable-tason virheen jos ajo epäonnistuu.
    async fn run(&self, subject: &mut dyn Subject, clock: Timestamp) -> Result<ScenarioResult>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_builder_is_deterministic() {
        let r = ScenarioResult::new("s1", true)
            .with_metric("z_metric", 1.0)
            .with_metric("a_metric", 0.5)
            .with_note("ok");
        // BTreeMap → avaimet aakkosjärjestyksessä.
        let keys: Vec<&String> = r.metrics.keys().collect();
        assert_eq!(keys, vec!["a_metric", "z_metric"]);
        assert!(r.passed);
        assert_eq!(r.notes, vec!["ok".to_string()]);
    }

    #[test]
    fn result_roundtrips_through_json() {
        let r = ScenarioResult::new("s2", false).with_metric("recall_at_k", 0.9);
        let json = serde_json::to_string(&r).expect("ser");
        let back: ScenarioResult = serde_json::from_str(&json).expect("de");
        assert_eq!(r, back);
    }
}

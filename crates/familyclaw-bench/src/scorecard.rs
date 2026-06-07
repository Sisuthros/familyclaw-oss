//! [`Scorecard`]: kaikkien skenaariotulosten aggregaatti (JSON + markdown).
//!
//! Scorecard on harnessin **julkinen artefakti** (design §4, §6): skeptikko
//! ajaa benchmarkin ja saa tavu-tavulta saman raportin. Siksi `clock` on
//! **injektoitu referenssihetki** — järjestelmäkelloa ei lueta koskaan, jotta
//! tuloste pysyy reprodusoitavana.

use serde::{Deserialize, Serialize};

use familyclaw_core::{time, Timestamp};

use crate::error::Result;
use crate::scenario::ScenarioResult;

/// Aggregoitu tuloskortti: subjektin nimi, skenaariotulokset ja referenssikello.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scorecard {
    /// Benchmarkatun subjektin nimi (esim. `"familyclaw"`).
    pub subject: String,
    /// Yksittäisten skenaarioiden tulokset ajojärjestyksessä.
    pub scenarios: Vec<ScenarioResult>,
    /// Injektoitu referenssihetki — EI järjestelmäkello (reprodusoitavuus).
    pub clock: Timestamp,
}

impl Scorecard {
    /// Rakentaa scorecardin subjektista, tuloksista ja injektoidusta kellosta.
    ///
    /// Skenaariotulokset **lajitellaan tunnisteen mukaan** (`id`), jotta
    /// tuloste on tavu-tavulta deterministinen riippumatta siitä missä
    /// järjestyksessä harness ajoi skenaariot (design §2.2, §6).
    #[must_use]
    pub fn new(
        subject: impl Into<String>,
        scenarios: Vec<ScenarioResult>,
        clock: Timestamp,
    ) -> Self {
        let mut scenarios = scenarios;
        scenarios.sort_by(|a, b| a.id.cmp(&b.id));
        Self {
            subject: subject.into(),
            scenarios,
            clock,
        }
    }

    /// Läpäisikö jokainen skenaario tavoitteensa.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.scenarios.iter().all(|s| s.passed)
    }

    /// Sarjallistaa scorecardin sisennettyyn JSON-muotoon.
    ///
    /// # Errors
    /// Palauttaa [`BenchError::Serde`](crate::BenchError::Serde) jos
    /// sarjallistus epäonnistuu.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Renderöi scorecardin ihmisluettavaksi markdowniksi (`SCORECARD.md`).
    ///
    /// Tuloste on deterministinen: kentät ja avaimet kiinteässä järjestyksessä,
    /// kello injektoidusta arvosta (RFC 3339).
    #[must_use]
    pub fn to_markdown(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        out.push_str("# FamilyClaw Continuity Scorecard\n\n");
        // `write!` Stringiin ei voi epäonnistua, joten tulos ohitetaan turvallisesti.
        let _ = writeln!(out, "- **Subject:** {}", self.subject);
        let _ = writeln!(
            out,
            "- **Reference clock:** {}",
            time::to_rfc3339(self.clock)
        );
        let _ = writeln!(
            out,
            "- **Overall:** {}\n",
            if self.all_passed() { "PASS" } else { "FAIL" }
        );

        for scenario in &self.scenarios {
            let _ = writeln!(
                out,
                "## {} — {}\n",
                scenario.id,
                if scenario.passed { "PASS" } else { "FAIL" }
            );
            if !scenario.metrics.is_empty() {
                out.push_str("| Metric | Value |\n|--------|-------|\n");
                // BTreeMap → deterministinen avainjärjestys.
                for (key, value) in &scenario.metrics {
                    let _ = writeln!(out, "| {key} | {value:.4} |");
                }
                out.push('\n');
            }
            for note in &scenario.notes {
                let _ = writeln!(out, "- {note}");
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_clock() -> Timestamp {
        time::from_unix_secs(1_717_000_000).expect("valid")
    }

    #[test]
    fn json_is_deterministic_for_same_input() {
        let card = Scorecard::new(
            "familyclaw",
            vec![ScenarioResult::new("s1", true).with_metric("resume_correctness", 1.0)],
            fixed_clock(),
        );
        let a = card.to_json().expect("json a");
        let b = card.to_json().expect("json b");
        assert_eq!(a, b);
    }

    #[test]
    fn markdown_contains_subject_and_clock() {
        let card = Scorecard::new("familyclaw", Vec::new(), fixed_clock());
        let md = card.to_markdown();
        assert!(md.contains("familyclaw"));
        assert!(md.contains(&time::to_rfc3339(fixed_clock())));
        assert!(md.contains("PASS"));
    }

    #[test]
    fn all_passed_reflects_scenarios() {
        let card = Scorecard::new(
            "x",
            vec![
                ScenarioResult::new("a", true),
                ScenarioResult::new("b", false),
            ],
            fixed_clock(),
        );
        assert!(!card.all_passed());
    }
}

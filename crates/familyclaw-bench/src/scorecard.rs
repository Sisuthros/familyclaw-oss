//! [`Scorecard`]: the aggregate of all scenario results (JSON + markdown).
//!
//! The scorecard is the harness's **public artifact** (design §4, §6): a
//! skeptic runs the benchmark and gets the byte-for-byte same report.
//! That's why `clock` is an **injected reference instant** — the system
//! clock is never read, so the output stays reproducible.

use serde::{Deserialize, Serialize};

use familyclaw_core::{time, Timestamp};

use crate::error::Result;
use crate::scenario::ScenarioResult;

/// An aggregated scorecard: subject name, scenario results, and reference clock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scorecard {
    /// The benchmarked subject's name (e.g. `"familyclaw"`).
    pub subject: String,
    /// Individual scenario results in run order.
    pub scenarios: Vec<ScenarioResult>,
    /// The injected reference instant — NOT the system clock (reproducibility).
    pub clock: Timestamp,
}

impl Scorecard {
    /// Builds a scorecard from a subject, results, and an injected clock.
    ///
    /// Scenario results are **sorted by ID**, so the output is
    /// byte-for-byte deterministic regardless of the order the harness ran
    /// the scenarios in (design §2.2, §6).
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

    /// Whether every scenario met its goal.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.scenarios.iter().all(|s| s.passed)
    }

    /// Serializes the scorecard to indented JSON.
    ///
    /// # Errors
    /// Returns [`BenchError::Serde`](crate::BenchError::Serde) if
    /// serialization fails.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Renders the scorecard as human-readable markdown (`SCORECARD.md`).
    ///
    /// The output is deterministic: fields and keys in a fixed order, clock
    /// from the injected value (RFC 3339).
    #[must_use]
    pub fn to_markdown(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        out.push_str("# FamilyClaw Continuity Scorecard\n\n");
        // `write!` to a String cannot fail, so the result is safely ignored.
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
                // BTreeMap → deterministic key order.
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

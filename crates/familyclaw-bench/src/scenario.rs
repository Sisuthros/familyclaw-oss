//! The [`Scenario`] interface: a scripted continuity workload.
//!
//! A single scenario runs one deterministic test sequence against a
//! [`Subject`] and produces a typed [`ScenarioResult`]. The harness
//! aggregates multiple results into a single scorecard (design §3).
//!
//! ## Reproducibility
//! [`Scenario::run`] receives a [`Timestamp`] injected as the reference
//! instant — the system clock is never read. Same subject + same clock →
//! identical result.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use familyclaw_core::Timestamp;

use crate::error::Result;
use crate::subject::Subject;

/// A single scenario run's typed result.
///
/// `metrics` is a [`BTreeMap`] (not a [`HashMap`](std::collections::HashMap))
/// so the key order is deterministic — the scorecard stays byte-for-byte
/// reproducible (design §2.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioResult {
    /// The scenario's stable ID (matches [`Scenario::id`]).
    pub id: String,
    /// Whether the scenario met its goal.
    pub passed: bool,
    /// Named metrics in deterministic key order.
    pub metrics: BTreeMap<String, f64>,
    /// Human-readable notes (e.g. which attack landed and what happened).
    pub notes: Vec<String>,
}

impl ScenarioResult {
    /// Builds a result from an ID and pass state, with no metrics.
    #[must_use]
    pub fn new(id: impl Into<String>, passed: bool) -> Self {
        Self {
            id: id.into(),
            passed,
            metrics: BTreeMap::new(),
            notes: Vec::new(),
        }
    }

    /// Adds a named metric (builder style).
    #[must_use]
    pub fn with_metric(mut self, key: impl Into<String>, value: f64) -> Self {
        self.metrics.insert(key.into(), value);
        self
    }

    /// Adds a human-readable note (builder style).
    #[must_use]
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

/// A scripted continuity workload that the harness runs against a [`Subject`].
///
/// Each scenario (S1 Crash Matrix, S2 Retention Curve, S3 Dream Quality)
/// implements this interface and returns a typed [`ScenarioResult`].
#[async_trait]
pub trait Scenario: Send + Sync {
    /// The scenario's stable ID (e.g. `"s1_crash_matrix"`).
    fn id(&self) -> &str;

    /// Runs the scenario against the given subject with an injected clock.
    ///
    /// # Errors
    /// Returns [`BenchError::Scenario`](crate::BenchError::Scenario) or a
    /// subject/durable-layer error if the run fails.
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
        // BTreeMap → keys in alphabetical order.
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

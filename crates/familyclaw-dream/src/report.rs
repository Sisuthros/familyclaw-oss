//! The dream cycle's result report: [`DreamReport`] and [`Reflection`].
//!
//! A dream cycle ([`crate::DreamCycle`]) produces a report describing
//! *what happened overnight*: how many memories were merged, dropped, or
//! archived, and which reflections consolidation produced. The report is
//! pure data — it serializes to logs and mirrors the Amplifier prosthesis's
//! "freshness audit" feedback as a native feature (design §2.3).

use serde::{Deserialize, Serialize};

use familyclaw_core::{MessageId, Timestamp};

/// A single dream reflection — a machine-readable record of what a
/// consolidation phase did to one memory.
///
/// Reflections are not free-form prose but structured events, so they can
/// be audited and replayed deterministically. The `note` field is a
/// human-readable summary, but `kind` + `memory` are the machine-readable
/// truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reflection {
    /// Which consolidation phase this reflection relates to.
    pub kind: ReflectionKind,
    /// The memory this reflection concerns (the target or the kept representative).
    pub memory: MessageId,
    /// Human-readable summary (e.g. `"merged 3 near-duplicates"`).
    pub note: String,
}

impl Reflection {
    /// Builds a reflection for the given phase, memory, and summary.
    #[must_use]
    pub fn new(kind: ReflectionKind, memory: MessageId, note: impl Into<String>) -> Self {
        Self {
            kind,
            memory,
            note: note.into(),
        }
    }
}

/// Which consolidation phase produced the reflection.
///
/// `#[non_exhaustive]` so new phases (e.g. a future latent-based
/// clustering phase) can be added without breaking readers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReflectionKind {
    /// Near-identical memories were merged into one representative.
    Merged,
    /// An outdated/contradicted memory was dropped (tombstoned).
    Dropped,
    /// A relative date ("yesterday") was converted to an absolute date.
    DateAbsolutized,
    /// An important memory was strengthened (its persistence was increased).
    Strengthened,
    /// A low-retention memory was archived.
    Archived,
}

impl ReflectionKind {
    /// Stable, machine-readable name (`snake_case`) — matches the serde representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ReflectionKind::Merged => "merged",
            ReflectionKind::Dropped => "dropped",
            ReflectionKind::DateAbsolutized => "date_absolutized",
            ReflectionKind::Strengthened => "strengthened",
            ReflectionKind::Archived => "archived",
        }
    }
}

impl std::fmt::Display for ReflectionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The rolled-up result of a single dream cycle.
///
/// The counters report *how many* memories each phase processed;
/// [`reflections`](DreamReport::reflections) holds the per-event records.
/// The report is built phase-by-phase from [`DreamReport::default`] or
/// [`DreamReport::new`] and assembled at the end of the dream cycle.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DreamReport {
    /// The instant the dream cycle ran (UTC). `None` until set.
    #[serde(default)]
    pub ran_at: Option<Timestamp>,
    /// Number of merged (removed) duplicates — NOT including the retained
    /// representatives.
    pub merged: usize,
    /// Number of dropped (tombstoned) outdated/contradicted memories.
    pub dropped: usize,
    /// Number of archived low-retention memories.
    pub archived: usize,
    /// Number of strengthened important memories.
    pub strengthened: usize,
    /// Number of absolutized dates.
    pub dates_absolutized: usize,
    /// Total number of memories scanned at the start of the dream cycle.
    pub scanned: usize,
    /// Per-event reflections, in the order they occurred.
    #[serde(default)]
    pub reflections: Vec<Reflection>,
}

impl DreamReport {
    /// Creates an empty report with the given run instant.
    #[must_use]
    pub fn new(ran_at: Timestamp) -> Self {
        Self {
            ran_at: Some(ran_at),
            ..Self::default()
        }
    }

    /// Records a reflection and increments the matching counter.
    ///
    /// This is the report's only mutation path, so the counters and
    /// reflections always stay in sync (one reflection ⇒ one counter increment).
    pub fn record(&mut self, reflection: Reflection) {
        match reflection.kind {
            ReflectionKind::Merged => self.merged += 1,
            ReflectionKind::Dropped => self.dropped += 1,
            ReflectionKind::DateAbsolutized => self.dates_absolutized += 1,
            ReflectionKind::Strengthened => self.strengthened += 1,
            ReflectionKind::Archived => self.archived += 1,
        }
        self.reflections.push(reflection);
    }

    /// Whether the dream cycle made any changes.
    #[must_use]
    pub fn made_changes(&self) -> bool {
        self.merged > 0
            || self.dropped > 0
            || self.archived > 0
            || self.strengthened > 0
            || self.dates_absolutized > 0
    }

    /// Total number of reflections (same as the sum of the counters).
    #[must_use]
    pub fn total_actions(&self) -> usize {
        self.merged + self.dropped + self.archived + self.strengthened + self.dates_absolutized
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time;

    #[test]
    fn reflection_kind_as_str_matches_serde() {
        let kinds = [
            ReflectionKind::Merged,
            ReflectionKind::Dropped,
            ReflectionKind::DateAbsolutized,
            ReflectionKind::Strengthened,
            ReflectionKind::Archived,
        ];
        for k in kinds {
            assert_eq!(k.to_string(), k.as_str());
            let json = serde_json::to_string(&k).expect("serialize kind");
            assert_eq!(json, format!("\"{}\"", k.as_str()));
            let back: ReflectionKind = serde_json::from_str(&json).expect("deserialize kind");
            assert_eq!(back, k);
        }
    }

    #[test]
    fn empty_report_made_no_changes() {
        let r = DreamReport::default();
        assert!(!r.made_changes());
        assert_eq!(r.total_actions(), 0);
        assert!(r.ran_at.is_none());
    }

    #[test]
    fn new_sets_ran_at() {
        let now = time::now();
        let r = DreamReport::new(now);
        assert_eq!(r.ran_at, Some(now));
        assert!(!r.made_changes());
    }

    #[test]
    fn record_increments_matching_counter_only() {
        let mut r = DreamReport::default();
        let id = MessageId::new();
        r.record(Reflection::new(ReflectionKind::Merged, id, "m"));
        r.record(Reflection::new(ReflectionKind::Merged, id, "m2"));
        r.record(Reflection::new(ReflectionKind::Dropped, id, "d"));
        r.record(Reflection::new(ReflectionKind::Archived, id, "a"));
        r.record(Reflection::new(ReflectionKind::Strengthened, id, "s"));
        r.record(Reflection::new(ReflectionKind::DateAbsolutized, id, "da"));

        assert_eq!(r.merged, 2);
        assert_eq!(r.dropped, 1);
        assert_eq!(r.archived, 1);
        assert_eq!(r.strengthened, 1);
        assert_eq!(r.dates_absolutized, 1);
        assert_eq!(r.reflections.len(), 6);
        assert_eq!(r.total_actions(), 6);
        assert!(r.made_changes());
    }

    #[test]
    fn counters_stay_in_sync_with_reflections() {
        let mut r = DreamReport::default();
        for _ in 0..10 {
            r.record(Reflection::new(
                ReflectionKind::Merged,
                MessageId::new(),
                "x",
            ));
        }
        assert_eq!(r.merged, r.reflections.len());
        assert_eq!(r.total_actions(), r.reflections.len());
    }

    #[test]
    fn report_serde_roundtrip() {
        let mut r = DreamReport::new(time::now());
        r.record(Reflection::new(
            ReflectionKind::DateAbsolutized,
            MessageId::new(),
            "yesterday → 2026-06-03",
        ));
        let json = serde_json::to_string(&r).expect("serialize");
        let back: DreamReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(r, back);
    }
}

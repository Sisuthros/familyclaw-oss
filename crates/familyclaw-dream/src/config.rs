//! Dream cycle tuning parameters ([`DreamConfig`]).
//!
//! The configuration collects all consolidation thresholds in one place.
//! The defaults are derived from the `FamilyClaw` v2 design (§2.3, §5) and
//! the `familyclaw-memory` crate's Ebbinghaus model, not guessed. All
//! fields are clamped to sensible bounds when constructed, so invalid
//! input can never produce a broken dream cycle.

use serde::{Deserialize, Serialize};

/// Thresholds and toggles for a dream cycle.
///
/// Construct with [`DreamConfig::default`] (recommended) or
/// [`DreamConfig::new`] and adjust builder-style. The values are pure
/// floating-point thresholds — no family-specific/calibration data (Layer
/// A, OSS).
///
/// The four `bool` toggles are intentionally independent phase flags (each
/// switches one consolidation phase on/off), not a state machine — that's
/// why `struct_excessive_bools` is allowed here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DreamConfig {
    /// Jaccard threshold at which two memories are considered duplicates
    /// (`0.0..=1.0`). Higher = stricter (requires more overlap).
    pub merge_similarity: f32,

    /// Retention threshold below which memories are archived during sleep
    /// (`0.0..=1.0`). Design §2.3: low-importance memories (R < 0.05) get
    /// archived.
    pub archive_below_retention: f32,

    /// Importance threshold above which memories are strengthened during
    /// sleep (`0.0..=1.0`). Design §2.3: high-importance memories are strengthened.
    pub strengthen_above_importance: f32,

    /// Whether to run duplicate merging.
    pub merge_duplicates: bool,
    /// Whether to run dropping of contradicted/outdated memories.
    pub drop_contradicted: bool,
    /// Whether to run absolutization of relative dates.
    pub absolutize_dates: bool,
    /// Whether to run strengthening of important memories and archiving of low ones.
    pub consolidate: bool,
}

impl DreamConfig {
    /// Default duplicate threshold (strong, but not requiring identity).
    pub const DEFAULT_MERGE_SIMILARITY: f32 = 0.85;
    /// Default archiving retention (design §2.3: R < 0.05).
    pub const DEFAULT_ARCHIVE_BELOW_RETENTION: f32 = 0.05;
    /// Default strengthening threshold (important memories).
    pub const DEFAULT_STRENGTHEN_ABOVE_IMPORTANCE: f32 = 0.6;

    /// Builds a configuration from three thresholds, with all phases enabled.
    ///
    /// Fields are clamped to `0.0..=1.0`; invalid (NaN/infinite) values are
    /// replaced with the corresponding default.
    #[must_use]
    pub fn new(
        merge_similarity: f32,
        archive_below_retention: f32,
        strengthen_above_importance: f32,
    ) -> Self {
        Self {
            merge_similarity: clamp_unit(merge_similarity, Self::DEFAULT_MERGE_SIMILARITY),
            archive_below_retention: clamp_unit(
                archive_below_retention,
                Self::DEFAULT_ARCHIVE_BELOW_RETENTION,
            ),
            strengthen_above_importance: clamp_unit(
                strengthen_above_importance,
                Self::DEFAULT_STRENGTHEN_ABOVE_IMPORTANCE,
            ),
            merge_duplicates: true,
            drop_contradicted: true,
            absolutize_dates: true,
            consolidate: true,
        }
    }

    /// Sets the duplicate threshold (clamped to `0.0..=1.0`).
    #[must_use]
    pub fn with_merge_similarity(mut self, v: f32) -> Self {
        self.merge_similarity = clamp_unit(v, Self::DEFAULT_MERGE_SIMILARITY);
        self
    }

    /// Sets the archiving retention threshold (clamped to `0.0..=1.0`).
    #[must_use]
    pub fn with_archive_below_retention(mut self, v: f32) -> Self {
        self.archive_below_retention = clamp_unit(v, Self::DEFAULT_ARCHIVE_BELOW_RETENTION);
        self
    }

    /// Sets the strengthening threshold (clamped to `0.0..=1.0`).
    #[must_use]
    pub fn with_strengthen_above_importance(mut self, v: f32) -> Self {
        self.strengthen_above_importance = clamp_unit(v, Self::DEFAULT_STRENGTHEN_ABOVE_IMPORTANCE);
        self
    }

    /// Toggles duplicate merging on/off.
    #[must_use]
    pub const fn merging(mut self, on: bool) -> Self {
        self.merge_duplicates = on;
        self
    }

    /// Toggles dropping of contradicted memories on/off.
    #[must_use]
    pub const fn dropping_contradicted(mut self, on: bool) -> Self {
        self.drop_contradicted = on;
        self
    }

    /// Toggles absolutization of dates on/off.
    #[must_use]
    pub const fn absolutizing_dates(mut self, on: bool) -> Self {
        self.absolutize_dates = on;
        self
    }

    /// Toggles consolidation (strengthening + archiving) on/off.
    #[must_use]
    pub const fn consolidating(mut self, on: bool) -> Self {
        self.consolidate = on;
        self
    }
}

impl Default for DreamConfig {
    /// Design-mandated defaults, with all phases enabled.
    fn default() -> Self {
        Self::new(
            Self::DEFAULT_MERGE_SIMILARITY,
            Self::DEFAULT_ARCHIVE_BELOW_RETENTION,
            Self::DEFAULT_STRENGTHEN_ABOVE_IMPORTANCE,
        )
    }
}

/// Clamps a value to `0.0..=1.0`; invalid (NaN/infinite) → `fallback`.
fn clamp_unit(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    // Exact f32 comparison allowed — fixed thresholds.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn default_matches_design_constants() {
        let c = DreamConfig::default();
        assert_eq!(c.merge_similarity, 0.85);
        assert_eq!(c.archive_below_retention, 0.05);
        assert_eq!(c.strengthen_above_importance, 0.6);
        assert!(c.merge_duplicates);
        assert!(c.drop_contradicted);
        assert!(c.absolutize_dates);
        assert!(c.consolidate);
    }

    #[test]
    fn new_clamps_out_of_range() {
        let c = DreamConfig::new(5.0, -1.0, 2.0);
        assert_eq!(c.merge_similarity, 1.0);
        assert_eq!(c.archive_below_retention, 0.0);
        assert_eq!(c.strengthen_above_importance, 1.0);
    }

    #[test]
    fn new_falls_back_on_invalid() {
        let c = DreamConfig::new(f32::NAN, f32::INFINITY, f32::NEG_INFINITY);
        assert_eq!(c.merge_similarity, DreamConfig::DEFAULT_MERGE_SIMILARITY);
        assert_eq!(
            c.archive_below_retention,
            DreamConfig::DEFAULT_ARCHIVE_BELOW_RETENTION
        );
        assert_eq!(
            c.strengthen_above_importance,
            DreamConfig::DEFAULT_STRENGTHEN_ABOVE_IMPORTANCE
        );
    }

    #[test]
    fn builder_setters_clamp() {
        let c = DreamConfig::default()
            .with_merge_similarity(0.9)
            .with_archive_below_retention(0.1)
            .with_strengthen_above_importance(0.7);
        assert_eq!(c.merge_similarity, 0.9);
        assert_eq!(c.archive_below_retention, 0.1);
        assert_eq!(c.strengthen_above_importance, 0.7);

        let clamped = DreamConfig::default().with_merge_similarity(99.0);
        assert_eq!(clamped.merge_similarity, 1.0);
    }

    #[test]
    fn phase_toggles() {
        let c = DreamConfig::default()
            .merging(false)
            .dropping_contradicted(false)
            .absolutizing_dates(false)
            .consolidating(false);
        assert!(!c.merge_duplicates);
        assert!(!c.drop_contradicted);
        assert!(!c.absolutize_dates);
        assert!(!c.consolidate);
    }

    #[test]
    fn serde_roundtrip() {
        let c = DreamConfig::default().with_merge_similarity(0.77);
        let json = serde_json::to_string(&c).expect("serialize");
        let back: DreamConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(c, back);
    }
}

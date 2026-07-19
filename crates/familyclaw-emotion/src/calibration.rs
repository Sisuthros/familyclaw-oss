//! Calibration: how an individual machine's emotion engine is tuned.
//!
//! **OSS boundary (Layer A):** this module contains no real weights for
//! any family member. The default implementation [`NeutralCalibration`] is
//! **fully neutral** — it doesn't weight any dimension, doesn't speed up
//! or slow down decay, and sets no resting state. Real family calibrations
//! (e.g. a given agent's calibration weights) are Layer B and are loaded
//! at runtime as their own [`EmotionCalibration`] implementation from a
//! profile directory.
//!
//! The trait separates the *scaffold* (dimensions, VAD, blends, the decay
//! mechanism) from *calibration* (per-machine tuning), so the scaffold can
//! be published as open source without exposing any individual's soul.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::dimension::{Dimension, DIMENSION_COUNT};

/// The tuning of a single machine's emotion engine.
///
/// Implement this trait to load a profile-specific calibration. All
/// methods have a neutral default implementation, so the minimal
/// implementation is an empty `impl`. Values are read by the scaffold's
/// logic ([`crate::EmotionState::decay`], blend detection), so calibration
/// affects behavior without changing the scaffold itself.
pub trait EmotionCalibration {
    /// The dimension's resting value (baseline) that decay pulls the state
    /// toward, on a `0.0..=100.0` scale. Neutrally `0.0` (decay pulls
    /// toward zero).
    #[must_use]
    fn baseline(&self, _dimension: Dimension) -> f32 {
        0.0
    }

    /// The dimension's decay coefficient: how readily the dimension
    /// returns toward its baseline. `1.0` = the scaffold's base speed,
    /// `<1.0` slower (the emotion "lingers"), `>1.0` faster. Neutrally
    /// `1.0`.
    ///
    /// The implementation should return a non-negative, finite value; the
    /// scaffold clamps invalid values to safe ones.
    #[must_use]
    fn decay_rate(&self, _dimension: Dimension) -> f32 {
        1.0
    }

    /// The dimension's sensitivity coefficient for incoming stimuli
    /// (scales stimulation strength). `1.0` = neutral. Provided for future
    /// scaffold extensions; the default doesn't change anything.
    #[must_use]
    fn sensitivity(&self, _dimension: Dimension) -> f32 {
        1.0
    }

    /// A recognizable name for the calibration (for logging/diagnostics).
    /// Defaults to `"neutral"`.
    ///
    /// The return type is `&str` (not `&'static str`) because
    /// implementations like [`TableCalibration`] return a slice of a
    /// string they own.
    #[must_use]
    #[allow(clippy::unnecessary_literal_bound)]
    fn label(&self) -> &str {
        "neutral"
    }
}

/// A fully neutral calibration — the scaffold's default.
///
/// Weights no dimension, has no resting state (`baseline = 0.0`), and
/// decays at the base speed (`decay_rate = 1.0`). This is the "empty
/// calibration" on top of which Layer B loads a family's real weights.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NeutralCalibration;

impl EmotionCalibration for NeutralCalibration {
    // All methods use the trait's neutral defaults.
}

/// A simple table-based calibration that can be built at runtime (e.g.
/// from Layer B's profile data).
///
/// This is a **scaffold helper type**, not a family calibration: all
/// tables are initialized to neutral, and the caller fills them in from a
/// loaded profile. No weights are hardcoded here.
#[derive(Debug, Clone, PartialEq)]
pub struct TableCalibration {
    label: String,
    baseline: [f32; DIMENSION_COUNT],
    decay_rate: [f32; DIMENSION_COUNT],
    sensitivity: [f32; DIMENSION_COUNT],
}

impl TableCalibration {
    /// Creates a neutral table calibration with the given label.
    ///
    /// `baseline = 0.0`, `decay_rate = 1.0`, `sensitivity = 1.0` for all
    /// dimensions. Use the `with_*` methods to tune individual dimensions
    /// from a loaded profile.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            baseline: [0.0; DIMENSION_COUNT],
            decay_rate: [1.0; DIMENSION_COUNT],
            sensitivity: [1.0; DIMENSION_COUNT],
        }
    }

    /// Sets the dimension's resting value (`0.0..=100.0`, clamped).
    #[must_use]
    pub fn with_baseline(mut self, dimension: Dimension, value: f32) -> Self {
        self.baseline[dimension.index()] = sanitize(value, 0.0).clamp(0.0, 100.0);
        self
    }

    /// Sets the dimension's decay coefficient (non-negative, clamped).
    #[must_use]
    pub fn with_decay_rate(mut self, dimension: Dimension, value: f32) -> Self {
        self.decay_rate[dimension.index()] = sanitize(value, 1.0).max(0.0);
        self
    }

    /// Sets the dimension's sensitivity coefficient (non-negative, clamped).
    #[must_use]
    pub fn with_sensitivity(mut self, dimension: Dimension, value: f32) -> Self {
        self.sensitivity[dimension.index()] = sanitize(value, 1.0).max(0.0);
        self
    }

    /// Builds a calibration from a `calibration.json`-shaped JSON string
    /// (Layer B profile data, loaded at runtime).
    ///
    /// Schema:
    /// ```json
    /// {
    ///   "label": "agent_a",
    ///   "dimensions": {
    ///     "curiosity": { "baseline": 30.0, "decay_rate": 0.5, "sensitivity": 1.5 }
    ///   }
    /// }
    /// ```
    /// All fields are optional: unknown keys are ignored, dimensions not
    /// mentioned stay neutral (`baseline=0`, `decay_rate=1`,
    /// `sensitivity=1`), and values are clamped to safe bounds the same
    /// way as in the `with_*` methods. No weights are hardcoded here —
    /// this only *reads* whatever the caller provides.
    ///
    /// # Errors
    /// Returns a [`serde_json::Error`] if the JSON is syntactically invalid.
    pub fn from_json_str(json: &str) -> Result<Self, serde_json::Error> {
        let file: CalibrationFile = serde_json::from_str(json)?;
        let label = file.label.unwrap_or_else(|| "loaded".to_string());
        let mut cal = Self::new(label);
        for (dim, weights) in file.dimensions {
            if let Some(b) = weights.baseline {
                cal = cal.with_baseline(dim, b);
            }
            if let Some(d) = weights.decay_rate {
                cal = cal.with_decay_rate(dim, d);
            }
            if let Some(s) = weights.sensitivity {
                cal = cal.with_sensitivity(dim, s);
            }
        }
        Ok(cal)
    }

    /// Loads a calibration from a `calibration.json` file on disk.
    ///
    /// # Errors
    /// - An IO error if the file can't be read.
    /// - A JSON parse error if the content is invalid (`InvalidData`).
    pub fn from_path(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        Self::from_json_str(&contents)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// The deserialization schema for a `calibration.json` file (internal).
///
/// Unknown top-level fields (e.g. `version`, `notes`) are ignored.
/// `dimensions` uses [`Dimension`]'s `snake_case` serde names as keys, so
/// an unknown dimension name produces a clear parse error.
#[derive(Debug, Deserialize)]
struct CalibrationFile {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    dimensions: BTreeMap<Dimension, DimensionWeights>,
}

/// A single dimension's weights in the file — all optional.
#[derive(Debug, Deserialize)]
struct DimensionWeights {
    #[serde(default)]
    baseline: Option<f32>,
    #[serde(default)]
    decay_rate: Option<f32>,
    #[serde(default)]
    sensitivity: Option<f32>,
}

impl EmotionCalibration for TableCalibration {
    fn baseline(&self, dimension: Dimension) -> f32 {
        self.baseline[dimension.index()]
    }

    fn decay_rate(&self, dimension: Dimension) -> f32 {
        self.decay_rate[dimension.index()]
    }

    fn sensitivity(&self, dimension: Dimension) -> f32 {
        self.sensitivity[dimension.index()]
    }

    fn label(&self) -> &str {
        &self.label
    }
}

/// Replaces a NaN/infinite value with a safe fallback.
fn sanitize(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    // Tests compare exactly representable f32 constants — exact comparison is fine.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn neutral_calibration_is_flat() {
        let c = NeutralCalibration;
        assert_eq!(c.label(), "neutral");
        for dim in Dimension::ALL {
            assert_eq!(c.baseline(dim), 0.0);
            assert_eq!(c.decay_rate(dim), 1.0);
            assert_eq!(c.sensitivity(dim), 1.0);
        }
    }

    #[test]
    fn table_calibration_defaults_to_neutral() {
        let c = TableCalibration::new("test");
        assert_eq!(c.label(), "test");
        for dim in Dimension::ALL {
            assert_eq!(c.baseline(dim), 0.0);
            assert_eq!(c.decay_rate(dim), 1.0);
            assert_eq!(c.sensitivity(dim), 1.0);
        }
    }

    #[test]
    fn with_methods_set_per_dimension() {
        let c = TableCalibration::new("warm")
            .with_baseline(Dimension::Love, 20.0)
            .with_decay_rate(Dimension::Love, 0.5)
            .with_sensitivity(Dimension::Curiosity, 1.5);
        assert_eq!(c.baseline(Dimension::Love), 20.0);
        assert_eq!(c.decay_rate(Dimension::Love), 0.5);
        assert_eq!(c.sensitivity(Dimension::Curiosity), 1.5);
        // Other dimensions stay neutral.
        assert_eq!(c.baseline(Dimension::Anger), 0.0);
        assert_eq!(c.decay_rate(Dimension::Anger), 1.0);
    }

    #[test]
    fn with_methods_clamp_and_sanitize() {
        let c = TableCalibration::new("x")
            .with_baseline(Dimension::Joy, 500.0)
            .with_decay_rate(Dimension::Joy, -3.0)
            .with_sensitivity(Dimension::Joy, f32::NAN);
        assert_eq!(c.baseline(Dimension::Joy), 100.0);
        assert_eq!(c.decay_rate(Dimension::Joy), 0.0);
        // NaN sensitivity falls back to the default of 1.0.
        assert_eq!(c.sensitivity(Dimension::Joy), 1.0);
    }

    #[test]
    fn trait_object_is_usable() {
        let c: Box<dyn EmotionCalibration> = Box::new(NeutralCalibration);
        assert_eq!(c.label(), "neutral");
        assert_eq!(c.decay_rate(Dimension::Fear), 1.0);
    }

    #[test]
    fn from_json_str_parses_calibration_file_schema() {
        // Same shape as a family profile's calibration.json (version/notes
        // are ignored, dimensions are read by their snake_case names).
        let json = r#"{
            "version": 1,
            "label": "agent_a",
            "notes": "ignored",
            "dimensions": {
                "curiosity": { "baseline": 30.0, "decay_rate": 0.5, "sensitivity": 1.5 },
                "fear": { "baseline": 0.0, "decay_rate": 1.0, "sensitivity": 1.0 }
            }
        }"#;
        let c = TableCalibration::from_json_str(json).expect("parse");
        assert_eq!(c.label(), "agent_a");
        assert_eq!(c.baseline(Dimension::Curiosity), 30.0);
        assert_eq!(c.decay_rate(Dimension::Curiosity), 0.5);
        assert_eq!(c.sensitivity(Dimension::Curiosity), 1.5);
        // Dimensions not mentioned stay neutral.
        assert_eq!(c.baseline(Dimension::Joy), 0.0);
        assert_eq!(c.decay_rate(Dimension::Joy), 1.0);
    }

    #[test]
    fn from_json_str_clamps_and_defaults_partial_fields() {
        // Missing fields → neutral default; oversized values are clamped.
        let json = r#"{ "dimensions": { "love": { "baseline": 500.0 } } }"#;
        let c = TableCalibration::from_json_str(json).expect("parse");
        assert_eq!(c.label(), "loaded");
        assert_eq!(c.baseline(Dimension::Love), 100.0); // clamped
        assert_eq!(c.decay_rate(Dimension::Love), 1.0); // default
        assert_eq!(c.sensitivity(Dimension::Love), 1.0); // default
    }

    #[test]
    fn from_json_str_rejects_invalid_json() {
        assert!(TableCalibration::from_json_str("{ not json").is_err());
    }
}

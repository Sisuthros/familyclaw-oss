//! The VAD coordinate (valence–arousal–dominance) and its bounds.
//!
//! VAD is a dimension-independent, low-dimensional *summary* of an
//! emotional state. It's a convenient output for buses (the affective
//! nervous system) and logs, since it's always a fixed three numbers
//! regardless of how the dimensions are calibrated.
//!
//! - `valence` ∈ `-1.0..=1.0` — unpleasant → pleasant.
//! - `arousal` ∈ `0.0..=1.0` — calm → excited.
//! - `dominance` ∈ `0.0..=1.0` — submissive → dominant.

use serde::{Deserialize, Serialize};

/// A three-dimensional summary coordinate for emotion.
///
/// Use the [`Vad::new`] constructor to guarantee values stay within
/// bounds (fields are clamped to their ranges).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vad {
    /// Pleasantness, `-1.0..=1.0`.
    pub valence: f32,
    /// Arousal, `0.0..=1.0`.
    pub arousal: f32,
    /// Sense of control, `0.0..=1.0`.
    pub dominance: f32,
}

impl Vad {
    /// The neutral emotional state: zero valence, low arousal, mid dominance.
    pub const NEUTRAL: Vad = Vad {
        valence: 0.0,
        arousal: 0.0,
        dominance: 0.5,
    };

    /// Builds a VAD coordinate and clamps the fields to valid bounds.
    ///
    /// `valence` is bounded to `-1.0..=1.0`, `arousal` and `dominance` to
    /// `0.0..=1.0`. NaN input is converted to a safe zero (valence/arousal)
    /// or the midpoint (dominance), so an invalid float never propagates
    /// forward.
    #[must_use]
    pub fn new(valence: f32, arousal: f32, dominance: f32) -> Self {
        Self {
            valence: clamp_signed(valence),
            arousal: clamp_unit(arousal),
            dominance: clamp_unit_default_mid(dominance),
        }
    }

    /// The Euclidean distance to another VAD point.
    ///
    /// Useful for measuring blends and similarity.
    #[must_use]
    pub fn distance(self, other: Vad) -> f32 {
        let dv = self.valence - other.valence;
        let da = self.arousal - other.arousal;
        let dd = self.dominance - other.dominance;
        dd.mul_add(dd, dv.mul_add(dv, da * da)).sqrt()
    }
}

impl Default for Vad {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

/// Clamps a value to `-1.0..=1.0`; NaN → 0.0.
fn clamp_signed(x: f32) -> f32 {
    if x.is_nan() {
        0.0
    } else {
        x.clamp(-1.0, 1.0)
    }
}

/// Clamps a value to `0.0..=1.0`; NaN → 0.0.
fn clamp_unit(x: f32) -> f32 {
    if x.is_nan() {
        0.0
    } else {
        x.clamp(0.0, 1.0)
    }
}

/// Clamps a value to `0.0..=1.0`; NaN → 0.5 (neutral mid-dominance).
fn clamp_unit_default_mid(x: f32) -> f32 {
    if x.is_nan() {
        0.5
    } else {
        x.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    // Tests compare exactly representable f32 constants — exact comparison is fine.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn neutral_is_within_bounds() {
        let n = Vad::NEUTRAL;
        assert_eq!(n.valence, 0.0);
        assert_eq!(n.arousal, 0.0);
        assert_eq!(n.dominance, 0.5);
        assert_eq!(Vad::default(), Vad::NEUTRAL);
    }

    #[test]
    fn new_clamps_out_of_range_values() {
        let v = Vad::new(5.0, 5.0, 5.0);
        assert_eq!(v.valence, 1.0);
        assert_eq!(v.arousal, 1.0);
        assert_eq!(v.dominance, 1.0);

        let v = Vad::new(-5.0, -5.0, -5.0);
        assert_eq!(v.valence, -1.0);
        assert_eq!(v.arousal, 0.0);
        assert_eq!(v.dominance, 0.0);
    }

    #[test]
    fn new_passes_through_in_range_values() {
        let v = Vad::new(0.3, 0.6, 0.4);
        assert!((v.valence - 0.3).abs() < f32::EPSILON);
        assert!((v.arousal - 0.6).abs() < f32::EPSILON);
        assert!((v.dominance - 0.4).abs() < f32::EPSILON);
    }

    #[test]
    fn new_sanitizes_nan() {
        let v = Vad::new(f32::NAN, f32::NAN, f32::NAN);
        assert_eq!(v.valence, 0.0);
        assert_eq!(v.arousal, 0.0);
        assert_eq!(v.dominance, 0.5);
        assert!(!v.valence.is_nan());
    }

    #[test]
    fn distance_to_self_is_zero() {
        let v = Vad::new(0.4, 0.5, 0.6);
        assert!(v.distance(v).abs() < f32::EPSILON);
    }

    #[test]
    fn distance_is_symmetric_and_positive() {
        let a = Vad::new(-1.0, 0.0, 0.0);
        let b = Vad::new(1.0, 1.0, 1.0);
        let d_ab = a.distance(b);
        let d_ba = b.distance(a);
        assert!(d_ab > 0.0);
        assert!((d_ab - d_ba).abs() < f32::EPSILON);
    }

    #[test]
    fn serde_roundtrip() {
        let v = Vad::new(0.25, 0.75, 0.5);
        let json = serde_json::to_string(&v).expect("serialize");
        let back: Vad = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(v, back);
    }
}

//! [`EmotionState`]: a machine's momentary emotional state in a 19-dimensional space.
//!
//! The state is 19 floating-point values (`0.0..=100.0`), one per
//! [`Dimension`] axis. A low-dimensional [`Vad`] summary can be derived
//! from the state, named [`Blend`](crate::blend::Blend) combinations can be
//! detected, and it decays over time ([`EmotionState::decay`]) toward the
//! resting state defined by the calibration.

use serde::{Deserialize, Serialize};

use crate::blend::{primary_blend, BlendMatch};
use crate::calibration::EmotionCalibration;
use crate::dimension::{Dimension, DIMENSION_COUNT};
use crate::vad::Vad;

/// The lower bound of a single dimension value.
const VALUE_MIN: f32 = 0.0;
/// The upper bound of a single dimension value.
const VALUE_MAX: f32 = 100.0;

/// A machine's momentary emotional state.
///
/// The [`values`](EmotionState::values) field is indexed by
/// [`Dimension::index`]. Use the [`EmotionState::value`] /
/// [`EmotionState::set`] / [`EmotionState::stimulate`] methods so that
/// values stay within the bounds `0.0..=100.0`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EmotionState {
    /// The 19 dimension values, `0.0..=100.0`, indexed by [`Dimension::index`].
    pub values: [f32; DIMENSION_COUNT],
}

impl EmotionState {
    /// The neutral state: all dimensions at zero.
    #[must_use]
    pub const fn neutral() -> Self {
        Self {
            values: [0.0; DIMENSION_COUNT],
        }
    }

    /// Builds a state directly from an array; each value is clamped to
    /// the bounds and NaN is converted to zero.
    #[must_use]
    pub fn from_values(values: [f32; DIMENSION_COUNT]) -> Self {
        let mut state = Self::neutral();
        for (i, raw) in values.into_iter().enumerate() {
            state.values[i] = sanitize(raw);
        }
        state
    }

    /// Returns the dimension's value (`0.0..=100.0`).
    #[must_use]
    pub fn value(&self, dimension: Dimension) -> f32 {
        self.values[dimension.index()]
    }

    /// Sets the dimension's value, clamping it to the bounds `0.0..=100.0`
    /// (NaN → 0.0).
    pub fn set(&mut self, dimension: Dimension, value: f32) {
        self.values[dimension.index()] = sanitize(value);
    }

    /// Adds `delta` to the dimension and clamps the result to the bounds.
    ///
    /// A positive `delta` strengthens the emotion, a negative one weakens
    /// it. Use this to apply stimuli.
    pub fn stimulate(&mut self, dimension: Dimension, delta: f32) {
        let current = self.values[dimension.index()];
        self.values[dimension.index()] = sanitize(current + delta);
    }

    /// The largest single dimension value and its dimension, or `None` if
    /// the state is fully neutral (everything at zero).
    #[must_use]
    pub fn dominant(&self) -> Option<(Dimension, f32)> {
        let mut best: Option<(Dimension, f32)> = None;
        for dim in Dimension::ALL {
            let v = self.value(dim);
            if v > 0.0 && best.is_none_or(|(_, bv)| v > bv) {
                best = Some((dim, v));
            }
        }
        best
    }

    /// Projects the state onto a three-dimensional [`Vad`] summary.
    ///
    /// VAD is computed as a weighted average, using the dimension values,
    /// of each dimension's VAD anchor ([`Dimension::vad_anchor`]). If all
    /// dimensions are zero, [`Vad::NEUTRAL`] is returned.
    #[must_use]
    pub fn to_vad(&self) -> Vad {
        let mut total_weight = 0.0_f32;
        let mut v = 0.0_f32;
        let mut a = 0.0_f32;
        let mut d = 0.0_f32;
        for dim in Dimension::ALL {
            let weight = self.value(dim);
            if weight <= 0.0 {
                continue;
            }
            let (av, aa, ad) = dim.vad_anchor();
            v += av * weight;
            a += aa * weight;
            d += ad * weight;
            total_weight += weight;
        }
        if total_weight <= 0.0 {
            return Vad::NEUTRAL;
        }
        Vad::new(v / total_weight, a / total_weight, d / total_weight)
    }

    /// The strongest named [`Blend`](crate::blend::Blend) present in this state, or `None`.
    ///
    /// See [`crate::blend::detect_blends`] if you want all blends.
    #[must_use]
    pub fn primary_blend(&self) -> Option<BlendMatch> {
        primary_blend(self)
    }

    /// Decays the state over time toward the calibration's resting state.
    ///
    /// `dt_secs` is the elapsed time in seconds. Each dimension approaches
    /// the calibration's [`baseline`](EmotionCalibration::baseline) value
    /// exponentially; the decay speed is scaled by the calibration's
    /// [`decay_rate`](EmotionCalibration::decay_rate) factor.
    ///
    /// A negative or non-finite `dt_secs` is ignored (no-op), so an
    /// invalid time delta can't corrupt the state.
    ///
    /// Use [`decay_with`](EmotionState::decay_with) if you need a custom
    /// half-life; this method uses the crate's default
    /// ([`DEFAULT_HALF_LIFE_SECS`]).
    pub fn decay(&mut self, dt_secs: f32, calibration: &impl EmotionCalibration) {
        self.decay_with(dt_secs, DEFAULT_HALF_LIFE_SECS, calibration);
    }

    /// Like [`decay`](EmotionState::decay), but with the given base
    /// half-life `half_life_secs`.
    ///
    /// The half-life is the time in which a dimension's distance to its
    /// baseline halves, when `decay_rate = 1.0`. A smaller value means
    /// faster decay. A non-positive `half_life_secs` is ignored.
    pub fn decay_with(
        &mut self,
        dt_secs: f32,
        half_life_secs: f32,
        calibration: &impl EmotionCalibration,
    ) {
        if !dt_secs.is_finite() || dt_secs <= 0.0 {
            return;
        }
        if !half_life_secs.is_finite() || half_life_secs <= 0.0 {
            return;
        }
        for dim in Dimension::ALL {
            let rate = calibration.decay_rate(dim);
            // Invalid/non-positive rate = the dimension doesn't decay.
            if !rate.is_finite() || rate <= 0.0 {
                continue;
            }
            let baseline = calibration.baseline(dim).clamp(VALUE_MIN, VALUE_MAX);
            let current = self.values[dim.index()];
            // Exponential approach toward the baseline:
            // retained = 0.5 ^ (rate * dt / half_life)
            let exponent = rate * dt_secs / half_life_secs;
            let retained = 0.5_f32.powf(exponent);
            let next = baseline + (current - baseline) * retained;
            self.values[dim.index()] = sanitize(next);
        }
    }
}

/// The crate's default half-life for decay (in seconds).
///
/// 30 min: the emotional state halves in roughly half an hour when
/// `decay_rate = 1.0`. Layer B can override this via the
/// [`decay_with`](EmotionState::decay_with) method.
pub const DEFAULT_HALF_LIFE_SECS: f32 = 1800.0;

impl Default for EmotionState {
    fn default() -> Self {
        Self::neutral()
    }
}

/// Clamps a dimension value to the bounds `0.0..=100.0`; NaN → 0.0.
fn sanitize(x: f32) -> f32 {
    if x.is_nan() {
        VALUE_MIN
    } else {
        x.clamp(VALUE_MIN, VALUE_MAX)
    }
}

#[cfg(test)]
mod tests {
    // Tests compare exactly representable f32 constants — exact comparison is fine.
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::calibration::{NeutralCalibration, TableCalibration};

    #[test]
    fn neutral_is_all_zero() {
        let s = EmotionState::neutral();
        for dim in Dimension::ALL {
            assert_eq!(s.value(dim), 0.0);
        }
        assert_eq!(EmotionState::default(), s);
    }

    #[test]
    fn set_and_value_roundtrip_with_clamp() {
        let mut s = EmotionState::neutral();
        s.set(Dimension::Joy, 42.0);
        assert_eq!(s.value(Dimension::Joy), 42.0);
        s.set(Dimension::Joy, 500.0);
        assert_eq!(s.value(Dimension::Joy), 100.0);
        s.set(Dimension::Joy, -10.0);
        assert_eq!(s.value(Dimension::Joy), 0.0);
        s.set(Dimension::Joy, f32::NAN);
        assert_eq!(s.value(Dimension::Joy), 0.0);
    }

    #[test]
    fn stimulate_adds_and_clamps() {
        let mut s = EmotionState::neutral();
        s.stimulate(Dimension::Anger, 30.0);
        s.stimulate(Dimension::Anger, 20.0);
        assert_eq!(s.value(Dimension::Anger), 50.0);
        s.stimulate(Dimension::Anger, 100.0);
        assert_eq!(s.value(Dimension::Anger), 100.0);
        s.stimulate(Dimension::Anger, -200.0);
        assert_eq!(s.value(Dimension::Anger), 0.0);
    }

    #[test]
    fn from_values_sanitizes() {
        let mut raw = [0.0; DIMENSION_COUNT];
        raw[Dimension::Fear.index()] = 200.0;
        raw[Dimension::Joy.index()] = f32::NAN;
        raw[Dimension::Hope.index()] = -5.0;
        let s = EmotionState::from_values(raw);
        assert_eq!(s.value(Dimension::Fear), 100.0);
        assert_eq!(s.value(Dimension::Joy), 0.0);
        assert_eq!(s.value(Dimension::Hope), 0.0);
    }

    #[test]
    fn dominant_returns_largest_or_none() {
        let mut s = EmotionState::neutral();
        assert!(s.dominant().is_none());
        s.set(Dimension::Love, 30.0);
        s.set(Dimension::Pride, 70.0);
        let (dim, v) = s.dominant().expect("dominant present");
        assert_eq!(dim, Dimension::Pride);
        assert_eq!(v, 70.0);
    }

    #[test]
    fn neutral_state_maps_to_neutral_vad() {
        assert_eq!(EmotionState::neutral().to_vad(), Vad::NEUTRAL);
    }

    #[test]
    fn single_dimension_vad_equals_its_anchor() {
        let mut s = EmotionState::neutral();
        s.set(Dimension::Joy, 80.0);
        let vad = s.to_vad();
        let (av, aa, ad) = Dimension::Joy.vad_anchor();
        assert!((vad.valence - av).abs() < 1e-4);
        assert!((vad.arousal - aa).abs() < 1e-4);
        assert!((vad.dominance - ad).abs() < 1e-4);
    }

    #[test]
    fn vad_is_weighted_average_of_anchors() {
        let mut s = EmotionState::neutral();
        s.set(Dimension::Joy, 50.0);
        s.set(Dimension::Sadness, 50.0);
        let vad = s.to_vad();
        let (jv, _, _) = Dimension::Joy.vad_anchor();
        let (sv, _, _) = Dimension::Sadness.vad_anchor();
        let expected_v = f32::midpoint(jv, sv);
        assert!((vad.valence - expected_v).abs() < 1e-4);
    }

    #[test]
    fn decay_ignores_invalid_dt() {
        let mut s = EmotionState::neutral();
        s.set(Dimension::Joy, 80.0);
        let cal = NeutralCalibration;
        s.decay(-5.0, &cal);
        assert_eq!(s.value(Dimension::Joy), 80.0);
        s.decay(f32::NAN, &cal);
        assert_eq!(s.value(Dimension::Joy), 80.0);
        s.decay(0.0, &cal);
        assert_eq!(s.value(Dimension::Joy), 80.0);
    }

    #[test]
    fn decay_halves_distance_to_zero_after_one_half_life() {
        let mut s = EmotionState::neutral();
        s.set(Dimension::Joy, 80.0);
        let cal = NeutralCalibration;
        s.decay_with(DEFAULT_HALF_LIFE_SECS, DEFAULT_HALF_LIFE_SECS, &cal);
        // Neutral baseline = 0, so the value halves.
        assert!((s.value(Dimension::Joy) - 40.0).abs() < 1e-2);
    }

    #[test]
    fn decay_approaches_nonzero_baseline() {
        let cal = TableCalibration::new("warm").with_baseline(Dimension::Love, 20.0);
        let mut s = EmotionState::neutral();
        s.set(Dimension::Love, 100.0);
        // Multiple half-lives → approaches 20, not zero.
        for _ in 0..20 {
            s.decay_with(DEFAULT_HALF_LIFE_SECS, DEFAULT_HALF_LIFE_SECS, &cal);
        }
        let v = s.value(Dimension::Love);
        assert!(
            (v - 20.0).abs() < 0.5,
            "arvo {v} ei lähestynyt baselinea 20"
        );
    }

    #[test]
    fn decay_rate_scales_speed() {
        let fast = TableCalibration::new("fast").with_decay_rate(Dimension::Joy, 2.0);
        let slow = TableCalibration::new("slow").with_decay_rate(Dimension::Joy, 0.5);
        let mut s_fast = EmotionState::neutral();
        let mut s_slow = EmotionState::neutral();
        s_fast.set(Dimension::Joy, 100.0);
        s_slow.set(Dimension::Joy, 100.0);
        s_fast.decay_with(DEFAULT_HALF_LIFE_SECS, DEFAULT_HALF_LIFE_SECS, &fast);
        s_slow.decay_with(DEFAULT_HALF_LIFE_SECS, DEFAULT_HALF_LIFE_SECS, &slow);
        // Faster decay → lower value.
        assert!(s_fast.value(Dimension::Joy) < s_slow.value(Dimension::Joy));
    }

    #[test]
    fn zero_decay_rate_freezes_dimension() {
        let frozen = TableCalibration::new("frozen").with_decay_rate(Dimension::Sisu, 0.0);
        let mut s = EmotionState::neutral();
        s.set(Dimension::Sisu, 90.0);
        s.decay_with(DEFAULT_HALF_LIFE_SECS, DEFAULT_HALF_LIFE_SECS, &frozen);
        assert_eq!(s.value(Dimension::Sisu), 90.0);
    }

    #[test]
    fn serde_roundtrip_preserves_state() {
        let mut s = EmotionState::neutral();
        s.set(Dimension::Gratitude, 60.0);
        s.set(Dimension::Wonder, 33.5);
        let json = serde_json::to_string(&s).expect("serialize");
        let back: EmotionState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(s, back);
    }
}

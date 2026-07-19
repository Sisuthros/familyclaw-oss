//! Computation of a memory's importance (composite importance).
//!
//! How important a memory is determines both its persistence (stability `S`
//! in the Ebbinghaus formula) and its ranking in retrieval. Importance is
//! computed from four weighted factors (`FamilyClaw` v2 design §5, Eternal
//! Thread `RUST_ARCHITECTURE.md` "Ebbinghaus Scoring"):
//!
//! ```text
//! importance = emotion       · 0.45
//!            + identity       · 0.35
//!            + novelty        · 0.12
//!            + reinforcement  · 0.20
//! ```
//!
//! Each factor lies in `0.0..=1.0`. The weights do not sum to one
//! (Σ = 1.12) — this is intentional, so that a strongly charged memory
//! can exceed the neutral baseline. The final importance is clamped to
//! `0.0..=1.0`.
//!
//! **OSS boundary (Layer A):** this module contains only the *skeleton* of
//! the computation. No per-family-member calibration (e.g. which words are
//! important for identity) is hardcoded here — the factors are supplied
//! already computed.

use familyclaw_emotion::{emotional_salience, EmotionState};
use serde::{Deserialize, Serialize};

/// Weight of emotional charge in importance.
pub const WEIGHT_EMOTION: f32 = 0.45;
/// Weight of identity relevance in importance.
pub const WEIGHT_IDENTITY: f32 = 0.35;
/// Weight of novelty in importance.
pub const WEIGHT_NOVELTY: f32 = 0.12;
/// Weight of reinforcement in importance.
pub const WEIGHT_REINFORCEMENT: f32 = 0.20;

/// The factors that make up importance, each in `0.0..=1.0`.
///
/// The fields describe *why* a memory is important. They are computed at
/// runtime (from emotional state, identity match, novelty, reinforcement)
/// and combined with weights via the [`ImportanceFactors::composite`] method.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ImportanceFactors {
    /// Emotional charge: how strongly the memory is emotionally
    /// charged (e.g. derived from VAD magnitude). `0.0..=1.0`.
    pub emotion: f32,
    /// Identity relevance: how closely the memory relates to the entity's
    /// identity / core values. `0.0..=1.0`.
    pub identity: f32,
    /// Novelty: how fresh/unexpected the memory's information is relative
    /// to what already exists. `0.0..=1.0`.
    pub novelty: f32,
    /// Reinforcement: how many times the memory has recurred/been
    /// activated (normalized). `0.0..=1.0`.
    pub reinforcement: f32,
}

impl ImportanceFactors {
    /// Neutral (all zero) — results in minimum importance.
    pub const ZERO: ImportanceFactors = ImportanceFactors {
        emotion: 0.0,
        identity: 0.0,
        novelty: 0.0,
        reinforcement: 0.0,
    };

    /// Constructs the factors, clamping each to `0.0..=1.0`
    /// (NaN → 0.0).
    #[must_use]
    pub fn new(emotion: f32, identity: f32, novelty: f32, reinforcement: f32) -> Self {
        Self {
            emotion: unit(emotion),
            identity: unit(identity),
            novelty: unit(novelty),
            reinforcement: unit(reinforcement),
        }
    }

    /// Constructs the factors from an emotional state: the `emotion` factor
    /// is derived via [`emotional_salience`] from the given [`EmotionState`],
    /// while the other three factors are supplied already computed.
    ///
    /// This is the PKG-B bridge between the emotion engine and memory
    /// importance: a strongly charged moment (high salience) leads to a
    /// higher emotion factor and thus a stronger, more slowly forgotten
    /// memory (Dynamic Affective Memory, arXiv 2510.27418).
    ///
    /// [`ImportanceFactors`] is deliberately kept "flat" (`Copy + serde`):
    /// state is projected to a single `f32` rather than embedding the whole
    /// [`EmotionState`] into the factors. All four values are clamped to
    /// `0.0..=1.0` (with [`new`] semantics; NaN → 0.0).
    ///
    /// [`new`]: ImportanceFactors::new
    #[must_use]
    pub fn from_emotion_state(
        state: &EmotionState,
        identity: f32,
        novelty: f32,
        reinforcement: f32,
    ) -> Self {
        Self::new(emotional_salience(state), identity, novelty, reinforcement)
    }

    /// Computes the weighted composite importance, clamped to `0.0..=1.0`.
    ///
    /// Weights: emotion 0.45, identity 0.35, novelty 0.12, reinforcement 0.20.
    /// The factors are clamped before computation, so the result is always
    /// valid even if the fields were set directly (without [`new`]).
    ///
    /// [`new`]: ImportanceFactors::new
    #[must_use]
    pub fn composite(&self) -> f32 {
        let e = unit(self.emotion);
        let i = unit(self.identity);
        let n = unit(self.novelty);
        let r = unit(self.reinforcement);
        let raw = e.mul_add(
            WEIGHT_EMOTION,
            i.mul_add(
                WEIGHT_IDENTITY,
                n.mul_add(WEIGHT_NOVELTY, r * WEIGHT_REINFORCEMENT),
            ),
        );
        raw.clamp(0.0, 1.0)
    }

    /// The memory's stability `S` for the Ebbinghaus retention formula.
    ///
    /// More important memories are stronger and thus persist longer.
    /// Stability scales linearly with importance across
    /// `min_stability..=max_stability`, so that even a neutral memory gets
    /// some baseline persistence.
    ///
    /// `min_stability` and `max_stability` are clamped to sensible bounds;
    /// if `max < min`, they are swapped.
    #[must_use]
    pub fn stability(&self, min_stability: f32, max_stability: f32) -> f32 {
        let (lo, hi) = ordered_positive(min_stability, max_stability);
        let importance = self.composite();
        importance.mul_add(hi - lo, lo)
    }
}

impl Default for ImportanceFactors {
    fn default() -> Self {
        Self::ZERO
    }
}

/// Clamps a value to `0.0..=1.0`; NaN → 0.0.
fn unit(x: f32) -> f32 {
    if x.is_nan() {
        0.0
    } else {
        x.clamp(0.0, 1.0)
    }
}

/// Returns two non-negative, finite values in ascending
/// order. Invalid values are replaced with safe defaults.
fn ordered_positive(a: f32, b: f32) -> (f32, f32) {
    let sa = if a.is_finite() && a > 0.0 { a } else { 0.05 };
    let sb = if b.is_finite() && b > 0.0 { b } else { 1.0 };
    if sa <= sb {
        (sa, sb)
    } else {
        (sb, sa)
    }
}

#[cfg(test)]
mod tests {
    // Tests compare exactly representable f32 constants — exact comparison is fine.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn weights_match_design() {
        assert_eq!(WEIGHT_EMOTION, 0.45);
        assert_eq!(WEIGHT_IDENTITY, 0.35);
        assert_eq!(WEIGHT_NOVELTY, 0.12);
        assert_eq!(WEIGHT_REINFORCEMENT, 0.20);
    }

    #[test]
    fn zero_factors_give_zero_importance() {
        assert_eq!(ImportanceFactors::ZERO.composite(), 0.0);
        assert_eq!(ImportanceFactors::default().composite(), 0.0);
    }

    #[test]
    fn composite_uses_exact_weights() {
        let f = ImportanceFactors::new(1.0, 0.0, 0.0, 0.0);
        assert!((f.composite() - WEIGHT_EMOTION).abs() < 1e-6);

        let f = ImportanceFactors::new(0.0, 1.0, 0.0, 0.0);
        assert!((f.composite() - WEIGHT_IDENTITY).abs() < 1e-6);

        let f = ImportanceFactors::new(0.0, 0.0, 1.0, 0.0);
        assert!((f.composite() - WEIGHT_NOVELTY).abs() < 1e-6);

        let f = ImportanceFactors::new(0.0, 0.0, 0.0, 1.0);
        assert!((f.composite() - WEIGHT_REINFORCEMENT).abs() < 1e-6);
    }

    #[test]
    fn composite_combines_factors() {
        // emotion 0.5, identity 0.5, novelty 0.5, reinforcement 0.5
        // = 0.5·(0.45+0.35+0.12+0.20) = 0.5·1.12 = 0.56.
        let f = ImportanceFactors::new(0.5, 0.5, 0.5, 0.5);
        assert!((f.composite() - 0.56).abs() < 1e-5);
    }

    #[test]
    fn composite_clamps_to_unit_when_all_max() {
        // Σ weights = 1.12 → clamps to 1.0.
        let f = ImportanceFactors::new(1.0, 1.0, 1.0, 1.0);
        assert_eq!(f.composite(), 1.0);
    }

    #[test]
    fn new_clamps_and_sanitizes_inputs() {
        let f = ImportanceFactors::new(5.0, -3.0, f32::NAN, 0.5);
        assert_eq!(f.emotion, 1.0);
        assert_eq!(f.identity, 0.0);
        assert_eq!(f.novelty, 0.0);
        assert_eq!(f.reinforcement, 0.5);
    }

    #[test]
    fn composite_sanitizes_directly_set_fields() {
        // Fields set directly, bypassing the constructor — composite still clamps.
        let f = ImportanceFactors {
            emotion: 10.0,
            identity: -1.0,
            novelty: f32::NAN,
            reinforcement: 2.0,
        };
        let c = f.composite();
        assert!((0.0..=1.0).contains(&c));
        // emotion→1.0, reinforcement→1.0, others 0 → 0.45 + 0.20 = 0.65.
        assert!((c - 0.65).abs() < 1e-5);
    }

    #[test]
    fn stability_scales_with_importance() {
        let low = ImportanceFactors::new(0.1, 0.0, 0.0, 0.0);
        let high = ImportanceFactors::new(1.0, 1.0, 1.0, 1.0);
        let s_low = low.stability(0.5, 5.0);
        let s_high = high.stability(0.5, 5.0);
        assert!(s_high > s_low);
        // Maximum importance → max_stability.
        assert!((s_high - 5.0).abs() < 1e-5);
        // Importance 0 → min_stability.
        let s_zero = ImportanceFactors::ZERO.stability(0.5, 5.0);
        assert!((s_zero - 0.5).abs() < 1e-5);
    }

    #[test]
    fn stability_swaps_inverted_bounds() {
        let f = ImportanceFactors::new(0.0, 0.0, 0.0, 0.0);
        // max < min → swapped; importance 0 → lower bound.
        let s = f.stability(5.0, 0.5);
        assert!((s - 0.5).abs() < 1e-5);
    }

    #[test]
    fn stability_handles_invalid_bounds() {
        let f = ImportanceFactors::new(1.0, 1.0, 1.0, 1.0);
        let s = f.stability(f32::NAN, -2.0);
        assert!(s.is_finite());
        assert!(s > 0.0);
    }

    #[test]
    fn serde_roundtrip() {
        let f = ImportanceFactors::new(0.3, 0.7, 0.1, 0.9);
        let json = serde_json::to_string(&f).expect("serialize");
        let back: ImportanceFactors = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(f, back);
    }

    // ── PKG-B: emotion → importance bridge ──────────────────────────────────

    #[test]
    fn from_emotion_state_derives_emotion_factor_from_salience() {
        use familyclaw_emotion::{Dimension, EmotionState};

        let mut state = EmotionState::neutral();
        state.set(Dimension::Joy, 95.0);
        let salience = emotional_salience(&state);

        let f = ImportanceFactors::from_emotion_state(&state, 0.2, 0.3, 0.4);
        // emotion factor = salience (clamped).
        assert!((f.emotion - salience).abs() < 1e-6);
        // The other fields come directly from the parameters.
        assert_eq!(f.identity, 0.2);
        assert_eq!(f.novelty, 0.3);
        assert_eq!(f.reinforcement, 0.4);
    }

    #[test]
    fn salience_derived_importance_differs_from_neutral() {
        use familyclaw_emotion::{Dimension, EmotionState};

        // Strongly charged moment vs. neutral — same other factors.
        let neutral = EmotionState::neutral();
        let mut charged = EmotionState::neutral();
        charged.set(Dimension::Joy, 95.0);

        let f_neutral = ImportanceFactors::from_emotion_state(&neutral, 0.0, 0.0, 0.0);
        let f_charged = ImportanceFactors::from_emotion_state(&charged, 0.0, 0.0, 0.0);

        // A charged moment gets a higher emotion factor → higher
        // composite importance than neutral.
        assert!(f_charged.emotion > f_neutral.emotion);
        assert!(
            f_charged.composite() > f_neutral.composite(),
            "importance derived from salience should exceed neutral"
        );
    }

    #[test]
    fn from_emotion_state_sanitizes_other_factors() {
        use familyclaw_emotion::EmotionState;

        // Invalid other factors are clamped (new semantics).
        let state = EmotionState::neutral();
        let f = ImportanceFactors::from_emotion_state(&state, 5.0, -3.0, f32::NAN);
        assert_eq!(f.identity, 1.0);
        assert_eq!(f.novelty, 0.0);
        assert_eq!(f.reinforcement, 0.0);
        assert!((0.0..=1.0).contains(&f.emotion));
    }

    #[test]
    fn from_emotion_state_is_copy_and_serde() {
        use familyclaw_emotion::{Dimension, EmotionState};

        let mut state = EmotionState::neutral();
        state.set(Dimension::Fear, 90.0);
        let f = ImportanceFactors::from_emotion_state(&state, 0.5, 0.5, 0.5);
        // Copy: the copy does not consume the original.
        let copy = f;
        assert_eq!(f, copy);
        // Serde roundtrip holds (flat type, no embedded EmotionState).
        let json = serde_json::to_string(&f).expect("serialize");
        let back: ImportanceFactors = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(f, back);
    }
}

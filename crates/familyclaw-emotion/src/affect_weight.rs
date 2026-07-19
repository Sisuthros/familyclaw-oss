//! Emotional *salience weight* (affective salience) — how "worth
//! remembering" a momentary emotional state is.
//!
//! This module provides a single pure function,
//! [`emotional_salience`], which projects an [`EmotionState`] onto a single
//! number in the range `0.0..=1.0`. The idea is inspired by *Dynamic
//! Affective Memory* work (arXiv 2510.27418): strongly charged moments —
//! high arousal combined with a clear valence sign — are more important
//! to retain than dull, neutral moments.
//!
//! ## Cross-crate wiring
//! This crate does **not** call any other crate — it only offers a pure
//! function. `familyclaw-memory` wires this in as an importance weight on
//! its own side (`ImportanceFactors::from_emotion_state` +
//! `MemoryBuilder::emotion_state`, alpha.5), so the dependency direction
//! stays correct (memory → emotion, not the other way around).

use crate::state::EmotionState;

/// The maximum dimension value (scale `0.0..=100.0`).
const VALUE_MAX: f32 = 100.0;

/// Estimates the *salience* of an emotional state in the range `0.0..=1.0`.
///
/// Salience is high when the state is **strong** and **clearly charged**
/// — that is, when both arousal and the absolute value of valence are
/// large. A neutral state → close to zero; strong joy OR strong fear →
/// close to one.
///
/// ## Model (lightweight, transparent)
/// Salience combines three signals from the VAD projection
/// ([`EmotionState::to_vad`]) and the state's raw intensity:
///
/// 1. **Arousal** (`arousal`, `0.0..=1.0`) — how activated the moment is.
/// 2. **Valence magnitude** (`|valence|`, `0.0..=1.0`) — how clearly
///    positive or negative it is (the sign doesn't matter: both peaks
///    and troughs are worth remembering).
/// 3. **Intensity** (the largest dimension value scaled to `0.0..=1.0`) —
///    a fully dull state isn't important even if its anchors point to
///    the extremes.
///
/// The result is a weighted combination of these, clamped to `0.0..=1.0`.
/// The weights favor the combination of arousal and valence magnitude
/// (Dynamic Affective Memory: charged moments stick in memory), but
/// intensity acts as a gate so that a near-neutral state doesn't get a
/// high value.
///
/// NaN input is already sanitized inside [`EmotionState`] (values are
/// always within bounds), so the result is always finite.
///
/// # Example
/// ```
/// use familyclaw_emotion::{Dimension, EmotionState, emotional_salience};
///
/// // Neutral state → low salience.
/// let neutral = EmotionState::neutral();
/// assert!(emotional_salience(&neutral) < 0.1);
///
/// // Strong joy → high salience.
/// let mut joyful = EmotionState::neutral();
/// joyful.set(Dimension::Joy, 95.0);
/// assert!(emotional_salience(&joyful) > emotional_salience(&neutral));
/// ```
#[must_use]
pub fn emotional_salience(state: &EmotionState) -> f32 {
    let vad = state.to_vad();

    // Arousal is already 0..1.
    let arousal = vad.arousal.clamp(0.0, 1.0);
    // Valence magnitude: the extremes (either sign) are important.
    let valence_mag = vad.valence.abs().clamp(0.0, 1.0);

    // Raw intensity = the largest dimension value scaled to 0..1.
    let intensity = state
        .dominant()
        .map_or(0.0, |(_, v)| (v / VALUE_MAX).clamp(0.0, 1.0));

    // Charge component: arousal and valence magnitude together.
    // Use the average so that each alone raises salience, but both
    // together raise it the most.
    let charge = f32::midpoint(arousal, valence_mag);

    // Intensity acts as a gate: a near-neutral state dampens the
    // result even if the anchors point to the extremes.
    let raw = charge * intensity;

    raw.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    // Exact f32 comparisons are deliberately allowed here.
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::dimension::Dimension;

    #[test]
    fn neutral_state_has_low_salience() {
        let s = EmotionState::neutral();
        assert!(emotional_salience(&s) < 0.05);
    }

    #[test]
    fn salience_is_within_unit_range() {
        // Go through a few extreme states — the result always stays within 0..1.
        for dim in Dimension::ALL {
            let mut s = EmotionState::neutral();
            s.set(dim, 100.0);
            let v = emotional_salience(&s);
            assert!(
                (0.0..=1.0).contains(&v),
                "salience {v} ulkona rajoista: {dim}"
            );
        }
    }

    #[test]
    fn high_arousal_extreme_valence_is_salient() {
        // Strong joy: high arousal + clear positive valence.
        let mut s = EmotionState::neutral();
        s.set(Dimension::Joy, 95.0);
        assert!(
            emotional_salience(&s) > 0.4,
            "voimakkaan ilon pitäisi olla salientti"
        );
    }

    #[test]
    fn strong_negative_is_also_salient() {
        // The sign of valence must not matter: fear is just as worth
        // remembering as joy.
        let mut fear = EmotionState::neutral();
        fear.set(Dimension::Fear, 95.0);
        assert!(
            emotional_salience(&fear) > 0.4,
            "voimakkaan pelon pitäisi olla salientti"
        );
    }

    #[test]
    fn stronger_intensity_raises_salience() {
        // Same dimension, different intensity → higher intensity = higher
        // salience (the intensity gate works).
        let mut weak = EmotionState::neutral();
        weak.set(Dimension::Joy, 20.0);
        let mut strong = EmotionState::neutral();
        strong.set(Dimension::Joy, 90.0);
        assert!(emotional_salience(&strong) > emotional_salience(&weak));
    }

    #[test]
    fn low_arousal_calm_dimension_is_less_salient_than_high_arousal() {
        // Tenderness is low-arousal warmth; joy is high-arousal.
        // At the same intensity, higher arousal → higher salience.
        let mut calm = EmotionState::neutral();
        calm.set(Dimension::Tenderness, 90.0);
        let mut excited = EmotionState::neutral();
        excited.set(Dimension::Joy, 90.0);
        assert!(
            emotional_salience(&excited) > emotional_salience(&calm),
            "korkean virityksen tilan pitäisi olla salientimpi"
        );
    }

    #[test]
    fn salience_is_deterministic() {
        let mut s = EmotionState::neutral();
        s.set(Dimension::Anger, 80.0);
        s.set(Dimension::Pride, 60.0);
        let a = emotional_salience(&s);
        let b = emotional_salience(&s);
        assert_eq!(a, b);
    }
}

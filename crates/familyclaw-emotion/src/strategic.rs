//! Strategic emotion appraisal — *how strongly* an emotion should be
//! expressed in a given situation.
//!
//! This module is **inspired by `EmoMAS`** (arXiv 2604.07003: Bayesian,
//! game-theoretic emotion expression in multi-agent systems), but it is
//! **not a full implementation**: there is no game-theory solver here, no
//! Nash equilibrium, and no opponent modeling. Instead, it offers a
//! lightweight Bayesian-*style* heuristic that's enough for an MVP: start
//! from the emotional state as a prior and adjust it based on the
//! situation's stakes (`stakes`) and sociality (`social`).
//!
//! ## Idea
//! The `EmoMAS` insight is that the *expression intensity* of an emotion
//! should be chosen strategically: in a high-stakes, strongly social
//! situation, a small, deliberate model beats a large one by choosing an
//! expression that fits the situation. This function does the same
//! thing, lightly: it doesn't decide *which* emotion to express (that's
//! [`crate::governor`]'s job), only *how strongly* the momentary affect
//! should be shown.

use crate::affect_weight::emotional_salience;
use crate::state::EmotionState;

/// A lightweight description of the situation in which an emotion is expressed.
///
/// Both fields are in the range `0.0..=1.0`. Use [`Situation::new`] so
/// that values are clamped to the bounds (NaN → 0.0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Situation {
    /// Stakes: how much is at play in the situation. `0.0` = mundane,
    /// `1.0` = a highly important moment. High stakes *amplify* the
    /// recommended expression (clarity matters when a lot is on the line).
    pub stakes: f32,
    /// Sociality: how public the situation is / how many people are
    /// present. `0.0` = private, `1.0` = strongly social. High
    /// sociality *dampens* raw expression somewhat (social regulation)
    /// but interacts with the stakes.
    pub social: f32,
}

impl Situation {
    /// Builds a situation and clamps the fields to the bounds `0.0..=1.0`
    /// (NaN → 0.0).
    #[must_use]
    pub fn new(stakes: f32, social: f32) -> Self {
        Self {
            stakes: clamp_unit(stakes),
            social: clamp_unit(social),
        }
    }

    /// The neutral situation: mundane, private.
    pub const NEUTRAL: Situation = Situation {
        stakes: 0.0,
        social: 0.0,
    };
}

impl Default for Situation {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

/// A strategic emotion appraiser (EmoMAS-inspired, lightweight heuristic).
///
/// The appraiser is a carrier type that may later have per-being tuning
/// in Layer B. The crate provides [`StrategicAppraisal::balanced`] — a
/// neutral default that favors neither over- nor under-expression.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrategicAppraisal {
    /// How strongly stakes amplify expression (`0.0..=1.0`).
    /// A high value means high stakes lead to a clearly stronger
    /// expression.
    stakes_gain: f32,
    /// How strongly sociality dampens raw expression
    /// (`0.0..=1.0`). A high value means more social regulation.
    social_damping: f32,
}

impl StrategicAppraisal {
    /// Balanced default: moderate stakes amplification and moderate
    /// social damping.
    #[must_use]
    pub const fn balanced() -> Self {
        Self {
            stakes_gain: 0.5,
            social_damping: 0.3,
        }
    }

    /// Builds an appraiser with explicit coefficients (clamped to the
    /// bounds `0.0..=1.0`, NaN → the balanced default).
    #[must_use]
    pub fn new(stakes_gain: f32, social_damping: f32) -> Self {
        let default = Self::balanced();
        Self {
            stakes_gain: sanitize(stakes_gain, default.stakes_gain),
            social_damping: sanitize(social_damping, default.social_damping),
        }
    }

    /// Recommended expression intensity in the range `0.0..=1.0`.
    ///
    /// ## Lightweight Bayesian-style model
    /// - **Prior**: the momentary affect's salience
    ///   ([`emotional_salience`]) — a strongly charged state "wants"
    ///   to be expressed.
    /// - **Evidence 1 — stakes**: high stakes raise the recommendation
    ///   above the prior (`stakes_gain` controls by how much). This is
    ///   the EmoMAS-style strategic choice: when a lot is on the line,
    ///   a clear expression pays off.
    /// - **Evidence 2 — sociality**: a strongly social situation
    ///   dampens raw expression (`social_damping`), but only to the
    ///   extent that the stakes don't *already* demand clarity — high
    ///   stakes override the social damping.
    ///
    /// The result is always clamped to `0.0..=1.0`. This is a heuristic,
    /// not a game-theoretic solution (see the module doc).
    #[must_use]
    pub fn recommend_intensity(&self, state: &EmotionState, situation: &Situation) -> f32 {
        let prior = emotional_salience(state).clamp(0.0, 1.0);
        let stakes = situation.stakes.clamp(0.0, 1.0);
        let social = situation.social.clamp(0.0, 1.0);

        // Stakes amplification: push the prior toward 1.0, more so the
        // higher the stakes and the larger stakes_gain.
        // lift = stakes_gain * stakes * (1 - prior) → never exceeds 1.0.
        let lift = self.stakes_gain * stakes * (1.0 - prior);
        let boosted = (prior + lift).clamp(0.0, 1.0);

        // Social damping: reduce raw expression, but high stakes
        // protect against damping (clarity wins when a lot is on the
        // line). The effective damping scales with (1 - stakes).
        let damping = self.social_damping * social * (1.0 - stakes);
        let damped = boosted * (1.0 - damping);

        damped.clamp(0.0, 1.0)
    }
}

impl Default for StrategicAppraisal {
    fn default() -> Self {
        Self::balanced()
    }
}

/// Clamps a value to the range `0.0..=1.0`; NaN → 0.0.
fn clamp_unit(x: f32) -> f32 {
    if x.is_nan() {
        0.0
    } else {
        x.clamp(0.0, 1.0)
    }
}

/// Clamps a coefficient to `0.0..=1.0`; non-finite → fallback.
fn sanitize(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        fallback.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    // Exact f32 comparisons are deliberately allowed here.
    #![allow(clippy::float_cmp)]

    use super::*;
    use crate::dimension::Dimension;

    fn joyful_state() -> EmotionState {
        let mut s = EmotionState::neutral();
        s.set(Dimension::Joy, 80.0);
        s
    }

    #[test]
    fn situation_clamps_and_sanitizes() {
        let s = Situation::new(5.0, -1.0);
        assert_eq!(s.stakes, 1.0);
        assert_eq!(s.social, 0.0);
        let nan = Situation::new(f32::NAN, f32::NAN);
        assert_eq!(nan.stakes, 0.0);
        assert_eq!(nan.social, 0.0);
        assert_eq!(Situation::default(), Situation::NEUTRAL);
    }

    #[test]
    fn intensity_is_within_unit_range() {
        let appraisal = StrategicAppraisal::balanced();
        let s = joyful_state();
        for &stakes in &[0.0_f32, 0.5, 1.0] {
            for &social in &[0.0_f32, 0.5, 1.0] {
                let v = appraisal.recommend_intensity(&s, &Situation::new(stakes, social));
                assert!(
                    (0.0..=1.0).contains(&v),
                    "intensiteetti {v} ulkona rajoista"
                );
            }
        }
    }

    #[test]
    fn higher_stakes_raise_intensity() {
        // Important test from the brief: appraisal grows with stakes.
        let appraisal = StrategicAppraisal::balanced();
        let s = joyful_state();
        let low = appraisal.recommend_intensity(&s, &Situation::new(0.0, 0.0));
        let high = appraisal.recommend_intensity(&s, &Situation::new(1.0, 0.0));
        assert!(high > low, "korkeampi stakes → korkeampi intensiteetti");
    }

    #[test]
    fn stakes_are_monotonic() {
        let appraisal = StrategicAppraisal::balanced();
        let s = joyful_state();
        let a = appraisal.recommend_intensity(&s, &Situation::new(0.0, 0.0));
        let b = appraisal.recommend_intensity(&s, &Situation::new(0.5, 0.0));
        let c = appraisal.recommend_intensity(&s, &Situation::new(1.0, 0.0));
        assert!(
            a <= b && b <= c,
            "intensiteetin pitäisi kasvaa monotonisesti"
        );
    }

    #[test]
    fn social_dampens_at_low_stakes() {
        // At low stakes, sociality dampens expression.
        let appraisal = StrategicAppraisal::balanced();
        let s = joyful_state();
        let private = appraisal.recommend_intensity(&s, &Situation::new(0.0, 0.0));
        let public = appraisal.recommend_intensity(&s, &Situation::new(0.0, 1.0));
        assert!(
            public < private,
            "sosiaalisuus hillitsee matalilla panoksilla"
        );
    }

    #[test]
    fn high_stakes_protect_against_social_damping() {
        // High stakes override social damping: clarity wins.
        let appraisal = StrategicAppraisal::balanced();
        let s = joyful_state();
        let private = appraisal.recommend_intensity(&s, &Situation::new(1.0, 0.0));
        let public = appraisal.recommend_intensity(&s, &Situation::new(1.0, 1.0));
        assert!(
            (private - public).abs() < 1e-6,
            "stakes=1.0 → sosiaalisuus ei enää hillitse (private={private}, public={public})"
        );
    }

    #[test]
    fn neutral_state_low_stakes_is_quiet() {
        // Neutral state + mundane situation → low expression.
        let appraisal = StrategicAppraisal::balanced();
        let s = EmotionState::neutral();
        let v = appraisal.recommend_intensity(&s, &Situation::NEUTRAL);
        assert!(v < 0.1, "neutraali + arkinen → hiljainen ilmaisu, sai {v}");
    }

    #[test]
    fn new_sanitizes_coefficients() {
        let a = StrategicAppraisal::new(f32::NAN, 5.0);
        // NaN → the balanced default, 5.0 → clamps to 1.0. The result
        // stays usable.
        let s = joyful_state();
        let v = a.recommend_intensity(&s, &Situation::new(1.0, 1.0));
        assert!((0.0..=1.0).contains(&v));
        assert_eq!(
            StrategicAppraisal::default(),
            StrategicAppraisal::balanced()
        );
    }
}

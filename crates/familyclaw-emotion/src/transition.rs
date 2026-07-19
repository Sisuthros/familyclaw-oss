//! Emotional-state *inertia* — a lightweight HMM-style state transition.
//!
//! This module is **inspired by** EQ-Negotiator's (arXiv 2511.03370)
//! HMM-based emotional-state tracking, but it is **not a full hidden
//! Markov model**: no emission distributions, no Viterbi, no
//! Baum–Welch training. What it offers is one lightweight idea:
//! *emotional inertia* — the previous emotional state influences the
//! next one, so affect doesn't jump implausibly from one moment to
//! the next.
//!
//! ## Idea
//! In an HMM, the state transition probability favors staying in the
//! same state (diagonal weighting). Here we model the same idea
//! continuously: the next state is an *interpolation* between the
//! previous (prior) and the newly observed (evidence) state. `inertia`
//! controls how much the past weighs in.

use crate::dimension::Dimension;
use crate::state::EmotionState;

/// A lightweight emotional-state transition that models emotional inertia.
///
/// Use [`EmotionTransition::new`] so that `inertia` is clamped to the
/// range `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmotionTransition {
    /// How much the previous state weighs in the next one, `0.0..=1.0`.
    /// `0.0` = no inertia (the new observation fully replaces it), `1.0` =
    /// full inertia (the state doesn't change at all). Typical value ~0.6.
    inertia: f32,
}

impl EmotionTransition {
    /// Builds a transition with the given inertia (clamped to `0.0..=1.0`,
    /// NaN → [`DEFAULT_INERTIA`]).
    #[must_use]
    pub fn new(inertia: f32) -> Self {
        Self {
            inertia: if inertia.is_finite() {
                inertia.clamp(0.0, 1.0)
            } else {
                DEFAULT_INERTIA
            },
        }
    }

    /// A transition built with the crate's default inertia.
    #[must_use]
    pub fn balanced() -> Self {
        Self::new(DEFAULT_INERTIA)
    }

    /// The active inertia value (`0.0..=1.0`).
    #[must_use]
    pub const fn inertia(self) -> f32 {
        self.inertia
    }

    /// Blends the previous state (`prev`) with the newly observed state
    /// (`observed`) according to the inertia.
    ///
    /// For each dimension:
    /// `next = inertia * prev + (1 - inertia) * observed`.
    ///
    /// High inertia → the next state stays close to the previous one
    /// (slow, believable change). Low inertia → the new observation
    /// takes over quickly. The result is clamped to [`EmotionState`]'s
    /// own bounds (`from_values` sanitizes the values).
    #[must_use]
    pub fn blend(self, prev: &EmotionState, observed: &EmotionState) -> EmotionState {
        let keep = self.inertia;
        let take = 1.0 - keep;
        let mut next = EmotionState::neutral();
        for dim in Dimension::ALL {
            let p = prev.value(dim);
            let o = observed.value(dim);
            // p*keep + o*take, expressed via mul_add.
            let v = o.mul_add(take, p * keep);
            next.set(dim, v);
        }
        next
    }
}

impl Default for EmotionTransition {
    fn default() -> Self {
        Self::balanced()
    }
}

/// The crate's default inertia — a reasonable "the past weighs in, but doesn't lock in".
pub const DEFAULT_INERTIA: f32 = 0.6;

#[cfg(test)]
mod tests {
    // Exact f32 comparisons are deliberately allowed here.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn new_clamps_and_sanitizes_inertia() {
        assert_eq!(EmotionTransition::new(5.0).inertia(), 1.0);
        assert_eq!(EmotionTransition::new(-1.0).inertia(), 0.0);
        assert_eq!(EmotionTransition::new(f32::NAN).inertia(), DEFAULT_INERTIA);
        assert_eq!(EmotionTransition::default().inertia(), DEFAULT_INERTIA);
    }

    #[test]
    fn zero_inertia_takes_observation_fully() {
        let prev = {
            let mut s = EmotionState::neutral();
            s.set(Dimension::Joy, 100.0);
            s
        };
        let observed = {
            let mut s = EmotionState::neutral();
            s.set(Dimension::Joy, 0.0);
            s.set(Dimension::Sadness, 80.0);
            s
        };
        let next = EmotionTransition::new(0.0).blend(&prev, &observed);
        assert_eq!(next, observed, "inertia 0 → havainto korvaa kokonaan");
    }

    #[test]
    fn full_inertia_keeps_previous() {
        let prev = {
            let mut s = EmotionState::neutral();
            s.set(Dimension::Love, 70.0);
            s
        };
        let observed = EmotionState::neutral();
        let next = EmotionTransition::new(1.0).blend(&prev, &observed);
        assert_eq!(next, prev, "inertia 1 → tila ei muutu");
    }

    #[test]
    fn half_inertia_is_midpoint() {
        let prev = {
            let mut s = EmotionState::neutral();
            s.set(Dimension::Joy, 100.0);
            s
        };
        let observed = EmotionState::neutral();
        let next = EmotionTransition::new(0.5).blend(&prev, &observed);
        assert!((next.value(Dimension::Joy) - 50.0).abs() < 1e-4);
    }

    #[test]
    fn inertia_slows_change() {
        // Higher inertia → the next state stays closer to the previous one.
        let prev = {
            let mut s = EmotionState::neutral();
            s.set(Dimension::Joy, 100.0);
            s
        };
        let observed = EmotionState::neutral();
        let slow = EmotionTransition::new(0.8).blend(&prev, &observed);
        let fast = EmotionTransition::new(0.2).blend(&prev, &observed);
        assert!(
            slow.value(Dimension::Joy) > fast.value(Dimension::Joy),
            "korkea inertia pitää tilan lähempänä edellistä"
        );
    }

    #[test]
    fn blend_result_stays_in_bounds() {
        // Even with both at the extremes, the result stays within 0..100.
        let prev = EmotionState::from_values([100.0; crate::dimension::DIMENSION_COUNT]);
        let observed = EmotionState::neutral();
        let next = EmotionTransition::balanced().blend(&prev, &observed);
        for dim in Dimension::ALL {
            let v = next.value(dim);
            assert!(
                (0.0..=100.0).contains(&v),
                "arvo {v} ulkona rajoista: {dim}"
            );
        }
    }

    #[test]
    fn repeated_application_converges_toward_observation() {
        // Repeating the same observation → the state approaches the observation (inertia <1).
        let observed = {
            let mut s = EmotionState::neutral();
            s.set(Dimension::Hope, 90.0);
            s
        };
        let mut state = EmotionState::neutral();
        let t = EmotionTransition::new(0.5);
        for _ in 0..20 {
            state = t.blend(&state, &observed);
        }
        assert!(
            (state.value(Dimension::Hope) - 90.0).abs() < 0.5,
            "toistuva havainto vetää tilan lähelle havaintoa"
        );
    }
}

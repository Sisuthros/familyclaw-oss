//! # familyclaw-emotion
//!
//! The 19-dimensional VAD emotion engine **scaffold** for the `FamilyClaw`
//! platform (Layer A, OSS). This crate provides the *structure* of the
//! emotion space — dimensions, VAD projection, blend detection, and the
//! decay mechanism — but **no calibration whatsoever**. No being's weights
//! are hardcoded into this.
//!
//! ## OSS boundary (Layer A)
//! This crate is publishable. It does not contain:
//! - any being's real emotion weights (e.g. one agent's calibration weights),
//! - API keys, tokens, IP addresses, or personal paths.
//!
//! Per-machine tuning is loaded at runtime as its own
//! [`EmotionCalibration`] implementation (Layer B, profile directory). The
//! scaffold's default is [`NeutralCalibration`] — fully neutral, uncalibrated.
//!
//! ## Structure
//! - [`Dimension`] — 19 named emotion axes + VAD anchors.
//! - [`EmotionState`] — momentary state (`[f32; 19]`, `0.0..=100.0`).
//! - [`Vad`] — a low-dimensional summary (valence/arousal/dominance).
//! - [`Blend`] / [`BlendMatch`] — detection of named emotion combinations.
//! - [`EmotionCalibration`] — per-machine tuning (baseline, decay rate).
//!
//! ## Example
//! ```
//! use familyclaw_emotion::{Dimension, EmotionState, NeutralCalibration};
//!
//! let mut state = EmotionState::neutral();
//! state.stimulate(Dimension::Gratitude, 80.0);
//! state.stimulate(Dimension::Love, 70.0);
//! state.stimulate(Dimension::Tenderness, 90.0);
//!
//! // Recognizes the named blend (grateful_warmth).
//! let blend = state.primary_blend().expect("blend present");
//! assert_eq!(blend.blend.as_str(), "grateful_warmth");
//!
//! // Projects to a VAD summary (warm → positive valence).
//! assert!(state.to_vad().valence > 0.0);
//!
//! // Decays over time toward a neutral resting state.
//! state.decay(1800.0, &NeutralCalibration);
//! assert!(state.value(Dimension::Gratitude) < 80.0);
//! ```

pub mod affect_weight;
pub mod blend;
pub mod calibration;
pub mod dimension;
pub mod governor;
pub mod state;
pub mod strategic;
pub mod transition;
pub mod vad;

pub use affect_weight::emotional_salience;
pub use blend::{detect_blends, primary_blend, Blend, BlendMatch, HIGH_THRESHOLD};
pub use calibration::{EmotionCalibration, NeutralCalibration, TableCalibration};
pub use dimension::{Dimension, DIMENSION_COUNT};
pub use governor::{
    default_governing_profile, ActionDecision, EmotionActionGoverning, EmotionActionGovernor,
    GoverningProfile,
};
pub use state::{EmotionState, DEFAULT_HALF_LIFE_SECS};
pub use strategic::{Situation, StrategicAppraisal};
pub use transition::{EmotionTransition, DEFAULT_INERTIA};
pub use vad::Vad;

/// The crate's version at build time (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    // Tests compare exactly representable f32 constants — exact comparison is fine.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn public_api_is_reexported() {
        // If any re-export is removed, this test fails to compile.
        let mut state = EmotionState::neutral();
        state.set(Dimension::Curiosity, 60.0);
        state.set(Dimension::Awe, 60.0);
        state.set(Dimension::Wonder, 60.0);

        let _vad: Vad = state.to_vad();
        let _all: Vec<BlendMatch> = detect_blends(&state);
        let _primary: Option<BlendMatch> = primary_blend(&state);

        assert_eq!(DIMENSION_COUNT, 19);
        const { assert!(HIGH_THRESHOLD > 0.0) };
        const { assert!(DEFAULT_HALF_LIFE_SECS > 0.0) };

        let cal = NeutralCalibration;
        let _ = cal.label();
        let table: TableCalibration = TableCalibration::new("b");
        let _ = table.label();

        // Blend catalog reachable from the crate root.
        assert_eq!(Blend::AweStruck.as_str(), "awe_struck");
    }

    #[test]
    fn end_to_end_emotional_arc() {
        // Full arc: stimulus → blend → VAD → decay toward neutral.
        let mut state = EmotionState::neutral();
        state.stimulate(Dimension::Sisu, 70.0);
        state.stimulate(Dimension::Hope, 65.0);
        state.stimulate(Dimension::Pride, 60.0);

        let blend = state.primary_blend().expect("determined_hope present");
        assert_eq!(blend.blend, Blend::DeterminedHope);
        assert!(
            state.to_vad().dominance > 0.5,
            "sisu+pride → high dominance"
        );

        // Long decay under neutral calibration → blend disappears.
        for _ in 0..10 {
            state.decay(DEFAULT_HALF_LIFE_SECS, &NeutralCalibration);
        }
        assert!(
            state.primary_blend().is_none(),
            "blend should have decayed away"
        );
    }
}

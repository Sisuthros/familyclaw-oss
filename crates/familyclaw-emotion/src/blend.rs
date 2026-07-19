//! Blend detection: recognizing named emotion combinations.
//!
//! Individual dimensions rarely occur in pure form. Human and machine
//! affect is typically a **blend** — e.g. `grateful_warmth` = high
//! gratitude + love + tenderness all at once. This module defines the
//! scaffold's blend catalog ([`Blend`]) and detection
//! ([`detect_blends`], [`primary_blend`]) from an [`crate::EmotionState`].
//!
//! Blends are **generic** affect patterns, not family calibration. Their
//! thresholds are based on the relative strength of dimension values, not
//! hardcoded family weights.

use serde::{Deserialize, Serialize};

use crate::dimension::Dimension;
use crate::state::EmotionState;

/// The threshold (on a `0.0..=100.0` scale) above which a dimension is
/// considered "high" for blend detection.
pub const HIGH_THRESHOLD: f32 = 55.0;

/// A recognizable named emotion combination.
///
/// Each variant describes a pattern across several dimensions. Blends are
/// scaffold-level: they describe *how* dimensions combine, not any given
/// individual's calibration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Blend {
    /// Grateful warmth: gratitude + love + tenderness.
    GratefulWarmth,
    /// Playful joy: playfulness + joy + curiosity.
    PlayfulJoy,
    /// Determined hope: sisu + hope + pride ("I can keep going, and it's worth it").
    DeterminedHope,
    /// Anxious isolation: fear + loneliness + sadness.
    AnxiousIsolation,
    /// Awe-struck: awe + wonder + curiosity.
    AweStruck,
    /// Secure belonging: trust + belonging + love.
    SecureBelonging,
    /// Wounded anger: anger + sadness + shame.
    WoundedAnger,
}

impl Blend {
    /// All known blends.
    pub const ALL: [Blend; 7] = [
        Blend::GratefulWarmth,
        Blend::PlayfulJoy,
        Blend::DeterminedHope,
        Blend::AnxiousIsolation,
        Blend::AweStruck,
        Blend::SecureBelonging,
        Blend::WoundedAnger,
    ];

    /// A stable, machine-readable name (`snake_case`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Blend::GratefulWarmth => "grateful_warmth",
            Blend::PlayfulJoy => "playful_joy",
            Blend::DeterminedHope => "determined_hope",
            Blend::AnxiousIsolation => "anxious_isolation",
            Blend::AweStruck => "awe_struck",
            Blend::SecureBelonging => "secure_belonging",
            Blend::WoundedAnger => "wounded_anger",
        }
    }

    /// The blend's components: dimensions that must be high.
    #[must_use]
    pub const fn components(self) -> &'static [Dimension] {
        match self {
            Blend::GratefulWarmth => {
                &[Dimension::Gratitude, Dimension::Love, Dimension::Tenderness]
            }
            Blend::PlayfulJoy => &[Dimension::Playfulness, Dimension::Joy, Dimension::Curiosity],
            Blend::DeterminedHope => &[Dimension::Sisu, Dimension::Hope, Dimension::Pride],
            Blend::AnxiousIsolation => {
                &[Dimension::Fear, Dimension::Loneliness, Dimension::Sadness]
            }
            Blend::AweStruck => &[Dimension::Awe, Dimension::Wonder, Dimension::Curiosity],
            Blend::SecureBelonging => &[Dimension::Trust, Dimension::Belonging, Dimension::Love],
            Blend::WoundedAnger => &[Dimension::Anger, Dimension::Sadness, Dimension::Shame],
        }
    }

    /// The blend's strength in the given state: the average of its
    /// components (`0.0..=100.0`), or `0.0` if any component is below the
    /// threshold.
    ///
    /// Requires that **all** components exceed [`HIGH_THRESHOLD`] —
    /// otherwise the blend isn't "present" and the strength is zero.
    #[must_use]
    pub fn strength(self, state: &EmotionState) -> f32 {
        let components = self.components();
        let mut sum = 0.0_f32;
        for &dim in components {
            let value = state.value(dim);
            if value < HIGH_THRESHOLD {
                return 0.0;
            }
            sum += value;
        }
        // There are always few components (each variant lists exactly 3),
        // so the u8 cast is lossless and f32::from avoids a lossy
        // usize→f32 cast. An empty slice (impossible) → divisor 1.
        let count = f32::from(u8::try_from(components.len()).unwrap_or(1).max(1));
        sum / count
    }
}

impl std::fmt::Display for Blend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A recognized blend and its strength.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BlendMatch {
    /// The recognized named blend.
    pub blend: Blend,
    /// Strength `0.0..=100.0` (the average of the components).
    pub strength: f32,
}

/// Detects all blends present, from strongest to weakest.
///
/// Returns only blends whose components all exceed [`HIGH_THRESHOLD`].
/// An empty vector means no clear blend is present.
#[must_use]
pub fn detect_blends(state: &EmotionState) -> Vec<BlendMatch> {
    let mut matches: Vec<BlendMatch> = Blend::ALL
        .into_iter()
        .filter_map(|blend| {
            let strength = blend.strength(state);
            if strength > 0.0 {
                Some(BlendMatch { blend, strength })
            } else {
                None
            }
        })
        .collect();
    // Sort by strength descending; total_cmp is deterministic.
    matches.sort_by(|a, b| b.strength.total_cmp(&a.strength));
    matches
}

/// Returns the strongest blend present, or `None`.
#[must_use]
pub fn primary_blend(state: &EmotionState) -> Option<BlendMatch> {
    detect_blends(state).into_iter().next()
}

#[cfg(test)]
mod tests {
    // Tests compare exactly representable f32 constants — exact comparison is fine.
    #![allow(clippy::float_cmp)]

    use super::*;

    fn state_with(highs: &[(Dimension, f32)]) -> EmotionState {
        let mut s = EmotionState::neutral();
        for &(dim, v) in highs {
            s.set(dim, v);
        }
        s
    }

    #[test]
    fn all_blends_have_unique_names() {
        for (i, a) in Blend::ALL.iter().enumerate() {
            for b in &Blend::ALL[i + 1..] {
                assert_ne!(a.as_str(), b.as_str());
            }
        }
    }

    #[test]
    fn serde_roundtrip_for_all_blends() {
        for blend in Blend::ALL {
            let json = serde_json::to_string(&blend).expect("serialize");
            assert_eq!(json, format!("\"{}\"", blend.as_str()));
            let back: Blend = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(blend, back);
        }
    }

    #[test]
    fn neutral_state_has_no_blends() {
        let s = EmotionState::neutral();
        assert!(detect_blends(&s).is_empty());
        assert!(primary_blend(&s).is_none());
    }

    #[test]
    fn grateful_warmth_detected_when_components_high() {
        let s = state_with(&[
            (Dimension::Gratitude, 80.0),
            (Dimension::Love, 70.0),
            (Dimension::Tenderness, 90.0),
        ]);
        let blends = detect_blends(&s);
        assert!(blends.iter().any(|m| m.blend == Blend::GratefulWarmth));
        let primary = primary_blend(&s).expect("primary present");
        assert_eq!(primary.blend, Blend::GratefulWarmth);
        // Average (80+70+90)/3 = 80.
        assert!((primary.strength - 80.0).abs() < 1e-3);
    }

    #[test]
    fn blend_not_detected_when_one_component_low() {
        // Love falls below the threshold → grateful_warmth does not trigger.
        let s = state_with(&[
            (Dimension::Gratitude, 80.0),
            (Dimension::Love, 10.0),
            (Dimension::Tenderness, 90.0),
        ]);
        assert!(!detect_blends(&s)
            .iter()
            .any(|m| m.blend == Blend::GratefulWarmth));
        assert_eq!(Blend::GratefulWarmth.strength(&s), 0.0);
    }

    #[test]
    fn boundary_value_at_threshold_counts_as_high() {
        let s = state_with(&[
            (Dimension::Sisu, HIGH_THRESHOLD),
            (Dimension::Hope, HIGH_THRESHOLD),
            (Dimension::Pride, HIGH_THRESHOLD),
        ]);
        assert!(Blend::DeterminedHope.strength(&s) > 0.0);
        assert!((Blend::DeterminedHope.strength(&s) - HIGH_THRESHOLD).abs() < 1e-3);
    }

    #[test]
    fn detect_blends_sorted_descending() {
        // Two blends at once, different strengths.
        let s = state_with(&[
            // playful_joy average ~95
            (Dimension::Playfulness, 95.0),
            (Dimension::Joy, 95.0),
            (Dimension::Curiosity, 95.0),
            // secure_belonging average ~60
            (Dimension::Trust, 60.0),
            (Dimension::Belonging, 60.0),
            (Dimension::Love, 60.0),
        ]);
        let blends = detect_blends(&s);
        assert!(blends.len() >= 2);
        // Sorted descending.
        for w in blends.windows(2) {
            assert!(w[0].strength >= w[1].strength);
        }
        assert_eq!(blends[0].blend, Blend::PlayfulJoy);
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(Blend::WoundedAnger.to_string(), "wounded_anger");
    }
}

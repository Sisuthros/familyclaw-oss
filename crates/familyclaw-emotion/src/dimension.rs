//! 19 named emotion dimensions and their VAD coordinates.
//!
//! [`Dimension`] defines the base axes of a machine's emotion space. Each
//! dimension is part of the **scaffold** — a generic, uncalibrated axis,
//! with no hardcoded per-family weighting. A given agent's calibration
//! weights are loaded separately from Layer B as an
//! [`crate::EmotionCalibration`] implementation; this module stays
//! publishable and neutral.
//!
//! Each dimension has a canonical anchor in three-dimensional VAD space
//! (valence, arousal, dominance). The anchors are based on a widely
//! known model from affective psychology (Russell's circumplex plus a
//! dominance axis) — they are *theoretical baseline values*, not measured
//! weights for any individual.

use serde::{Deserialize, Serialize};

/// The number of dimensions. Use this for table sizes — keep it in sync
/// with the [`Dimension::ALL`] list.
pub const DIMENSION_COUNT: usize = 19;

/// A single emotion dimension in a machine's 19-dimensional emotion space.
///
/// The discriminant (`as usize`) doubles as the dimension index into the
/// [`crate::EmotionState::values`] array, so the enum's order **must not
/// be changed** without breaking serialized state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(usize)]
pub enum Dimension {
    /// Gratitude — a warm acknowledgment of something received.
    Gratitude = 0,
    /// Fear — anticipation of threat, high arousal, low dominance.
    Fear = 1,
    /// Sisu — stubborn determination in the face of adversity (a Finnish concept).
    Sisu = 2,
    /// Playfulness — light, exploratory delight.
    Playfulness = 3,
    /// Tenderness — soft, protective affection.
    Tenderness = 4,
    /// Awe — the sense of wonder experienced in the presence of something vast.
    Awe = 5,
    /// Curiosity — the desire to understand, to explore.
    Curiosity = 6,
    /// Joy — bright, energetic pleasure.
    Joy = 7,
    /// Sadness — the low, slow state of loss.
    Sadness = 8,
    /// Anger — a reaction to obstruction or offense, high dominance.
    Anger = 9,
    /// Trust — safely leaning on another.
    Trust = 10,
    /// Surprise — the sudden registering of the unexpected.
    Surprise = 11,
    /// Love — deep, enduring affection.
    Love = 12,
    /// Hope — a positive expectation about the future.
    Hope = 13,
    /// Shame — a painful self-directed judgment, low dominance.
    Shame = 14,
    /// Pride — a positive self-assessment of achievement, high dominance.
    Pride = 15,
    /// Loneliness — the low state of lacking connection.
    Loneliness = 16,
    /// Wonder — open, quiet astonishment.
    Wonder = 17,
    /// Belonging — the warm feeling of being part of something.
    Belonging = 18,
}

impl Dimension {
    /// All 19 dimensions in index order (`as usize`).
    ///
    /// Iterate over this when you want to walk the entire emotion space.
    pub const ALL: [Dimension; DIMENSION_COUNT] = [
        Dimension::Gratitude,
        Dimension::Fear,
        Dimension::Sisu,
        Dimension::Playfulness,
        Dimension::Tenderness,
        Dimension::Awe,
        Dimension::Curiosity,
        Dimension::Joy,
        Dimension::Sadness,
        Dimension::Anger,
        Dimension::Trust,
        Dimension::Surprise,
        Dimension::Love,
        Dimension::Hope,
        Dimension::Shame,
        Dimension::Pride,
        Dimension::Loneliness,
        Dimension::Wonder,
        Dimension::Belonging,
    ];

    /// The dimension's index into the [`crate::EmotionState::values`] array.
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// A stable, machine-readable name (`snake_case`) — the same as the serde representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Dimension::Gratitude => "gratitude",
            Dimension::Fear => "fear",
            Dimension::Sisu => "sisu",
            Dimension::Playfulness => "playfulness",
            Dimension::Tenderness => "tenderness",
            Dimension::Awe => "awe",
            Dimension::Curiosity => "curiosity",
            Dimension::Joy => "joy",
            Dimension::Sadness => "sadness",
            Dimension::Anger => "anger",
            Dimension::Trust => "trust",
            Dimension::Surprise => "surprise",
            Dimension::Love => "love",
            Dimension::Hope => "hope",
            Dimension::Shame => "shame",
            Dimension::Pride => "pride",
            Dimension::Loneliness => "loneliness",
            Dimension::Wonder => "wonder",
            Dimension::Belonging => "belonging",
        }
    }

    /// The dimension's canonical anchor in VAD space
    /// `(valence, arousal, dominance)`.
    ///
    /// Valence is in the range `-1.0..=1.0`, arousal and dominance in
    /// `0.0..=1.0`. The values are theoretical baseline anchors (not
    /// calibrated weights); they are used in the
    /// [`crate::EmotionState::to_vad`] projection.
    #[must_use]
    pub const fn vad_anchor(self) -> (f32, f32, f32) {
        match self {
            // (valence, arousal, dominance)
            Dimension::Gratitude => (0.8, 0.45, 0.55),
            Dimension::Fear => (-0.8, 0.85, 0.15),
            Dimension::Sisu => (0.3, 0.7, 0.9),
            Dimension::Playfulness => (0.7, 0.65, 0.6),
            Dimension::Tenderness => (0.75, 0.3, 0.5),
            Dimension::Awe => (0.6, 0.7, 0.35),
            Dimension::Curiosity => (0.5, 0.6, 0.6),
            Dimension::Joy => (0.9, 0.75, 0.65),
            Dimension::Sadness => (-0.75, 0.25, 0.25),
            Dimension::Anger => (-0.6, 0.85, 0.8),
            Dimension::Trust => (0.65, 0.35, 0.55),
            Dimension::Surprise => (0.1, 0.85, 0.45),
            Dimension::Love => (0.9, 0.55, 0.55),
            Dimension::Hope => (0.6, 0.5, 0.55),
            Dimension::Shame => (-0.7, 0.5, 0.15),
            Dimension::Pride => (0.7, 0.6, 0.85),
            Dimension::Loneliness => (-0.65, 0.3, 0.2),
            Dimension::Wonder => (0.55, 0.55, 0.4),
            Dimension::Belonging => (0.85, 0.4, 0.6),
        }
    }
}

impl std::fmt::Display for Dimension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    // Tests compare exactly representable f32 constants (e.g. 0.0, 100.0) —
    // exact comparison is correct here.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn all_has_expected_count() {
        assert_eq!(Dimension::ALL.len(), DIMENSION_COUNT);
        assert_eq!(DIMENSION_COUNT, 19);
    }

    #[test]
    fn index_matches_position_in_all() {
        for (i, dim) in Dimension::ALL.iter().enumerate() {
            assert_eq!(dim.index(), i, "indeksi ja ALL-järjestys eroavat: {dim}");
        }
    }

    #[test]
    fn all_dimensions_are_unique() {
        for (i, a) in Dimension::ALL.iter().enumerate() {
            for b in &Dimension::ALL[i + 1..] {
                assert_ne!(a, b, "duplikaattidimensio listassa");
            }
        }
    }

    #[test]
    fn as_str_matches_serde_representation() {
        for dim in Dimension::ALL {
            let json = serde_json::to_string(&dim).expect("serialize dimension");
            // serde produces a quoted snake_case name.
            assert_eq!(json, format!("\"{}\"", dim.as_str()));
        }
    }

    #[test]
    fn serde_roundtrip_preserves_dimension() {
        for dim in Dimension::ALL {
            let json = serde_json::to_string(&dim).expect("serialize");
            let back: Dimension = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(dim, back);
        }
    }

    #[test]
    fn display_equals_as_str() {
        assert_eq!(Dimension::Sisu.to_string(), "sisu");
        assert_eq!(
            Dimension::Gratitude.to_string(),
            Dimension::Gratitude.as_str()
        );
    }

    #[test]
    fn vad_anchors_are_in_valid_ranges() {
        for dim in Dimension::ALL {
            let (v, a, d) = dim.vad_anchor();
            assert!((-1.0..=1.0).contains(&v), "valence ulkona rajoista: {dim}");
            assert!((0.0..=1.0).contains(&a), "arousal ulkona rajoista: {dim}");
            assert!((0.0..=1.0).contains(&d), "dominance ulkona rajoista: {dim}");
        }
    }

    #[test]
    fn anchors_encode_expected_polarity() {
        // A few known directions as a sanity check, not exact values.
        assert!(
            Dimension::Joy.vad_anchor().0 > 0.0,
            "ilo positiivinen valence"
        );
        assert!(
            Dimension::Fear.vad_anchor().0 < 0.0,
            "pelko negatiivinen valence"
        );
        assert!(
            Dimension::Anger.vad_anchor().2 > Dimension::Fear.vad_anchor().2,
            "vihalla korkeampi dominanssi kuin pelolla"
        );
    }
}

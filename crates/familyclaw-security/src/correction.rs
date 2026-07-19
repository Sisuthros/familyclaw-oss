//! Human-in-the-loop corrections — a human veto in memory retrieval.
//!
//! [`HumanCorrection`] is a guiding correction that comes directly from a
//! human (e.g. a human family member), and it has the **highest possible
//! priority** in memory retrieval ([`CorrectionPriority::MAX`] = `1.0`).
//! When retrieval produces a tie, or when an automatic memory conflicts with
//! a human correction, the human correction **always wins**.
//!
//! ## Decay: Slow, not eternal
//! Unlike an identity anchor (λ=0, [`crate::DecayLambda::ZERO`]), a human
//! correction is not eternal — it decays **slowly** ([`DecayClass::Slow`]).
//! A correction ("never do X", "Y is a mistake") is very long-lived but will
//! eventually yield if it is never reaffirmed — unlike identity, which is
//! permanent.
//!
//! ## Priority calculation
//! In retrieval, a correction's effective score is
//! `priority · retention(age)`. Because decay is slow, a human correction
//! stays at the top of retrieval for a long time, but its influence fades if
//! it is never reaffirmed.

use serde::{Deserialize, Serialize};

use familyclaw_core::time::{self, Timestamp};

use crate::anchor::DecayLambda;
use crate::error::{Result, SecurityError};

/// A memory's decay class — a named forgetting rate.
///
/// Classes map to concrete λ coefficients ([`DecayClass::lambda`]). The
/// security layer uses two of these: [`DecayClass::Eternal`] for identity
/// anchors and [`DecayClass::Slow`] for human corrections. The other classes
/// are available for the memory substrate's (familyclaw-memory) general
/// decay calculations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecayClass {
    /// Never forgotten (λ=0) — identity anchors.
    Eternal,
    /// Very slow forgetting — human corrections (the human veto right).
    Slow,
    /// Ordinary Ebbinghaus decay.
    Normal,
    /// Fast forgetting — transient, low-significance information.
    Fast,
}

impl DecayClass {
    /// Half-life in seconds for each class (except [`Self::Eternal`]).
    ///
    /// The values are *framework* defaults (Layer A) — they do not
    /// calibrate any specific being. Per-instance tuning may override these
    /// at runtime.
    const SLOW_HALF_LIFE_SECS: f64 = 60.0 * 60.0 * 24.0 * 90.0; // ~90 days
    const NORMAL_HALF_LIFE_SECS: f64 = 60.0 * 60.0 * 24.0 * 7.0; // ~7 days
    const FAST_HALF_LIFE_SECS: f64 = 60.0 * 60.0; // 1 h

    /// Returns the decay λ corresponding to the class.
    ///
    /// λ is derived from the half-life: `λ = ln(2) / half_life`.
    /// [`Self::Eternal`] gives [`DecayLambda::ZERO`].
    #[must_use]
    pub fn lambda(self) -> DecayLambda {
        let half_life = match self {
            Self::Eternal => return DecayLambda::ZERO,
            Self::Slow => Self::SLOW_HALF_LIFE_SECS,
            Self::Normal => Self::NORMAL_HALF_LIFE_SECS,
            Self::Fast => Self::FAST_HALF_LIFE_SECS,
        };
        // half_life is always a constant > 0, so new() cannot fail; but we
        // do not use unwrap/expect on the production path — derive λ directly.
        DecayLambda::new(std::f64::consts::LN_2 / half_life).unwrap_or(DecayLambda::ZERO)
    }

    /// Whether this is the eternal class (never forgotten).
    #[must_use]
    pub fn is_eternal(self) -> bool {
        matches!(self, Self::Eternal)
    }
}

/// A correction's priority in the range `0.0..=1.0`.
///
/// A human correction always uses [`CorrectionPriority::MAX`] (`1.0`), which
/// guarantees it the highest weight in retrieval. The type is a newtype so
/// that the priority stays within its bounded range and is not confused
/// with other `f64` values.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CorrectionPriority(f64);

impl CorrectionPriority {
    /// The highest priority (`1.0`) — the default for a human correction.
    pub const MAX: Self = Self(1.0);

    /// The lowest priority (`0.0`).
    pub const MIN: Self = Self(0.0);

    /// Constructs a priority. The value must be `0.0..=1.0` and finite.
    ///
    /// # Errors
    /// [`SecurityError::InvalidInput`] if the value is not finite or is
    /// outside the range `0.0..=1.0`.
    pub fn new(value: f64) -> Result<Self> {
        if !value.is_finite() {
            return Err(SecurityError::invalid_input(format!(
                "priority must be finite, got {value}"
            )));
        }
        if !(0.0..=1.0).contains(&value) {
            return Err(SecurityError::invalid_input(format!(
                "priority must be in 0.0..=1.0, got {value}"
            )));
        }
        Ok(Self(value))
    }

    /// Returns the priority as a float.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl Default for CorrectionPriority {
    /// Defaults to the highest priority — a human correction wins by default.
    fn default() -> Self {
        Self::MAX
    }
}

/// A correction issued by a human — a guiding veto in memory retrieval.
///
/// A correction carries content ([`content`](HumanCorrection::content)), a
/// high priority, and slow decay. It does not delete other memories — it
/// outweighs them in ranking.
///
/// **OSS boundary:** the type is a generic framework. The correction's
/// *content* (the human's concrete vetoes) is Layer B data, not this code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HumanCorrection {
    /// The correction's text content (an instruction, prohibition, or fix).
    pub content: String,

    /// The correction's priority in retrieval — defaults to
    /// [`CorrectionPriority::MAX`].
    pub priority: CorrectionPriority,

    /// The correction's decay class — always [`DecayClass::Slow`].
    pub decay: DecayClass,

    /// When the correction was issued (UTC).
    pub applied_at: Timestamp,
}

impl HumanCorrection {
    /// Constructs a human correction: priority `1.0`, decay `Slow`, timestamp now.
    ///
    /// # Errors
    /// [`SecurityError::InvalidInput`] if `content` is empty.
    pub fn new(content: impl Into<String>) -> Result<Self> {
        let content = content.into();
        if content.trim().is_empty() {
            return Err(SecurityError::invalid_input(
                "correction content must not be empty",
            ));
        }
        Ok(Self {
            content,
            priority: CorrectionPriority::MAX,
            decay: DecayClass::Slow,
            applied_at: time::now(),
        })
    }

    /// The effective retrieval score at age `age_secs`: `priority · retention`.
    ///
    /// This is compared against the scores of ordinary memories during
    /// retrieval. Because a human correction's priority is `1.0` and its
    /// decay is slow, its score stays above the others for a long time.
    #[must_use]
    pub fn effective_score(&self, age_secs: f64) -> f64 {
        self.priority.get() * self.decay.lambda().retention(age_secs)
    }

    /// Whether this correction beats a given competing score at age `age_secs`.
    ///
    /// Used to resolve ties: a human correction also beats an exactly equal
    /// competitor (`>=`), so that the human veto gets the edge over a mere
    /// automatic memory.
    #[must_use]
    pub fn wins_against(&self, competitor_score: f64, age_secs: f64) -> bool {
        self.effective_score(age_secs) >= competitor_score
    }
}

#[cfg(test)]
mod tests {
    // Tests compare known f64 constants (0.0, 1.0) exactly — exact
    // comparison here is intentional and correct.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn decay_class_eternal_has_zero_lambda() {
        assert!(DecayClass::Eternal.lambda().is_eternal());
        assert!(DecayClass::Eternal.is_eternal());
    }

    #[test]
    fn decay_class_ordering_of_speed() {
        // A faster class = a larger λ.
        let slow = DecayClass::Slow.lambda().get();
        let normal = DecayClass::Normal.lambda().get();
        let fast = DecayClass::Fast.lambda().get();
        assert!(slow > 0.0);
        assert!(slow < normal, "slow should decay slower than normal");
        assert!(normal < fast, "normal should decay slower than fast");
    }

    #[test]
    fn slow_decay_retains_most_after_a_day() {
        // ~90-day half-life → after one day, ~99% remains.
        let day = 60.0 * 60.0 * 24.0;
        let retention = DecayClass::Slow.lambda().retention(day);
        assert!(
            retention > 0.98,
            "slow retention after 1 day was {retention}"
        );
    }

    #[test]
    fn priority_max_is_one_min_is_zero() {
        assert_eq!(CorrectionPriority::MAX.get(), 1.0);
        assert_eq!(CorrectionPriority::MIN.get(), 0.0);
    }

    #[test]
    fn priority_default_is_max() {
        assert_eq!(CorrectionPriority::default(), CorrectionPriority::MAX);
    }

    #[test]
    fn priority_rejects_out_of_range_and_nonfinite() {
        assert!(CorrectionPriority::new(-0.01).is_err());
        assert!(CorrectionPriority::new(1.01).is_err());
        assert!(CorrectionPriority::new(f64::NAN).is_err());
        assert!(CorrectionPriority::new(0.0).is_ok());
        assert!(CorrectionPriority::new(1.0).is_ok());
        assert!(CorrectionPriority::new(0.5).is_ok());
    }

    #[test]
    fn correction_new_sets_max_priority_and_slow_decay() {
        let c = HumanCorrection::new("agent_a lives in city X, not city Y").expect("valid");
        assert_eq!(c.priority, CorrectionPriority::MAX);
        assert_eq!(c.decay, DecayClass::Slow);
        assert_eq!(c.content, "agent_a lives in city X, not city Y");
    }

    #[test]
    fn correction_new_rejects_empty_content() {
        assert!(HumanCorrection::new("   ").is_err());
        assert!(HumanCorrection::new("").is_err());
    }

    #[test]
    fn correction_effective_score_starts_at_priority() {
        let c = HumanCorrection::new("rule").expect("valid");
        // Age 0 → retention 1.0 → score = priority = 1.0.
        let score = c.effective_score(0.0);
        assert!((score - 1.0).abs() < 1e-9, "score was {score}");
    }

    #[test]
    fn correction_wins_ties_against_automatic_memory() {
        let c = HumanCorrection::new("the veto").expect("valid");
        // A fresh correction (score 1.0) beats an exactly equal competitor.
        assert!(c.wins_against(1.0, 0.0));
        // And anything smaller.
        assert!(c.wins_against(0.9, 0.0));
        // But not a competitor that is genuinely larger (e.g. another veto).
        assert!(!c.wins_against(1.0001, 0.0));
    }

    #[test]
    fn decay_class_lambda_matches_half_life_definition() {
        // λ = ln(2)/half_life → after the half-life, retention ≈ 0.5 for each class.
        let cases = [
            (DecayClass::Slow, 60.0 * 60.0 * 24.0 * 90.0),
            (DecayClass::Normal, 60.0 * 60.0 * 24.0 * 7.0),
            (DecayClass::Fast, 60.0 * 60.0),
        ];
        for (class, half_life) in cases {
            let r = class.lambda().retention(half_life);
            assert!(
                (r - 0.5).abs() < 1e-9,
                "{class:?} retention at half-life was {r}, expected 0.5"
            );
        }
        // Eternal never fades.
        assert_eq!(DecayClass::Eternal.lambda().retention(1.0e9), 1.0);
    }

    #[test]
    fn effective_score_negative_age_treated_as_fresh() {
        // Negative age → retention is clamped to zero → score = priority.
        let c = HumanCorrection::new("veto").expect("valid");
        let score = c.effective_score(-100.0);
        assert!((score - 1.0).abs() < 1e-9, "score was {score}");
    }

    #[test]
    fn effective_score_scales_with_priority() {
        // A lower priority → a directly proportional lower fresh score.
        let mut half = HumanCorrection::new("veto").expect("valid");
        half.priority = CorrectionPriority::new(0.5).expect("valid");
        assert!((half.effective_score(0.0) - 0.5).abs() < 1e-9);

        let mut zero = HumanCorrection::new("veto").expect("valid");
        zero.priority = CorrectionPriority::MIN;
        assert_eq!(zero.effective_score(0.0), 0.0);
        // Zero priority does not strictly beat even a zero competitor
        // (only an exactly equal one, since wins_against uses >=).
        assert!(zero.wins_against(0.0, 0.0));
        assert!(!zero.wins_against(0.0001, 0.0));
    }

    #[test]
    fn wins_against_boundary_uses_greater_or_equal() {
        // The exact tie boundary: equal → wins; a hair larger → loses.
        let c = HumanCorrection::new("veto").expect("valid");
        let score = c.effective_score(0.0);
        assert!(c.wins_against(score, 0.0), "equal score must win (>=)");
        assert!(
            !c.wins_against(score + f64::EPSILON, 0.0),
            "strictly larger competitor must win"
        );
    }

    #[test]
    fn correction_stores_only_generic_content_field() {
        // The content is generic text; the serialized form contains it
        // as-is (the framework doesn't hide anything), but no API keys etc.
        let c = HumanCorrection::new("agent_a prefers concise answers").expect("valid");
        let json = serde_json::to_string(&c).expect("serialize");
        assert!(json.contains("agent_a prefers concise answers"));
        assert!(json.contains("slow")); // decay class snake_case
    }

    #[test]
    fn correction_score_decays_slowly_but_monotonically() {
        let c = HumanCorrection::new("rule").expect("valid");
        let month = 60.0 * 60.0 * 24.0 * 30.0;
        let year = 60.0 * 60.0 * 24.0 * 365.0;

        let fresh = c.effective_score(0.0);
        let aged_month = c.effective_score(month);
        let aged_year = c.effective_score(year);

        // Monotonically decreasing, but never vanishes entirely.
        assert!(aged_month < fresh, "month score should be below fresh");
        assert!(aged_year < aged_month, "year score should be below month");
        assert!(aged_year > 0.0, "should not vanish entirely in a year");

        // ~90-day half-life: after a month, still at the top of retrieval
        // against an ordinary memory (the veto is long-lived).
        assert!(aged_month > 0.7, "month retention was {aged_month}");
        assert!(c.wins_against(0.7, month));

        // After a year the veto has faded noticeably (~4 half-lives) —
        // identity would have remained (λ=0), but a correction is allowed to yield.
        assert!(aged_year < 0.2, "year retention was {aged_year}");
    }

    #[test]
    fn correction_serde_roundtrip() {
        let c = HumanCorrection::new("important veto").expect("valid");
        let json = serde_json::to_string(&c).expect("serialize");
        let back: HumanCorrection = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(c, back);
    }

    #[test]
    fn decay_class_serde_roundtrip() {
        for class in [
            DecayClass::Eternal,
            DecayClass::Slow,
            DecayClass::Normal,
            DecayClass::Fast,
        ] {
            let json = serde_json::to_string(&class).expect("serialize");
            let back: DecayClass = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(class, back);
        }
        // snake_case form.
        assert_eq!(
            serde_json::to_string(&DecayClass::Slow).expect("ser"),
            "\"slow\""
        );
    }
}

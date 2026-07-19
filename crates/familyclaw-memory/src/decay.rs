//! Memory decay: [`DecayPolicy`] and Ebbinghaus-based retention.
//!
//! Memories do not fade uniformly. Eternal Thread models forgetting using
//! the Ebbinghaus forgetting curve — exponential retention whose rate is
//! controlled by the [`DecayPolicy`] chosen for a memory. Identity anchors
//! (`ProtectedCore`, λ = 0.0) never decay; everyday
//! observations (`Fast`) fade quickly.
//!
//! ## Ebbinghaus model
//! Retention at time `t` (elapsed time in seconds):
//!
//! ```text
//! R(t) = e^(-λ · t / S)
//! ```
//!
//! - `λ` (lambda) = the policy's decay constant (`decay_lambda`),
//! - `S` = the memory's **stability**, which grows with importance and
//!   reinforcement (stronger memories persist longer),
//! - `R(t)` ∈ `0.0..=1.0` — remaining retention (1.0 = fully fresh).
//!
//! The policy λ values are taken from the `FamilyClaw` v2 design (§2.3, §5):
//! `ProtectedCore = 0.0`, `Slow = 0.02`, `Normal = 0.18`, `Fast = 0.5`.

use serde::{Deserialize, Serialize};

/// Unit scale for the stability parameter (`S`), in seconds.
///
/// A stability of `S = 1.0` corresponds to roughly a one-day time scale: at
/// this value, retention follows the policy's λ purely on a daily basis.
/// Higher stability stretches the memory over a longer time span.
const STABILITY_TIME_SCALE_SECS: f32 = 86_400.0;

/// Minimum allowed stability, to avoid division by zero in the retention
/// formula.
const MIN_STABILITY: f32 = 0.05;

/// How quickly a memory decays along the Ebbinghaus curve.
///
/// Each variant carries a fixed λ decay constant (`decay_lambda`).
/// A smaller λ means slower forgetting. `ProtectedCore` (λ = 0.0) never
/// decays — it is an identity anchor (design §2: `ProtectedCore`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecayPolicy {
    /// Core of identity — never decays (λ = 0.0).
    ///
    /// Used for memories that form the entity's identity
    /// (e.g. name, family, a core value). These anchors remain fresh
    /// indefinitely.
    ProtectedCore,
    /// Slow decay (λ = 0.02) — meaningful, durable memory.
    Slow,
    /// Ordinary decay (λ = 0.18) — the Ebbinghaus baseline value.
    Normal,
    /// Fast decay (λ = 0.5) — transient, everyday observation.
    Fast,
}

impl DecayPolicy {
    /// All policies from slowest to fastest decay.
    pub const ALL: [DecayPolicy; 4] = [
        DecayPolicy::ProtectedCore,
        DecayPolicy::Slow,
        DecayPolicy::Normal,
        DecayPolicy::Fast,
    ];

    /// The policy's Ebbinghaus decay constant `λ`.
    ///
    /// `0.0` means "never decays" ([`DecayPolicy::ProtectedCore`]).
    #[must_use]
    pub const fn decay_lambda(self) -> f32 {
        match self {
            DecayPolicy::ProtectedCore => 0.0,
            DecayPolicy::Slow => 0.02,
            DecayPolicy::Normal => 0.18,
            DecayPolicy::Fast => 0.5,
        }
    }

    /// Is this a protected identity anchor (never decays)?
    #[must_use]
    pub const fn is_protected(self) -> bool {
        matches!(self, DecayPolicy::ProtectedCore)
    }

    /// Stable, machine-readable name (`snake_case`) — same as the serde
    /// representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DecayPolicy::ProtectedCore => "protected_core",
            DecayPolicy::Slow => "slow",
            DecayPolicy::Normal => "normal",
            DecayPolicy::Fast => "fast",
        }
    }

    /// Ebbinghaus retention after elapsed time `dt_secs`, at the given
    /// memory `stability`.
    ///
    /// Returns a value in `0.0..=1.0`: `1.0` = fully fresh,
    /// approaching `0.0` as the memory is forgotten. `ProtectedCore` always
    /// returns `1.0`. A negative or non-finite `dt_secs` is treated as zero
    /// (fresh); a non-positive stability is clamped to a safe minimum.
    ///
    /// Formula: `R = e^(-λ · t / (S · TIME_SCALE))`.
    #[must_use]
    pub fn retention(self, dt_secs: f32, stability: f32) -> f32 {
        let lambda = self.decay_lambda();
        // A protected core or zero λ never decays.
        if lambda <= 0.0 {
            return 1.0;
        }
        // Invalid/negative time delta = memory is still fresh.
        let dt = if dt_secs.is_finite() && dt_secs > 0.0 {
            dt_secs
        } else {
            0.0
        };
        // Stability is clamped to a safe minimum to avoid division by zero.
        let s = if stability.is_finite() && stability > MIN_STABILITY {
            stability
        } else {
            MIN_STABILITY
        };
        let exponent = -lambda * dt / (s * STABILITY_TIME_SCALE_SECS);
        let r = exponent.exp();
        // Numerical safeguard to keep within bounds.
        r.clamp(0.0, 1.0)
    }
}

impl std::fmt::Display for DecayPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Default for DecayPolicy {
    /// The default policy is [`DecayPolicy::Normal`] — the Ebbinghaus baseline value.
    fn default() -> Self {
        DecayPolicy::Normal
    }
}

#[cfg(test)]
mod tests {
    // Tests compare exactly representable f32 constants — exact comparison is fine.
    #![allow(clippy::float_cmp)]

    use super::*;

    #[test]
    fn lambda_values_match_design() {
        assert_eq!(DecayPolicy::ProtectedCore.decay_lambda(), 0.0);
        assert_eq!(DecayPolicy::Slow.decay_lambda(), 0.02);
        assert_eq!(DecayPolicy::Normal.decay_lambda(), 0.18);
        assert_eq!(DecayPolicy::Fast.decay_lambda(), 0.5);
    }

    #[test]
    fn protected_core_never_decays() {
        let p = DecayPolicy::ProtectedCore;
        assert!(p.is_protected());
        // Arbitrarily long time, small stability → retention still full.
        assert_eq!(p.retention(0.0, 1.0), 1.0);
        assert_eq!(p.retention(1e9, 0.01), 1.0);
        assert_eq!(p.retention(f32::MAX, 0.0), 1.0);
    }

    #[test]
    fn non_protected_are_not_protected() {
        assert!(!DecayPolicy::Slow.is_protected());
        assert!(!DecayPolicy::Normal.is_protected());
        assert!(!DecayPolicy::Fast.is_protected());
    }

    #[test]
    fn fresh_memory_has_full_retention() {
        for p in DecayPolicy::ALL {
            assert!(
                (p.retention(0.0, 1.0) - 1.0).abs() < 1e-6,
                "{p} not fresh at zero"
            );
        }
    }

    #[test]
    fn retention_decreases_over_time() {
        let p = DecayPolicy::Normal;
        let r_day = p.retention(STABILITY_TIME_SCALE_SECS, 1.0);
        let r_week = p.retention(STABILITY_TIME_SCALE_SECS * 7.0, 1.0);
        assert!(r_day < 1.0);
        assert!(
            r_week < r_day,
            "week retention {r_week} is not below day retention {r_day}"
        );
        assert!(r_week > 0.0);
    }

    #[test]
    fn faster_policy_decays_faster() {
        let t = STABILITY_TIME_SCALE_SECS * 3.0;
        let slow = DecayPolicy::Slow.retention(t, 1.0);
        let normal = DecayPolicy::Normal.retention(t, 1.0);
        let fast = DecayPolicy::Fast.retention(t, 1.0);
        assert!(fast < normal, "fast {fast} is not below normal {normal}");
        assert!(normal < slow, "normal {normal} is not below slow {slow}");
    }

    #[test]
    fn higher_stability_retains_longer() {
        let p = DecayPolicy::Normal;
        let t = STABILITY_TIME_SCALE_SECS * 5.0;
        let weak = p.retention(t, 1.0);
        let strong = p.retention(t, 4.0);
        assert!(
            strong > weak,
            "strong memory {strong} does not persist longer than weak {weak}"
        );
    }

    #[test]
    fn retention_known_value_at_one_unit() {
        // Normal λ=0.18, dt = 1 day, S = 1.0 → R = e^(-0.18) ≈ 0.8353.
        let r = DecayPolicy::Normal.retention(STABILITY_TIME_SCALE_SECS, 1.0);
        let expected = (-0.18_f32).exp();
        assert!(
            (r - expected).abs() < 1e-4,
            "retention {r} does not match expected {expected}"
        );
    }

    #[test]
    fn invalid_dt_is_treated_as_fresh() {
        let p = DecayPolicy::Fast;
        assert_eq!(p.retention(-100.0, 1.0), 1.0);
        assert_eq!(p.retention(f32::NAN, 1.0), 1.0);
        assert_eq!(p.retention(f32::INFINITY, 1.0), 1.0);
    }

    #[test]
    fn nonpositive_stability_does_not_divide_by_zero() {
        let p = DecayPolicy::Normal;
        let r = p.retention(STABILITY_TIME_SCALE_SECS, 0.0);
        assert!(r.is_finite());
        assert!((0.0..=1.0).contains(&r));
        let r_neg = p.retention(STABILITY_TIME_SCALE_SECS, -5.0);
        assert!(r_neg.is_finite());
    }

    #[test]
    fn retention_stays_in_unit_range() {
        for p in DecayPolicy::ALL {
            for &t in &[0.0, 1.0, 1e3, 1e6, 1e9] {
                for &s in &[0.1, 1.0, 10.0] {
                    let r = p.retention(t, s);
                    assert!(
                        (0.0..=1.0).contains(&r),
                        "{p} t={t} s={s} → r={r} out of bounds"
                    );
                }
            }
        }
    }

    #[test]
    fn display_and_as_str_match_serde() {
        for p in DecayPolicy::ALL {
            assert_eq!(p.to_string(), p.as_str());
            let json = serde_json::to_string(&p).expect("serialize policy");
            assert_eq!(json, format!("\"{}\"", p.as_str()));
            let back: DecayPolicy = serde_json::from_str(&json).expect("deserialize policy");
            assert_eq!(back, p);
        }
    }

    #[test]
    fn default_is_normal() {
        assert_eq!(DecayPolicy::default(), DecayPolicy::Normal);
    }
}

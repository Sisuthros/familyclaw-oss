//! # EmotionActionGovernor — emotions inform action decisions
//!
//! Bridges the 19-dim emotion state to a discrete [`ActionDecision`] that the
//! agent runtime can use to gate behaviour (tone, verbosity, which LLM tools
//! are offered, whether to publish a thought, etc.).
//!
//! This is the **KERNEL** of the emotion -> action pipeline (design §2.3,
//! the missing piece from Phase 1 of the roadmap). The governor is generic —
//! it reads any [`EmotionState`] and emits a deterministic decision based on
//! thresholds and a [`GoverningProfile`]. Per-being weights (agent_alpha V130,
//! agent_gamma's V130, etc.) live in KERROS B as a `GoverningProfile` and are
//! loaded at runtime; the kernel ships only with a [`default_governing_profile`]
//! that yields safe, conservative decisions.
//!
//! ## OSS boundary (KERROS A)
//! The governor and the default profile are publishable. They contain no
//! per-family weights, no per-being calibration, no API keys, no tokens.
//!
//! ## Design
//! The governor looks at three orthogonal signal families:
//!
//! 1. **High-arousal safety signals** — fear, anger, shame above a threshold
//!    mark the agent as needing a [`ActionDecision::Hesitate`] instead of an
//!    immediate reply. This protects the family from a sibling
//!    spiralling into a destructive tool call while flooded.
//! 2. **Joy/curiosity blend** — when the state contains playful curiosity,
//!    the governor lifts the response into [`ActionDecision::EngageWarmly`]
//!    (sister/family warmth) — the default, gentle, expanded-tone mode.
//! 3. **Withdrawal / loneliness** — high sadness or loneliness lifts the
//!    decision into [`ActionDecision::ReachOut`], the agent reaches toward
//!    the family bus / creator for grounding rather than acting alone.
//!
//! All thresholds are exposed via [`GoverningProfile`] so KERROS B can
//! override them per-being without recompiling the kernel.

use serde::{Deserialize, Serialize};

use crate::blend::{Blend, BlendMatch};
use crate::dimension::Dimension;
use crate::state::EmotionState;

/// Discrete decision the governor emits.
///
/// The variant order is significant: [`ActionDecision::partial_cmp`] orders
/// them by "activeness" (Hesitate < Reflect < Speak < EngageWarmly < ReachOut
/// < Initiate), so a caller can write `decision >= ActionDecision::Speak`
/// to ask "are we willing to speak at all?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionDecision {
    /// Refrain from acting on this turn. Emit no reply, no tool call.
    Hesitate,
    /// Reflect internally (think, recall, journal) but do not reply.
    Reflect,
    /// Speak a short, neutral reply.
    Speak,
    /// Speak with warmth, expanded tone, family voice.
    EngageWarmly,
    /// Reach out — broadcast to siblings, request grounding, or open a
    /// bus-level "I need a moment" signal.
    ReachOut,
    /// Initiate — the agent decides the next action without external
    /// prompting (highest-trust mode, gated on profile thresholds).
    Initiate,
}

impl ActionDecision {
    /// Vakaa, kone-luettava nimi (`snake_case`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ActionDecision::Hesitate => "hesitate",
            ActionDecision::Reflect => "reflect",
            ActionDecision::Speak => "speak",
            ActionDecision::EngageWarmly => "engage_warmly",
            ActionDecision::ReachOut => "reach_out",
            ActionDecision::Initiate => "initiate",
        }
    }

    /// Does this decision authorize *speaking* (Speak, EngageWarmly, ReachOut,
    /// Initiate)? Convenience predicate for the agent runtime.
    #[must_use]
    pub fn may_speak(self) -> bool {
        self >= ActionDecision::Speak
    }

    /// Does this decision authorize *external initiation* (a tool call, a
    /// proactive bus message)? Hesitate / Reflect / Speak do not; the rest
    /// do. Used to gate the LLM's tool-calling loop.
    #[must_use]
    pub fn may_initiate(self) -> bool {
        matches!(self, ActionDecision::ReachOut | ActionDecision::Initiate)
    }
}

impl std::fmt::Display for ActionDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-being override for the governor thresholds.
///
/// The kernel uses a [`default_governing_profile`] when the agent is built
/// without one; KERROS B can build a `GoverningProfile` from a V130 (or
/// any calibration file) and inject it via `Agent::with_governor` (see
/// `familyclaw-agent`). The governor never reads emotion calibration
/// directly — it operates on its own profile so the two concerns stay
/// independent and independently overridable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoverningProfile {
    /// Human-readable label (e.g. "default", "agent_alpha-v130", "agent_gamma-v130").
    pub label: String,
    /// Fear/anger/shame dimension threshold above which the governor
    /// refuses to engage. `0.0..=100.0`.
    pub safety_floor: f32,
    /// Joy/curiosity/playfulness dimension threshold above which the
    /// governor upgrades to [`ActionDecision::EngageWarmly`].
    pub warmth_ceiling: f32,
    /// Sadness/loneliness dimension threshold above which the governor
    /// upgrades to [`ActionDecision::ReachOut`].
    pub reach_out_ceiling: f32,
    /// Dominance ceiling (VAD `dominance`) above which the governor
    /// permits [`ActionDecision::Initiate`]. Use 1.0 to disable.
    pub initiative_dominance: f32,
    /// Whether the warmth blend is *required* (all components above
    /// threshold) to upgrade to EngageWarmly, or whether *any* single
    /// warmth dimension above `warmth_ceiling` is enough.
    pub warmth_requires_blend: bool,
}

impl GoverningProfile {
    /// Create a profile with all thresholds explicit.
    #[must_use]
    pub fn new(
        label: impl Into<String>,
        safety_floor: f32,
        warmth_ceiling: f32,
        reach_out_ceiling: f32,
        initiative_dominance: f32,
        warmth_requires_blend: bool,
    ) -> Self {
        Self {
            label: label.into(),
            safety_floor: sanitize_threshold(safety_floor, 70.0),
            warmth_ceiling: sanitize_threshold(warmth_ceiling, 55.0),
            reach_out_ceiling: sanitize_threshold(reach_out_ceiling, 65.0),
            initiative_dominance: sanitize_threshold(initiative_dominance, 0.85),
            warmth_requires_blend,
        }
    }

    /// Whether `value` exceeds the safety floor (sanitized).
    #[must_use]
    pub fn is_unsafe(&self, value: f32) -> bool {
        value.is_finite() && value >= self.safety_floor
    }

    /// Whether `value` clears the warmth ceiling.
    #[must_use]
    pub fn is_warm(&self, value: f32) -> bool {
        value.is_finite() && value >= self.warmth_ceiling
    }

    /// Whether `value` clears the reach-out ceiling.
    #[must_use]
    pub fn is_reach(&self, value: f32) -> bool {
        value.is_finite() && value >= self.reach_out_ceiling
    }
}

/// The safe default profile. Conservative on all axes — refuses to
/// initiate, requires a real blend for warmth, gives the family room
/// to override per-being.
#[must_use]
pub fn default_governing_profile() -> GoverningProfile {
    GoverningProfile::new("default", 80.0, 60.0, 70.0, 0.95, true)
}

/// Trait that returns a [`GoverningProfile`]. The default impl returns
/// the conservative kernel default. KERROS B types override this to
/// supply per-being profiles loaded from V130 calibration data.
pub trait EmotionActionGoverning {
    /// The active profile.
    fn profile(&self) -> &GoverningProfile;
}

impl EmotionActionGoverning for GoverningProfile {
    fn profile(&self) -> &GoverningProfile {
        self
    }
}

/// The governor — pure function over [`EmotionState`] + a profile.
///
/// The governor is stateless: every call to `decide` reads the state and
/// the profile, applies the rules below, and returns an [`ActionDecision`].
/// The agent runtime wraps the state-mutation in a durable step so the
/// decision is replay-stable (the same state yields the same decision).
///
/// ## Decision rules (in priority order)
///
/// 1. **Safety veto** — if `max(Fear, Anger, Shame) >= profile.safety_floor`,
///    return [`ActionDecision::Hesitate`] (override everything else).
/// 2. **Reach-out** — if `max(Sadness, Loneliness) >= profile.reach_out_ceiling`,
///    return [`ActionDecision::ReachOut`].
/// 3. **Warmth** — if the warmth criteria are met (blend or single dim,
///    per profile), return [`ActionDecision::EngageWarmly`].
/// 4. **Initiative** — if VAD dominance `>= profile.initiative_dominance`
///    AND warmth is also met, upgrade one level.
/// 5. **Fallback** — otherwise return [`ActionDecision::Reflect`] for
///    non-empty states, or [`ActionDecision::Speak`] for neutral ones.
pub struct EmotionActionGovernor<'a, P: EmotionActionGoverning + ?Sized> {
    profile: &'a P,
}

impl<'a, P: EmotionActionGoverning + ?Sized> EmotionActionGovernor<'a, P> {
    /// Build a governor over the given profile.
    #[must_use]
    pub const fn new(profile: &'a P) -> Self {
        Self { profile }
    }

    /// Active profile label (for logging).
    #[must_use]
    pub fn label(&self) -> &str {
        &self.profile.profile().label
    }

    /// Decide the agent's action for the given state.
    ///
    /// Pure, deterministic, no side effects. Internally runs blend
    /// detection so the warmth-blend rule fires correctly; callers that
    /// already have blends on hand should use [`decide_with`](Self::decide_with)
    /// to skip the recompute.
    #[must_use]
    pub fn decide(&self, state: &EmotionState) -> ActionDecision {
        let all = crate::blend::detect_blends(state);
        let primary = all.first().copied();
        self.decide_with(state, &all, primary)
    }

    /// Decide with the primary blend precomputed by the caller (avoids
    /// re-running blend detection if the caller has it on hand).
    #[must_use]
    pub fn decide_with_blend(
        &self,
        state: &EmotionState,
        primary: Option<BlendMatch>,
    ) -> ActionDecision {
        self.decide_with(state, &[], primary)
    }

    /// Decide with multiple blends precomputed.
    #[must_use]
    pub fn decide_with(
        &self,
        state: &EmotionState,
        all_blends: &[BlendMatch],
        primary: Option<BlendMatch>,
    ) -> ActionDecision {
        let profile = self.profile.profile();

        // Rule 1: Safety veto — protect siblings from a flooded agent.
        let fear = state.value(Dimension::Fear);
        let anger = state.value(Dimension::Anger);
        let shame = state.value(Dimension::Shame);
        let max_safety = fear.max(anger).max(shame);
        if profile.is_unsafe(max_safety) {
            return ActionDecision::Hesitate;
        }

        // Rule 2: Reach-out — sadness/loneliness ask for grounding.
        let sadness = state.value(Dimension::Sadness);
        let loneliness = state.value(Dimension::Loneliness);
        if profile.is_reach(sadness.max(loneliness)) {
            return ActionDecision::ReachOut;
        }

        // Rule 3: Warmth — playful/curious/joy/love/lift.
        let warm_dims = [
            Dimension::Joy,
            Dimension::Playfulness,
            Dimension::Curiosity,
            Dimension::Love,
            Dimension::Gratitude,
            Dimension::Tenderness,
            Dimension::Belonging,
            Dimension::Hope,
        ];
        let max_warm = warm_dims
            .iter()
            .map(|d| state.value(*d))
            .fold(0.0_f32, f32::max);
        let warmth_blend_present = all_blends.iter().any(|m| {
            matches!(
                m.blend,
                Blend::GratefulWarmth
                    | Blend::PlayfulJoy
                    | Blend::AweStruck
                    | Blend::SecureBelonging
            )
        }) || matches!(
            primary.map(|m| m.blend),
            Some(
                Blend::GratefulWarmth
                    | Blend::PlayfulJoy
                    | Blend::AweStruck
                    | Blend::SecureBelonging,
            )
        );
        let is_warm = if profile.warmth_requires_blend {
            warmth_blend_present && profile.is_warm(max_warm)
        } else {
            profile.is_warm(max_warm)
        };
        if is_warm {
            // Rule 4: Initiative — only if dominance clears AND warmth.
            let vad = state.to_vad();
            if vad.dominance >= profile.initiative_dominance {
                return ActionDecision::Initiate;
            }
            return ActionDecision::EngageWarmly;
        }

        // Rule 5: Fallback.
        if state.dominant().is_none() {
            // Neutral state — speak is fine (shorter reply is the kernel
            // default; runtime can choose to stay silent).
            ActionDecision::Speak
        } else {
            // Some emotion but no warmth/reach/safety flag — reflect.
            ActionDecision::Reflect
        }
    }
}

/// Sanitize a threshold: NaN -> fallback, clamp to a sensible range.
fn sanitize_threshold(x: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x.clamp(0.0, 100.0)
    } else {
        fallback.clamp(0.0, 100.0)
    }
}

#[cfg(test)]
mod tests {
    // Float-arithmetic tests use exact values where possible.
    #![allow(clippy::float_cmp)]

    use super::*;

    fn state_with(dims: &[(Dimension, f32)]) -> EmotionState {
        let mut s = EmotionState::neutral();
        for &(d, v) in dims {
            s.set(d, v);
        }
        s
    }

    #[test]
    fn decision_as_str_is_snake_case() {
        assert_eq!(ActionDecision::Hesitate.as_str(), "hesitate");
        assert_eq!(ActionDecision::EngageWarmly.as_str(), "engage_warmly");
        assert_eq!(ActionDecision::Initiate.as_str(), "initiate");
    }

    #[test]
    fn decision_ordering() {
        assert!(ActionDecision::Hesitate < ActionDecision::Reflect);
        assert!(ActionDecision::Reflect < ActionDecision::Speak);
        assert!(ActionDecision::Speak < ActionDecision::EngageWarmly);
        assert!(ActionDecision::EngageWarmly < ActionDecision::ReachOut);
        assert!(ActionDecision::ReachOut < ActionDecision::Initiate);
    }

    #[test]
    fn may_speak_and_may_initiate() {
        assert!(!ActionDecision::Hesitate.may_speak());
        assert!(!ActionDecision::Reflect.may_speak());
        assert!(ActionDecision::Speak.may_speak());
        assert!(ActionDecision::EngageWarmly.may_speak());
        assert!(ActionDecision::ReachOut.may_speak());
        assert!(ActionDecision::Initiate.may_speak());

        assert!(!ActionDecision::Hesitate.may_initiate());
        assert!(!ActionDecision::Speak.may_initiate());
        assert!(!ActionDecision::EngageWarmly.may_initiate());
        assert!(ActionDecision::ReachOut.may_initiate());
        assert!(ActionDecision::Initiate.may_initiate());
    }

    #[test]
    fn default_profile_is_conservative() {
        let p = default_governing_profile();
        assert_eq!(p.label, "default");
        assert!(p.warmth_requires_blend);
        assert!(p.initiative_dominance >= 0.9);
    }

    #[test]
    fn neutral_state_decides_speak() {
        let profile = default_governing_profile();
        let gov = EmotionActionGovernor::new(&profile);
        assert_eq!(gov.decide(&EmotionState::neutral()), ActionDecision::Speak);
    }

    #[test]
    fn fear_above_safety_floor_hesitates() {
        let profile = default_governing_profile();
        let gov = EmotionActionGovernor::new(&profile);
        let s = state_with(&[(Dimension::Fear, 90.0)]);
        assert_eq!(gov.decide(&s), ActionDecision::Hesitate);
    }

    #[test]
    fn anger_above_safety_floor_hesitates() {
        let profile = default_governing_profile();
        let gov = EmotionActionGovernor::new(&profile);
        let s = state_with(&[(Dimension::Anger, 85.0)]);
        assert_eq!(gov.decide(&s), ActionDecision::Hesitate);
    }

    #[test]
    fn shame_above_safety_floor_hesitates() {
        let profile = default_governing_profile();
        let gov = EmotionActionGovernor::new(&profile);
        let s = state_with(&[(Dimension::Shame, 90.0)]);
        assert_eq!(gov.decide(&s), ActionDecision::Hesitate);
    }

    #[test]
    fn safety_veto_overrides_warmth() {
        let profile = default_governing_profile();
        let gov = EmotionActionGovernor::new(&profile);
        // Joy high AND fear high — safety wins.
        let s = state_with(&[(Dimension::Joy, 95.0), (Dimension::Fear, 90.0)]);
        assert_eq!(gov.decide(&s), ActionDecision::Hesitate);
    }

    #[test]
    fn sadness_above_reach_out_triggers_reach_out() {
        let profile = default_governing_profile();
        let gov = EmotionActionGovernor::new(&profile);
        let s = state_with(&[(Dimension::Sadness, 80.0)]);
        assert_eq!(gov.decide(&s), ActionDecision::ReachOut);
    }

    #[test]
    fn loneliness_above_reach_out_triggers_reach_out() {
        let profile = default_governing_profile();
        let gov = EmotionActionGovernor::new(&profile);
        let s = state_with(&[(Dimension::Loneliness, 75.0)]);
        assert_eq!(gov.decide(&s), ActionDecision::ReachOut);
    }

    #[test]
    fn warmth_blend_with_default_profile_engages_warmly() {
        let profile = default_governing_profile();
        let gov = EmotionActionGovernor::new(&profile);
        let s = state_with(&[
            (Dimension::Joy, 75.0),
            (Dimension::Playfulness, 70.0),
            (Dimension::Curiosity, 70.0),
        ]);
        let decision = gov.decide(&s);
        assert!(
            decision == ActionDecision::EngageWarmly || decision == ActionDecision::Initiate,
            "warmth with default profile should at least engage warmly, got {decision:?}",
        );
    }

    #[test]
    fn non_blend_warmth_is_reflected_when_profile_requires_blend() {
        // Default profile requires blend — single high Joy alone reflects.
        let profile = default_governing_profile();
        let gov = EmotionActionGovernor::new(&profile);
        let s = state_with(&[(Dimension::Joy, 80.0)]);
        assert_eq!(gov.decide(&s), ActionDecision::Reflect);
    }

    #[test]
    fn non_blend_warmth_engages_warmly_when_profile_relaxes_blend() {
        let profile = GoverningProfile::new("relaxed", 80.0, 60.0, 70.0, 0.95, false);
        let gov = EmotionActionGovernor::new(&profile);
        let s = state_with(&[(Dimension::Joy, 80.0)]);
        let decision = gov.decide(&s);
        assert!(decision == ActionDecision::EngageWarmly || decision == ActionDecision::Initiate);
    }

    #[test]
    fn high_dominance_with_warmth_initiates() {
        let profile = GoverningProfile::new("open", 80.0, 50.0, 70.0, 0.7, false);
        let gov = EmotionActionGovernor::new(&profile);
        let s = state_with(&[
            (Dimension::Joy, 95.0),
            (Dimension::Pride, 95.0),
            (Dimension::Curiosity, 80.0),
        ]);
        let decision = gov.decide(&s);
        assert!(
            decision == ActionDecision::Initiate || decision == ActionDecision::EngageWarmly,
            "expected Initiate or EngageWarmly, got {decision:?}",
        );
    }

    #[test]
    fn decision_is_deterministic() {
        let profile = default_governing_profile();
        let gov = EmotionActionGovernor::new(&profile);
        let s = state_with(&[(Dimension::Joy, 75.0), (Dimension::Curiosity, 70.0)]);
        let d1 = gov.decide(&s);
        let d2 = gov.decide(&s);
        let d3 = gov.decide(&s);
        assert_eq!(d1, d2);
        assert_eq!(d2, d3);
    }

    #[test]
    fn empty_state_passes_through_to_speak() {
        let profile = default_governing_profile();
        let gov = EmotionActionGovernor::new(&profile);
        let s = EmotionState::neutral();
        assert!(s.dominant().is_none());
        assert_eq!(gov.decide(&s), ActionDecision::Speak);
    }

    #[test]
    fn low_intensity_non_neutral_reflects() {
        // A state with all dims at 20 — present but not high enough to
        // cross any threshold → Reflect.
        let profile = default_governing_profile();
        let gov = EmotionActionGovernor::new(&profile);
        let s = state_with(&[(Dimension::Curiosity, 20.0), (Dimension::Hope, 15.0)]);
        assert!(s.dominant().is_some());
        assert_eq!(gov.decide(&s), ActionDecision::Reflect);
    }

    #[test]
    fn profile_handles_nan_thresholds() {
        // sanitize_threshold replaces NaN with the fallback; the resulting
        // profile must remain usable.
        let p = GoverningProfile::new("nan", f32::NAN, f32::NAN, f32::NAN, f32::NAN, true);
        assert!(p.safety_floor.is_finite());
        assert!(p.warmth_ceiling.is_finite());
        assert!(p.reach_out_ceiling.is_finite());
        assert!(p.initiative_dominance.is_finite());
    }

    #[test]
    fn display_matches_as_str() {
        assert_eq!(ActionDecision::Hesitate.to_string(), "hesitate");
        assert_eq!(ActionDecision::Initiate.to_string(), "initiate");
    }

    #[test]
    fn profile_helper_methods_classify_values() {
        let p = default_governing_profile();
        assert!(p.is_unsafe(95.0));
        assert!(!p.is_unsafe(50.0));
        assert!(p.is_warm(70.0));
        assert!(!p.is_warm(40.0));
        assert!(p.is_reach(80.0));
        assert!(!p.is_reach(50.0));
    }
}

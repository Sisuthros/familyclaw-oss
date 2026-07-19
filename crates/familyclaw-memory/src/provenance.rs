//! Memory **provenance** ([`Provenance`]) and poisoning protection ([`ProvenanceGate`]).
//!
//! Eternal Thread's memory is an attack surface: *Sleeper Memory Poisoning*
//! (arXiv 2605.15338) reports a 99.8% injection success rate when memory has
//! no provenance information, and *`MemPoison`* (arXiv 2605.29960) bypasses
//! selective memory. The Eywa principle responds: **"evidence before belief"**
//! — immutable sources ([`Provenance::DirectExperience`]) → derived facts
//! ([`Provenance::Derived`]) → external, trust-weighted claims
//! ([`Provenance::External`]).
//!
//! [`ProvenanceGate`] is the gatekeeper: it rejects a low-trust external
//! source before it can enter memory. Direct experience and derived memories
//! always pass through — only external claims are weighed against the
//! trust threshold.
//!
//! ## Example
//! ```
//! use familyclaw_memory::{Provenance, ProvenanceGate};
//!
//! let gate = ProvenanceGate::new(0.6);
//!
//! // Direct experience always passes.
//! assert!(gate.admit(&Provenance::DirectExperience));
//!
//! // A trusted external source passes.
//! assert!(gate.admit(&Provenance::external("web", 0.9)));
//!
//! // A low-trust external source is rejected (poisoning protection).
//! assert!(!gate.admit(&Provenance::external("web", 0.1)));
//! ```

use familyclaw_core::MessageId;
use serde::{Deserialize, Serialize};

/// A memory's provenance — where this information comes from and how
/// trustworthy it is.
///
/// Provenance orders memories into a trust hierarchy:
/// 1. [`DirectExperience`](Provenance::DirectExperience) — the entity's own
///    observation (highest trust, not weighed).
/// 2. [`Derived`](Provenance::Derived) — derived from existing memories
///    (e.g. reflection, synthesis); inherits the trustworthiness of its
///    sources.
/// 3. [`External`](Provenance::External) — an external source (e.g. `"web"`,
///    `"tool"`) with explicit trust `0.0..=1.0` — weighed by
///    [`ProvenanceGate`] before entering memory.
///
/// The default is [`DirectExperience`](Provenance::DirectExperience): old
/// memories persisted before provenance tracking existed are interpreted as
/// direct experience (a backward-compatible serde default in
/// [`Memory`](crate::Memory)).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Provenance {
    /// The entity's own direct observation. Highest trust — never rejected.
    DirectExperience,

    /// Derived from existing memories (reflection, synthesis).
    ///
    /// `from` references the source memories' identifiers, so the
    /// derivation chain remains auditable (Eywa: derived facts point back
    /// to their sources).
    Derived {
        /// Identifiers of the source memories this was derived from.
        from: Vec<MessageId>,
    },

    /// An external source with explicit trust.
    ///
    /// `source` is a generic source identifier (e.g. `"web"`, `"tool"`,
    /// `"doc"`); `trust` is the `0.0..=1.0` trust weighed by
    /// [`ProvenanceGate`]. Low `trust` = possible
    /// poisoning → rejected at the gate.
    External {
        /// Generic source identifier (e.g. `"web"`).
        source: String,
        /// Trust in the source, `0.0..=1.0`.
        trust: f32,
    },
}

impl Provenance {
    /// Constructs an [`External`](Provenance::External) provenance; `trust`
    /// is clamped to `0.0..=1.0` (non-finite values → `0.0`).
    #[must_use]
    pub fn external(source: impl Into<String>, trust: f32) -> Self {
        Self::External {
            source: source.into(),
            trust: clamp_trust(trust),
        }
    }

    /// Constructs a [`Derived`](Provenance::Derived) provenance from the
    /// given source identifiers.
    #[must_use]
    pub fn derived(from: impl IntoIterator<Item = MessageId>) -> Self {
        Self::Derived {
            from: from.into_iter().collect(),
        }
    }

    /// The provenance's effective trust factor, `0.0..=1.0`,
    /// for retrieval weighting.
    ///
    /// - [`DirectExperience`](Provenance::DirectExperience) → `1.0`
    ///   (own observation, full trust).
    /// - [`Derived`](Provenance::Derived) → `1.0` (derived from already
    ///   accepted memories; the sources were weighed at
    ///   write time).
    /// - [`External`](Provenance::External) → `trust` (weighed,
    ///   `0.0..=1.0`).
    #[must_use]
    pub fn trust(&self) -> f32 {
        match self {
            Self::DirectExperience | Self::Derived { .. } => 1.0,
            Self::External { trust, .. } => clamp_trust(*trust),
        }
    }

    /// Is the provenance external (i.e. must the gate weigh it)?
    #[must_use]
    pub const fn is_external(&self) -> bool {
        matches!(self, Self::External { .. })
    }
}

impl Default for Provenance {
    /// The default is direct experience — old memories without provenance
    /// information are interpreted as a trusted own observation.
    fn default() -> Self {
        Self::DirectExperience
    }
}

/// Clamps trust to `0.0..=1.0`; non-finite values (NaN, ±∞) → `0.0`
/// (safe default: unknown trust = no trust).
fn clamp_trust(trust: f32) -> f32 {
    if trust.is_finite() {
        trust.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Provenance gate: poisoning protection that rejects low-trust external
/// sources before they can enter memory.
///
/// Direct experience and derived memories **always** pass through (their
/// trust is `1.0`). Only [`Provenance::External`] is weighed: if its `trust`
/// falls below [`min_trust`](ProvenanceGate::min_trust),
/// [`admit`](ProvenanceGate::admit) returns `false` and the caller must
/// reject the memory (Sleeper Memory Poisoning protection).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceGate {
    /// Minimum acceptable trust for an external source, `0.0..=1.0`.
    min_trust: f32,
}

impl ProvenanceGate {
    /// Creates a gate with the given trust threshold; `min_trust` is
    /// clamped to `0.0..=1.0` (non-finite values → `0.0` = admit everything).
    #[must_use]
    pub fn new(min_trust: f32) -> Self {
        Self {
            min_trust: clamp_trust(min_trust),
        }
    }

    /// The gate's trust threshold (`0.0..=1.0`).
    #[must_use]
    pub const fn min_trust(&self) -> f32 {
        self.min_trust
    }

    /// Is the given provenance admitted into memory?
    ///
    /// - Direct experience and derived memories → always `true`.
    /// - External source → `true` only if `trust >= min_trust`.
    ///
    /// A low-trust external claim is rejected (`false`) — this is
    /// poisoning protection: an untrusted "fact" injected by an attacker
    /// cannot enter memory to contaminate later retrieval.
    #[must_use]
    pub fn admit(&self, provenance: &Provenance) -> bool {
        match provenance {
            Provenance::DirectExperience | Provenance::Derived { .. } => true,
            Provenance::External { trust, .. } => clamp_trust(*trust) >= self.min_trust,
        }
    }
}

impl Default for ProvenanceGate {
    /// A moderate default threshold (`0.5`): an external source needs at
    /// least medium trust to enter memory.
    fn default() -> Self {
        Self::new(0.5)
    }
}

#[cfg(test)]
mod tests {
    // Some tests compare exact f32 constants — exact comparison is fine here.
    #![allow(clippy::float_cmp)]

    use super::*;
    use familyclaw_core::MessageId;

    #[test]
    fn gate_admits_direct_experience() {
        let gate = ProvenanceGate::new(0.9);
        // Direct experience passes even when the threshold is high.
        assert!(gate.admit(&Provenance::DirectExperience));
    }

    #[test]
    fn gate_admits_derived_chain() {
        let gate = ProvenanceGate::new(0.99);
        let sources = vec![MessageId::new(), MessageId::new()];
        let derived = Provenance::derived(sources.clone());
        // Derived always passes; the source chain remains auditable.
        assert!(gate.admit(&derived));
        match derived {
            Provenance::Derived { from } => assert_eq!(from, sources),
            other => panic!("expected Derived, got {other:?}"),
        }
    }

    #[test]
    fn gate_rejects_low_trust_external() {
        let gate = ProvenanceGate::new(0.6);
        // A low-trust external source is rejected (poisoning protection).
        assert!(!gate.admit(&Provenance::external("web", 0.1)));
    }

    #[test]
    fn gate_admits_high_trust_external() {
        let gate = ProvenanceGate::new(0.6);
        // A sufficiently trusted external source passes.
        assert!(gate.admit(&Provenance::external("web", 0.9)));
    }

    #[test]
    fn gate_boundary_is_inclusive() {
        let gate = ProvenanceGate::new(0.5);
        // Exactly at the threshold → admitted (>=).
        assert!(gate.admit(&Provenance::external("tool", 0.5)));
        // Just below → rejected.
        assert!(!gate.admit(&Provenance::external("tool", 0.4999)));
    }

    #[test]
    fn external_trust_is_clamped() {
        // Above the bound clamps to 1.0, below to 0.0.
        assert_eq!(Provenance::external("web", 5.0).trust(), 1.0);
        assert_eq!(Provenance::external("web", -2.0).trust(), 0.0);
        // NaN → 0.0 (safe default).
        assert_eq!(Provenance::external("web", f32::NAN).trust(), 0.0);
    }

    #[test]
    fn trust_levels_per_variant() {
        assert_eq!(Provenance::DirectExperience.trust(), 1.0);
        assert_eq!(Provenance::derived([MessageId::new()]).trust(), 1.0);
        assert_eq!(Provenance::external("web", 0.3).trust(), 0.3);
    }

    #[test]
    fn default_is_direct_experience() {
        assert_eq!(Provenance::default(), Provenance::DirectExperience);
        assert!(!Provenance::default().is_external());
    }

    #[test]
    fn gate_default_threshold() {
        let gate = ProvenanceGate::default();
        assert_eq!(gate.min_trust(), 0.5);
    }

    #[test]
    fn gate_min_trust_clamped() {
        assert_eq!(ProvenanceGate::new(2.0).min_trust(), 1.0);
        assert_eq!(ProvenanceGate::new(-1.0).min_trust(), 0.0);
        // Non-finite threshold → 0.0 (admit everything).
        assert_eq!(ProvenanceGate::new(f32::INFINITY).min_trust(), 0.0);
    }

    #[test]
    fn is_external_detects_variant() {
        assert!(Provenance::external("web", 0.5).is_external());
        assert!(!Provenance::DirectExperience.is_external());
        assert!(!Provenance::derived([]).is_external());
    }

    #[test]
    fn serde_roundtrip_all_variants() {
        let cases = vec![
            Provenance::DirectExperience,
            Provenance::derived([MessageId::new(), MessageId::new()]),
            Provenance::external("web", 0.42),
        ];
        for p in cases {
            let json = serde_json::to_string(&p).expect("serialize");
            let back: Provenance = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(p, back);
        }
    }

    #[test]
    fn external_serde_uses_generic_source() {
        // Layer B: the source is generic, no family names.
        let p = Provenance::external("web", 0.8);
        let json = serde_json::to_string(&p).expect("serialize");
        assert!(json.contains("external"));
        assert!(json.contains("web"));
    }
}

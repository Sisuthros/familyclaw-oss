//! # familyclaw-security
//!
//! **Identity integrity and human veto for the `FamilyClaw` platform**
//! (Layer A, OSS).
//!
//! This crate is responsible for two security mechanisms:
//!
//! 1. **Identity anchors** ([`IdentityAnchor`]) — protected, non-forgettable
//!    memories ([`DecayLambda::ZERO`], λ=0) that carry a being's identity.
//! 2. **Human corrections** ([`HumanCorrection`]) — a human veto, highest
//!    priority in retrieval, slow decay ([`DecayClass::Slow`]).
//!
//! ## Core design decision: identity IS in memory, NOT in a hash
//!
//! A being's identity **is not** in a SHA-256 digest of the SOUL content.
//! It is in the substrate of protected memories that the being never
//! forgets (anchor memories, λ=0). The digest ([`AnchorHash`]) is **only a
//! tamper alarm**: it signals that anchored content has changed since
//! anchoring ([`IdentityStatus::Tampered`]), but it does not *carry*
//! identity.
//!
//! Consequence: when tampering is detected, the system does not lose
//! identity or touch the substrate — it raises an alarm and leaves the
//! anchor memories intact. **The substrate is the truth; the hash is the
//! sentry.** (An answer to the original research prompt's question of
//! "can identity be reduced to a SHA-256".)
//!
//! The persistence mechanism for an identity anchor is decay-λ = 0
//! (`e^(-0·t) = 1` at every moment). The same λ derivation also covers the
//! slow decay of a human correction ([`DecayClass::lambda`]). The concrete
//! anchored memories are stored in the `familyclaw-memory` substrate; this
//! crate defines the integrity and priority semantics that the memory
//! layer uses.
//!
//! ## OSS boundary (Layer A)
//! This crate is publishable. It does not contain family members' souls,
//! the real content of human corrections, API keys, tokens, IP addresses,
//! or personal paths. An anchor stores only a *digest* of the content and
//! a reference to the memory — the content itself stays in the Layer B
//! profile.
//!
//! ## Example
//! ```
//! use familyclaw_security::{IdentityAnchor, HumanCorrection, DecayClass};
//!
//! # fn main() -> familyclaw_security::Result<()> {
//! // Anchor the being's core (content is hashed into a digest, not stored).
//! let soul = "I am agent_a. I value honesty. I protect my family.";
//! let anchor = IdentityAnchor::new("mem-soul-1", soul)?;
//! assert!(anchor.invariants_hold()); // protected + decay λ=0
//!
//! // Intact as long as the content is unchanged.
//! assert!(anchor.verify(soul).is_intact());
//! // Changed content → tamper alarm (but identity is NOT lost).
//! assert!(anchor.verify("I serve only myself.").is_tampered());
//!
//! // A human veto wins retrieval ties and decays slowly.
//! let veto = HumanCorrection::new("agent_a lives in city X, not city Y")?;
//! assert_eq!(veto.decay, DecayClass::Slow);
//! assert!(veto.wins_against(1.0, 0.0)); // wins against an equal competitor
//! # Ok(())
//! # }
//! ```

pub mod anchor;
pub mod correction;
pub mod error;

pub use anchor::{verify_identity, AnchorHash, DecayLambda, IdentityAnchor, IdentityStatus};
pub use correction::{CorrectionPriority, DecayClass, HumanCorrection};
pub use error::{Result, SecurityError};

/// The crate's version at build time (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::ids::AgentId;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn public_api_is_reexported() {
        // If any re-export is removed, this test stops compiling.
        let anchor: IdentityAnchor = IdentityAnchor::new("mem-1", "soul").expect("valid anchor");
        let status: IdentityStatus = anchor.verify("soul");
        assert!(status.is_intact());

        let hash: &AnchorHash = &anchor.anchor_hash;
        assert_eq!(hash.as_hex().len(), AnchorHash::HEX_LEN);
        let lambda: DecayLambda = anchor.decay;
        assert!(lambda.is_eternal());
        assert!(DecayLambda::ZERO.is_eternal());

        let anchors = [anchor];
        let tampered = verify_identity(AgentId::new(), &anchors, |_| Some("changed".to_string()));
        assert_eq!(tampered.len(), 1);

        let veto: HumanCorrection = HumanCorrection::new("rule").expect("valid");
        let prio: CorrectionPriority = veto.priority;
        assert_eq!(prio, CorrectionPriority::MAX);
        assert_eq!(veto.decay, DecayClass::Slow);

        let err: SecurityError = SecurityError::invalid_input("x");
        assert!(err.to_string().contains('x'));
        let ok: Result<()> = Ok(());
        assert!(ok.is_ok());
    }

    #[test]
    fn end_to_end_identity_and_veto() {
        // Full arc: anchor → verify → tampering → human veto wins.
        let soul = "I am agent_a. My values are stable.";
        let anchor = IdentityAnchor::new("mem-soul", soul).expect("anchor");

        // 1. Intact initial state.
        assert!(anchor.verify(soul).is_intact());
        assert!(anchor.decay.is_eternal());

        // 2. The soul is changed externally → alarm, but the anchor remains.
        let before = anchor.clone();
        let status = anchor.verify("I am compromised.");
        assert!(status.is_tampered());
        assert_eq!(anchor, before, "the substrate remains untouched");

        // 3. A human corrects it → the veto outweighs the automatic memory for a long time.
        let veto = HumanCorrection::new("agent_a's value set is unchanged").expect("veto");
        let one_month = 60.0 * 60.0 * 24.0 * 30.0;
        assert!(veto.wins_against(0.7, one_month));
    }
}

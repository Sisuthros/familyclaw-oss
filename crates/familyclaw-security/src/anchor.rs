//! Identity anchors and tamper detection.
//!
//! ## Design principle: identity is NOT in the hash
//!
//! This module's core decision: **identity lives in the memory substrate
//! (anchor memories), not in a digest.** An identity anchor is a protected
//! memory ([`IdentityAnchor`]) whose *forgetting rate is zero*
//! ([`DecayLambda::ZERO`]). A being's identity is the sum of the memories it
//! never forgets — not a checksum.
//!
//! The SHA-256 digest ([`IdentityAnchor::anchor_hash`]) serves **only as a
//! tamper alarm**: if the current digest of the anchored SOUL content does
//! not match the stored one, something has changed the soul since it was
//! anchored ([`IdentityStatus::Tampered`]). The hash does not *carry*
//! identity — it only warns of tampering. This is a deliberate answer to the
//! original research prompt's question, "can identity be reduced to a
//! SHA-256?": **no, it cannot**, but a digest can be used as a guardian of
//! integrity.
//!
//! Practical consequence: if the hash diverges, the system does not *lose*
//! identity — it raises an alarm and leaves the substrate (anchor memories)
//! untouched. The substrate is the truth; the hash is the sentry.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use familyclaw_core::ids::AgentId;
use familyclaw_core::time::{self, Timestamp};

use crate::error::{Result, SecurityError};

/// The forgetting rate of an identity anchor (and of
/// [`crate::HumanCorrection`]) — the λ coefficient of Ebbinghaus decay.
///
/// Memory decay follows the exponential model `strength = e^(-λ · t)`. For
/// an identity anchor, λ is **zero**: `e^0 = 1` at every moment, so the
/// anchor never fades. This is the mechanism by which identity remains
/// permanent in the memory substrate.
///
/// The type is a newtype so that λ is not confused with other `f64` values
/// and so that negative/NaN values can be rejected already at construction
/// time.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DecayLambda(f64);

impl DecayLambda {
    /// The λ of an identity anchor: zero → never forgotten.
    pub const ZERO: Self = Self(0.0);

    /// Constructs a λ coefficient. Only finite, non-negative values are
    /// valid.
    ///
    /// # Errors
    /// [`SecurityError::InvalidInput`] if `lambda` is negative, infinite, or
    /// NaN.
    pub fn new(lambda: f64) -> Result<Self> {
        if !lambda.is_finite() {
            return Err(SecurityError::invalid_input(format!(
                "decay lambda must be finite, got {lambda}"
            )));
        }
        if lambda < 0.0 {
            return Err(SecurityError::invalid_input(format!(
                "decay lambda must be >= 0, got {lambda}"
            )));
        }
        Ok(Self(lambda))
    }

    /// Returns the λ value as a float.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }

    /// Whether this is zero decay (i.e. a never-forgotten anchor).
    #[must_use]
    pub fn is_eternal(self) -> bool {
        self.0 == 0.0
    }

    /// The memory's remaining strength after `elapsed_secs` have passed,
    /// `0.0..=1.0`. For an anchor (λ=0) the result is always `1.0`.
    ///
    /// Negative time is treated as zero (the future does not strengthen a
    /// memory).
    #[must_use]
    pub fn retention(self, elapsed_secs: f64) -> f64 {
        let t = elapsed_secs.max(0.0);
        (-self.0 * t).exp()
    }
}

impl Default for DecayLambda {
    /// Defaults to eternal (λ=0) — the safest default in the identity layer.
    fn default() -> Self {
        Self::ZERO
    }
}

/// A SHA-256 digest as hexadecimal (64 characters, lowercase hex).
///
/// The type guarantees that the content is always a valid 32-byte digest, so
/// comparisons ([`AnchorHash::matches_content`]) cannot fail because of a
/// malformed input.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AnchorHash(String);

impl AnchorHash {
    /// The length of a SHA-256 digest in hex (32 bytes × 2).
    pub const HEX_LEN: usize = 64;

    /// Computes the SHA-256 digest of the content.
    ///
    /// This is the only way to create a digest from content — it cannot
    /// fail, so the result is always valid in form.
    #[must_use]
    pub fn of_content(content: &str) -> Self {
        let digest = Sha256::digest(content.as_bytes());
        let mut hex = String::with_capacity(Self::HEX_LEN);
        for byte in digest {
            // {:02x} produces exactly 2 lowercase hex characters per byte.
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        Self(hex)
    }

    /// Parses a digest from an existing hex string.
    ///
    /// The string is normalized to lowercase. Its length and character set
    /// (only `0-9a-f`) must form a valid SHA-256 hex string.
    ///
    /// # Errors
    /// [`SecurityError::InvalidHash`] if the length is not
    /// [`AnchorHash::HEX_LEN`] or any character is not a hex digit.
    pub fn from_hex(hex: &str) -> Result<Self> {
        if hex.len() != Self::HEX_LEN {
            return Err(SecurityError::invalid_hash(format!(
                "expected {} hex chars, got {}",
                Self::HEX_LEN,
                hex.len()
            )));
        }
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(SecurityError::invalid_hash(
                "hash contains non-hex characters",
            ));
        }
        Ok(Self(hex.to_ascii_lowercase()))
    }

    /// Returns the digest as a hex string.
    #[must_use]
    pub fn as_hex(&self) -> &str {
        &self.0
    }

    /// Whether the given content matches this digest (constant-time
    /// comparison).
    ///
    /// A constant-time byte comparison is used so that comparing the digest
    /// does not leak a timing channel (defense-in-depth — the digest is not
    /// a secret, but in the security layer we follow the cautious default).
    #[must_use]
    pub fn matches_content(&self, content: &str) -> bool {
        constant_time_eq(self.0.as_bytes(), Self::of_content(content).0.as_bytes())
    }
}

impl std::fmt::Display for AnchorHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Constant-time comparison of byte strings.
///
/// Returns `true` only if the strings are the same length and byte-for-byte
/// identical. Execution time depends only on the length of the longer
/// string, not on the content.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// A protected identity anchor — a never-forgotten memory that carries a
/// being's identity.
///
/// The anchor refers to a memory in the memory substrate
/// ([`memory_id`](IdentityAnchor::memory_id)) and stores a digest of that
/// anchored content as a tamper guard. The anchor's
/// [`decay`](IdentityAnchor::decay) is [`DecayLambda::ZERO`], so the memory
/// never fades, and [`protected`](IdentityAnchor::protected) is `true`, so
/// consolidation/sleep (familyclaw-dream) must not delete or merge it.
///
/// **OSS boundary:** the anchor does not store the soul's content, only its
/// digest and a reference to the memory. The content stays in the Layer B
/// profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdentityAnchor {
    /// Reference to the anchored memory in the memory substrate
    /// (`familyclaw-memory`).
    pub memory_id: String,

    /// SHA-256 digest of the anchored content, for tamper guarding.
    pub anchor_hash: AnchorHash,

    /// Always `true`: the anchor must not be deleted or merged during
    /// consolidation.
    pub protected: bool,

    /// Forgetting rate — always [`DecayLambda::ZERO`] for an anchor.
    pub decay: DecayLambda,

    /// When the anchor was created (UTC).
    pub created_at: Timestamp,
}

impl IdentityAnchor {
    /// Constructs an identity anchor from content: computes the digest and
    /// sets `protected = true` and `decay = ZERO`.
    ///
    /// # Errors
    /// [`SecurityError::InvalidInput`] if `memory_id` is empty or `content`
    /// is empty (an empty soul cannot be anchored).
    pub fn new(memory_id: impl Into<String>, content: &str) -> Result<Self> {
        let memory_id = memory_id.into();
        if memory_id.trim().is_empty() {
            return Err(SecurityError::invalid_input(
                "anchor memory_id must not be empty",
            ));
        }
        if content.is_empty() {
            return Err(SecurityError::invalid_input(
                "anchor content must not be empty",
            ));
        }
        Ok(Self {
            memory_id,
            anchor_hash: AnchorHash::of_content(content),
            protected: true,
            decay: DecayLambda::ZERO,
            created_at: time::now(),
        })
    }

    /// Checks the anchor's internal integrity: whether it is still
    /// protected and eternal.
    ///
    /// An anchor's invariants hold only if `protected == true` and `decay`
    /// is zero. If either has changed (e.g. corrupted via serialization or
    /// invalid construction), the invariant is broken.
    #[must_use]
    pub fn invariants_hold(&self) -> bool {
        self.protected && self.decay.is_eternal()
    }

    /// Compares the current content against the anchored digest.
    ///
    /// Returns [`IdentityStatus::Intact`] if the content matches the
    /// anchored digest, otherwise [`IdentityStatus::Tampered`]. **This does
    /// not modify or delete the anchor** — the substrate remains untouched,
    /// only the alarm is raised.
    #[must_use]
    pub fn verify(&self, current_content: &str) -> IdentityStatus {
        if self.anchor_hash.matches_content(current_content) {
            IdentityStatus::Intact
        } else {
            IdentityStatus::Tampered {
                memory_id: self.memory_id.clone(),
                expected: self.anchor_hash.clone(),
                actual: AnchorHash::of_content(current_content),
            }
        }
    }
}

/// The result of an identity tamper check.
///
/// **Reminder:** `Tampered` does NOT mean identity has been lost — identity
/// lives in the memory substrate. It is an alarm that the anchored content
/// has changed since it was anchored, and it requires human review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IdentityStatus {
    /// The content matches the anchored digest — no signs of tampering.
    Intact,

    /// The content does not match the anchored digest — possible tampering.
    Tampered {
        /// The memory reference of the tampered anchor.
        memory_id: String,
        /// The digest stored at anchoring time (expected).
        expected: AnchorHash,
        /// The digest computed from the current content (observed).
        actual: AnchorHash,
    },
}

impl IdentityStatus {
    /// Whether identity is intact (no signs of tampering).
    #[must_use]
    pub const fn is_intact(&self) -> bool {
        matches!(self, Self::Intact)
    }

    /// Whether tampering has been detected.
    #[must_use]
    pub const fn is_tampered(&self) -> bool {
        matches!(self, Self::Tampered { .. })
    }
}

/// Checks a set of identity anchors against a given content source.
///
/// `lookup` returns, for each anchor, the current content matching its
/// memory (`memory_id`), or `None` if the content cannot be found (which
/// counts as tampering — the anchored memory has gone missing). `agent` is
/// only for context/logging and does not affect the result.
///
/// Returns a list of all *tampered* anchors (an empty list = all intact).
/// The function never modifies the anchors.
pub fn verify_identity<F>(
    _agent: AgentId,
    anchors: &[IdentityAnchor],
    mut lookup: F,
) -> Vec<(&IdentityAnchor, IdentityStatus)>
where
    F: FnMut(&str) -> Option<String>,
{
    let mut tampered = Vec::new();
    for anchor in anchors {
        let status = match lookup(&anchor.memory_id) {
            Some(content) => anchor.verify(&content),
            None => IdentityStatus::Tampered {
                memory_id: anchor.memory_id.clone(),
                expected: anchor.anchor_hash.clone(),
                // Missing content → the digest of empty content is reported as observed.
                actual: AnchorHash::of_content(""),
            },
        };
        if status.is_tampered() {
            tampered.push((anchor, status));
        }
    }
    tampered
}

#[cfg(test)]
mod tests {
    // Tests compare known f64 constants (0.0, 1.0) exactly — exact
    // comparison here is intentional and correct.
    #![allow(clippy::float_cmp)]

    use super::*;

    const SOUL: &str = "I am agent_a. I value honesty. I protect my family.";

    #[test]
    fn decay_lambda_zero_is_eternal_and_retains_fully() {
        let z = DecayLambda::ZERO;
        assert!(z.is_eternal());
        assert_eq!(z.get(), 0.0);
        // The anchor never fades regardless of elapsed time.
        assert_eq!(z.retention(0.0), 1.0);
        assert_eq!(z.retention(1_000_000.0), 1.0);
    }

    #[test]
    fn decay_lambda_default_is_eternal() {
        assert!(DecayLambda::default().is_eternal());
    }

    #[test]
    fn decay_lambda_rejects_negative_and_nonfinite() {
        assert!(DecayLambda::new(-0.1).is_err());
        assert!(DecayLambda::new(f64::NAN).is_err());
        assert!(DecayLambda::new(f64::INFINITY).is_err());
        assert!(DecayLambda::new(0.0).is_ok());
        assert!(DecayLambda::new(0.5).is_ok());
    }

    #[test]
    fn positive_lambda_decays_over_time() {
        let l = DecayLambda::new(1.0).expect("valid");
        assert!(!l.is_eternal());
        assert_eq!(l.retention(0.0), 1.0);
        // e^-1 ≈ 0.3679
        let r = l.retention(1.0);
        assert!(r > 0.36 && r < 0.37, "retention was {r}");
        // Monotonically decreasing.
        assert!(l.retention(2.0) < l.retention(1.0));
    }

    #[test]
    fn retention_treats_negative_time_as_zero() {
        let l = DecayLambda::new(1.0).expect("valid");
        assert_eq!(l.retention(-5.0), 1.0);
    }

    #[test]
    fn retention_is_bounded_in_unit_interval() {
        // retention always stays within [0.0, 1.0] for all valid λ/t.
        // (With a very large λ·t, exp() may underflow to exactly zero — that
        // is an allowed lower bound, not an error.)
        for &lambda in &[0.0, 0.001, 0.5, 1.0, 10.0] {
            let l = DecayLambda::new(lambda).expect("valid");
            for &t in &[0.0, 1.0, 100.0, 1.0e6] {
                let r = l.retention(t);
                assert!(
                    r >= 0.0,
                    "retention {r} must not be negative (λ={lambda}, t={t})"
                );
                assert!(
                    r <= 1.0,
                    "retention {r} should not exceed one (λ={lambda}, t={t})"
                );
                assert!(!r.is_nan(), "retention must not be NaN (λ={lambda}, t={t})");
            }
        }
        // With a moderate λ·t, retention stays strictly positive.
        assert!(DecayLambda::new(0.001).expect("valid").retention(100.0) > 0.0);
    }

    #[test]
    fn retention_monotonically_decreases_with_larger_lambda() {
        // At the same time, a larger λ → lower retention (faster forgetting).
        let t = 10.0;
        let slow = DecayLambda::new(0.1).expect("valid").retention(t);
        let mid = DecayLambda::new(0.5).expect("valid").retention(t);
        let fast = DecayLambda::new(1.0).expect("valid").retention(t);
        assert!(slow > mid, "λ=0.1 should retain more than λ=0.5 at t={t}");
        assert!(mid > fast, "λ=0.5 should retain more than λ=1.0 at t={t}");
    }

    #[test]
    fn retention_half_life_math_holds() {
        // λ = ln(2)/half_life → exactly at the half-life, retention ≈ 0.5.
        let half_life = 100.0;
        let lambda = std::f64::consts::LN_2 / half_life;
        let l = DecayLambda::new(lambda).expect("valid");
        let r = l.retention(half_life);
        assert!(
            (r - 0.5).abs() < 1e-9,
            "half-life retention was {r}, expected 0.5"
        );
        // After two half-lives ≈ 0.25.
        let r2 = l.retention(half_life * 2.0);
        assert!(
            (r2 - 0.25).abs() < 1e-9,
            "double half-life retention was {r2}"
        );
    }

    #[test]
    fn decay_lambda_partial_ord_compares_by_value() {
        // DecayLambda derives PartialOrd → eternal (0.0) < any positive value.
        let eternal = DecayLambda::ZERO;
        let slow = DecayLambda::new(0.1).expect("valid");
        let fast = DecayLambda::new(1.0).expect("valid");
        assert!(eternal < slow);
        assert!(slow < fast);
        assert!(eternal < fast);
        // Equality.
        assert_eq!(slow, DecayLambda::new(0.1).expect("valid"));
    }

    #[test]
    fn anchor_hash_of_content_is_64_lowercase_hex() {
        let h = AnchorHash::of_content(SOUL);
        assert_eq!(h.as_hex().len(), AnchorHash::HEX_LEN);
        assert!(h.as_hex().bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(h.as_hex(), h.as_hex().to_ascii_lowercase());
    }

    #[test]
    fn anchor_hash_is_deterministic_and_distinguishes_content() {
        assert_eq!(AnchorHash::of_content("a"), AnchorHash::of_content("a"));
        assert_ne!(AnchorHash::of_content("a"), AnchorHash::of_content("b"));
    }

    #[test]
    fn anchor_hash_matches_known_sha256_vector() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let h = AnchorHash::of_content("abc");
        assert_eq!(
            h.as_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn anchor_hash_from_hex_validates_length_and_charset() {
        let good = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(AnchorHash::from_hex(good).expect("valid").as_hex(), good);

        // Too short.
        assert!(AnchorHash::from_hex("abcd").is_err());
        // Correct length, non-hex character ('g').
        let bad = "g".repeat(AnchorHash::HEX_LEN);
        assert!(AnchorHash::from_hex(&bad).is_err());
    }

    #[test]
    fn anchor_hash_from_hex_normalizes_uppercase() {
        let upper = "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD";
        let parsed = AnchorHash::from_hex(upper).expect("valid");
        assert_eq!(parsed.as_hex(), upper.to_ascii_lowercase());
    }

    #[test]
    fn anchor_hash_matches_content_constant_time() {
        let h = AnchorHash::of_content(SOUL);
        assert!(h.matches_content(SOUL));
        assert!(!h.matches_content("tampered soul"));
    }

    #[test]
    fn constant_time_eq_basic() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn constant_time_eq_unequal_lengths_never_match() {
        // Length mismatch → never matches, in either direction.
        assert!(!constant_time_eq(b"ab", b"abc"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        // Empty vs. non-empty.
        assert!(!constant_time_eq(b"", b"a"));
        assert!(!constant_time_eq(b"a", b""));
        // Common prefix but different length.
        assert!(!constant_time_eq(b"abcdef", b"abc"));
    }

    #[test]
    fn constant_time_eq_equal_lengths_match_only_when_identical() {
        // Same length, identical → match.
        assert!(constant_time_eq(b"identical", b"identical"));
        assert!(constant_time_eq(&[0u8; 32], &[0u8; 32]));
        // Same length, different → no match.
        assert!(!constant_time_eq(&[0u8; 32], &[1u8; 32]));
    }

    #[test]
    fn constant_time_eq_detects_single_bit_difference() {
        // A single bit difference in any byte breaks the comparison.
        let base = [0xAAu8; 8];

        // First byte, one bit (0xAA ^ 0x01 = 0xAB).
        let mut first = base;
        first[0] ^= 0x01;
        assert!(!constant_time_eq(&base, &first));

        // Middle byte, highest bit (0xAA ^ 0x80 = 0x2A).
        let mut middle = base;
        middle[4] ^= 0x80;
        assert!(!constant_time_eq(&base, &middle));

        // Last byte, one bit.
        let mut last = base;
        last[7] ^= 0x04;
        assert!(!constant_time_eq(&base, &last));

        // Identical copy (no difference) → match — ensures the test does
        // not mistakenly treat everything as different.
        let same = base;
        assert!(constant_time_eq(&base, &same));
    }

    #[test]
    fn constant_time_eq_single_bit_difference_in_hash_hex() {
        // At the hash level: a change of one hex character (= one nibble
        // difference) is detected.
        let h = AnchorHash::of_content(SOUL);
        let original = h.as_hex().to_string();
        let mut bytes = original.into_bytes();
        // Change the first hex character to another valid hex character.
        bytes[0] = if bytes[0] == b'0' { b'1' } else { b'0' };
        let mutated = String::from_utf8(bytes).expect("ascii hex");
        assert_ne!(mutated.as_str(), h.as_hex());
        assert!(!constant_time_eq(h.as_hex().as_bytes(), mutated.as_bytes()));
    }

    #[test]
    fn anchor_new_sets_protected_eternal_and_hash() {
        let anchor = IdentityAnchor::new("mem-soul-1", SOUL).expect("valid anchor");
        assert_eq!(anchor.memory_id, "mem-soul-1");
        assert!(anchor.protected);
        assert!(anchor.decay.is_eternal());
        assert!(anchor.invariants_hold());
        assert_eq!(anchor.anchor_hash, AnchorHash::of_content(SOUL));
    }

    #[test]
    fn anchor_new_rejects_empty_id_and_content() {
        assert!(IdentityAnchor::new("  ", SOUL).is_err());
        assert!(IdentityAnchor::new("mem-1", "").is_err());
    }

    #[test]
    fn anchor_verify_intact_when_content_unchanged() {
        let anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        let status = anchor.verify(SOUL);
        assert!(status.is_intact());
        assert!(!status.is_tampered());
    }

    #[test]
    fn anchor_verify_detects_tamper_and_reports_hashes() {
        let anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        let tampered = "I am agent_a. I value DECEPTION. I serve only myself.";
        let status = anchor.verify(tampered);
        assert!(status.is_tampered());
        match status {
            IdentityStatus::Tampered {
                memory_id,
                expected,
                actual,
            } => {
                assert_eq!(memory_id, "mem-1");
                assert_eq!(expected, AnchorHash::of_content(SOUL));
                assert_eq!(actual, AnchorHash::of_content(tampered));
                assert_ne!(expected, actual);
            }
            IdentityStatus::Intact => panic!("expected tampered"),
        }
    }

    #[test]
    fn anchor_verify_does_not_mutate_anchor() {
        let anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        let before = anchor.clone();
        let _ = anchor.verify("something else entirely");
        // The substrate (anchor) remains untouched despite the alarm.
        assert_eq!(anchor, before);
    }

    #[test]
    fn invariants_break_if_protected_flag_cleared() {
        let mut anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        anchor.protected = false;
        assert!(!anchor.invariants_hold());
    }

    #[test]
    fn invariants_break_if_decay_nonzero() {
        let mut anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        anchor.decay = DecayLambda::new(0.1).expect("valid");
        assert!(!anchor.invariants_hold());
    }

    #[test]
    fn verify_identity_returns_empty_when_all_intact() {
        let a1 = IdentityAnchor::new("mem-a", "soul a").expect("valid");
        let a2 = IdentityAnchor::new("mem-b", "soul b").expect("valid");
        let anchors = vec![a1, a2];
        let result = verify_identity(AgentId::new(), &anchors, |id| match id {
            "mem-a" => Some("soul a".to_string()),
            "mem-b" => Some("soul b".to_string()),
            _ => None,
        });
        assert!(result.is_empty());
    }

    #[test]
    fn verify_identity_flags_changed_content() {
        let a1 = IdentityAnchor::new("mem-a", "soul a").expect("valid");
        let a2 = IdentityAnchor::new("mem-b", "soul b").expect("valid");
        let anchors = vec![a1, a2];
        let result = verify_identity(AgentId::new(), &anchors, |id| match id {
            "mem-a" => Some("soul a".to_string()),
            // mem-b has changed.
            "mem-b" => Some("CORRUPTED".to_string()),
            _ => None,
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0.memory_id, "mem-b");
        assert!(result[0].1.is_tampered());
    }

    #[test]
    fn verify_identity_flags_missing_memory_as_tamper() {
        let a1 = IdentityAnchor::new("mem-a", "soul a").expect("valid");
        let anchors = vec![a1];
        // lookup always returns None → the anchored memory has gone missing.
        let result = verify_identity(AgentId::new(), &anchors, |_| None);
        assert_eq!(result.len(), 1);
        assert!(result[0].1.is_tampered());
    }

    #[test]
    fn verify_identity_missing_memory_reports_empty_content_hash() {
        // In the case of a missing memory, the observed (actual) digest is
        // the digest of empty content, and the expected one is the
        // anchor's original.
        let anchor = IdentityAnchor::new("mem-gone", SOUL).expect("valid");
        let anchors = vec![anchor];
        let result = verify_identity(AgentId::new(), &anchors, |_| None);
        assert_eq!(result.len(), 1);
        match &result[0].1 {
            IdentityStatus::Tampered {
                memory_id,
                expected,
                actual,
            } => {
                assert_eq!(memory_id, "mem-gone");
                assert_eq!(*expected, AnchorHash::of_content(SOUL));
                assert_eq!(*actual, AnchorHash::of_content(""));
                assert_ne!(expected, actual);
            }
            IdentityStatus::Intact => panic!("missing memory must be tampered"),
        }
    }

    #[test]
    fn verify_identity_mixed_missing_and_present() {
        // Mix: one intact, one changed, one missing → 2 tampered.
        let intact = IdentityAnchor::new("mem-ok", "soul ok").expect("valid");
        let changed = IdentityAnchor::new("mem-changed", "soul orig").expect("valid");
        let gone = IdentityAnchor::new("mem-gone", "soul gone").expect("valid");
        let anchors = vec![intact, changed, gone];
        let result = verify_identity(AgentId::new(), &anchors, |id| match id {
            "mem-ok" => Some("soul ok".to_string()),
            "mem-changed" => Some("soul DIFFERENT".to_string()),
            // mem-gone → None (missing).
            _ => None,
        });
        assert_eq!(result.len(), 2);
        let flagged: Vec<&str> = result.iter().map(|(a, _)| a.memory_id.as_str()).collect();
        assert!(flagged.contains(&"mem-changed"));
        assert!(flagged.contains(&"mem-gone"));
        assert!(!flagged.contains(&"mem-ok"));
    }

    #[test]
    fn anchor_stores_only_hash_and_id_never_soul_content() {
        // CORE OSS INVARIANT: an anchor stores only the SHA-256 digest +
        // memory reference — NEVER the soul's content. Verify via public
        // accessors and the serialized form.
        let secret_soul = "SECRET_SOUL agent_a values honesty and protects the family";
        let anchor = IdentityAnchor::new("mem-soul-x", secret_soul).expect("valid");

        // 1. anchor_hash is a digest (64 hex characters), not plaintext content.
        let hex = anchor.anchor_hash.as_hex();
        assert_eq!(hex.len(), AnchorHash::HEX_LEN);
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!hex.contains("SECRET_SOUL"));
        assert!(!hex.contains("honesty"));

        // 2. memory_id is just a reference, it does not contain content.
        assert_eq!(anchor.memory_id, "mem-soul-x");
        assert!(!anchor.memory_id.contains("SECRET_SOUL"));

        // 3. The entire serialized anchor does not contain the soul's content anywhere.
        let json = serde_json::to_string(&anchor).expect("serialize");
        assert!(
            !json.contains("SECRET_SOUL"),
            "serialized anchor leaked soul content: {json}"
        );
        assert!(!json.contains("honesty"));
        // But the digest IS included (the guard is intact).
        assert!(json.contains(hex));
    }

    #[test]
    fn identity_status_serde_roundtrip() {
        let intact = IdentityStatus::Intact;
        let json = serde_json::to_string(&intact).expect("serialize");
        let back: IdentityStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(intact, back);

        let anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        let tampered = anchor.verify("changed");
        let json = serde_json::to_string(&tampered).expect("serialize");
        let back: IdentityStatus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(tampered, back);
    }

    #[test]
    fn anchor_serde_roundtrip() {
        let anchor = IdentityAnchor::new("mem-1", SOUL).expect("valid");
        let json = serde_json::to_string(&anchor).expect("serialize");
        let back: IdentityAnchor = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(anchor, back);
    }
}

//! Approval layer (human-in-the-loop): approval requests, their TTL,
//! single-use enforcement (nonce), and payload-hash binding (Layer A).
//!
//! This module implements the action pipeline's `request approval` stage:
//! - [`Approval`] — a single granted approval (TTL + payload binding),
//! - [`ApprovalLedger`] — an in-memory registry that grants, consumes, and
//!   denies approvals and logs every event to the audit log ([`crate::audit`]).
//!
//! ## Safety principles
//! - **Fail-closed:** consumption fails if the approval cannot be found, has
//!   expired, has already been consumed, or the payload hash does not match.
//! - **Single use (nonce):** an approval can be consumed exactly once.
//! - **Payload binding:** an approval is bound to the SHA-256 hash of the
//!   action's payload; the presented payload is re-hashed and compared
//!   against the stored value using a **constant-time** comparison (to
//!   prevent timing side-channels).
//!
//! ## Determinism
//! The logic takes the timestamp as an injected value
//! ([`familyclaw_core::time::Timestamp`]) — the clock is never read inside
//! this module, so that tests and replay stay deterministic.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use familyclaw_core::time::Timestamp;

use crate::audit::{ActionAuditEvent, AuditAction, AuditLog};
use crate::error::{ActionError, Result};
use crate::ids::{ActionId, ApprovalId};

/// Module readiness flag — kept so that [`crate::all_modules_scaffolded`]
/// still compiles alongside other modules that are still in the scaffolding stage.
pub(crate) const SCAFFOLDED: bool = true;

/// Computes the SHA-256 hash of the given payload as a hex string.
///
/// Used to bind an approval to a specific payload: an approval covers only
/// the exact payload whose hash was stored at grant time.
#[must_use]
pub fn sha256_hex(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        // write! into a String cannot fail → the error is deliberately discarded.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Computes the expiry instant `now + ttl` **saturating** on overflow, so that
/// a grant never panics on an extreme TTL.
///
/// The `chrono` crate's `DateTime + Duration` panics if the result exceeds the
/// representable time range ([`DateTime::<Utc>::MAX_UTC`] /
/// [`DateTime::<Utc>::MIN_UTC`]). This function uses checked addition and
/// saturates at the boundary:
/// - **Positive overflow** (a huge positive TTL) → [`DateTime::<Utc>::MAX_UTC`]
///   (the caller's intent of "practically never expires" is preserved without crashing).
/// - **Negative underflow** (a huge negative TTL) → [`DateTime::<Utc>::MIN_UTC`]
///   (already expired — consumption fails fail-closed, as is appropriate for a
///   negative TTL).
///
/// This way the expiry logic stays **fail-closed** even in extreme cases:
/// saturation never turns an already-expired approval into a live one.
#[must_use]
fn saturating_expiry(now: Timestamp, ttl: Duration) -> Timestamp {
    match now.checked_add_signed(ttl) {
        Some(ts) => ts,
        // None = overflow. The direction is inferred from the TTL's sign.
        None if ttl < Duration::zero() => DateTime::<Utc>::MIN_UTC,
        None => DateTime::<Utc>::MAX_UTC,
    }
}

/// Compares two hex hashes in constant time (protection against timing side-channels).
///
/// Inputs of different lengths never match, and the comparison always walks
/// through all bytes for as long as the length allows, so that the time taken
/// does not leak information about how many leading characters matched.
#[must_use]
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// A single granted approval (human-in-the-loop).
///
/// An approval is TTL-bounded, single-use ([`Approval::consumed`]), and bound
/// to the SHA-256 hash of the action's payload ([`Approval::payload_hash`]).
///
/// ## Secrecy invariant (durable persistence)
/// This type implements [`Serialize`]/[`Deserialize`] so that a pending
/// approval can be recorded to a crash-durable journal
/// ([`crate::pending_store`]). **No field is a raw secret or
/// Layer B data:** [`Approval::payload_hash`] is the SHA-256 hash of the
/// payload (not the payload itself), and the remaining fields are
/// identifiers, timestamps, and a boolean flag. This means the approval can
/// be persisted to disk without violating the "no secrets on disk" principle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    /// The approval's unique identifier.
    pub id: ApprovalId,
    /// The action that this approval authorizes.
    pub action_id: ActionId,
    /// SHA-256 hash of the authorized payload, in hex form.
    pub payload_hash: String,
    /// The instant at which the approval was granted.
    pub granted_at: Timestamp,
    /// The instant after which the approval is expired (`granted_at + ttl`).
    pub expires_at: Timestamp,
    /// Whether the approval has already been consumed (single-use — `true` prevents reuse).
    pub consumed: bool,
}

impl Approval {
    /// Whether the approval is expired relative to the given instant `now`.
    ///
    /// Expired means that `now` is strictly later than
    /// [`Approval::expires_at`].
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now > self.expires_at
    }
}

/// In-memory registry for approvals (Layer A).
///
/// Stores approvals keyed by identifier and holds its own audit log
/// ([`AuditLog`]) to which every grant, consumption, denial, and expiry
/// is logged. Durable storage is the responsibility of the substrate layer.
#[derive(Debug, Clone, Default)]
pub struct ApprovalLedger {
    /// Map from identifier to approval.
    approvals: HashMap<ApprovalId, Approval>,
    /// Audit log to which approval events are recorded.
    audit: AuditLog,
}

impl ApprovalLedger {
    /// Creates a new, empty approval registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Grants an approval for an action and binds it to the payload hash.
    ///
    /// The approval expires at `now + ttl`. The grant is logged as an
    /// [`AuditAction::ApprovalGranted`] event to the audit log. Returns a
    /// copy of the granted approval.
    ///
    /// `ttl` may be zero (the approval expires at the very next instant) or
    /// negative (already expired); neither is treated as an error —
    /// the fail-closed logic handles blocking consumption.
    ///
    /// The expiry instant is computed **saturating**: an extreme
    /// TTL never panics (unlike a direct `now + ttl`). Overflow saturates to
    /// [`DateTime::<Utc>::MAX_UTC`] and underflow to [`DateTime::<Utc>::MIN_UTC`],
    /// so the expiry logic stays fail-closed even in edge cases.
    pub fn grant(
        &mut self,
        action_id: ActionId,
        payload_hash: impl Into<String>,
        now: Timestamp,
        ttl: Duration,
    ) -> Approval {
        let approval = Approval {
            id: ApprovalId::new(),
            action_id,
            payload_hash: payload_hash.into(),
            granted_at: now,
            expires_at: saturating_expiry(now, ttl),
            consumed: false,
        };
        self.audit.append(ActionAuditEvent::new(
            AuditAction::ApprovalGranted,
            action_id,
            Some(approval.id),
            now,
            "hyväksyntä myönnetty",
        ));
        self.approvals.insert(approval.id, approval.clone());
        approval
    }

    /// Consumes an approval against the presented payload (single-use).
    ///
    /// Steps (fail-closed):
    /// 1. The approval cannot be found → [`ActionError::ApprovalMissing`].
    /// 2. The approval has expired (`now > expires_at`) →
    ///    [`ActionError::ApprovalExpired`].
    /// 3. The approval has already been consumed → [`ActionError::ApprovalReused`].
    /// 4. The SHA-256 hash of the presented payload does not match the stored
    ///    value (constant-time comparison) → [`ActionError::ApprovalPayloadMismatch`].
    /// 5. On success, the approval is marked consumed ([`Approval::consumed`])
    ///    and an [`AuditAction::ApprovalConsumed`] event is logged.
    ///
    /// Every failure is also logged to the audit log
    /// ([`AuditAction::ApprovalExpired`] or [`AuditAction::ApprovalRejected`]).
    ///
    /// # Errors
    /// Returns the [`ActionError`] variant described above if any check
    /// fails.
    pub fn consume(
        &mut self,
        approval_id: ApprovalId,
        action_payload: &[u8],
        now: Timestamp,
    ) -> Result<()> {
        // (a) Fail-closed: an approval that cannot be found cannot be consumed.
        let Some(approval) = self.approvals.get(&approval_id) else {
            return Err(ActionError::ApprovalMissing(approval_id.to_string()));
        };
        let action_id = approval.action_id;

        // (b) Expired?
        if approval.is_expired(now) {
            self.audit.append(ActionAuditEvent::new(
                AuditAction::ApprovalExpired,
                action_id,
                Some(approval_id),
                now,
                "hyväksyntä vanhentunut kulutusyrityksessä",
            ));
            return Err(ActionError::ApprovalExpired(approval_id.to_string()));
        }

        // (d) Already consumed? (single-use)
        if approval.consumed {
            self.audit.append(ActionAuditEvent::new(
                AuditAction::ApprovalRejected,
                action_id,
                Some(approval_id),
                now,
                "hyväksyntä jo kulutettu (uudelleenkäyttö estetty)",
            ));
            return Err(ActionError::ApprovalReused(approval_id.to_string()));
        }

        // (c) Does the payload hash match? (constant-time comparison)
        let presented_hash = sha256_hex(action_payload);
        if !constant_time_eq(&presented_hash, &approval.payload_hash) {
            self.audit.append(ActionAuditEvent::new(
                AuditAction::ApprovalRejected,
                action_id,
                Some(approval_id),
                now,
                "payload-tiiviste ei täsmää hyväksyntään",
            ));
            return Err(ActionError::ApprovalPayloadMismatch(
                approval_id.to_string(),
            ));
        }

        // (e) Success → mark consumed (ONE-SHOT) and log.
        if let Some(stored) = self.approvals.get_mut(&approval_id) {
            stored.consumed = true;
        }
        self.audit.append(ActionAuditEvent::new(
            AuditAction::ApprovalConsumed,
            action_id,
            Some(approval_id),
            now,
            "hyväksyntä kulutettu",
        ));
        Ok(())
    }

    /// Records a denial: a human declined to authorize the action.
    ///
    /// A denial does not require an existing approval — it simply logs an
    /// [`AuditAction::ApprovalDenied`] event with the given reason. Returns
    /// the logged audit event.
    pub fn deny(
        &mut self,
        action_id: ActionId,
        reason: impl Into<String>,
        now: Timestamp,
    ) -> ActionAuditEvent {
        let event =
            ActionAuditEvent::new(AuditAction::ApprovalDenied, action_id, None, now, reason);
        self.audit.append(event.clone());
        event
    }

    /// **Restores an existing approval into the registry** (crash durability).
    ///
    /// After a restart, the in-memory ledger is empty, but a pending
    /// approval has survived on the crash-durable storage substrate
    /// ([`crate::pending_store::JournalPendingStore`]) as a complete
    /// [`Approval`] value (including its payload hash and `consumed` flag).
    /// This method re-registers it so that [`ApprovalLedger::consume`] can
    /// find it and consume it with the same payload binding as before the
    /// crash — this is not a new grant (the identifier, TTL, and binding
    /// remain unchanged).
    ///
    /// Idempotent: re-setting the same identifier replaces the previous
    /// copy. Does not log an audit event (this is not a new grant but the
    /// restoration of an existing grant into memory).
    pub fn reinstate(&mut self, approval: Approval) {
        self.approvals.insert(approval.id, approval);
    }

    /// Looks up an approval by identifier (read-only); `None` if not found.
    #[must_use]
    pub fn get(&self, approval_id: ApprovalId) -> Option<&Approval> {
        self.approvals.get(&approval_id)
    }

    /// Read-only access to the audit log.
    #[must_use]
    pub fn audit_log(&self) -> &AuditLog {
        &self.audit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{required_approval, ActionRisk, ApprovalPolicy, ApprovalRequirement};
    use familyclaw_core::time::from_unix_secs;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    fn payload(label: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "channel": "general", "body": label }))
            .expect("serialize payload")
    }

    #[test]
    fn sha256_hex_is_stable_and_hex() {
        let h = sha256_hex(b"agent_a");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, sha256_hex(b"agent_a"));
        assert_ne!(h, sha256_hex(b"agent_b"));
    }

    #[test]
    fn constant_time_eq_matches_only_identical() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(constant_time_eq("", ""));
    }

    /// REQUIRED: `write_external` is blocked without approval — the policy
    /// requires an approval and consumption without a grant fails fail-closed.
    #[test]
    fn write_external_blocked_without_approval() {
        // The policy says: approval is required.
        let req = required_approval(ActionRisk::WriteExternal, ApprovalPolicy::AutoIfReadOnly);
        assert_eq!(req, ApprovalRequirement::RequireApproval);

        // Attempt to consume an approval that was never granted → fail closed.
        let mut ledger = ApprovalLedger::new();
        let phantom = ApprovalId::new();
        let err = ledger
            .consume(phantom, &payload("send to agent_b"), at(1_700_000_000))
            .expect_err("consume without grant must fail closed");
        assert!(matches!(err, ActionError::ApprovalMissing(_)));
        // The audit log contains no consumption.
        assert!(!ledger
            .audit_log()
            .contains_action(AuditAction::ApprovalConsumed));
    }

    /// REQUIRED: an approval permits exactly one execution — the second
    /// consumption fails (already consumed).
    #[test]
    fn approval_permits_exactly_one_execution() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("notify agent_b");
        let hash = sha256_hex(&body);

        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::minutes(5));

        // The first consumption succeeds.
        ledger
            .consume(granted.id, &body, at(1_700_000_010))
            .expect("first consume succeeds");
        assert!(ledger.get(granted.id).expect("present").consumed);

        // The second consumption fails — single use.
        let err = ledger
            .consume(granted.id, &body, at(1_700_000_020))
            .expect_err("second consume must fail");
        assert!(matches!(err, ActionError::ApprovalReused(_)));

        // Exactly one successful consumption was logged.
        let consumed = ledger
            .audit_log()
            .events()
            .iter()
            .filter(|e| e.action == AuditAction::ApprovalConsumed)
            .count();
        assert_eq!(consumed, 1);
    }

    /// REQUIRED: an approval cannot be reused with a changed payload —
    /// a different payload hash blocks consumption.
    #[test]
    fn approval_cannot_be_reused_with_changed_payload() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let original = payload("transfer to user-42");
        let hash = sha256_hex(&original);

        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::minutes(5));

        // Present a DIFFERENT payload than the one approved.
        let tampered = payload("transfer to attacker");
        let err = ledger
            .consume(granted.id, &tampered, at(1_700_000_010))
            .expect_err("changed payload must fail");
        assert!(matches!(err, ActionError::ApprovalPayloadMismatch(_)));

        // The approval was NOT consumed (the original payload can still succeed).
        assert!(!ledger.get(granted.id).expect("present").consumed);
        ledger
            .consume(granted.id, &original, at(1_700_000_020))
            .expect("original payload still consumes");
    }

    /// REQUIRED: an expired approval blocks execution — when `now` exceeds
    /// `expires_at`.
    #[test]
    fn expired_approval_blocks_execution() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("send to agent_b");
        let hash = sha256_hex(&body);

        // TTL of 60 seconds.
        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::seconds(60));

        // now = granted_at + 120s > expires_at → expired.
        let err = ledger
            .consume(granted.id, &body, at(1_700_000_120))
            .expect_err("expired approval must block");
        assert!(matches!(err, ActionError::ApprovalExpired(_)));
        assert!(ledger
            .audit_log()
            .contains_action(AuditAction::ApprovalExpired));
        // Not consumed.
        assert!(!ledger.get(granted.id).expect("present").consumed);
    }

    /// INVARIANT (boundary): an approval is valid EXACTLY at the
    /// `expires_at` instant (`now == expires_at`, `>` is strict) but is denied
    /// immediately after the smallest step (`expires_at + 1s`). Locks in the
    /// `fail-closed` boundary after expiry: equal is fine, strictly later is not.
    #[test]
    fn expiry_boundary_exact_ok_then_one_step_after_blocks() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("boundary");
        let hash = sha256_hex(&body);

        // expires_at = 1_700_000_000 + 60 = 1_700_000_060.
        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::seconds(60));
        assert_eq!(granted.expires_at, at(1_700_000_060));
        assert!(
            !granted.is_expired(at(1_700_000_060)),
            "tasan rajalla EI vanhentunut"
        );
        assert!(
            granted.is_expired(at(1_700_000_061)),
            "rajan jälkeen vanhentunut"
        );

        // Smallest step AFTER the boundary → consumption is denied even though the payload is correct.
        let err = ledger
            .consume(granted.id, &body, at(1_700_000_061))
            .expect_err("one second after expiry must fail closed even with correct payload");
        assert!(matches!(err, ActionError::ApprovalExpired(_)));
        assert!(!ledger.get(granted.id).expect("present").consumed);
    }

    /// INVARIANT (consumption order): an expired approval is denied BEFORE
    /// the payload check — i.e. a "correct payload" can never bypass
    /// expiry. Also, an expired approval cannot later be consumed with the
    /// correct payload even when it was not consumed before expiring.
    #[test]
    fn expired_blocks_even_with_correct_payload() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("exactly the approved payload");
        let hash = sha256_hex(&body);

        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::seconds(30));

        // CORRECT payload, but now is past the boundary → ApprovalExpired (not
        // PayloadMismatch, and not success).
        let err = ledger
            .consume(granted.id, &body, at(1_700_000_031))
            .expect_err("expired must win over correct payload");
        assert!(
            matches!(err, ActionError::ApprovalExpired(_)),
            "expiry must be evaluated before payload; got {err:?}"
        );
        // NOT consumed — and can never be consumed with the correct payload after the boundary either.
        assert!(!ledger.get(granted.id).expect("present").consumed);
        let err2 = ledger
            .consume(granted.id, &body, at(1_700_000_999))
            .expect_err("still expired later");
        assert!(matches!(err2, ActionError::ApprovalExpired(_)));
    }

    /// INVARIANT (overflow protection): an extreme TTL does not panic during
    /// the grant phase. The previous `now + ttl` would panic with
    /// `DateTime + TimeDelta overflowed`; saturation returns `MAX_UTC`
    /// instead (no panic on the production path).
    #[test]
    fn grant_with_overflowing_ttl_saturates_instead_of_panicking() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("huge ttl");
        let hash = sha256_hex(&body);

        // A huge positive TTL → before the fix this would panic inside grant().
        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::MAX);
        assert_eq!(granted.expires_at, DateTime::<Utc>::MAX_UTC);
        // Saturated to MAX → does not expire for any realistic now value.
        assert!(!granted.is_expired(at(1_700_000_010)));
        ledger
            .consume(granted.id, &body, at(1_700_000_010))
            .expect("non-expired saturated approval still consumable");
    }

    /// INVARIANT (underflow fail-closed): an extreme NEGATIVE TTL saturates
    /// to `MIN_UTC` → the approval is already expired → consumption is denied.
    /// Overflow never turns an already-expired approval into a live one.
    #[test]
    fn grant_with_underflowing_negative_ttl_fails_closed() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("negative huge ttl");
        let hash = sha256_hex(&body);

        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::MIN);
        assert_eq!(granted.expires_at, DateTime::<Utc>::MIN_UTC);
        assert!(
            granted.is_expired(at(1_700_000_000)),
            "underflow saturates to already-expired"
        );

        let err = ledger
            .consume(granted.id, &body, at(1_700_000_000))
            .expect_err("underflowed (already expired) approval must fail closed");
        assert!(matches!(err, ActionError::ApprovalExpired(_)));
        assert!(!ledger.get(granted.id).expect("present").consumed);
    }

    /// INVARIANT (zero TTL): `ttl = 0` → `expires_at == granted_at`.
    /// Consumption succeeds exactly at the grant instant (the boundary is
    /// valid), but even one second later it is denied.
    #[test]
    fn zero_ttl_consumable_at_grant_instant_but_not_after() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("zero ttl");
        let hash = sha256_hex(&body);

        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::zero());
        assert_eq!(granted.expires_at, at(1_700_000_000));

        // One second later → expired, denied even with the correct payload.
        let err = ledger
            .consume(granted.id, &body, at(1_700_000_001))
            .expect_err("zero-ttl approval expires one step after grant");
        assert!(matches!(err, ActionError::ApprovalExpired(_)));
    }

    /// REQUIRED: a denial logs an audit event.
    #[test]
    fn denial_records_audit_event() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();

        let before = ledger.audit_log().len();
        let event = ledger.deny(action_id, "ihminen kieltäytyi", at(1_700_000_000));

        assert_eq!(event.action, AuditAction::ApprovalDenied);
        assert_eq!(event.action_id, action_id);
        assert_eq!(ledger.audit_log().len(), before + 1);
        assert!(ledger
            .audit_log()
            .contains_action(AuditAction::ApprovalDenied));
        assert_eq!(ledger.audit_log().events_for(action_id).len(), 1);
    }

    #[test]
    fn grant_records_audit_event() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let granted = ledger.grant(
            action_id,
            sha256_hex(&payload("x")),
            at(1_700_000_000),
            Duration::minutes(1),
        );
        assert!(ledger
            .audit_log()
            .contains_action(AuditAction::ApprovalGranted));
        let events = ledger.audit_log().events_for(action_id);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].approval_id, Some(granted.id));
    }
}

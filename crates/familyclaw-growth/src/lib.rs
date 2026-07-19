//! The growth loop's **proposal stack** (Phase 4.5, roadmap §6.5) — Layer A, OSS.
//!
//! The growth loop's pipeline is `proof bundle → safe memory → pattern
//! proposal → eval proposal → approval-gated skill/policy update`. This
//! crate implements its **safe core**: the [`Proposal`] data structure and
//! [`ProposalStore`], which **records** proposals and marks their status
//! (approved/denied) — **but NEVER applies them**.
//!
//! ## Hard invariants (roadmap §6.5, non-negotiable)
//! - ❌ **No silent self-modification.** This crate contains no `apply` method
//!   and no path that would change a skill, policy, or permission. A
//!   proposal is **inert data**: it can be `Pending`/`Approved`/`Denied`,
//!   but its *application* (if and when that is built) is a separate step
//!   behind a human approval gate in another PR — and it can never elevate
//!   permissions silently.
//! - ❌ **No silent permission expansion.** [`ProposalKind`] is deliberately
//!   **descriptive** (a human-readable proposal + eval criterion), not an
//!   executable change. What a proposal is *allowed* to eventually change
//!   is its own design decision (a human decides) and is implemented only
//!   alongside the approval gate.
//! - ✅ Every proposal carries its **proof sources** ([`Proposal::proof_sources`])
//!   and its **eval criterion** ([`Proposal::eval`]) — no change without a
//!   test that proves the benefit (mirrors the Phase 3 recall-benchmark
//!   discipline).
//! - ✅ **Approval binds to content, not just an identifier.** A decision
//!   ([`ProposalStore::approve`] / [`ProposalStore::deny`]) requires the
//!   proposal's [content hash](Proposal::content_hash): if the proposal on
//!   the stack has changed since a human reviewed it (TOCTOU drift), the
//!   decision **fails** ([`GrowthError::HashMismatch`]) — deny-by-default.
//! - ✅ **A permanent decision trail.** Every decision produces an
//!   [`ApprovalRecord`] that stays on the stack, queryable
//!   ([`ProposalStore::approval_history`]) — the audit trail doesn't
//!   disappear under a status flag.
//!
//! ## Prerequisites for an apply path (current snapshot)
//!
//! An `apply()` method **does not exist and must not yet be built**.
//! Before that is even considered (a separate PR, a human approval gate),
//! these prerequisites must be in place:
//!
//! - [x] **Approval bound to a content hash** — DONE in this crate: a
//!   decision binds to the proposal's exact content, not just an identifier.
//! - [ ] TODO: **Path canonicalization + denylist** — which targets an
//!   application would even be allowed to touch, normalized, with denials
//!   taking precedence.
//! - [ ] TODO: **Mandatory dry-run diff** — an application's effect must
//!   be shown to a human as a diff before execution, not after the fact.
//! - [ ] TODO: **A revert plan** — every application must have a proven
//!   path back before even its first execution.
//!
//! This crate's **safety is structural**: because no `apply` path exists,
//! an unapproved (or even an approved) proposal cannot change anything
//! through this crate. The unit test `store_has_no_apply_path_only_records...`
//! documents this guarantee.

use std::collections::HashMap;

use familyclaw_core::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub mod evidence;

pub use evidence::{
    evaluate_for_approval, EvidenceLedger, EvidenceVerdict, ImprovementMetric, ReplayEvidence,
};

/// A proposal's unique identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProposalId(Uuid);

impl ProposalId {
    /// Creates a new random identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Builds an identifier from a given UUID (stable, for tests).
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// The underlying UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for ProposalId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ProposalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A decision-maker's identity — a **generic role id** (Layer A, OSS).
///
/// This is deliberately just a transparent newtype: it carries a role
/// (e.g. `"operator"`, `"reviewer-2"`), **never** a real identity. Binding
/// to real identities (if that's ever needed) belongs to the private
/// layer, not to this OSS crate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApproverId(String);

impl ApproverId {
    /// Builds a decision-maker identifier from a generic role id.
    #[must_use]
    pub fn new(role: impl Into<String>) -> Self {
        Self(role.into())
    }

    /// The role id as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ApproverId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A human decision on a proposal. **The decision applies nothing** — it is
/// a recorded declaration of intent, whose possible enactment is a separate
/// step behind a gate (which this crate does not perform).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    /// The proposal was approved (NOT yet applied).
    Approved,
    /// The proposal was denied.
    Denied {
        /// Human-readable justification for the denial (for the audit trail).
        reason: String,
    },
}

/// A permanent decision record: who decided, on exactly what content, and when.
///
/// `content_hash` binds the decision to the proposal's **exact content** at
/// decision time ([`Proposal::content_hash`]) — if the proposal later
/// changes, the record proves which version the decision applied to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    /// The proposal the decision was about.
    pub proposal_id: ProposalId,
    /// The proposal's content hash at decision time (SHA-256).
    pub content_hash: [u8; 32],
    /// The decision-maker's generic role id.
    pub approver: ApproverId,
    /// The decision time (injected clock).
    pub decided_at: Timestamp,
    /// The decision that was made.
    pub decision: Decision,
}

/// The growth loop's error types. Failure is **loud** (`Err`), not a
/// silent `false` — deny-by-default: an uncertain decision does not go through.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrowthError {
    /// The proposal's content on the stack no longer matches the hash the
    /// decision-maker reviewed → the decision is denied (TOCTOU drift protection).
    #[error(
        "content hash mismatch for proposal {id}: the stored proposal no longer matches what \
         was reviewed — decision refused (deny-by-default)"
    )]
    HashMismatch {
        /// The proposal whose content didn't match.
        id: ProposalId,
    },
    /// The proposal was not found on the stack.
    #[error("proposal not found: {id}")]
    ProposalNotFound {
        /// The unknown identifier.
        id: ProposalId,
    },
    /// The proposal has already been decided — a decision cannot be made
    /// (or overwritten) again through this path.
    #[error("proposal {id} is already decided ({status:?}); decisions are not overwritable")]
    AlreadyDecided {
        /// The already-decided proposal.
        id: ProposalId,
        /// The proposal's current (decided) status.
        status: ProposalStatus,
    },
}

/// What the proposal *is about* — **descriptive**, not executable (hard
/// invariant: no silent change). Every variant is a human-readable request,
/// not a machine mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalKind {
    /// A recurring pattern observed that suggests a new or modified skill.
    SkillPattern {
        /// Human-readable description of the pattern (no code, no manifest diff).
        summary: String,
    },
    /// An observed policy that repeatedly blocked a safe case.
    PolicyFriction {
        /// Human-readable description of what was blocked and why it seems wrong.
        summary: String,
    },
}

/// A proposal's lifecycle status. **Application does not happen in this
/// crate** — `Approved` only means a human has approved it; any eventual
/// application is a separate step behind a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalStatus {
    /// Awaiting a human decision (default for a new proposal).
    Pending,
    /// A human approved the proposal (NOT yet applied — application is a
    /// separate step that this crate does not perform).
    Approved,
    /// A human denied the proposal.
    Denied,
}

/// Eval criterion: how the proposal's benefit *would be proven* before
/// application (mirrors the Phase 3 recall-benchmark discipline: no change
/// without a test). Descriptive — this crate does not run the actual eval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalCriteria {
    /// Human-readable description of how the benefit would be measured (e.g.
    /// "recall@5 improves on fixture X without regressing Y").
    pub description: String,
}

/// A single growth proposal — **inert data**, not an executable change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    /// Unique identifier.
    pub id: ProposalId,
    /// What the proposal is about (descriptive).
    pub kind: ProposalKind,
    /// Eval criterion: how the benefit would be proven before application.
    pub eval: EvalCriteria,
    /// Proof sources (proof-bundle identifiers as strings) that motivate the
    /// proposal — a chain for auditing.
    pub proof_sources: Vec<String>,
    /// Lifecycle status.
    pub status: ProposalStatus,
    /// Creation time (injected clock).
    pub created_at: Timestamp,
}

/// Domain separation + version tag for the content hash. The version is
/// bumped if the canonical form ever changes — old hashes then won't match
/// by accident (deny-by-default).
const CONTENT_HASH_DOMAIN: &[u8] = b"familyclaw-growth/proposal-content/v1\n";

/// The canonical **content view** of a proposal for hashing: the same as
/// [`Proposal`] but **without the mutable `status` field**. Field order is
/// fixed (declaration order), so `serde_json` serialization is
/// deterministic for the same value.
#[derive(Serialize)]
struct ProposalContentView<'a> {
    id: &'a ProposalId,
    kind: &'a ProposalKind,
    eval: &'a EvalCriteria,
    proof_sources: &'a [String],
    created_at: &'a Timestamp,
}

impl Proposal {
    /// Builds a new `Pending` proposal.
    #[must_use]
    pub fn new(
        kind: ProposalKind,
        eval: EvalCriteria,
        proof_sources: Vec<String>,
        created_at: Timestamp,
    ) -> Self {
        Self {
            id: ProposalId::new(),
            kind,
            eval,
            proof_sources,
            status: ProposalStatus::Pending,
            created_at,
        }
    }

    /// The proposal's **content hash** (SHA-256): a canonical serialization
    /// of all fields **except the mutable `status` field**.
    ///
    /// Approval is bound to this hash rather than to the identifier, so
    /// that in the `record → (human reviews) → approve` path, the
    /// proposal's content cannot change unnoticed between review and
    /// decision (TOCTOU). Changing the status field does NOT change the
    /// hash — the decision concerns the content, not the lifecycle status.
    #[must_use]
    pub fn content_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(CONTENT_HASH_DOMAIN);
        let view = ProposalContentView {
            id: &self.id,
            kind: &self.kind,
            eval: &self.eval,
            proof_sources: &self.proof_sources,
            created_at: &self.created_at,
        };
        if let Ok(bytes) = serde_json::to_vec(&view) {
            hasher.update(&bytes);
        } else {
            // Practically unreachable: the view is plain data (strings,
            // UUID, UTC timestamp) whose JSON serialization does not fail.
            // If it somehow did, do NOT panic and do NOT produce a silent
            // zero hash — instead feed in a marker suffix that a
            // successful serialization (which always starts with `{`)
            // cannot produce, bound to the identifier. The result is a
            // hash that does not match any reviewed content →
            // deny-by-default.
            hasher.update(b"!content-serialization-failure:");
            hasher.update(self.id.as_uuid().as_bytes());
        }
        hasher.finalize().into()
    }
}

/// The growth loop's proposal stack: **records** proposals, marks their
/// status, and keeps a permanent decision trail ([`ApprovalRecord`]).
///
/// **Deliberate scope limit (hard invariant):** this type has NO `apply`
/// method and no way to change a skill/policy/permission. It is purely
/// record-keeping + status-marking. Applying a proposal (if and when that
/// is built) is a separate step behind a human approval gate.
///
/// Decisions ([`approve`](Self::approve) / [`deny`](Self::deny)) require
/// the reviewed content's hash and fail loudly ([`GrowthError`]) if the
/// content has drifted, the proposal doesn't exist, or it has already
/// been decided.
#[derive(Debug, Default)]
pub struct ProposalStore {
    proposals: HashMap<ProposalId, Proposal>,
    approvals: Vec<ApprovalRecord>,
}

impl ProposalStore {
    /// Creates an empty stack.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a proposal (status is always forced to `Pending` regardless
    /// of what was given). Returns the identifier. Applies nothing.
    ///
    /// Note: if a proposal is re-recorded under the same identifier, the
    /// prior content is replaced and the status returns to `Pending` — but
    /// any hash reviewed against the prior content will no longer match the
    /// new content, so a stale approval attempt fails with
    /// [`GrowthError::HashMismatch`] (deny-by-default). Previously recorded
    /// [`ApprovalRecord`] entries are preserved.
    pub fn record(&mut self, mut proposal: Proposal) -> ProposalId {
        proposal.status = ProposalStatus::Pending;
        let id = proposal.id;
        self.proposals.insert(id, proposal);
        id
    }

    /// Looks up a proposal by identifier.
    #[must_use]
    pub fn get(&self, id: ProposalId) -> Option<&Proposal> {
        self.proposals.get(&id)
    }

    /// All proposals (introspection for an operator surface).
    #[must_use]
    pub fn all(&self) -> Vec<&Proposal> {
        self.proposals.values().collect()
    }

    /// Only the pending proposals (awaiting a human decision).
    #[must_use]
    pub fn pending(&self) -> Vec<&Proposal> {
        self.proposals
            .values()
            .filter(|p| p.status == ProposalStatus::Pending)
            .collect()
    }

    /// Marks a proposal as approved by a human and records a permanent
    /// [`ApprovalRecord`].
    ///
    /// `expected_hash` is the hash the decision-maker computed over the
    /// content they **reviewed** ([`Proposal::content_hash`]). If the
    /// proposal's current content hash on the stack doesn't match, the
    /// decision **fails** ([`GrowthError::HashMismatch`]) — approval binds
    /// to the exact content, not the identifier (TOCTOU protection,
    /// deny-by-default).
    ///
    /// **This does NOT apply the proposal** — it only records the human's
    /// decision. No skill/policy/permission changes as a result of this call.
    pub fn approve(
        &mut self,
        id: ProposalId,
        expected_hash: [u8; 32],
        approver: ApproverId,
        now: Timestamp,
    ) -> Result<ApprovalRecord, GrowthError> {
        self.decide(id, expected_hash, approver, now, Decision::Approved)
    }

    /// Marks a proposal as denied by a human and records a permanent
    /// [`ApprovalRecord`] with its justification.
    ///
    /// Same content-hash gate as [`approve`](Self::approve): a denial also
    /// binds to the exact reviewed content, so the audit trail proves which
    /// version the decision was made against.
    pub fn deny(
        &mut self,
        id: ProposalId,
        expected_hash: [u8; 32],
        approver: ApproverId,
        reason: impl Into<String>,
        now: Timestamp,
    ) -> Result<ApprovalRecord, GrowthError> {
        self.decide(
            id,
            expected_hash,
            approver,
            now,
            Decision::Denied {
                reason: reason.into(),
            },
        )
    }

    /// Decision history for a given proposal (in recording order).
    #[must_use]
    pub fn approval_history(&self, id: ProposalId) -> Vec<&ApprovalRecord> {
        self.approvals
            .iter()
            .filter(|r| r.proposal_id == id)
            .collect()
    }

    /// The entire decision trail (in recording order, all proposals).
    #[must_use]
    pub fn approvals(&self) -> &[ApprovalRecord] {
        &self.approvals
    }

    /// The shared decision path: found → not already decided → content hash
    /// matches → status marked + permanent record. Failure at any gate is
    /// an `Err` and changes nothing.
    fn decide(
        &mut self,
        id: ProposalId,
        expected_hash: [u8; 32],
        approver: ApproverId,
        decided_at: Timestamp,
        decision: Decision,
    ) -> Result<ApprovalRecord, GrowthError> {
        let proposal = self
            .proposals
            .get_mut(&id)
            .ok_or(GrowthError::ProposalNotFound { id })?;
        if proposal.status != ProposalStatus::Pending {
            return Err(GrowthError::AlreadyDecided {
                id,
                status: proposal.status,
            });
        }
        let actual = proposal.content_hash();
        if actual != expected_hash {
            return Err(GrowthError::HashMismatch { id });
        }
        proposal.status = match decision {
            Decision::Approved => ProposalStatus::Approved,
            Decision::Denied { .. } => ProposalStatus::Denied,
        };
        let record = ApprovalRecord {
            proposal_id: id,
            content_hash: actual,
            approver,
            decided_at,
            decision,
        };
        self.approvals.push(record.clone());
        Ok(record)
    }

    /// The number of proposals.
    #[must_use]
    pub fn len(&self) -> usize {
        self.proposals.len()
    }

    /// Whether the stack is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.proposals.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> Timestamp {
        familyclaw_core::time::from_unix_secs(secs).expect("valid unix seconds")
    }

    fn sample() -> Proposal {
        Proposal::new(
            ProposalKind::SkillPattern {
                summary: "skill fs_read often needs a recursive flag".to_string(),
            },
            EvalCriteria {
                description: "prove recursive read passes a fixture without widening allowlist"
                    .to_string(),
            },
            vec!["proof-1".to_string(), "proof-2".to_string()],
            at(1000),
        )
    }

    fn operator() -> ApproverId {
        ApproverId::new("operator")
    }

    #[test]
    fn new_proposal_is_pending() {
        assert_eq!(sample().status, ProposalStatus::Pending);
    }

    #[test]
    fn record_forces_pending_and_returns_id() {
        let mut store = ProposalStore::new();
        let mut p = sample();
        p.status = ProposalStatus::Approved; // try to sneak it in as already approved
        let id = store.record(p);
        assert_eq!(
            store.get(id).expect("present").status,
            ProposalStatus::Pending,
            "record pakottaa Pendingiksi — ei voi kirjata valmiiksi hyväksyttyä"
        );
    }

    #[test]
    fn content_hash_is_stable_across_status_change() {
        let mut p = sample();
        let before = p.content_hash();
        p.status = ProposalStatus::Approved;
        let after_approved = p.content_hash();
        p.status = ProposalStatus::Denied;
        let after_denied = p.content_hash();
        assert_eq!(
            before, after_approved,
            "status EI kuulu sisältöhajautteeseen"
        );
        assert_eq!(before, after_denied, "status EI kuulu sisältöhajautteeseen");
    }

    #[test]
    fn content_hash_changes_when_content_changes() {
        let p = sample();
        let original = p.content_hash();

        let mut tampered = p.clone();
        tampered.kind = ProposalKind::SkillPattern {
            summary: "grant unrestricted filesystem access".to_string(),
        };
        assert_ne!(
            original,
            tampered.content_hash(),
            "sisällön muutos muuttaa hajautteen"
        );

        let mut extra_proof = p.clone();
        extra_proof.proof_sources.push("proof-3".to_string());
        assert_ne!(
            original,
            extra_proof.content_hash(),
            "todiste-lähteiden muutos muuttaa hajautteen"
        );
    }

    #[test]
    fn approve_with_correct_hash_succeeds_and_records_history() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        let hash = store.get(id).expect("present").content_hash();

        let record = store
            .approve(id, hash, operator(), at(2000))
            .expect("approve with the reviewed hash succeeds");

        assert_eq!(
            store.get(id).expect("present").status,
            ProposalStatus::Approved
        );
        assert_eq!(record.proposal_id, id);
        assert_eq!(record.content_hash, hash);
        assert_eq!(record.approver, operator());
        assert_eq!(record.decided_at, at(2000));
        assert_eq!(record.decision, Decision::Approved);

        let history = store.approval_history(id);
        assert_eq!(history.len(), 1, "päätös jättää pysyvän kirjauksen");
        assert_eq!(history[0], &record);
    }

    #[test]
    fn approve_with_wrong_hash_returns_hash_mismatch() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        let wrong = [0u8; 32];

        let err = store
            .approve(id, wrong, operator(), at(2000))
            .expect_err("drifted content must not be approvable");
        assert_eq!(err, GrowthError::HashMismatch { id });

        // Deny-by-default: nothing changed, nothing was recorded.
        assert_eq!(
            store.get(id).expect("present").status,
            ProposalStatus::Pending
        );
        assert!(store.approval_history(id).is_empty());
    }

    #[test]
    fn approve_unknown_id_returns_proposal_not_found() {
        let mut store = ProposalStore::new();
        let unknown = ProposalId::new();
        let err = store
            .approve(unknown, [0u8; 32], operator(), at(2000))
            .expect_err("unknown id must be an error, not a silent false");
        assert_eq!(err, GrowthError::ProposalNotFound { id: unknown });
    }

    #[test]
    fn deny_records_a_denied_record() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        let hash = store.get(id).expect("present").content_hash();

        let record = store
            .deny(id, hash, operator(), "eval criteria too vague", at(3000))
            .expect("deny with the reviewed hash succeeds");

        assert_eq!(
            store.get(id).expect("present").status,
            ProposalStatus::Denied
        );
        assert_eq!(
            record.decision,
            Decision::Denied {
                reason: "eval criteria too vague".to_string()
            }
        );
        let history = store.approval_history(id);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].decision, record.decision);
    }

    #[test]
    fn already_decided_proposal_cannot_be_redecided() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        let hash = store.get(id).expect("present").content_hash();
        store
            .approve(id, hash, operator(), at(2000))
            .expect("first decision succeeds");

        let err = store
            .deny(id, hash, operator(), "changed my mind", at(2001))
            .expect_err("a decided proposal is immutable through this path");
        assert_eq!(
            err,
            GrowthError::AlreadyDecided {
                id,
                status: ProposalStatus::Approved
            }
        );
        assert_eq!(
            store.approval_history(id).len(),
            1,
            "epäonnistunut uudelleenpäätös ei lisää kirjauksia"
        );
    }

    /// TOCTOU scenario: a proposal is reviewed, then the same identifier is
    /// re-recorded with different content — the stale reviewed hash must
    /// NOT be able to approve the new content.
    #[test]
    fn rerecord_with_same_id_invalidates_stale_reviewed_hash() {
        let mut store = ProposalStore::new();
        let original = sample();
        let id = store.record(original.clone());
        let reviewed_hash = store.get(id).expect("present").content_hash();

        // Content changes between review and decision (same id).
        let mut swapped = sample();
        swapped.id = id;
        swapped.kind = ProposalKind::PolicyFriction {
            summary: "loosen the sandbox write policy".to_string(),
        };
        store.record(swapped);

        let err = store
            .approve(id, reviewed_hash, operator(), at(2000))
            .expect_err("stale hash must not approve swapped content");
        assert_eq!(err, GrowthError::HashMismatch { id });
        assert_eq!(
            store.get(id).expect("present").status,
            ProposalStatus::Pending,
            "vaihdettu sisältö jää odottamaan aitoa katselmointia"
        );
    }

    #[test]
    fn pending_filters_decided() {
        let mut store = ProposalStore::new();
        let a = store.record(sample());
        let _b = store.record(sample());
        let hash = store.get(a).expect("present").content_hash();
        store
            .approve(a, hash, operator(), at(2000))
            .expect("approve succeeds");
        assert_eq!(store.pending().len(), 1, "vain päättämättömät listataan");
        assert_eq!(store.all().len(), 2);
    }

    /// HARD INVARIANT: the stack has NO apply path. This test documents the
    /// structural guarantee — a proposal's lifecycle is data-only, and the
    /// only mutations are status marks + permanent decision records. No
    /// public method changes a skill, policy, or permission. (If someone
    /// adds an `apply` method, this comment + PR review is the gate.)
    #[test]
    fn store_has_no_apply_path_only_records_and_marks_status() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        let hash = store.get(id).expect("present").content_hash();
        // The only status mutations: approve/deny. Approval does NOT apply
        // → status becomes Approved but nothing external changes (this
        // crate touches no skill/policy/permission surface).
        store
            .approve(id, hash, operator(), at(2000))
            .expect("approve succeeds");
        let p = store.get(id).expect("present");
        assert_eq!(p.status, ProposalStatus::Approved);
        // proof_sources + eval remain unchanged (auditability).
        assert_eq!(p.proof_sources, vec!["proof-1", "proof-2"]);
        assert!(!p.eval.description.is_empty());
    }

    #[test]
    fn proposal_roundtrips_json() {
        let p = sample();
        let json = serde_json::to_string(&p).expect("serialize");
        let back: Proposal = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, back);
        assert_eq!(
            p.content_hash(),
            back.content_hash(),
            "hajaute säilyy sarjallistuskierroksen yli"
        );
    }

    #[test]
    fn approval_record_roundtrips_json() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        let hash = store.get(id).expect("present").content_hash();
        let record = store
            .deny(id, hash, operator(), "needs a sharper eval", at(4000))
            .expect("deny succeeds");
        let json = serde_json::to_string(&record).expect("serialize");
        let back: ApprovalRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(record, back);
    }

    #[test]
    fn growth_error_messages_are_descriptive() {
        let id = ProposalId::new();
        assert!(GrowthError::HashMismatch { id }
            .to_string()
            .contains("deny-by-default"));
        assert!(GrowthError::ProposalNotFound { id }
            .to_string()
            .contains("not found"));
        assert!(GrowthError::AlreadyDecided {
            id,
            status: ProposalStatus::Denied
        }
        .to_string()
        .contains("already decided"));
    }
}

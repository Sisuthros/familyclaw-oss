//! Dependability Harness adapter for action pipeline outcomes.
//!
//! The adapter deliberately does not upgrade the current status-based
//! [`crate::VerificationResult`] into independent evidence. An executor's own
//! success status is a claim; a critical side effect still needs a separate
//! postcondition/read-back adapter before its receipt can pass.

use familyclaw_core::Timestamp;
use familyclaw_harness::{
    DependabilityDimension, DependabilityPolicy, DependabilityReceipt, EvidenceCheck,
    EvidenceStrength,
};

use crate::executor::ActionStatus;
use crate::skills::PipelineOutcome;
use crate::task::TaskStatus;

/// Policy for read-only, non-side-effect actions.
///
/// This compatibility profile accepts the action's status verification as a
/// claim, while still requiring structural pipeline and audit evidence.
#[must_use]
pub fn read_only_action_policy() -> DependabilityPolicy {
    DependabilityPolicy::new()
        .require(DependabilityDimension::Tool, EvidenceStrength::Structural)
        .require(
            DependabilityDimension::Validation,
            EvidenceStrength::Claimed,
        )
        .require(
            DependabilityDimension::Observability,
            EvidenceStrength::Structural,
        )
}

/// Fail-closed policy for externally visible side effects.
///
/// Existing status-only verification cannot pass this profile: validation
/// must be independent, and governance + replay/idempotency evidence must be
/// present.
#[must_use]
pub fn critical_action_policy() -> DependabilityPolicy {
    DependabilityPolicy::new()
        .require(DependabilityDimension::Tool, EvidenceStrength::Structural)
        .require(
            DependabilityDimension::Validation,
            EvidenceStrength::Independent,
        )
        .require(
            DependabilityDimension::Governance,
            EvidenceStrength::Structural,
        )
        .require(
            DependabilityDimension::Recovery,
            EvidenceStrength::Structural,
        )
        .require(
            DependabilityDimension::Observability,
            EvidenceStrength::Structural,
        )
}

/// Converts an action pipeline outcome into a redacted Dependability Receipt.
///
/// `trace_id` is supplied by the caller so the receipt can join an existing
/// W3C/turn/workflow trace. The adapter stores only typed identifiers, safe
/// summaries and references already present in the redacted proof bundle.
#[must_use]
pub fn action_dependability_receipt(
    outcome: &PipelineOutcome,
    trace_id: impl Into<String>,
    generated_at: Timestamp,
    policy: &DependabilityPolicy,
) -> DependabilityReceipt {
    let mut checks = Vec::new();

    if outcome.awaiting_approval || outcome.status == TaskStatus::NeedsApproval {
        checks.push(EvidenceCheck::passed(
            DependabilityDimension::Governance,
            EvidenceStrength::Structural,
            "policy_paused_for_approval",
            "policy prevented execution until human approval",
        ));
    }

    match outcome.proof.as_ref() {
        None => {
            checks.push(EvidenceCheck::failed(
                DependabilityDimension::Tool,
                EvidenceStrength::Structural,
                "proof_bundle_missing",
                "action outcome has no proof bundle",
            ));
        }
        Some(proof) => {
            let proof_ref = format!("proof:{}", proof.id);
            let identity_matches = proof.task_id == outcome.task_id
                && proof.action_id == outcome.action_id
                && proof.status == ActionStatus::Succeeded
                && outcome.status == TaskStatus::Done;
            let tool_check = if identity_matches {
                EvidenceCheck::passed(
                    DependabilityDimension::Tool,
                    EvidenceStrength::Structural,
                    "pipeline_completion_contract",
                    "task/action identifiers and successful terminal states agree",
                )
            } else {
                EvidenceCheck::failed(
                    DependabilityDimension::Tool,
                    EvidenceStrength::Structural,
                    "pipeline_completion_contract",
                    "proof identity or terminal state does not match the pipeline outcome",
                )
            };
            checks.push(tool_check.with_evidence_ref(proof_ref.clone()));

            let validation_check = if proof.verification.verified {
                EvidenceCheck::passed(
                    DependabilityDimension::Validation,
                    EvidenceStrength::Claimed,
                    "executor_status_verification",
                    "executor success status passed the current pipeline verification",
                )
            } else {
                EvidenceCheck::failed(
                    DependabilityDimension::Validation,
                    EvidenceStrength::Claimed,
                    "executor_status_verification",
                    "current pipeline verification failed",
                )
            };
            checks.push(validation_check.with_evidence_ref(proof_ref));

            if proof.audit_event_ids.is_empty() {
                checks.push(EvidenceCheck::failed(
                    DependabilityDimension::Observability,
                    EvidenceStrength::Structural,
                    "action_audit_correlation",
                    "proof bundle has no correlated audit events",
                ));
            } else {
                let mut audit_check = EvidenceCheck::passed(
                    DependabilityDimension::Observability,
                    EvidenceStrength::Structural,
                    "action_audit_correlation",
                    "proof bundle references correlated action audit events",
                );
                for id in &proof.audit_event_ids {
                    audit_check = audit_check.with_evidence_ref(format!("audit:{id}"));
                }
                checks.push(audit_check);
            }
        }
    }

    DependabilityReceipt::evaluate(
        outcome.task_id.to_string(),
        trace_id,
        generated_at,
        checks,
        policy,
    )
}

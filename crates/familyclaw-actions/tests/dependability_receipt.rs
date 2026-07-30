//! End-to-end action adapter tests for the Dependability Harness.

use familyclaw_actions::task::ActionTask;
use familyclaw_actions::{
    action_dependability_receipt, critical_action_policy, read_only_action_policy,
    DiscordThreadSummaryMock, Pipeline,
};
use familyclaw_core::time::{from_unix_secs, Timestamp};
use familyclaw_harness::{
    DependabilityDimension, EvidenceState, EvidenceStrength, FailureCode, GateStatus,
};
use serde_json::json;

fn at(secs: i64) -> Timestamp {
    from_unix_secs(secs).expect("valid timestamp")
}

async fn successful_read_only_outcome() -> familyclaw_actions::PipelineOutcome {
    let skill = DiscordThreadSummaryMock::new();
    let mut pipeline = Pipeline::new();
    pipeline.register_skill(&skill).expect("register skill");
    let task = ActionTask::new(
        DiscordThreadSummaryMock::skill_id(),
        json!({
            "thread": [
                { "author": "agent_a", "text": "We should ship the fix" },
                { "author": "agent_b", "text": "Agreed" }
            ]
        }),
        at(1_700_000_100),
    );

    pipeline
        .run(&skill, task, at(1_700_000_101))
        .await
        .expect("pipeline run")
}

#[tokio::test]
async fn current_status_only_verification_cannot_pass_critical_action_policy() {
    let outcome = successful_read_only_outcome().await;
    assert!(outcome.is_done(), "the legacy pipeline reports Done");

    let receipt = action_dependability_receipt(
        &outcome,
        "trace-action-1",
        at(1_700_000_102),
        &critical_action_policy(),
    );

    assert_eq!(receipt.status(), GateStatus::Blocked);
    assert!(receipt.failures().iter().any(|failure| {
        failure.code == FailureCode::InsufficientEvidence
            && failure.dimension == Some(DependabilityDimension::Validation)
    }));
}

#[tokio::test]
async fn read_only_policy_passes_but_labels_status_check_as_claimed_evidence() {
    let outcome = successful_read_only_outcome().await;

    let receipt = action_dependability_receipt(
        &outcome,
        "trace-action-2",
        at(1_700_000_103),
        &read_only_action_policy(),
    );

    assert_eq!(receipt.status(), GateStatus::Passed);
    let validation = receipt
        .checks()
        .iter()
        .find(|check| check.dimension == DependabilityDimension::Validation)
        .expect("validation check");
    assert_eq!(validation.state, EvidenceState::Passed);
    assert_eq!(validation.strength, Some(EvidenceStrength::Claimed));
}

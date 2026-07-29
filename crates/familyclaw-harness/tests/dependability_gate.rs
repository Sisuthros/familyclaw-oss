//! Fail-closed behavior tests for dependability receipts and policies.

use familyclaw_core::time::from_unix_secs;
use familyclaw_harness::{
    DependabilityDimension, DependabilityPolicy, DependabilityReceipt, EvidenceCheck,
    EvidenceStrength, FailureCode, GateStatus,
};

fn at(secs: i64) -> familyclaw_core::Timestamp {
    from_unix_secs(secs).expect("valid timestamp")
}

#[test]
fn claimed_success_cannot_satisfy_independent_validation() {
    let policy = DependabilityPolicy::new()
        .require(
            DependabilityDimension::Context,
            EvidenceStrength::Structural,
        )
        .require(
            DependabilityDimension::Validation,
            EvidenceStrength::Independent,
        );
    let checks = vec![
        EvidenceCheck::passed(
            DependabilityDimension::Context,
            EvidenceStrength::Structural,
            "context_manifest",
            "context sources and compaction recorded",
        ),
        EvidenceCheck::passed(
            DependabilityDimension::Validation,
            EvidenceStrength::Claimed,
            "executor_success",
            "executor reported success",
        ),
    ];

    let receipt =
        DependabilityReceipt::evaluate("task-1", "trace-1", at(1_700_000_000), checks, &policy);

    assert_eq!(receipt.status(), GateStatus::Blocked);
    assert!(receipt.failures().iter().any(|failure| {
        failure.code == FailureCode::InsufficientEvidence
            && failure.dimension == Some(DependabilityDimension::Validation)
    }));
}

#[test]
fn all_required_dimensions_with_sufficient_evidence_pass() {
    let policy = DependabilityPolicy::new()
        .require(
            DependabilityDimension::Context,
            EvidenceStrength::Structural,
        )
        .require(
            DependabilityDimension::Validation,
            EvidenceStrength::Independent,
        );
    let checks = vec![
        EvidenceCheck::passed(
            DependabilityDimension::Context,
            EvidenceStrength::Structural,
            "context_manifest",
            "context manifest persisted",
        )
        .with_evidence_ref("audit:context-7"),
        EvidenceCheck::passed(
            DependabilityDimension::Validation,
            EvidenceStrength::Independent,
            "external_readback",
            "external state matched the requested postcondition",
        )
        .with_evidence_ref("proof:42"),
    ];

    let receipt =
        DependabilityReceipt::evaluate("task-2", "trace-2", at(1_700_000_001), checks, &policy);

    assert_eq!(receipt.status(), GateStatus::Passed);
    assert!(receipt.failures().is_empty());
    assert_eq!(receipt.schema_version(), 1);
}

#[test]
fn missing_required_dimension_blocks() {
    let policy = DependabilityPolicy::new().require(
        DependabilityDimension::Observability,
        EvidenceStrength::Structural,
    );

    let receipt =
        DependabilityReceipt::evaluate("turn-1", "trace-3", at(1_700_000_002), Vec::new(), &policy);

    assert_eq!(receipt.status(), GateStatus::Blocked);
    assert!(receipt.failures().iter().any(|failure| {
        failure.code == FailureCode::MissingDimension
            && failure.dimension == Some(DependabilityDimension::Observability)
    }));
}

#[test]
fn explicit_failed_check_always_blocks() {
    let policy = DependabilityPolicy::new().require(
        DependabilityDimension::Context,
        EvidenceStrength::Structural,
    );
    let checks = vec![
        EvidenceCheck::passed(
            DependabilityDimension::Context,
            EvidenceStrength::Structural,
            "context_manifest",
            "context recorded",
        ),
        EvidenceCheck::failed(
            DependabilityDimension::Recovery,
            EvidenceStrength::Structural,
            "resume_persist",
            "suspend state was not durable",
        ),
    ];

    let receipt =
        DependabilityReceipt::evaluate("turn-2", "trace-4", at(1_700_000_003), checks, &policy);

    assert_eq!(receipt.status(), GateStatus::Blocked);
    assert!(receipt
        .failures()
        .iter()
        .any(|failure| failure.code == FailureCode::FailedCheck));
}

#[test]
fn not_applicable_does_not_satisfy_a_required_dimension() {
    let policy = DependabilityPolicy::new().require(
        DependabilityDimension::Governance,
        EvidenceStrength::Structural,
    );
    let checks = vec![EvidenceCheck::not_applicable(
        DependabilityDimension::Governance,
        "human_approval",
        "caller claimed approval was not applicable",
    )];

    let receipt =
        DependabilityReceipt::evaluate("action-1", "trace-5", at(1_700_000_004), checks, &policy);

    assert_eq!(receipt.status(), GateStatus::Blocked);
    assert!(receipt
        .failures()
        .iter()
        .any(|failure| failure.code == FailureCode::MissingDimension));
}

#[test]
fn empty_subject_or_trace_id_blocks_an_otherwise_valid_receipt() {
    let policy = DependabilityPolicy::new().require(
        DependabilityDimension::Context,
        EvidenceStrength::Structural,
    );
    let checks = vec![EvidenceCheck::passed(
        DependabilityDimension::Context,
        EvidenceStrength::Structural,
        "context_manifest",
        "context manifest captured",
    )];

    let receipt = DependabilityReceipt::evaluate("   ", "", at(1_700_000_005), checks, &policy);

    assert_eq!(receipt.status(), GateStatus::Blocked);
    assert_eq!(
        receipt
            .failures()
            .iter()
            .filter(|failure| failure.code == FailureCode::InvalidIdentity)
            .count(),
        2
    );
}

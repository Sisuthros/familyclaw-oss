//! Model-agnostic dependability receipts and fail-closed completion gates.
//!
//! A model, provider, tool, or executor may contribute evidence, but none of
//! them may declare a task complete. [`DependabilityReceipt::evaluate`]
//! computes the only completion verdict exposed by this crate.

use std::collections::BTreeMap;

use familyclaw_core::Timestamp;
use serde::Serialize;

/// Current serialized receipt schema.
pub const RECEIPT_SCHEMA_VERSION: u16 = 1;

/// The system capability to which an evidence check belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependabilityDimension {
    /// Prompt/context assembly, source manifest, budgets and compaction.
    Context,
    /// Retrieved evidence, source identity and provenance.
    Retrieval,
    /// Memory admission, isolation and persistence.
    Memory,
    /// Model route attempts, failure classes and selected route.
    Model,
    /// Tool contract, taint, idempotency and side-effect identity.
    Tool,
    /// Output postconditions and independent read-back.
    Validation,
    /// Retry, replay, suspend and crash-boundary evidence.
    Recovery,
    /// Policy, approval and human intervention.
    Governance,
    /// Trace, audit and receipt persistence.
    Observability,
}

/// Strength of an evidence observation.
///
/// Declaration order is meaningful: stronger evidence compares greater than
/// weaker evidence, so an independent check can satisfy a structural policy,
/// while an executor's own claim cannot satisfy independent validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStrength {
    /// Self-report from a model, provider, tool, or executor.
    Claimed,
    /// An invariant checked by the `FamilyClaw` harness.
    Structural,
    /// A postcondition checked independently of the actor that did the work.
    Independent,
}

/// Observed result of one evidence check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceState {
    /// The check ran and passed.
    Passed,
    /// The check ran and failed. Any explicit failure blocks the gate.
    Failed,
    /// The check was considered and explicitly does not apply.
    NotApplicable,
}

/// One redacted evidence observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvidenceCheck {
    /// Harness dimension covered by this check.
    pub dimension: DependabilityDimension,
    /// Strength of positive/negative evidence. `None` for not-applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strength: Option<EvidenceStrength>,
    /// Stable machine-readable check name.
    pub name: String,
    /// Redacted operator-safe explanation. Never raw prompt/tool data.
    pub summary: String,
    /// Check state.
    pub state: EvidenceState,
    /// Stable references to existing redacted proof/audit records.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
}

impl EvidenceCheck {
    /// Builds a passing check.
    #[must_use]
    pub fn passed(
        dimension: DependabilityDimension,
        strength: EvidenceStrength,
        name: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            dimension,
            strength: Some(strength),
            name: name.into(),
            summary: summary.into(),
            state: EvidenceState::Passed,
            evidence_refs: Vec::new(),
        }
    }

    /// Builds an explicitly failed check.
    #[must_use]
    pub fn failed(
        dimension: DependabilityDimension,
        strength: EvidenceStrength,
        name: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            dimension,
            strength: Some(strength),
            name: name.into(),
            summary: summary.into(),
            state: EvidenceState::Failed,
            evidence_refs: Vec::new(),
        }
    }

    /// Builds an explicit not-applicable check.
    #[must_use]
    pub fn not_applicable(
        dimension: DependabilityDimension,
        name: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            dimension,
            strength: None,
            name: name.into(),
            summary: summary.into(),
            state: EvidenceState::NotApplicable,
            evidence_refs: Vec::new(),
        }
    }

    /// Adds a stable reference to a redacted proof or audit record.
    #[must_use]
    pub fn with_evidence_ref(mut self, evidence_ref: impl Into<String>) -> Self {
        self.evidence_refs.push(evidence_ref.into());
        self
    }
}

/// Minimum evidence strengths required for completion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DependabilityPolicy {
    required: BTreeMap<DependabilityDimension, EvidenceStrength>,
}

impl DependabilityPolicy {
    /// Creates an empty policy. Explicit failed checks still block it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requires a dimension at the specified minimum evidence strength.
    #[must_use]
    pub fn require(mut self, dimension: DependabilityDimension, minimum: EvidenceStrength) -> Self {
        self.required.insert(dimension, minimum);
        self
    }

    /// Read-only requirements for policy inspection/export.
    #[must_use]
    pub const fn requirements(&self) -> &BTreeMap<DependabilityDimension, EvidenceStrength> {
        &self.required
    }
}

/// Machine-readable reason why a gate blocked completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    /// A receipt subject or trace identifier was empty.
    InvalidIdentity,
    /// No passing evidence existed for a required dimension.
    MissingDimension,
    /// Passing evidence existed, but its strength was below policy.
    InsufficientEvidence,
    /// A check explicitly reported failure.
    FailedCheck,
}

/// One fail-closed gate finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GateFailure {
    /// Stable failure classification.
    pub code: FailureCode,
    /// Related dimension, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<DependabilityDimension>,
    /// Related check name, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_name: Option<String>,
    /// Redacted operator-safe explanation.
    pub summary: String,
}

/// Computed completion verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    /// Every policy requirement passed and no check explicitly failed.
    Passed,
    /// Completion was refused.
    Blocked,
}

/// Redacted, machine-readable evidence for a turn/task/workflow verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DependabilityReceipt {
    schema_version: u16,
    subject_id: String,
    trace_id: String,
    generated_at: Timestamp,
    status: GateStatus,
    checks: Vec<EvidenceCheck>,
    failures: Vec<GateFailure>,
}

impl DependabilityReceipt {
    /// Evaluates evidence against policy and computes the final status.
    ///
    /// Callers do not supply a status. Unknown/missing/weak evidence therefore
    /// cannot be converted into a successful receipt by self-report.
    #[must_use]
    pub fn evaluate(
        subject_id: impl Into<String>,
        trace_id: impl Into<String>,
        generated_at: Timestamp,
        checks: Vec<EvidenceCheck>,
        policy: &DependabilityPolicy,
    ) -> Self {
        let subject_id = subject_id.into();
        let trace_id = trace_id.into();
        let mut failures = Vec::new();

        if subject_id.trim().is_empty() {
            failures.push(GateFailure {
                code: FailureCode::InvalidIdentity,
                dimension: None,
                check_name: Some("subject_id".to_string()),
                summary: "receipt subject_id must not be empty".to_string(),
            });
        }
        if trace_id.trim().is_empty() {
            failures.push(GateFailure {
                code: FailureCode::InvalidIdentity,
                dimension: None,
                check_name: Some("trace_id".to_string()),
                summary: "receipt trace_id must not be empty".to_string(),
            });
        }

        for check in checks
            .iter()
            .filter(|check| check.state == EvidenceState::Failed)
        {
            failures.push(GateFailure {
                code: FailureCode::FailedCheck,
                dimension: Some(check.dimension),
                check_name: Some(check.name.clone()),
                summary: check.summary.clone(),
            });
        }

        for (&dimension, &minimum) in policy.requirements() {
            let strongest = checks
                .iter()
                .filter(|check| {
                    check.dimension == dimension && check.state == EvidenceState::Passed
                })
                .filter_map(|check| check.strength)
                .max();

            match strongest {
                None => failures.push(GateFailure {
                    code: FailureCode::MissingDimension,
                    dimension: Some(dimension),
                    check_name: None,
                    summary: format!("required {dimension:?} evidence is missing"),
                }),
                Some(actual) if actual < minimum => failures.push(GateFailure {
                    code: FailureCode::InsufficientEvidence,
                    dimension: Some(dimension),
                    check_name: None,
                    summary: format!(
                        "required {dimension:?} evidence strength is {minimum:?}, strongest is {actual:?}"
                    ),
                }),
                Some(_) => {}
            }
        }

        let status = if failures.is_empty() {
            GateStatus::Passed
        } else {
            GateStatus::Blocked
        };

        Self {
            schema_version: RECEIPT_SCHEMA_VERSION,
            subject_id,
            trace_id,
            generated_at,
            status,
            checks,
            failures,
        }
    }

    /// Serialized schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// Computed completion verdict.
    #[must_use]
    pub const fn status(&self) -> GateStatus {
        self.status
    }

    /// Gate findings; empty only when the receipt passed.
    #[must_use]
    pub fn failures(&self) -> &[GateFailure] {
        &self.failures
    }

    /// Evidence checks in caller-supplied order.
    #[must_use]
    pub fn checks(&self) -> &[EvidenceCheck] {
        &self.checks
    }

    /// Correlated subject identifier.
    #[must_use]
    pub fn subject_id(&self) -> &str {
        &self.subject_id
    }

    /// Cross-layer trace identifier.
    #[must_use]
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    /// Injected generation timestamp.
    #[must_use]
    pub const fn generated_at(&self) -> Timestamp {
        self.generated_at
    }
}

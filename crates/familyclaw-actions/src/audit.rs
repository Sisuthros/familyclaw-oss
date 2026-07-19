//! Audit trail: a tamper-evident event chain for the action pipeline's
//! stages (observe, plan, approve, execute, proof) (Layer A).
//!
//! This module defines:
//! - [`AuditAction`] — what happened (approval granted/consumed/…),
//! - [`ActionAuditEvent`] — a single log event (identifier, moment, reason),
//! - [`AuditLog`] — an in-memory append-only log of events.
//!
//! ## Determinism
//! Events take their timestamp injected
//! ([`familyclaw_core::time::Timestamp`]) — the clock is never read inside
//! this module's logic, so tests and replay stay deterministic.
//!
//! ## OSS boundary
//! Events contain no secrets: the free-form `reason` field is meant for a
//! short human-readable explanation, not a payload or keys.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use familyclaw_core::time::Timestamp;

use crate::ids::{ActionId, ApprovalId, AuditEventId};

/// Module readiness level — kept so [`crate::all_modules_scaffolded`]
/// still compiles alongside other modules still in scaffold stage.
pub(crate) const SCAFFOLDED: bool = true;

/// Type of an audit event: what happened in the action pipeline.
///
/// Serializes to `snake_case`, so the log can be filtered and read
/// by machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    /// Approval was granted (human-in-the-loop approved the action).
    ApprovalGranted,
    /// Approval was consumed successfully (the one-shot use was consumed).
    ApprovalConsumed,
    /// Approval was denied (the human refused).
    ApprovalDenied,
    /// Approval was found expired during a consumption attempt.
    ApprovalExpired,
    /// Consuming the approval failed (e.g. the payload hash did not match,
    /// or the approval had already been used).
    ApprovalRejected,
}

/// A single audit-log event from the action pipeline's approval stage.
///
/// Each event carries its own identifier, the identifier of the target
/// action ([`ActionId`]) and of the possible approval ([`ApprovalId`]), as
/// well as a timestamp and a short human-readable reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionAuditEvent {
    /// The event's unique identifier.
    pub id: AuditEventId,
    /// The event's type (what happened).
    pub action: AuditAction,
    /// The action the event relates to.
    pub action_id: ActionId,
    /// The approval's identifier if the event concerns a specific approval
    /// (`None` e.g. for a denial before an approval exists).
    pub approval_id: Option<ApprovalId>,
    /// The event's moment (injected — not read from the clock).
    pub at: Timestamp,
    /// Short human-readable explanation (NO secrets or payload).
    pub reason: String,
}

impl ActionAuditEvent {
    /// Builds a new audit event with a fresh identifier.
    ///
    /// The timestamp and reason are given by the caller; the identifier is
    /// generated randomly ([`AuditEventId::new`]).
    #[must_use]
    pub fn new(
        action: AuditAction,
        action_id: ActionId,
        approval_id: Option<ApprovalId>,
        at: Timestamp,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id: AuditEventId::new(),
            action,
            action_id,
            approval_id,
            at,
            reason: reason.into(),
        }
    }
}

/// In-memory append-only audit log.
///
/// The log is intentionally append-only (`append`): events are never
/// deleted or modified, which supports the tamper-evident property. This
/// Layer A implementation keeps events in memory; durable storage is the
/// substrate layer's responsibility.
#[derive(Debug, Clone, Default)]
pub struct AuditLog {
    /// Events in chronological insertion order.
    events: Vec<ActionAuditEvent>,
}

impl AuditLog {
    /// Creates a new empty audit log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends the event to the end of the log and returns the appended
    /// event's identifier.
    pub fn append(&mut self, event: ActionAuditEvent) -> AuditEventId {
        let id = event.id;
        self.events.push(event);
        id
    }

    /// Number of events in the log.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the log is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// All events in insertion order.
    #[must_use]
    pub fn events(&self) -> &[ActionAuditEvent] {
        &self.events
    }

    /// All events for a given action ([`ActionId`]) in insertion order.
    #[must_use]
    pub fn events_for(&self, action_id: ActionId) -> Vec<&ActionAuditEvent> {
        self.events
            .iter()
            .filter(|e| e.action_id == action_id)
            .collect()
    }

    /// Whether the log contains at least one event of the given type.
    #[must_use]
    pub fn contains_action(&self, action: AuditAction) -> bool {
        self.events.iter().any(|e| e.action == action)
    }
}

/// Type of an execution-pipeline (executor → verify → proof) audit event.
///
/// Broader than [`AuditAction`], which covers only the approval stage.
/// `AuditKind` describes the events of interest for the whole action
/// pipeline: approval, execution, redaction, taint marking, and policy
/// denial. Serializes to `snake_case` for machine filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    /// Approval was granted.
    ApprovalGranted,
    /// Approval was consumed (the one-shot use was consumed).
    ApprovalConsumed,
    /// Approval was denied.
    ApprovalDenied,
    /// Approval was found expired.
    ApprovalExpired,
    /// Action execution started.
    ActionStarted,
    /// Action execution succeeded.
    ActionSucceeded,
    /// Action execution failed.
    ActionFailed,
    /// Secret-looking values were redacted from the proof.
    RedactionApplied,
    /// The output was marked untrusted (taint).
    TaintMarked,
    /// Policy blocked the action.
    PolicyDenied,
    /// **The agent's turn started** — the tool loop's first round began.
    /// (TURN-AUDIT, roadmap §6 D6.)
    TurnStarted,
    /// **A tool call was dispatched** inside the tool loop (the skill's
    /// name + a redacted result in the `detail` field, never the raw
    /// payload). (TURN-AUDIT, roadmap §6 D6.)
    ToolDispatched,
    /// **The turn was suspended** waiting for human approval (suspend):
    /// `detail` carries the approval's identifier + a redacted summary.
    /// (TURN-AUDIT, roadmap §6 D6.)
    TurnSuspended,
    /// **A suspended turn was resumed** after approval was granted.
    /// (TURN-AUDIT, roadmap §6 D6.)
    TurnResumed,
    /// **The turn ended with a final answer** (`stop_reason` = answered).
    /// (TURN-AUDIT, roadmap §6 D6.)
    TurnAnswered,
    /// **The turn hit the iteration limit** without an answer
    /// (`stop_reason` = max-iter). (TURN-AUDIT, roadmap §6 D6.)
    TurnMaxIterations,
}

/// A single execution-pipeline audit event.
///
/// Unlike [`ActionAuditEvent`] (approval-specific), this describes any
/// [`AuditKind`] event with a free-form `detail` explanation.
///
/// ## OSS boundary
/// `detail` is meant for a short human-readable explanation — **never a raw
/// secret, token, or payload**. Secret values are redacted in the proof
/// bundle ([`crate::proof`]) and never end up in this field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecAuditEvent {
    /// The event's unique identifier.
    pub id: AuditEventId,
    /// The event's type.
    pub kind: AuditKind,
    /// The event's moment (injected — not read from the clock).
    pub at: Timestamp,
    /// The action the event relates to.
    pub action_id: ActionId,
    /// Short human-readable explanation (NO raw secrets).
    pub detail: String,
}

impl ExecAuditEvent {
    /// Builds a new execution-pipeline audit event with a fresh identifier.
    ///
    /// The timestamp and explanation are given by the caller; the
    /// identifier is generated randomly ([`AuditEventId::new`]).
    #[must_use]
    pub fn new(
        kind: AuditKind,
        action_id: ActionId,
        at: Timestamp,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: AuditEventId::new(),
            kind,
            at,
            action_id,
            detail: detail.into(),
        }
    }
}

/// A thread-safe in-memory collector for execution-pipeline audit events.
///
/// Events are held behind a [`std::sync::Mutex`] lock, so the collector can
/// be shared across concurrent executions. The collector is append-only:
/// events are never deleted or modified (tamper-evident). Durable storage
/// is the substrate layer's responsibility.
#[derive(Debug, Default)]
pub struct AuditCollector {
    /// Events in insertion order, behind the lock.
    events: Mutex<Vec<ExecAuditEvent>>,
}

impl AuditCollector {
    /// Creates a new empty collector.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the event and returns its identifier.
    ///
    /// If the lock is poisoned (a panic in another thread), the lock is
    /// recovered properly ([`std::sync::PoisonError::into_inner`]) to avoid
    /// data loss — recording never panics.
    pub fn record(&self, event: ExecAuditEvent) -> AuditEventId {
        let id = event.id;
        let mut guard = match self.events.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.push(event);
        id
    }

    /// Returns all recorded events in insertion order (a copy).
    #[must_use]
    pub fn list(&self) -> Vec<ExecAuditEvent> {
        match self.events.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Returns all events for a given [`ActionId`] in insertion order
    /// (a copy).
    ///
    /// Used, among other things, in the **turn-audit** (roadmap §6 D6)
    /// operator lookup: a single agent turn is identified by one
    /// [`ActionId`], so the full audit trail of one turn (start → tool
    /// calls → suspend/resume → `stop_reason`) is obtained by filtering on
    /// this identifier. The output never contains raw secrets — `detail` is
    /// already redacted at recording time.
    #[must_use]
    pub fn events_for(&self, action_id: ActionId) -> Vec<ExecAuditEvent> {
        let guard = match self.events.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard
            .iter()
            .filter(|e| e.action_id == action_id)
            .cloned()
            .collect()
    }

    /// Number of recorded events.
    #[must_use]
    pub fn len(&self) -> usize {
        match self.events.lock() {
            Ok(guard) => guard.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// Whether the collector is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time::from_unix_secs;

    fn ts() -> Timestamp {
        from_unix_secs(1_700_000_000).expect("valid unix seconds")
    }

    #[test]
    fn audit_kind_serde_snake_case() {
        let json = serde_json::to_string(&AuditKind::ActionSucceeded).expect("serialize");
        assert_eq!(json, "\"action_succeeded\"");
        let back: AuditKind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, AuditKind::ActionSucceeded);
    }

    #[test]
    fn collector_record_and_list() {
        let collector = AuditCollector::new();
        assert!(collector.is_empty());
        let action_id = ActionId::new();
        let id = collector.record(ExecAuditEvent::new(
            AuditKind::ActionStarted,
            action_id,
            ts(),
            "aloitettu",
        ));
        assert_eq!(collector.len(), 1);
        assert!(!collector.is_empty());
        let listed = collector.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].kind, AuditKind::ActionStarted);
        assert_eq!(listed[0].action_id, action_id);
    }

    #[test]
    fn audit_action_serde_snake_case() {
        let json = serde_json::to_string(&AuditAction::ApprovalGranted).expect("serialize");
        assert_eq!(json, "\"approval_granted\"");
        let back: AuditAction = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, AuditAction::ApprovalGranted);
    }

    #[test]
    fn turn_audit_kinds_serde_snake_case() {
        // TURN-AUDIT variants serialize to snake_case for machine
        // filtering (the operator's view).
        for (kind, expected) in [
            (AuditKind::TurnStarted, "\"turn_started\""),
            (AuditKind::ToolDispatched, "\"tool_dispatched\""),
            (AuditKind::TurnSuspended, "\"turn_suspended\""),
            (AuditKind::TurnResumed, "\"turn_resumed\""),
            (AuditKind::TurnAnswered, "\"turn_answered\""),
            (AuditKind::TurnMaxIterations, "\"turn_max_iterations\""),
        ] {
            let json = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(json, expected, "{kind:?} serializes to {expected}");
            let back: AuditKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn collector_events_for_filters_by_action_id() {
        let collector = AuditCollector::new();
        let turn_a = ActionId::new();
        let turn_b = ActionId::new();
        collector.record(ExecAuditEvent::new(
            AuditKind::TurnStarted,
            turn_a,
            ts(),
            "turn a alkoi",
        ));
        collector.record(ExecAuditEvent::new(
            AuditKind::TurnStarted,
            turn_b,
            ts(),
            "turn b alkoi",
        ));
        collector.record(ExecAuditEvent::new(
            AuditKind::TurnAnswered,
            turn_a,
            ts(),
            "turn a vastasi",
        ));

        let a_events = collector.events_for(turn_a);
        assert_eq!(a_events.len(), 2, "vain turn a:n tapahtumat");
        assert!(a_events.iter().all(|e| e.action_id == turn_a));
        assert_eq!(collector.events_for(turn_b).len(), 1);
        // Unknown identifier → empty.
        assert!(collector.events_for(ActionId::new()).is_empty());
    }

    #[test]
    fn append_increments_len_and_returns_id() {
        let mut log = AuditLog::new();
        assert!(log.is_empty());
        let action_id = ActionId::new();
        let event = ActionAuditEvent::new(
            AuditAction::ApprovalGranted,
            action_id,
            None,
            ts(),
            "myönnetty",
        );
        let id = log.append(event);
        assert_eq!(log.len(), 1);
        assert!(!log.is_empty());
        assert_eq!(log.events()[0].id, id);
    }

    #[test]
    fn events_for_filters_by_action_id() {
        let mut log = AuditLog::new();
        let a = ActionId::new();
        let b = ActionId::new();
        log.append(ActionAuditEvent::new(
            AuditAction::ApprovalGranted,
            a,
            None,
            ts(),
            "a granted",
        ));
        log.append(ActionAuditEvent::new(
            AuditAction::ApprovalGranted,
            b,
            None,
            ts(),
            "b granted",
        ));
        assert_eq!(log.events_for(a).len(), 1);
        assert_eq!(log.events_for(a)[0].action_id, a);
    }

    #[test]
    fn contains_action_detects_presence() {
        let mut log = AuditLog::new();
        assert!(!log.contains_action(AuditAction::ApprovalDenied));
        log.append(ActionAuditEvent::new(
            AuditAction::ApprovalDenied,
            ActionId::new(),
            None,
            ts(),
            "denied",
        ));
        assert!(log.contains_action(AuditAction::ApprovalDenied));
    }
}

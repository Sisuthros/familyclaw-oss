//! Turn audit recording and observability metric emission for [`super::Agent`].

use super::prelude::*;

use super::{Agent, MetricEvent};

impl Agent {
    /// The agent's **turn audit collector**, if installed (shared handle).
    ///
    /// The operator can read the entire recorded trace
    /// ([`AuditCollector::list`]) or filter to a single turn
    /// ([`turn_audit_for`](Agent::turn_audit_for)). `None` if no audit has
    /// been wired ([`with_turn_audit`](Agent::with_turn_audit)).
    #[must_use]
    pub fn turn_audit(&self) -> Option<Arc<AuditCollector>> {
        self.turn_audit.clone()
    }

    /// Retrieves **a single turn's full audit trace** by its correlation
    /// identifier (TURN-AUDIT, roadmap §6 D6).
    ///
    /// `turn_id` is the [`ActionId`] that [`handle_turn_with_origin`] /
    /// [`resume_approved`] generates at the start of the turn, and with
    /// which all events of that same turn (start → tool calls →
    /// suspend/resume → `stop_reason`) are marked. Returns events in
    /// insertion order, or an empty list if no audit is wired or there are
    /// no events for the identifier.
    ///
    /// The output never contains raw secrets — every event's
    /// `detail` was already redacted at the moment of recording.
    ///
    /// [`handle_turn_with_origin`]: Agent::handle_turn_with_origin
    /// [`resume_approved`]: Agent::resume_approved
    #[must_use]
    pub fn turn_audit_for(&self, turn_id: ActionId) -> Vec<ExecAuditEvent> {
        self.turn_audit
            .as_ref()
            .map(|a| a.events_for(turn_id))
            .unwrap_or_default()
    }

    /// Records a single turn-audit event, if a collector is installed (TURN-AUDIT).
    ///
    /// **Secrecy invariant (defense in depth):** `detail` is run through
    /// [`familyclaw_actions::redact_free_text`] before storage, so a
    /// secret embedded in free text (a lone `sk-…` token, `Bearer …`,
    /// `key=value`) is redacted even if the caller accidentally supplies an
    /// unredacted string. `turn_id` correlates all events of the same turn.
    /// `at` (D1) is injected — the clock is not read here.
    ///
    /// No-op when no audit is wired (`turn_audit = None`).
    pub(super) fn record_turn_audit(
        &self,
        turn_id: ActionId,
        kind: AuditKind,
        at: Timestamp,
        detail: impl Into<String>,
    ) {
        // Phase 2: every FRESH (non-replay) tool call bumps the tool metric.
        // Bound to this one audit-recording point, which is already called at
        // EVERY dispatch site (both tool loops) → covers everything without
        // repeating the emit call per site. Replay guard here: replay
        // must not double-count (the audit record, by contrast, is also made
        // during replay, so the emit is guarded separately).
        if matches!(kind, AuditKind::ToolDispatched) && !self.durable.is_replaying() {
            self.emit_metric(MetricEvent::ToolDispatched);
        }
        let Some(audit) = self.turn_audit.as_ref() else {
            return;
        };
        // Defense in depth: redact any secrets possibly embedded in free text
        // before storage. Discard the redaction report (we only need
        // the cleaned string).
        let (safe_detail, _) = familyclaw_actions::redact_free_text(&detail.into());
        audit.record(ExecAuditEvent::new(kind, turn_id, at, safe_detail));
    }

    /// Pushes a [`MetricEvent`] into the observability sink if installed.
    ///
    /// **Call ONLY on a fresh turn** (`!self.durable.is_replaying()`) —
    /// replay must not double-count metrics. Uses [`Sender::try_send`]:
    /// if the channel is full (consumer lagging) or closed, the event
    /// is **dropped** — observability must not block the turn nor grow
    /// the queue unboundedly. No-op when there is no sink.
    pub(super) fn emit_metric(&self, event: MetricEvent) {
        if let Some(sink) = self.metrics_sink.as_ref() {
            let _ = sink.try_send(event);
        }
    }
}

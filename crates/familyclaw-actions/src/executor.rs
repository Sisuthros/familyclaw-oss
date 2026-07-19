//! Execution layer (executor): runs an approved action via the skill and
//! collects the result for verification and proof (Layer A).
//! Mock execution only — no real network calls.
//!
//! This module defines:
//! - [`ActionStatus`] — the action's outcome (succeeded / failed),
//! - [`ActionRequest`] — the execution request (ids, payload, timestamp),
//! - [`ActionResult`] — the execution result (status, summary, redacted output,
//!   taint marker),
//! - [`ActionExecutor`] — an async trait whose implementation runs the action,
//! - [`MockActionExecutor`] — a test-oriented implementation (succeeds/fails).
//!
//! ## Determinism & OSS boundary
//! The execution request carries the timestamp injected ([`Timestamp`]). The
//! mock implementation makes no network calls and never reads the clock
//! inside the logic. The output is marked untrusted by default (`untrusted`),
//! until the source is explicitly trusted.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use familyclaw_core::time::Timestamp;

use crate::ids::{ActionId, ActionTaskId, SkillId};
use crate::Result;

/// Module readiness flag — kept so that [`crate::all_modules_scaffolded`]
/// still compiles alongside the other modules.
pub(crate) const SCAFFOLDED: bool = true;

/// The action's outcome.
///
/// Serializes to `snake_case` form for machine filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    /// The action succeeded.
    Succeeded,
    /// The action failed.
    Failed,
}

impl ActionStatus {
    /// Whether the action succeeded.
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

/// An execution request for a single action.
///
/// Carries all the identifiers the proof bundle needs for traceability, the
/// payload passed to the skill, and the injected timestamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRequest {
    /// The identifier of the action to execute.
    pub action_id: ActionId,
    /// The identifier of the skill to execute.
    pub skill_id: SkillId,
    /// The task this action is executed as part of.
    pub task_id: ActionTaskId,
    /// The input passed to the skill (generic JSON).
    pub payload: Value,
    /// The moment execution starts (injected — never read from the clock).
    pub now: Timestamp,
    /// Whether the input originates from an untrusted source (e.g. an MCP
    /// tool's output). If `true`, the taint **propagates** into the result
    /// ([`ActionResult::propagate_input_taint`]) and the executor cannot wash
    /// it off by marking its own output as trusted. Defaults to `false`.
    pub input_untrusted: bool,
}

impl ActionRequest {
    /// Builds a new execution request with trusted input
    /// (`input_untrusted = false`).
    ///
    /// If the input originates from an untrusted source, mark it via
    /// [`ActionRequest::with_input_taint`], so the taint propagates into the
    /// result and the proof.
    #[must_use]
    pub fn new(
        action_id: ActionId,
        skill_id: SkillId,
        task_id: ActionTaskId,
        payload: Value,
        now: Timestamp,
    ) -> Self {
        Self {
            action_id,
            skill_id,
            task_id,
            payload,
            now,
            input_untrusted: false,
        }
    }

    /// Sets the input's taint state (builder).
    ///
    /// `true` means the input is untrusted (e.g. MCP-sourced) data. Use this
    /// when the request is built from an untrusted result, so the taint
    /// doesn't disappear during execution.
    #[must_use]
    pub const fn with_input_taint(mut self, input_untrusted: bool) -> Self {
        self.input_untrusted = input_untrusted;
        self
    }
}

/// The result of executing an action.
///
/// `raw_output_redacted` is the output produced by execution, which is
/// redacted when the proof bundle is assembled
/// ([`crate::proof::build_proof`]); this field must never reach the proof
/// without redaction. `untrusted` defaults to `true` until the source is
/// explicitly trusted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResult {
    /// The action's outcome.
    pub status: ActionStatus,
    /// A short human-readable summary (NO raw secrets).
    pub output_summary: String,
    /// Whether the output originates from an untrusted source (taint).
    pub untrusted: bool,
    /// The output produced by execution (redacted before being attached to the proof).
    pub raw_output_redacted: Value,
    /// The moment execution finished (injected).
    pub finished_at: Timestamp,
}

impl ActionResult {
    /// A successful result. The output is marked untrusted by default.
    #[must_use]
    pub fn success(
        output_summary: impl Into<String>,
        raw_output: Value,
        finished_at: Timestamp,
    ) -> Self {
        Self {
            status: ActionStatus::Succeeded,
            output_summary: output_summary.into(),
            untrusted: true,
            raw_output_redacted: raw_output,
            finished_at,
        }
    }

    /// A failed result.
    #[must_use]
    pub fn failure(output_summary: impl Into<String>, finished_at: Timestamp) -> Self {
        Self {
            status: ActionStatus::Failed,
            output_summary: output_summary.into(),
            untrusted: true,
            raw_output_redacted: Value::Null,
            finished_at,
        }
    }

    /// Marks the output as trusted (removes the taint marker).
    ///
    /// Use only when the source has been explicitly established as trusted.
    #[must_use]
    pub const fn trusted(mut self) -> Self {
        self.untrusted = false;
        self
    }

    /// Propagates the input's taint into the result **monotonically**.
    ///
    /// If the input was untrusted (`input_untrusted = true`), the result is
    /// marked untrusted regardless of what the executor itself set. Taint can
    /// only **increase**, never disappear: a trusted executor cannot wash an
    /// untrusted input clean. If the input was trusted, this call does not
    /// change the result's own taint state.
    #[must_use]
    pub const fn propagate_input_taint(mut self, input_untrusted: bool) -> Self {
        if input_untrusted {
            self.untrusted = true;
        }
        self
    }
}

/// An action executor.
///
/// The implementation runs an approved action and returns an
/// [`ActionResult`]. Layer A implementations are **mocks** — no real network calls.
#[async_trait]
pub trait ActionExecutor: Send + Sync {
    /// Executes the action and returns the result.
    ///
    /// # Errors
    /// Returns [`crate::ActionError`] if execution cannot even start (e.g. an
    /// invalid request). A failure of the action itself is described as
    /// [`ActionStatus::Failed`] status in the result, not as an error.
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult>;
}

/// A test-oriented mock executor.
///
/// Replays a predetermined outcome without network calls: either succeeds,
/// returning the output given to it, or fails with the given explanation.
#[derive(Debug, Clone)]
pub struct MockActionExecutor {
    /// The outcome to return.
    status: ActionStatus,
    /// The output to return on success.
    output: Value,
    /// The summary text.
    summary: String,
    /// Whether to mark the output as untrusted (taint).
    untrusted: bool,
}

impl MockActionExecutor {
    /// A succeeding mock with the given output.
    ///
    /// The output is marked untrusted by default (`untrusted = true`).
    #[must_use]
    pub fn succeeding(output: Value) -> Self {
        Self {
            status: ActionStatus::Succeeded,
            output,
            summary: "mock action succeeded".to_string(),
            untrusted: true,
        }
    }

    /// A failing mock with the given explanation.
    #[must_use]
    pub fn failing(summary: impl Into<String>) -> Self {
        Self {
            status: ActionStatus::Failed,
            output: Value::Null,
            summary: summary.into(),
            untrusted: true,
        }
    }

    /// Marks the mock's output as trusted (removes the taint marker).
    #[must_use]
    pub const fn trusted(mut self) -> Self {
        self.untrusted = false;
        self
    }
}

#[async_trait]
impl ActionExecutor for MockActionExecutor {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let result = match self.status {
            ActionStatus::Succeeded => ActionResult {
                status: ActionStatus::Succeeded,
                output_summary: self.summary.clone(),
                untrusted: self.untrusted,
                raw_output_redacted: self.output.clone(),
                finished_at: request.now,
            },
            ActionStatus::Failed => ActionResult {
                status: ActionStatus::Failed,
                output_summary: self.summary.clone(),
                untrusted: self.untrusted,
                raw_output_redacted: Value::Null,
                finished_at: request.now,
            },
        };
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time::from_unix_secs;
    use serde_json::json;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    fn request() -> ActionRequest {
        ActionRequest::new(
            ActionId::new(),
            SkillId::new(),
            ActionTaskId::new(),
            json!({ "to": "general" }),
            at(1_700_000_000),
        )
    }

    #[tokio::test]
    async fn mock_success_returns_succeeded() {
        let exec = MockActionExecutor::succeeding(json!({ "ok": true }));
        let res = exec.execute(request()).await.expect("execute");
        assert!(res.status.is_success());
        assert_eq!(res.raw_output_redacted, json!({ "ok": true }));
        assert!(res.untrusted);
    }

    #[tokio::test]
    async fn mock_failure_returns_failed() {
        let exec = MockActionExecutor::failing("boom");
        let res = exec.execute(request()).await.expect("execute");
        assert_eq!(res.status, ActionStatus::Failed);
        assert_eq!(res.output_summary, "boom");
        assert_eq!(res.raw_output_redacted, Value::Null);
    }

    #[tokio::test]
    async fn trusted_mock_clears_taint() {
        let exec = MockActionExecutor::succeeding(json!({ "ok": true })).trusted();
        let res = exec.execute(request()).await.expect("execute");
        assert!(!res.untrusted);
    }

    #[test]
    fn action_result_constructors() {
        let ok = ActionResult::success("done", json!({ "x": 1 }), at(2));
        assert!(ok.status.is_success());
        assert!(ok.untrusted);
        let ok_trusted = ok.trusted();
        assert!(!ok_trusted.untrusted);

        let bad = ActionResult::failure("nope", at(3));
        assert_eq!(bad.status, ActionStatus::Failed);
    }
}

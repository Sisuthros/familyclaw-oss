//! Execution seam: the interface between orchestration and a concrete agent.
//!
//! This module defines **one shared seam** ([`TurnExecutor`]) through which
//! [`crate::orchestrator::Orchestrator`] delegates a single work step (a
//! "turn") to a concrete executor, without ever depending on any specific
//! agent, LLM provider, or transport layer.
//!
//! ## Seam model
//! The orchestrator produces an [`OrchestratedTurn`] description (what, who,
//! with what input, when) and passes it to a `TurnExecutor` implementation.
//! The implementation returns a [`crate::contract::Deliverable`], which can be
//! run through [`crate::contract::ContractBoard::fulfill`] verification
//! against the output schema and postconditions. This way, contract
//! guarantees hold regardless of who actually executed the turn.
//!
//! ## Division of responsibility
//! - **Consumer side (this module):** the seam's type + the hermetic
//!   [`MockTurnExecutor`] for deterministic testing and local runs.
//! - **Producer side (later):** the Layer B producer implements
//!   `LiveTurnExecutor` in the `familyclaw-agent` crate behind the **same**
//!   [`TurnExecutor`] trait, which plugs in the real LLM/transport layer
//!   without changing the orchestrator at all.
//!
//! ## Determinism
//! [`MockTurnExecutor`] does not read the clock or use randomness: the
//! deliverable's payload depends **only** on the [`OrchestratedTurn::input`]
//! input and the executor's identifier ([`OrchestratedTurn::assignee`]). The
//! same turn therefore always produces an identical deliverable.

use async_trait::async_trait;
use serde_json::{json, Value};

use familyclaw_core::ids::AgentId;
use familyclaw_core::time::Timestamp;
use familyclaw_core::Result;

use crate::contract::Deliverable;
use crate::orchestrator::NodeId;
use crate::task::TaskId;

/// A full description of a single delegatable work step ("turn") for an
/// executor.
///
/// The orchestrator builds this when it assigns a workflow node to a worker.
/// The description is self-contained: it includes the plan and node context,
/// the chosen executor, a human-readable title and description, a
/// machine-readable input, and the injected time (`now`). The timestamp is
/// always injected — the clock is never read within the seam.
#[derive(Debug, Clone)]
pub struct OrchestratedTurn {
    /// The human-readable identifier of the plan that produced this turn.
    pub plan_id: String,

    /// The workflow node's stable identifier (the name given by the planner).
    pub node_id: NodeId,

    /// The identifier of the task created on the task board.
    pub task_id: TaskId,

    /// The agent executing the turn (the worker chosen by the orchestrator).
    pub assignee: AgentId,

    /// Short title (taken from the node's title).
    pub title: String,

    /// Free-form description of the work step (taken from the node).
    pub description: String,

    /// Machine-readable input for the turn (typically validated against the
    /// capability's input schema before execution).
    pub input: Value,

    /// The injected execution time (UTC). The seam never reads the system
    /// clock — `now` is always supplied, for determinism.
    pub now: Timestamp,
}

impl OrchestratedTurn {
    /// Builds a turn description with all fields.
    ///
    /// `now` is the timestamp to inject (UTC); the caller is responsible for
    /// its determinism, so that the seam stays reproducible.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        plan_id: impl Into<String>,
        node_id: impl Into<NodeId>,
        task_id: TaskId,
        assignee: AgentId,
        title: impl Into<String>,
        description: impl Into<String>,
        input: Value,
        now: Timestamp,
    ) -> Self {
        Self {
            plan_id: plan_id.into(),
            node_id: node_id.into(),
            task_id,
            assignee,
            title: title.into(),
            description: description.into(),
            input,
            now,
        }
    }
}

/// The seam interface through which the orchestrator executes a single turn.
///
/// This is **the seam**: [`crate::orchestrator::Orchestrator`] depends on
/// this trait, never on a concrete agent. The consumer side (orchestrator +
/// [`MockTurnExecutor`]) lives in this crate; the producer side
/// (`LiveTurnExecutor` in the `familyclaw-agent` crate) is meant to implement
/// **the same** interface, so that the real LLM/transport layer plugs in
/// without changing the orchestrator.
///
/// Implementations must be [`Send`] + [`Sync`] so they can be shared across
/// tasks (`Arc<dyn TurnExecutor>`).
#[async_trait]
pub trait TurnExecutor: Send + Sync {
    /// Executes the given turn and returns the deliverable.
    ///
    /// The returned [`Deliverable`] can be run through
    /// [`crate::contract::ContractBoard::fulfill`] verification: only a
    /// deliverable that passes the output schema and postconditions fulfills
    /// the contract.
    ///
    /// # Errors
    /// Returns a [`familyclaw_core::FamilyClawError`] if execution fails
    /// (e.g. a producer-side transport or capability error). The hermetic
    /// [`MockTurnExecutor`] only returns an error in the `failing`-mode
    /// [`MockFailure::Error`] variant.
    async fn execute(&self, turn: OrchestratedTurn) -> Result<Deliverable>;
}

/// The way [`MockTurnExecutor`] simulates a failing execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockFailure {
    /// Execution produces a deliverable whose payload **breaches** the
    /// typical output schema (missing `headline` field). This proves the
    /// `fulfill` verification's `Failed` path without an error from
    /// execution itself.
    SchemaBreach,

    /// Execution returns an [`Err`], simulating a transport/capability
    /// error, so the turn never completes.
    Error,
}

/// A hermetic, deterministic [`TurnExecutor`] implementation for testing and
/// local runs.
///
/// **No network, no clock, no randomness.** The deliverable's payload is
/// derived solely from the [`OrchestratedTurn::input`] input and the
/// [`OrchestratedTurn::assignee`] executor, so the same turn always produces
/// an identical deliverable.
///
/// ## Payload shape
/// - If the input is an object containing a `brand` **or** `audience` field,
///   the mock produces a `HomepageDesign`-shaped deliverable:
///   `{ headline, sections, cta }`, with values derived deterministically
///   from the input.
/// - Otherwise, the input is echoed back under the `result` key together
///   with the executor's identifier (`assignee`).
///
/// ## Failure mode
/// [`MockTurnExecutor::failing`] returns an implementation that produces
/// either a breaching deliverable or an error ([`MockFailure`]) — this lets
/// tests verify the `Failed` path.
#[derive(Debug, Clone, Copy, Default)]
pub struct MockTurnExecutor {
    /// `None` = succeeding (deterministic) mode; `Some(_)` = simulated
    /// failure of the chosen kind.
    failure: Option<MockFailure>,
}

impl MockTurnExecutor {
    /// Builds a succeeding, deterministic mock.
    #[must_use]
    pub fn new() -> Self {
        Self { failure: None }
    }

    /// Builds a mock that **breaches the output schema** (missing
    /// `headline`).
    ///
    /// This is a shortcut for [`MockTurnExecutor::with_failure`] with the
    /// [`MockFailure::SchemaBreach`] mode; the produced deliverable makes
    /// [`crate::contract::ContractBoard::fulfill`] verification move the
    /// contract to the [`crate::contract::ContractStatus::Failed`] state.
    #[must_use]
    pub fn failing() -> Self {
        Self {
            failure: Some(MockFailure::SchemaBreach),
        }
    }

    /// Builds a mock with the chosen failure mode.
    #[must_use]
    pub fn with_failure(failure: MockFailure) -> Self {
        Self {
            failure: Some(failure),
        }
    }

    /// Derives the succeeding deliverable's payload purely from the turn.
    ///
    /// Depends only on `turn.input` and `turn.assignee` — never on the clock
    /// or randomness.
    fn success_payload(turn: &OrchestratedTurn) -> Value {
        let is_homepage = turn
            .input
            .as_object()
            .is_some_and(|o| o.contains_key("brand") || o.contains_key("audience"));

        if is_homepage {
            let brand = turn
                .input
                .get("brand")
                .and_then(Value::as_str)
                .unwrap_or("Brand");
            let audience = turn
                .input
                .get("audience")
                .and_then(Value::as_str)
                .unwrap_or("everyone");
            json!({
                "headline": format!("{brand} for {audience}"),
                "sections": [
                    { "kind": "hero", "title": format!("Welcome to {brand}") },
                    { "kind": "features", "title": "Why us" },
                    { "kind": "testimonials", "title": format!("Loved by {audience}") },
                ],
                "cta": format!("Get started with {brand}"),
            })
        } else {
            json!({
                "result": turn.input.clone(),
                "assignee": turn.assignee.to_string(),
            })
        }
    }
}

#[async_trait]
impl TurnExecutor for MockTurnExecutor {
    /// Executes the turn deterministically (or simulates a failure).
    ///
    /// # Errors
    /// Returns a [`familyclaw_core::FamilyClawError::Llm`] (a simulated
    /// producer-side transport/capability error) only when the mock was
    /// built with the [`MockFailure::Error`] mode.
    async fn execute(&self, turn: OrchestratedTurn) -> Result<Deliverable> {
        match self.failure {
            Some(MockFailure::Error) => Err(familyclaw_core::FamilyClawError::llm(format!(
                "mock executor failed turn for node {}",
                turn.node_id
            ))),
            Some(MockFailure::SchemaBreach) => {
                // Deliberately without a `headline` field → breaches the
                // typical HomepageDesign output schema, but is still a valid deliverable.
                let payload = json!({
                    "sections": [],
                    "cta": "",
                });
                Ok(Deliverable::new(turn.assignee, payload, turn.now))
            }
            None => {
                let payload = Self::success_payload(&turn);
                Ok(Deliverable::new(turn.assignee, payload, turn.now))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Capability, ContractBoard, ContractStatus, Field, FieldType, Schema};
    use familyclaw_core::time;
    use serde_json::json;

    fn ts(secs: i64) -> Timestamp {
        time::from_unix_secs(secs).expect("valid unix seconds")
    }

    fn turn_with(input: Value, assignee: AgentId) -> OrchestratedTurn {
        OrchestratedTurn::new(
            "plan",
            "node",
            TaskId::new(),
            assignee,
            "title",
            "description",
            input,
            ts(1000),
        )
    }

    /// A HomepageDesign-shaped output schema that the mock's succeeding
    /// deliverable satisfies but the `failing()` deliverable does not.
    fn homepage_capability() -> Capability {
        Capability::new(
            "design_homepage",
            Schema::empty(),
            Schema::new(vec![
                Field::required("headline", FieldType::Str),
                Field::required("sections", FieldType::Arr),
                Field::required("cta", FieldType::Str),
            ]),
        )
    }

    #[tokio::test]
    async fn execute_sets_from_to_assignee() {
        let mock = MockTurnExecutor::new();
        let assignee = AgentId::new();
        let turn = turn_with(json!({ "x": 1 }), assignee);
        let deliverable = mock.execute(turn).await.expect("execute");
        assert_eq!(deliverable.from, assignee);
    }

    #[tokio::test]
    async fn execute_uses_injected_now_for_deliverable_at() {
        let mock = MockTurnExecutor::new();
        let turn = turn_with(json!({ "x": 1 }), AgentId::new());
        let deliverable = mock.execute(turn).await.expect("execute");
        assert_eq!(deliverable.at, ts(1000));
    }

    #[tokio::test]
    async fn execute_is_deterministic_same_turn_twice() {
        let mock = MockTurnExecutor::new();
        let assignee = AgentId::new();
        // Same input + same executor → identical payload.
        let d1 = mock
            .execute(turn_with(json!({ "topic": "rust" }), assignee))
            .await
            .expect("d1");
        let d2 = mock
            .execute(turn_with(json!({ "topic": "rust" }), assignee))
            .await
            .expect("d2");
        assert_eq!(d1.payload, d2.payload);
    }

    #[tokio::test]
    async fn execute_payload_depends_on_assignee() {
        let mock = MockTurnExecutor::new();
        // Non-homepage input also echoes the executor's identifier → different payload.
        let d1 = mock
            .execute(turn_with(json!({ "topic": "rust" }), AgentId::new()))
            .await
            .expect("d1");
        let d2 = mock
            .execute(turn_with(json!({ "topic": "rust" }), AgentId::new()))
            .await
            .expect("d2");
        assert_ne!(d1.payload, d2.payload);
    }

    #[tokio::test]
    async fn execute_homepage_shaped_input_yields_homepage_payload() {
        let mock = MockTurnExecutor::new();
        let turn = turn_with(
            json!({ "brand": "DuckUps", "audience": "founders" }),
            AgentId::new(),
        );
        let deliverable = mock.execute(turn).await.expect("execute");
        let payload = &deliverable.payload;
        assert!(payload.get("headline").and_then(Value::as_str).is_some());
        assert!(payload.get("sections").and_then(Value::as_array).is_some());
        assert!(payload.get("cta").and_then(Value::as_str).is_some());
        // Sections is not empty.
        assert!(!payload["sections"].as_array().expect("array").is_empty());
    }

    #[tokio::test]
    async fn execute_homepage_payload_satisfies_output_schema() {
        // A succeeding deliverable must satisfy the HomepageDesign output
        // schema in ContractBoard.fulfill verification.
        let board = ContractBoard::new();
        let cap = homepage_capability();
        let provider = AgentId::new();
        let contract = board
            .propose(&cap, AgentId::new(), provider, json!({}), ts(1))
            .await
            .expect("propose");
        board.accept(contract.id, ts(2)).await.expect("accept");

        let mock = MockTurnExecutor::new();
        let turn = turn_with(
            json!({ "brand": "DuckUps", "audience": "founders" }),
            provider,
        );
        let deliverable = mock.execute(turn).await.expect("execute");

        let fulfilled = board
            .fulfill(contract.id, deliverable, ts(3))
            .await
            .expect("fulfill");
        assert_eq!(fulfilled.status, ContractStatus::Fulfilled);
    }

    #[tokio::test]
    async fn execute_non_homepage_input_echoes_under_result() {
        let mock = MockTurnExecutor::new();
        let assignee = AgentId::new();
        let turn = turn_with(json!({ "ping": "pong" }), assignee);
        let deliverable = mock.execute(turn).await.expect("execute");
        assert_eq!(deliverable.payload["result"], json!({ "ping": "pong" }));
        assert_eq!(deliverable.payload["assignee"], json!(assignee.to_string()));
    }

    #[tokio::test]
    async fn failing_variant_breaches_output_schema() {
        // failing() produces a deliverable without a `headline` field →
        // the HomepageDesign output schema is breached at the Schema.check level.
        let cap = homepage_capability();
        let mock = MockTurnExecutor::failing();
        let provider = AgentId::new();
        let deliverable = mock
            .execute(turn_with(json!({ "brand": "X" }), provider))
            .await
            .expect("execute (failing still returns a deliverable)");

        let violations = cap.output.check(&deliverable.payload);
        assert!(
            !violations.is_empty(),
            "failing() payload should breach output schema"
        );
        assert!(violations.iter().any(|v| v.field == "headline"));
    }

    #[tokio::test]
    async fn failing_variant_drives_contract_to_failed() {
        // Same proof via the ContractBoard.fulfill path:
        // a breaching deliverable moves the contract to the Failed state.
        let board = ContractBoard::new();
        let cap = homepage_capability();
        let provider = AgentId::new();
        let contract = board
            .propose(&cap, AgentId::new(), provider, json!({}), ts(1))
            .await
            .expect("propose");
        board.accept(contract.id, ts(2)).await.expect("accept");

        let mock = MockTurnExecutor::failing();
        let deliverable = mock
            .execute(turn_with(json!({ "brand": "X" }), provider))
            .await
            .expect("execute");

        let err = board
            .fulfill(contract.id, deliverable, ts(3))
            .await
            .expect_err("output schema breach");
        assert!(matches!(
            err,
            crate::contract::ContractError::OutputSchemaViolation(_)
        ));
        let after = board.get(contract.id).await.expect("present");
        assert_eq!(after.status, ContractStatus::Failed);
    }

    #[tokio::test]
    async fn error_variant_returns_err() {
        let mock = MockTurnExecutor::with_failure(MockFailure::Error);
        let result = mock
            .execute(turn_with(json!({ "x": 1 }), AgentId::new()))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn dyn_trait_object_is_usable() {
        // The seam is usable as a trait object (the orchestrator holds Arc<dyn _>).
        let exec: std::sync::Arc<dyn TurnExecutor> = std::sync::Arc::new(MockTurnExecutor::new());
        let assignee = AgentId::new();
        let deliverable = exec
            .execute(turn_with(json!({ "x": 1 }), assignee))
            .await
            .expect("execute via dyn");
        assert_eq!(deliverable.from, assignee);
    }
}

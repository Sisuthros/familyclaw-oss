//! # familyclaw-bridge
//!
//! The bridge layer (Layer A, OSS): an **agent registry, task board, and
//! event bus** as a pure, transport-layer-independent Rust interface.
//! Design §3: *"use what already exists"* —
//! this crate models the semantics of an existing `family-bridge` MCP as
//! native Rust, which MCP/HTTP adapters can wrap later.
//!
//! ## Parts
//! - [`agent`] — [`AgentRegistry`], [`AgentInfo`], liveness/heartbeat.
//! - [`task`] — [`Task`], the [`TaskStatus`] state machine, [`TaskBoard`] (incl. handoff).
//! - [`event`] — [`Event`], [`EventKind`], publish/subscribe ([`EventBus`]).
//! - [`work_executor`] — the [`WorkExecutor`] seam and [`DefaultSimulatingExecutor`]
//!   (Homepage Factory, Layer A producer).
//! - [`executor`] — the execution seam between orchestration and a concrete
//!   agent ([`TurnExecutor`], [`OrchestratedTurn`]) with the hermetic
//!   [`MockTurnExecutor`].
//! - [`bridge`] — [`FamilyBridge`] composes the above and publishes
//!   events on state changes.
//! - [`orchestrator`] — DAG-based multi-agent orchestration
//!   ([`OrchestrationPlan`], [`Orchestrator`]) that drives the task board
//!   only through legal state transitions.
//! - [`contract`] — typed FIPA `ContractNet` ([`Capability`], [`Contract`],
//!   [`ContractBoard`]) with verifiable fulfillment (schema + postconditions).
//! - [`contract_bus`] — transport-independent contract messages
//!   ([`ContractMessage`]) with plain serde.
//!
//! ## Design principles
//! - **Tokio-based, thread-safe.** Shared state is `Arc<RwLock<…>>`
//!   (registry, board) or `tokio::sync::broadcast` (bus). All
//!   facades are `Clone` and share their state.
//! - **No `unwrap()`/`expect()`/`panic!()` on the production path.** All
//!   errors flow through the [`familyclaw_core::Result`] and
//!   [`familyclaw_core::FamilyClawError`] types.
//! - **Strict task state machine.** Illegal transitions are rejected with
//!   an error, so durable replay and consolidation stay consistent.
//! - **OSS boundary (Layer A):** no hardcoded souls, keys, tokens, IP
//!   addresses, or personal paths. Types are generic.
//!
//! ## Example
//! ```
//! use familyclaw_bridge::{
//!     AgentInfo, AgentRole, FamilyBridge, HostKind, TaskStatus,
//! };
//! use familyclaw_core::ids::AgentId;
//!
//! # async fn run() -> familyclaw_core::Result<()> {
//! let bridge = FamilyBridge::new();
//! let mut events = bridge.subscribe();
//!
//! // Register two agents.
//! let a = AgentInfo::new(AgentId::new(), "agent_a", AgentRole::Strategy, HostKind::Local);
//! let b = AgentInfo::new(AgentId::new(), "agent_b", AgentRole::Executor, HostKind::Wsl);
//! let (a_id, b_id) = (a.id, b.id);
//! bridge.register_agent(a).await?;
//! bridge.register_agent(b).await?;
//!
//! // Create a task, pick it up, hand it off to the other agent.
//! let task = bridge.create_task("ship the seed", Some(a_id)).await?;
//! bridge.update_task_status(task.id, TaskStatus::Active).await?;
//! let handed = bridge.handoff_task(task.id, a_id, b_id).await?;
//! assert_eq!(handed.assignee, Some(b_id));
//! assert_eq!(handed.status, TaskStatus::Handed);
//! # Ok(())
//! # }
//! ```

pub mod agent;
pub mod bridge;
pub mod contract;
pub mod contract_bus;
pub mod event;
pub mod executor;
pub mod orchestrator;
pub mod task;
pub mod work_executor;

pub use agent::{AgentInfo, AgentRegistry, AgentRole, HostKind, Liveness};
pub use bridge::FamilyBridge;
pub use contract::{
    Capability, CapabilityRegistry, Clause, ClauseOp, Contract, ContractBoard, ContractError,
    ContractResult, ContractStatus, Deliverable, Field, FieldType, Schema, SchemaViolation,
};
pub use contract_bus::{ContractMessage, CONTRACT_CUSTOM_NAME};
pub use event::{Event, EventBus, EventKind, EventSubscriber};
pub use executor::{MockFailure, MockTurnExecutor, OrchestratedTurn, TurnExecutor};
pub use orchestrator::{
    NodeId, OrchestrationPlan, Orchestrator, RunReport, TaskNode, MAX_DELEGATION_DEPTH,
    STEP_ASSIGNED, STEP_FAILED, WORKFLOW_DONE,
};
pub use task::{Task, TaskBoard, TaskId, TaskStatus};
pub use work_executor::{DefaultSimulatingExecutor, WorkExecutor, WorkOutcome};

/// The crate's version at build time (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::ids::AgentId;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[tokio::test]
    async fn public_api_is_reexported() {
        // Verifies that the public surface is available from the crate root.
        // If any re-export is removed, this test will fail to compile.
        let bridge: FamilyBridge = FamilyBridge::new();
        let _registry: &AgentRegistry = bridge.registry();
        let _board: &TaskBoard = bridge.board();
        let _bus: &EventBus = bridge.bus();
        let mut _sub: EventSubscriber = bridge.subscribe();

        let info: AgentInfo = AgentInfo::new(
            AgentId::new(),
            "agent_a",
            AgentRole::Executor,
            HostKind::Local,
        );
        let id = info.id;
        bridge.register_agent(info).await.expect("register");
        assert_eq!(
            bridge.liveness(id).await.expect("liveness"),
            Liveness::Unknown
        );

        let task: Task = bridge.create_task("t", Some(id)).await.expect("create");
        let tid: TaskId = task.id;
        assert!(!tid.is_nil());
        assert_eq!(task.status, TaskStatus::Pending);

        let _ev: Event = Event::new(EventKind::Custom("x".into()), None);
    }
}
